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
    /// GRUB: the previous start of this system was interrupted (grubenv
    /// `recordfail=1`), so GRUB shows its menu for recovery.
    #[serde(default)]
    pub recordfail: bool,
    /// GRUB hidden by bootpick: also hidden after an interrupted start.
    #[serde(default)]
    pub hide_always: bool,
    /// Unticked GRUB block from an older bootpick that misses the system's
    /// own grubenv (see `upgrade`).
    #[serde(skip)]
    pub outdated: bool,
}

impl LoaderMenu {
    /// Offered for hiding: shown, changeable, not already handled by bootpick.
    pub fn can_hide(&self) -> bool {
        self.visible && self.problem.is_none() && !self.managed
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

/// `recordfail=1` in GRUB's environment block: GRUB sets it before every
/// boot and the system clears it once it has started, so it stays set after a
/// forced power-off, or when the system cannot reach this grubenv.
pub fn grubenv_recordfail(env: &str) -> bool {
    env.lines().any(|l| l.trim() == "recordfail=1")
}

/// (visible, timeout) from a generated grub.cfg plus custom.cfg, following
/// the `recordfail` branches the way GRUB will take them. Other `if`s are
/// treated as taken (both branches), which is close enough for timeouts.
pub fn grub_state(cfg: &str, custom: Option<&str>, recordfail: bool) -> (bool, Option<u32>) {
    let mut timeout: Option<i64> = None;
    let mut style = String::from("menu");
    let mut scan = |text: &str| {
        // Per open `if`: Some(active) for recordfail tests, None otherwise.
        let mut stack: Vec<Option<bool>> = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with("if ") {
                let rf = l.contains("recordfail}");
                let cond = if !rf {
                    None
                } else if l.contains("!= 1") || l.contains("!= \"1\"") {
                    Some(!recordfail)
                } else {
                    Some(recordfail)
                };
                stack.push(cond);
                continue;
            }
            if l.starts_with("else") || l.starts_with("elif") {
                if let Some(Some(t)) = stack.last_mut() {
                    *t = !*t;
                }
                continue;
            }
            if l == "fi" || l.starts_with("fi ") || l.starts_with("fi;") {
                stack.pop();
                continue;
            }
            if stack.iter().any(|b| *b == Some(false)) {
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

/// The bootpick block inside a custom.cfg ("" when absent).
fn block_of(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == BEGIN {
            inside = true;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
            if line.trim() == END {
                break;
            }
        }
    }
    out
}

/// `always`: hide even after an interrupted start; otherwise GRUB still shows
/// its menu (30 s) when the previous start of that system did not finish.
/// `os_root`: UUID of the system's own filesystem when GRUB lives elsewhere
/// (an ESP): that system clears `recordfail` only in its own
/// /boot/grub/grubenv, so the flag is read and written there instead.
fn grub_block_for(always: bool, os_root: Option<&str>) -> String {
    if always {
        return grub_block();
    }
    let mut b = format!("{}\n", BEGIN);
    if let Some(u) = os_root {
        b.push_str(&format!(
            "set bp_env=\nif search --no-floppy --fs-uuid --set=bp_root {u}; then\n  if [ -s ($bp_root)/boot/grub/grubenv ]; then\n    set bp_env=($bp_root)/boot/grub/grubenv\n    unset recordfail\n    load_env -f \"${{bp_env}}\" recordfail\n  fi\nfi\nexport bp_env\nfunction recordfail {{\n  set recordfail=1\n  if [ -n \"${{bp_env}}\" ]; then\n    save_env -f \"${{bp_env}}\" recordfail\n  elif [ -n \"${{have_grubenv}}\" ]; then\n    if [ -z \"${{boot_once}}\" ]; then save_env recordfail; fi\n  fi\n}}\n"
        ));
    }
    b.push_str(&format!("if [ \"${{recordfail}}\" != 1 ]; then\n  set timeout_style=hidden\n  set timeout=0\nfi\n{}\n", END));
    b
}

/// UUID of the system a GRUB boots when its files are not on that
/// system's root filesystem (e.g. GRUB kept on the ESP).
fn os_root_uuid(fs_root: &Path, cfg: &str, grub_uuid: &str) -> Option<String> {
    if ["etc/os-release", "usr/lib/os-release"].iter().any(|r| fs_root.join(r).exists()) {
        return None;
    }
    let u = host::grub_linux_root(cfg)?.strip_prefix("UUID=")?.to_string();
    (!u.eq_ignore_ascii_case(grub_uuid)).then_some(u)
}

const EMPTY_GRUBENV_HEAD: &str = "# GRUB Environment Block\n# WARNING: Do not edit this file by tools other than grub-editenv!!!\n";

fn empty_grubenv() -> String {
    let mut s = EMPTY_GRUBENV_HEAD.to_string();
    s.push_str(&"#".repeat(1024 - s.len()));
    s
}

fn grub_block() -> String {
    // Unconditional: a stuck `recordfail=1` would otherwise bring the menu
    // back on every boot. Shift / Esc still open it.
    format!("{}\nset timeout_style=hidden\nset timeout=0\n{}\n", BEGIN, END)
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
                out.push(LoaderMenu { id, kind: MenuKind::Grub, name: vendor.clone(), location: esp.label.clone(), visible: true, timeout: None, managed: false, problem: Some(Problem::Unreadable), recordfail: false, hide_always: false, outdated: false });
                continue;
            };
            let dir = root.join(&rel);
            let name = host::detect_grub(&root, &rel, &|spec| env.detect(spec)).map(|d| d.name).unwrap_or_else(|| vendor.clone());
            let Ok(cfg) = fs::read_to_string(dir.join("grub.cfg")) else {
                out.push(LoaderMenu { id, kind: MenuKind::Grub, name, location: label, visible: true, timeout: None, managed: false, problem: Some(Problem::Unreadable), recordfail: false, hide_always: false, outdated: false });
                continue;
            };
            let custom = fs::read_to_string(dir.join("custom.cfg")).ok();
            let managed = custom.as_deref().map(|c| c.contains(BEGIN)).unwrap_or(false);
            let block = custom.as_deref().map(block_of).unwrap_or_default();
            let read_rf = |d: &Path| fs::read_to_string(d.join("grubenv")).map(|e| grubenv_recordfail(&e)).unwrap_or(false);
            let os_root = os_root_uuid(&root, &cfg, &uuid);
            let outdated = managed && !block.contains("bp_env") && block.contains("recordfail") && os_root.is_some();
            let ext = if block.contains("bp_env") { os_root.and_then(|u| env.fs_by_uuid(&u, false)) } else { None };
            let recordfail = match &ext {
                Some((r, _, _)) => read_rf(&r.join("boot/grub")),
                None => read_rf(&dir),
            };
            drop(ext);
            let (visible, timeout) = grub_state(&cfg, custom.as_deref(), recordfail);
            let problem = if sources_custom(&cfg) { None } else { Some(Problem::NoCustomCfg) };
            let hide_always = managed && !block.contains("recordfail");
            out.push(LoaderMenu { id, kind: MenuKind::Grub, name, location: label, visible, timeout, managed, problem, recordfail, hide_always, outdated });
        }
    }
}

fn set_grub(env: &Env, uuid: &str, visible: bool, always: bool) -> Result<(), String> {
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
                let cfg = fs::read_to_string(root.join(&rel).join("grub.cfg")).unwrap_or_default();
                let os_root = if always { None } else { os_root_uuid(&root, &cfg, uuid) };
                // The system's own grubenv must exist for GRUB to use it.
                if let Some(u) = &os_root {
                    let (r, _, _g) = env.fs_by_uuid(u, true).ok_or("cannot open the system's partition to track interrupted starts")?;
                    let env_path = r.join("boot/grub/grubenv");
                    if !env_path.exists() {
                        fs::write(&env_path, empty_grubenv()).map_err(|e| format!("{}: {}", env_path.display(), e))?;
                    }
                }
                new.push_str(&grub_block_for(always, os_root.as_deref()));
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
        out.push(LoaderMenu { id: format!("sd:{}", esp.key), kind: MenuKind::SystemdBoot, name: sd_title(&mut st), location: esp.label.clone(), visible, timeout, managed: conf.contains(SD_MARK), problem: None, recordfail: false, hide_always: false, outdated: false });
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

/// `always` (GRUB only): keep the menu hidden after an interrupted start too.
pub fn set(env: &Env, id: &str, visible: bool, always: bool) -> Result<(), String> {
    let r = if let Some(u) = id.strip_prefix("grub:") {
        set_grub(env, u, visible, always)
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

/// Rewrites "unticked" GRUB blocks written before bootpick tracked
/// interrupted starts in the system's own grubenv (GRUB kept on an ESP).
pub fn upgrade(env: &Env) -> usize {
    let mut n = 0;
    for m in scan(env) {
        if m.kind != MenuKind::Grub || !m.managed || m.hide_always || !m.outdated {
            continue;
        }
        if set(env, &m.id, false, false).is_ok() {
            n += 1;
        }
    }
    n
}

/// Shows again every menu bootpick hid (on disable and uninstall).
pub fn restore_all(env: &Env) -> usize {
    let mut n = 0;
    for m in scan(env) {
        if m.managed && set(env, &m.id, true, true).is_ok() {
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
        assert_eq!(grub_state(UBUNTU_HEADER, None, false), (true, Some(10)));
        assert_eq!(grub_state(UBUNTU_HEADER, None, true), (true, Some(30)));
        // An old conditional block does not hide a menu stuck in recordfail.
        let old = format!("{}\nif [ \"${{recordfail}}\" != 1 ]; then\n  set timeout_style=hidden\n  set timeout=0\nfi\n{}\n", BEGIN, END);
        assert!(grub_state(UBUNTU_HEADER, Some(&old), true).0);
        assert!(!grub_state(UBUNTU_HEADER, Some(&old), false).0);
        assert_eq!(grub_state(UBUNTU_HEADER, Some(&grub_block()), true), (false, Some(0)));
        assert!(grubenv_recordfail("# GRUB Environment Block\nrecordfail=1\n####"));
        let custom = grub_block();
        assert_eq!(grub_state(UBUNTU_HEADER, Some(&custom), false), (false, Some(0)));
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

        set(&env, "grub:U1", false, true).unwrap();
        set(&env, "sd:k1", false, true).unwrap();
        assert!(scan(&env)[0].hide_always);
        // Unticked "also after a forced shutdown": shown only when recordfail is set.
        set(&env, "grub:U1", false, false).unwrap();
        let g = &scan(&env)[0];
        assert!(g.managed && !g.hide_always && !g.visible);
        fs::write(root.join("boot/grub/grubenv"), "# GRUB Environment Block\nrecordfail=1\n").unwrap();
        let g = &scan(&env)[0];
        assert!(g.managed && g.visible && g.recordfail && !g.can_hide());
        fs::remove_file(root.join("boot/grub/grubenv")).unwrap();
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

    #[test]
    fn grub_on_esp_uses_the_systems_grubenv() {
        // elementary-style: GRUB files on the ESP (U1), system root elsewhere (U2).
        let base = std::env::temp_dir().join(format!("bootpick-menus-esp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let esp = base.join("esp");
        let gfs = base.join("gfs");
        let os = base.join("os");
        fs::create_dir_all(esp.join("EFI/ubuntu")).unwrap();
        fs::create_dir_all(gfs.join("EFI/ubuntu/grub")).unwrap();
        fs::create_dir_all(os.join("boot/grub")).unwrap();
        fs::create_dir_all(os.join("etc")).unwrap();
        fs::write(os.join("etc/os-release"), "ID=elementary\nNAME=\"elementary OS\"\n").unwrap();
        fs::write(esp.join("EFI/ubuntu/grub.cfg"), "search.fs_uuid U1 root\nset prefix=($root)'/EFI/ubuntu/grub'\nconfigfile $prefix/grub.cfg\n").unwrap();
        let cfg = format!("{}menuentry 'elementary' {{\n  linux /vmlinuz root=UUID=U2 ro\n}}\n", UBUNTU_HEADER);
        fs::write(gfs.join("EFI/ubuntu/grub/grub.cfg"), cfg).unwrap();
        // Stale flag GRUB left on the ESP: the system never clears it there.
        fs::write(gfs.join("EFI/ubuntu/grub/grubenv"), "# GRUB Environment Block\nrecordfail=1\n").unwrap();
        let dirs = vec![("k1".to_string(), "SATA p1".to_string(), esp.clone())];
        let mut ov = RootOverride::new();
        ov.insert("U1".into(), gfs.clone());
        ov.insert("U2".into(), os.clone());
        let env = Env::Demo { dirs: &dirs, overrides: &ov };

        // A block from an older bootpick is upgraded when the app loads.
        let old = format!("{}\nif [ \"${{recordfail}}\" != 1 ]; then\n  set timeout_style=hidden\n  set timeout=0\nfi\n{}\n", BEGIN, END);
        fs::write(gfs.join("EFI/ubuntu/grub/custom.cfg"), old).unwrap();
        assert!(scan(&env)[0].outdated);
        assert_eq!(upgrade(&env), 1);
        assert!(!scan(&env)[0].outdated);
        assert_eq!(upgrade(&env), 0);
        let custom = fs::read_to_string(gfs.join("EFI/ubuntu/grub/custom.cfg")).unwrap();
        assert!(custom.contains("--set=bp_root U2") && custom.contains("save_env -f"), "{}", custom);
        let env_file = fs::read_to_string(os.join("boot/grub/grubenv")).unwrap();
        assert_eq!(env_file.len(), 1024);
        let g = &scan(&env)[0];
        assert_eq!(g.name, "elementary OS");
        assert!(g.managed && !g.visible && !g.recordfail, "{:?}", g);
        fs::write(os.join("boot/grub/grubenv"), "# GRUB Environment Block\nrecordfail=1\n").unwrap();
        assert!(scan(&env)[0].visible);

        // Ticked: unconditional block, no extra environment.
        set(&env, "grub:U1", false, true).unwrap();
        assert!(!fs::read_to_string(gfs.join("EFI/ubuntu/grub/custom.cfg")).unwrap().contains("bp_env"));
        set(&env, "grub:U1", true, true).unwrap();
        assert!(!gfs.join("EFI/ubuntu/grub/custom.cfg").exists());
        let _ = fs::remove_dir_all(&base);
    }
}
