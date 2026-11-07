//! Second boot menus: GRUB or systemd-boot can show their own menu after
//! bootpick has already started a system. Detects them and hides or restores
//! them.
//!
//! - GRUB: a marked block in `<grub dir>/custom.cfg`, which Debian/Ubuntu
//!   style configs source at the end (`41_custom`), so it survives
//!   `update-grub`. Removing the block restores the menu.
//! - systemd-boot: `timeout 0` in `\loader\loader.conf`, with the previous
//!   value kept in a marker comment.

use crate::host::{self, DirStorage, Mounted, Part, RootOverride};
use bootpick_core::storage::Storage;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MenuKind {
    Grub,
    SystemdBoot,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Problem {
    /// The partition with the GRUB files could not be read.
    Unreadable,
    /// grub.cfg does not source custom.cfg.
    NoCustomCfg,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct LoaderMenu {
    /// "grub:<fs uuid>" or "sd:<ESP key>".
    pub id: String,
    pub kind: MenuKind,
    /// The operating system behind it ("elementary OS").
    pub name: String,
    /// Where it lives ("SATA p6").
    pub location: String,
    pub visible: bool,
    /// Seconds the menu waits; None = until the user chooses.
    pub timeout: Option<u32>,
    /// Hidden by bootpick (so it can be restored).
    pub managed: bool,
    pub problem: Option<Problem>,
}

impl LoaderMenu {
    pub fn can_hide(&self) -> bool {
        self.visible && self.problem.is_none()
    }
}

const BEGIN: &str = "# >>> bootpick: hide this menu (bootpick starts each system directly)";
const END: &str = "# <<< bootpick";
const SD_MARK: &str = "# bootpick: hid this menu; previous setting:";

/// Where the files are: real partitions (helper, root) or demo directories.
pub enum Env<'a> {
    Real { parts: &'a [Part] },
    Demo { dirs: &'a [(String, String, PathBuf)], overrides: &'a RootOverride },
}

type Guard = Option<Box<dyn Any>>;

struct Esp {
    key: String,
    label: String,
    dir: PathBuf,
    _guard: Guard,
}

impl Env<'_> {
    fn esps(&self, writable: bool) -> Vec<Esp> {
        match self {
            Env::Real { parts } => host::fat_volumes(parts)
                .into_iter()
                .filter_map(|p| {
                    let m = Mounted::open(&p, writable)?;
                    Some(Esp { key: p.partuuid.clone().unwrap_or_default().to_ascii_lowercase(), label: host::disk_label(&p), dir: m.dir.clone(), _guard: Some(Box::new(m)) })
                })
                .collect(),
            Env::Demo { dirs, .. } => dirs.iter().map(|(k, l, d)| Esp { key: k.clone(), label: l.clone(), dir: d.clone(), _guard: None }).collect(),
        }
    }

    /// The root directory of the filesystem with this UUID.
    fn fs_by_uuid(&self, uuid: &str, writable: bool) -> Option<(PathBuf, String, Guard)> {
        match self {
            Env::Real { parts } => {
                let p = parts.iter().find(|p| p.uuid.as_deref().map(|u| u.eq_ignore_ascii_case(uuid)).unwrap_or(false))?;
                let m = Mounted::open(p, writable)?;
                Some((m.dir.clone(), host::disk_label(p), Some(Box::new(m))))
            }
            Env::Demo { overrides, .. } => overrides.get(uuid).map(|d| (d.clone(), "demo".to_string(), None)),
        }
    }

    /// The system behind a `root=` spec (for naming a GRUB menu).
    fn detect(&self, spec: &str) -> Option<host::Detection> {
        match self {
            Env::Real { parts } => host::detect_spec(parts, &RootOverride::new(), spec),
            Env::Demo { overrides, .. } => host::detect_spec(&[], overrides, spec),
        }
    }

    fn real(&self) -> bool {
        matches!(self, Env::Real { .. })
    }
}

// ----- GRUB ------------------------------------------------------------------------------

/// (fs uuid, grub dir relative to that filesystem) from an ESP stub grub.cfg.
fn grub_stub(cfg: &str) -> Option<(String, String)> {
    Some((host::grub_stub_uuid(cfg)?, host::grub_prefix(cfg)))
}

/// (visible, timeout) from a generated grub.cfg plus custom.cfg. Assignments
/// in the `recordfail` branch (shown only after a failed boot) are ignored.
pub fn grub_state(cfg: &str, custom: Option<&str>) -> (bool, Option<u32>) {
    let mut timeout: Option<i64> = None;
    let mut style = String::from("menu");
    let mut scan = |text: &str| {
        let mut stack: Vec<bool> = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with("if ") {
                stack.push(l.contains("recordfail}\" = 1") || l.contains("recordfail}\" = \"1\""));
                continue;
            }
            if l.starts_with("else") || l.starts_with("elif") {
                if let Some(t) = stack.last_mut() {
                    *t = false;
                }
                continue;
            }
            if l == "fi" || l.starts_with("fi ") || l.starts_with("fi;") {
                stack.pop();
                continue;
            }
            if stack.iter().any(|b| *b) {
                continue;
            }
            if let Some(v) = l.strip_prefix("set timeout=") {
                timeout = v.trim_matches(|c| c == '"' || c == '\'').parse().ok();
            } else if let Some(v) = l.strip_prefix("set timeout_style=") {
                style = v.trim_matches(|c| c == '"' || c == '\'').to_string();
            }
        }
    };
    scan(cfg);
    if let Some(c) = custom.filter(|_| sources_custom(cfg)) {
        scan(c);
    }
    let t = timeout.unwrap_or(-1);
    let visible = style != "hidden" && t != 0;
    (visible, if t < 0 { None } else { Some(t as u32) })
}

fn sources_custom(cfg: &str) -> bool {
    cfg.lines().any(|l| l.contains("custom.cfg") && (l.contains("source") || l.contains("-f")))
}

fn strip_block(text: &str) -> String {
    let mut out = Vec::new();
    let mut skip = false;
    for line in text.lines() {
        if line.trim() == BEGIN {
            skip = true;
            continue;
        }
        if skip {
            if line.trim() == END {
                skip = false;
            }
            continue;
        }
        out.push(line);
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

fn grub_block() -> String {
    format!("{}\nif [ \"${{recordfail}}\" != 1 ]; then\n  set timeout_style=hidden\n  set timeout=0\nfi\n{}\n", BEGIN, END)
}

fn grub_menus(env: &Env, out: &mut Vec<LoaderMenu>) {
    let mut seen = Vec::new();
    for esp in env.esps(false) {
        let mut st = DirStorage::new(&esp.dir);
        for (vendor, is_dir) in st.list_dir("\\EFI") {
            if !is_dir || vendor.eq_ignore_ascii_case("bootpick") {
                continue;
            }
            let Ok(stub) = fs::read_to_string(st.resolve(&format!("\\EFI\\{}\\grub.cfg", vendor))) else { continue };
            let Some((uuid, rel)) = grub_stub(&stub) else { continue };
            if seen.contains(&uuid) {
                continue;
            }
            seen.push(uuid.clone());
            let id = format!("grub:{}", uuid);
            let Some((root, label, _g)) = env.fs_by_uuid(&uuid, false) else {
                out.push(LoaderMenu { id, kind: MenuKind::Grub, name: vendor.clone(), location: esp.label.clone(), visible: true, timeout: None, managed: false, problem: Some(Problem::Unreadable) });
                continue;
            };
            let dir = root.join(&rel);
            let name = host::detect_grub(&root, &rel, &|spec| env.detect(spec)).map(|d| d.name).unwrap_or_else(|| vendor.clone());
            let Ok(cfg) = fs::read_to_string(dir.join("grub.cfg")) else {
                out.push(LoaderMenu { id, kind: MenuKind::Grub, name, location: label, visible: true, timeout: None, managed: false, problem: Some(Problem::Unreadable) });
                continue;
            };
            let custom = fs::read_to_string(dir.join("custom.cfg")).ok();
            let (visible, timeout) = grub_state(&cfg, custom.as_deref());
            let managed = custom.as_deref().map(|c| c.contains(BEGIN)).unwrap_or(false);
            let problem = if sources_custom(&cfg) { None } else { Some(Problem::NoCustomCfg) };
            out.push(LoaderMenu { id, kind: MenuKind::Grub, name, location: label, visible, timeout, managed, problem });
        }
    }
}

fn set_grub(env: &Env, uuid: &str, visible: bool) -> Result<(), String> {
    for esp in env.esps(false) {
        let mut st = DirStorage::new(&esp.dir);
        for (vendor, is_dir) in st.list_dir("\\EFI") {
            if !is_dir {
                continue;
            }
            let Ok(stub) = fs::read_to_string(st.resolve(&format!("\\EFI\\{}\\grub.cfg", vendor))) else { continue };
            let Some((u, rel)) = grub_stub(&stub) else { continue };
            if !u.eq_ignore_ascii_case(uuid) {
                continue;
            }
            let (root, _, _g) = env.fs_by_uuid(uuid, true).ok_or("cannot open the partition with the GRUB files")?;
            let path = root.join(&rel).join("custom.cfg");
            let old = fs::read_to_string(&path).unwrap_or_default();
            let mut new = strip_block(&old);
            if !visible {
                if !new.is_empty() && !new.ends_with('\n') {
                    new.push('\n');
                }
                new.push_str(&grub_block());
            }
            if new.trim().is_empty() {
                if path.exists() {
                    fs::remove_file(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
                }
            } else {
                write_atomic(&path, &new)?;
            }
            return Ok(());
        }
    }
    Err("GRUB menu not found".into())
}

// ----- systemd-boot ----------------------------------------------------------------------

const SD_VENDOR: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

fn sd_efivar_timeout() -> Option<String> {
    let data = fs::read(format!("/sys/firmware/efi/efivars/LoaderConfigTimeout-{}", SD_VENDOR)).ok()?;
    let u: Vec<u16> = data.get(4..)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
    Some(String::from_utf16_lossy(&u))
}

/// (visible, timeout) for a loader.conf `timeout` value (None = not set).
fn sd_state(value: Option<&str>) -> (bool, Option<u32>) {
    match value.map(str::trim) {
        None | Some("0") | Some("menu-hidden") | Some("menu-disabled") => (false, Some(0)),
        Some("menu-force") => (true, None),
        Some(v) => match v.parse::<u32>() {
            Ok(n) => (n > 0, Some(n)),
            Err(_) => (true, None),
        },
    }
}

fn loader_timeout(conf: &str) -> Option<String> {
    let mut v = None;
    for line in conf.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("timeout") {
            if rest.starts_with(char::is_whitespace) {
                v = Some(rest.trim().to_string());
            }
        }
    }
    v
}

fn sd_title(st: &mut DirStorage) -> String {
    let mut names: Vec<String> = Vec::new();
    for (n, is_dir) in st.list_dir("\\loader\\entries") {
        if is_dir || !n.to_ascii_lowercase().ends_with(".conf") {
            continue;
        }
        if let Ok(t) = fs::read_to_string(st.resolve(&format!("\\loader\\entries\\{}", n))) {
            if let Some(title) = t.lines().find_map(|l| l.trim().strip_prefix("title").map(|s| s.trim().to_string())) {
                // "Pop!_OS" for "Pop!_OS", "Pop!_OS (recovery)"...
                let base = title.split(" (").next().unwrap_or(&title).to_string();
                if !names.contains(&base) {
                    names.push(base);
                }
            }
        }
    }
    names.sort_by_key(|n| n.len());
    names.into_iter().next().unwrap_or_else(|| "Linux".into())
}

fn sd_menus(env: &Env, out: &mut Vec<LoaderMenu>) {
    let efivar = if env.real() { sd_efivar_timeout() } else { None };
    for esp in env.esps(false) {
        let mut st = DirStorage::new(&esp.dir);
        let has_sd = st.exists("\\EFI\\systemd\\systemd-bootx64.efi") || st.exists("\\EFI\\BOOT\\BOOTX64.EFI") && st.exists("\\loader\\entries");
        let Ok(conf) = fs::read_to_string(st.resolve("\\loader\\loader.conf")) else { continue };
        if !has_sd {
            continue;
        }
        let file_value = loader_timeout(&conf);
        let value = efivar.clone().or(file_value);
        let (visible, timeout) = sd_state(value.as_deref());
        out.push(LoaderMenu { id: format!("sd:{}", esp.key), kind: MenuKind::SystemdBoot, name: sd_title(&mut st), location: esp.label.clone(), visible, timeout, managed: conf.contains(SD_MARK), problem: None });
    }
}

fn set_sd(env: &Env, key: &str, visible: bool) -> Result<(), String> {
    for esp in env.esps(true) {
        if esp.key != key {
            continue;
        }
        let path = DirStorage::new(&esp.dir).resolve("\\loader\\loader.conf");
        let conf = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut lines: Vec<String> = Vec::new();
        let mut previous: Option<String> = None;
        let mut skip_next_timeout = false;
        for line in conf.lines() {
            let l = line.trim();
            if let Some(prev) = l.strip_prefix(SD_MARK) {
                previous = Some(prev.trim().to_string());
                skip_next_timeout = true;
                continue;
            }
            if l.starts_with("timeout") && l[7..].starts_with(char::is_whitespace) {
                if skip_next_timeout {
                    skip_next_timeout = false;
                    continue;
                }
                if !visible {
                    previous.get_or_insert_with(|| l.to_string());
                    continue;
                }
            }
            lines.push(line.to_string());
        }
        if visible {
            if let Some(p) = previous.filter(|p| p != "none") {
                lines.push(p);
            }
        } else {
            lines.push(format!("{} {}", SD_MARK, previous.unwrap_or_else(|| "none".into())));
            lines.push("timeout 0".into());
            if env.real() && sd_efivar_timeout().is_some() {
                // A timeout set with `bootctl set-timeout` overrides loader.conf.
                let _ = std::process::Command::new("bootctl").args(["set-timeout", ""]).status();
            }
        }
        write_atomic(&path, &(lines.join("\n") + "\n"))?;
        return Ok(());
    }
    Err("systemd-boot menu not found".into())
}

// ----- public API ------------------------------------------------------------------------

pub fn scan(env: &Env) -> Vec<LoaderMenu> {
    let mut out = Vec::new();
    grub_menus(env, &mut out);
    sd_menus(env, &mut out);
    out
}

pub fn set(env: &Env, id: &str, visible: bool) -> Result<(), String> {
    let r = if let Some(u) = id.strip_prefix("grub:") {
        set_grub(env, u, visible)
    } else if let Some(k) = id.strip_prefix("sd:") {
        set_sd(env, k, visible)
    } else {
        Err(format!("unknown menu {}", id))
    };
    if env.real() {
        let _ = std::process::Command::new("sync").status();
    }
    r
}

/// Shows again every menu bootpick hid (on disable and uninstall).
pub fn restore_all(env: &Env) -> usize {
    let mut n = 0;
    for m in scan(env) {
        if m.managed && set(env, &m.id, true).is_ok() {
            n += 1;
        }
    }
    n
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("bootpick-tmp");
    fs::write(&tmp, text).map_err(|e| format!("{}: {}", path.display(), e))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UBUNTU_HEADER: &str = r#"
if [ "${recordfail}" = 1 ] ; then
  set timeout=30
else
  if [ x$feature_timeout_style = xy ] ; then
    set timeout_style=menu
    set timeout=10
  # Fallback normal timeout code in case the timeout_style feature is
  # unavailable.
  else
    set timeout=10
  fi
fi
### BEGIN /etc/grub.d/41_custom ###
if [ -f  ${config_directory}/custom.cfg ]; then
  source ${config_directory}/custom.cfg
elif [ -z "${config_directory}" -a -f  $prefix/custom.cfg ]; then
  source $prefix/custom.cfg
fi
"#;

    #[test]
    fn grub_visibility() {
        assert_eq!(grub_state(UBUNTU_HEADER, None), (true, Some(10)));
        let custom = grub_block();
        assert_eq!(grub_state(UBUNTU_HEADER, Some(&custom)), (false, Some(0)));
        assert!(sources_custom(UBUNTU_HEADER));
        assert!(!sources_custom("set timeout=5\n"));
    }

    #[test]
    fn grub_block_round_trip() {
        let user = "menuentry 'Mine' {\n  true\n}\n";
        let hidden = format!("{}{}", user, grub_block());
        assert_eq!(strip_block(&hidden), user);
        assert_eq!(strip_block(&grub_block()), "");
    }

    #[test]
    fn stub_prefix() {
        assert_eq!(grub_stub("search.fs_uuid abcd-1234 root\nset prefix=($root)'/boot/grub'\nconfigfile $prefix/grub.cfg\n"), Some(("abcd-1234".into(), "boot/grub".into())));
        assert_eq!(grub_stub("search.fs_uuid abcd-1234 root\nset prefix=($root)/grub\n"), Some(("abcd-1234".into(), "grub".into())));
    }

    #[test]
    fn sd_values() {
        assert_eq!(sd_state(None), (false, Some(0)));
        assert_eq!(sd_state(Some("5")), (true, Some(5)));
        assert_eq!(sd_state(Some("menu-force")), (true, None));
        assert_eq!(loader_timeout("default Pop_OS-current\ntimeout 5\n"), Some("5".into()));
    }

    #[test]
    fn hide_and_restore_round_trip() {
        let base = std::env::temp_dir().join(format!("bootpick-menus-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let esp = base.join("esp");
        let root = base.join("root");
        fs::create_dir_all(esp.join("EFI/ubuntu")).unwrap();
        fs::create_dir_all(esp.join("EFI/systemd")).unwrap();
        fs::create_dir_all(esp.join("loader/entries")).unwrap();
        fs::create_dir_all(root.join("boot/grub")).unwrap();
        fs::write(esp.join("EFI/ubuntu/grub.cfg"), "search.fs_uuid U1 root\nset prefix=($root)'/boot/grub'\nconfigfile $prefix/grub.cfg\n").unwrap();
        fs::write(esp.join("EFI/systemd/systemd-bootx64.efi"), b"x").unwrap();
        fs::write(esp.join("loader/loader.conf"), "default Pop_OS-current\ntimeout 5\n").unwrap();
        fs::write(esp.join("loader/entries/Pop_OS-current.conf"), "title Pop!_OS\n").unwrap();
        fs::write(root.join("boot/grub/grub.cfg"), UBUNTU_HEADER).unwrap();
        fs::write(root.join("boot/grub/custom.cfg"), "# mine\n").unwrap();
        let dirs = vec![("k1".to_string(), "SATA p1".to_string(), esp.clone())];
        let mut ov = RootOverride::new();
        ov.insert("U1".into(), root.clone());
        let env = Env::Demo { dirs: &dirs, overrides: &ov };

        let m = scan(&env);
        assert_eq!(m.len(), 2);
        assert!(m.iter().all(|m| m.visible && !m.managed && m.can_hide()));
        assert_eq!(m[1].name, "Pop!_OS");

        set(&env, "grub:U1", false).unwrap();
        set(&env, "sd:k1", false).unwrap();
        let m = scan(&env);
        assert!(m.iter().all(|m| !m.visible && m.managed), "{:?}", m);
        assert!(fs::read_to_string(root.join("boot/grub/custom.cfg")).unwrap().starts_with("# mine\n"));

        assert_eq!(restore_all(&env), 2);
        let m = scan(&env);
        assert!(m.iter().all(|m| m.visible && !m.managed), "{:?}", m);
        assert_eq!(fs::read_to_string(root.join("boot/grub/custom.cfg")).unwrap(), "# mine\n");
        assert_eq!(fs::read_to_string(esp.join("loader/loader.conf")).unwrap(), "default Pop_OS-current\ntimeout 5\n");
        let _ = fs::remove_dir_all(&base);
    }
}
