//! Host-side discovery: partitions and volumes (see the `sys` module), the same entry
//! scan the boot menu performs, firmware boot options (efivars) and real OS
//! names (os-release behind each loader).

use bootpick_core::scan::{parse_load_option, Kind, Scan};
use bootpick_core::storage::Storage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
pub use crate::sys::{partitions, Mounted};
use crate::sys;

pub const EFI_GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";

// ----- a directory as core Storage ---------------------------------------------------

/// Maps ESP-style paths (`\EFI\bootpick\x`) onto a directory, matching names
/// case-insensitively like FAT does.
pub struct DirStorage {
    pub root: PathBuf,
}

impl DirStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DirStorage { root: root.into() }
    }

    pub fn resolve(&self, p: &str) -> PathBuf {
        let mut cur = self.root.clone();
        for comp in p.split(['\\', '/']).filter(|c| !c.is_empty() && *c != "." && *c != "..") {
            let direct = cur.join(comp);
            if direct.exists() {
                cur = direct;
                continue;
            }
            let found = fs::read_dir(&cur).ok().and_then(|rd| {
                rd.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(comp)).map(|e| e.path())
            });
            cur = found.unwrap_or(direct);
        }
        cur
    }
}

impl Storage for DirStorage {
    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        let p = self.resolve(path);
        let meta = fs::symlink_metadata(&p).ok()?;
        if !meta.is_file() {
            return None;
        }
        fs::read(p).ok()
    }
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        let p = self.resolve(path);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&p, data).map_err(|e| e.to_string())
    }
    fn list_dir(&mut self, path: &str) -> Vec<(String, bool)> {
        let mut v: Vec<(String, bool)> = fs::read_dir(self.resolve(path))
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let ft = e.file_type().ok()?;
                        if ft.is_symlink() {
                            return None;
                        }
                        Some((e.file_name().to_string_lossy().into_owned(), ft.is_dir()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
    fn exists(&mut self, path: &str) -> bool {
        self.resolve(path).exists()
    }
}

// ----- partitions ---------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Part {
    /// Kernel name ("sda1"); only the Linux layer needs it.
    #[cfg_attr(windows, allow(dead_code))]
    pub name: String,
    pub path: String,
    pub pkname: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub tran: Option<String>,
    pub partn: Option<u32>,
    pub partuuid: Option<String>,
    pub parttype: Option<String>,
    pub fstype: Option<String>,
    pub uuid: Option<String>,
    pub mountpoint: Option<String>,
}

/// "NVMe p1", "SATA p9", "USB p1": the same labels the boot menu shows.
pub fn disk_label(p: &Part) -> String {
    let bus = match p.tran.as_deref() {
        Some("nvme") => "NVMe",
        Some("sata") | Some("ata") => "SATA",
        Some("usb") => "USB",
        Some("mmc") => "SD/eMMC",
        Some(other) if !other.is_empty() => other,
        _ => "",
    };
    match (bus, p.partn) {
        ("", Some(n)) => format!("partition {}", n),
        ("", None) => String::new(),
        (b, Some(n)) => format!("{} p{}", b, n),
        (b, None) => b.to_string(),
    }
}

/// Partitions the boot menu would scan: FAT file systems with a GPT GUID.
pub fn fat_volumes(parts: &[Part]) -> Vec<Part> {
    parts
        .iter()
        .filter(|p| p.kind.as_deref() == Some("part") && p.fstype.as_deref() == Some("vfat") && p.partuuid.is_some())
        .cloned()
        .collect()
}

pub fn is_esp(p: &Part) -> bool {
    p.parttype.as_deref().map(|t| t.eq_ignore_ascii_case(ESP_TYPE)).unwrap_or(false)
}

// ----- firmware variables ---------------------------------------------------------------

pub fn efivar(name: &str) -> Option<Vec<u8>> {
    sys::efivar_vendor(name, EFI_GLOBAL)
}

pub fn boot_order() -> Vec<u16> {
    efivar("BootOrder").map(|d| d.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()).unwrap_or_default()
}

/// (number, active, description) for every Boot#### in BootOrder.
pub fn boot_options() -> Vec<(u16, bool, String)> {
    boot_order()
        .into_iter()
        .filter_map(|n| {
            let data = efivar(&format!("Boot{:04X}", n))?;
            let (active, desc) = parse_load_option(&data)?;
            Some((n, active, desc))
        })
        .collect()
}

/// Every Boot#### variable present (also those not in BootOrder).
pub fn all_boot_options() -> Vec<(u16, bool, String)> {
    sys::boot_numbers()
        .into_iter()
        .filter_map(|n| {
            let data = efivar(&format!("Boot{:04X}", n))?;
            let (active, desc) = parse_load_option(&data)?;
            Some((n, active, desc))
        })
        .collect()
}

/// A firmware load option that starts `loader` (`\EFI\...`) from a GPT
/// partition: the contents of a Boot#### variable.
#[cfg_attr(not(windows), allow(dead_code))] // Linux creates entries with efibootmgr
pub fn load_option(label: &str, loader: &str, partn: u32, start_lba: u64, size_lba: u64, part_guid: [u8; 16]) -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect() };
    // Hard drive media node, file path node, end node.
    let mut dp = vec![0x04, 0x01, 42, 0];
    dp.extend(partn.to_le_bytes());
    dp.extend(start_lba.to_le_bytes());
    dp.extend(size_lba.to_le_bytes());
    dp.extend(part_guid);
    dp.extend([0x02, 0x02]); // GPT, GUID signature
    let path = utf16(loader);
    dp.extend([0x04, 0x04]);
    dp.extend(((4 + path.len()) as u16).to_le_bytes());
    dp.extend(path);
    dp.extend([0x7f, 0xff, 4, 0]);
    let mut v = Vec::new();
    v.extend(1u32.to_le_bytes()); // LOAD_OPTION_ACTIVE
    v.extend((dp.len() as u16).to_le_bytes());
    v.extend(utf16(label));
    v.extend(dp);
    v
}

/// Joins a `/`-separated relative path one component at a time (Windows
/// volume paths of the `\?\` form do not accept forward slashes).
pub fn sub(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|c| !c.is_empty()).fold(root.to_path_buf(), |p, c| p.join(c))
}

// ----- scan + OS detection ----------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Detection {
    /// os-release NAME (e.g. "elementary OS") or GRUB menu title.
    pub name: String,
    /// os-release ID (e.g. "elementary"), lowercase.
    pub id: String,
    pub version: String,
    /// Icon name the boot menu should use, when one ships for this OS.
    pub icon: String,
}

pub fn icon_for_os_id(id: &str) -> &'static str {
    match id {
        "pop" => "os_pop",
        "ubuntu" | "kubuntu" | "xubuntu" | "lubuntu" => "os_ubuntu",
        "elementary" => "os_elementary",
        "debian" => "os_debian",
        "fedora" => "os_fedora",
        "arch" | "endeavouros" => "os_arch",
        "linuxmint" => "os_mint",
        "manjaro" => "os_manjaro",
        _ => "os_linux",
    }
}

/// Where to look for the real OS behind an entry.
enum Hint {
    /// Contents of \EFI\<vendor>\grub.cfg on the ESP.
    GrubStub(String),
    /// Contents of a BLS entry (`options root=...`).
    Bls(String),
}

#[allow(dead_code)]
pub struct HostScan {
    pub scan: Scan,
    /// FAT partition for each `scan.volumes` index.
    pub vol_parts: Vec<Part>,
    pub detections: BTreeMap<String, Detection>,
    /// The partition holding \EFI\bootpick\bootpick.efi, if installed.
    pub bootpick_esp: Option<Part>,
}

/// Maps a filesystem UUID or device path to a directory; used by demo mode.
pub type RootOverride = BTreeMap<String, PathBuf>;

pub fn scan_host(parts: &[Part], overrides: &RootOverride) -> HostScan {
    let mut scan = Scan::default();
    let mut vol_parts = Vec::new();
    let mut hints: Vec<(String, Hint)> = Vec::new();
    let mut bootpick_esp = None;
    for p in fat_volumes(parts) {
        let Some(m) = Mounted::open(&p, false) else { continue };
        let mut st = DirStorage::new(&m.dir);
        let first = scan.entries.len();
        let key = p.partuuid.clone().unwrap_or_default().to_ascii_lowercase();
        scan.add_volume(&key, &disk_label(&p), &mut st);
        if st.exists("\\EFI\\bootpick\\bootpick.efi") && (bootpick_esp.is_none() || is_esp(&p)) {
            bootpick_esp = Some(p.clone());
        }
        collect_hints(&mut st, &scan, first, &mut hints);
        vol_parts.push(p);
    }
    scan.finish_volumes();
    for (n, active, desc) in boot_options() {
        if active {
            scan.add_firmware_option(n, &desc);
        }
    }
    scan.fw_setup_supported = efivar("OsIndicationsSupported").map(|d| d.first().map(|b| b & 1 != 0).unwrap_or(false)).unwrap_or(false);
    let detections = resolve_hints(parts, hints, overrides);
    HostScan { scan, vol_parts, detections, bootpick_esp }
}

/// Scans plain directories as volumes (demo mode, no root needed).
pub fn scan_dirs(dirs: &[(String, String, PathBuf)], overrides: &RootOverride) -> HostScan {
    let mut scan = Scan::default();
    let mut hints = Vec::new();
    for (key, disk, dir) in dirs {
        let mut st = DirStorage::new(dir);
        let first = scan.entries.len();
        scan.add_volume(key, disk, &mut st);
        collect_hints(&mut st, &scan, first, &mut hints);
    }
    scan.finish_volumes();
    scan.fw_setup_supported = true;
    let detections = resolve_hints(&[], hints, overrides);
    HostScan { scan, vol_parts: Vec::new(), detections, bootpick_esp: None }
}

fn collect_hints(st: &mut DirStorage, scan: &Scan, first: usize, hints: &mut Vec<(String, Hint)>) {
    for e in &scan.entries[first..] {
        match &e.kind {
            Kind::Efi { path, .. } => {
                let parts: Vec<&str> = path.split('\\').filter(|s| !s.is_empty()).collect();
                if parts.len() == 3 && parts[0].eq_ignore_ascii_case("efi") {
                    let file = parts[2].to_ascii_lowercase();
                    if file.starts_with("shim") || file.starts_with("grub") {
                        if let Some(cfg) = st.read_string(&format!("\\EFI\\{}\\grub.cfg", parts[1])) {
                            hints.push((e.id.clone(), Hint::GrubStub(cfg)));
                        }
                    }
                }
            }
            Kind::LinuxBls { entry, .. } => {
                if let Some(text) = st.read_string(&format!("\\loader\\entries\\{}", entry)) {
                    hints.push((e.id.clone(), Hint::Bls(text)));
                }
            }
            Kind::BootNext(_) => {}
        }
    }
}

/// `search.fs_uuid X root` / `search --fs-uuid --set=root X` in a GRUB stub.
pub fn grub_stub_uuid(cfg: &str) -> Option<String> {
    for line in cfg.lines() {
        let l = line.trim();
        if !l.starts_with("search") {
            continue;
        }
        let toks: Vec<&str> = l.split_whitespace().collect();
        if toks[0] == "search.fs_uuid" {
            return toks.get(1).map(|s| s.to_string());
        }
        if toks.iter().any(|t| *t == "--fs-uuid" || *t == "-u") {
            return toks.iter().rev().find(|t| !t.starts_with('-') && t.len() >= 8).map(|s| s.to_string());
        }
    }
    None
}

/// GRUB's directory relative to the filesystem found by the stub
/// (`set prefix=($root)'/boot/grub'` -> "boot/grub").
pub fn grub_prefix(cfg: &str) -> String {
    let mut prefix = None;
    for line in cfg.lines() {
        if let Some(v) = line.trim().strip_prefix("set prefix=") {
            let v = v.split(')').next_back().unwrap_or(v);
            prefix = Some(v.trim_matches(|c| c == '\'' || c == '"').trim_start_matches('/').to_string());
        }
    }
    prefix.unwrap_or_else(|| "boot/grub".into())
}

/// `root=...` of the first `linux` line in a GRUB menu: the system it boots.
pub fn grub_linux_root(cfg: &str) -> Option<String> {
    for line in cfg.lines() {
        // grub-mkconfig separates the command from its arguments with a tab.
        let mut words = line.split_whitespace();
        if matches!(words.next(), Some("linux" | "linuxefi")) {
            return words.find_map(|t| t.strip_prefix("root=")).map(|s| s.to_string());
        }
    }
    None
}

/// The system behind a GRUB whose files are in `fs_root/prefix`: the
/// filesystem's own os-release, else the root= of its first menu entry
/// (GRUB kept on an ESP or a separate /boot), else the first menu title.
pub fn detect_grub(fs_root: &Path, prefix: &str, lookup: &dyn Fn(&str) -> Option<Detection>) -> Option<Detection> {
    for rel in ["etc/os-release", "usr/lib/os-release"] {
        if let Some(d) = fs::read_to_string(sub(fs_root, rel)).ok().and_then(|t| parse_os_release(&t)) {
            return Some(d);
        }
    }
    let cfg = fs::read_to_string(sub(fs_root, prefix).join("grub.cfg")).ok();
    if let Some(d) = cfg.as_deref().and_then(grub_linux_root).and_then(|spec| lookup(&spec)) {
        return Some(d);
    }
    if let Some(title) = cfg.as_deref().and_then(grub_first_title) {
        let id = title.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        return Some(Detection { icon: icon_for_os_id(&id).to_string(), name: title, id, version: String::new() });
    }
    detect_in_dir(fs_root)
}

/// Detection for a `root=` / `UUID=` spec: demo override, the running
/// system, or a temporary read-only mount.
pub fn detect_spec(parts: &[Part], overrides: &RootOverride, spec: &str) -> Option<Detection> {
    let bare = spec.trim_start_matches("UUID=").trim_start_matches("PARTUUID=").to_string();
    if let Some(dir) = overrides.get(&bare).or_else(|| overrides.get(spec)) {
        return detect_in_dir(dir);
    }
    let p = find_part(parts, spec)?;
    if p.mountpoint.as_deref() == Some("/") {
        return detect_in_dir(Path::new("/"));
    }
    let m = Mounted::open(p, false)?;
    detect_in_dir(&m.dir)
}

/// `root=UUID=...`, `root=PARTUUID=...` or `root=/dev/...` from BLS options.
fn bls_root(text: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim();
        if !l.starts_with("options") {
            continue;
        }
        for tok in l.split_whitespace() {
            if let Some(v) = tok.strip_prefix("root=") {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn find_part<'a>(parts: &'a [Part], spec: &str) -> Option<&'a Part> {
    if let Some(u) = spec.strip_prefix("UUID=") {
        return parts.iter().find(|p| p.uuid.as_deref().map(|x| x.eq_ignore_ascii_case(u)).unwrap_or(false));
    }
    if let Some(u) = spec.strip_prefix("PARTUUID=") {
        return parts.iter().find(|p| p.partuuid.as_deref().map(|x| x.eq_ignore_ascii_case(u)).unwrap_or(false));
    }
    if spec.starts_with("/dev/") {
        return parts.iter().find(|p| p.path == spec);
    }
    parts.iter().find(|p| p.uuid.as_deref().map(|x| x.eq_ignore_ascii_case(spec)).unwrap_or(false))
}

pub fn parse_os_release(text: &str) -> Option<Detection> {
    let mut kv = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    let name = kv.get("NAME").cloned()?;
    let id = kv.get("ID").cloned().unwrap_or_default().to_ascii_lowercase();
    Some(Detection {
        icon: icon_for_os_id(&id).to_string(),
        name,
        id,
        version: kv.get("VERSION_ID").cloned().unwrap_or_default(),
    })
}

/// First `menuentry 'Title'` of a GRUB config (separate /boot partitions).
fn grub_first_title(cfg: &str) -> Option<String> {
    for line in cfg.lines() {
        let l = line.trim_start();
        if let Some(rest) = l.strip_prefix("menuentry ") {
            let q = rest.chars().next()?;
            if q == '\'' || q == '"' {
                return rest[1..].split(q).next().map(|s| s.to_string());
            }
        }
    }
    None
}

pub fn detect_in_dir(dir: &Path) -> Option<Detection> {
    for rel in ["etc/os-release", "usr/lib/os-release"] {
        if let Ok(t) = fs::read_to_string(sub(dir, rel)) {
            if let Some(d) = parse_os_release(&t) {
                return Some(d);
            }
        }
    }
    // A separate /boot partition: fall back to the GRUB menu title.
    for rel in ["grub/grub.cfg", "boot/grub/grub.cfg"] {
        if let Ok(t) = fs::read_to_string(sub(dir, rel)) {
            if let Some(title) = grub_first_title(&t) {
                let id = title.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                return Some(Detection { icon: icon_for_os_id(&id).to_string(), name: title, id, version: String::new() });
            }
        }
    }
    None
}

fn resolve_hints(parts: &[Part], hints: Vec<(String, Hint)>, overrides: &RootOverride) -> BTreeMap<String, Detection> {
    let mut out = BTreeMap::new();
    let mut cache: BTreeMap<String, Option<Detection>> = BTreeMap::new();
    for (id, hint) in hints {
        let (spec, prefix) = match hint {
            Hint::GrubStub(cfg) => (grub_stub_uuid(&cfg).map(|u| format!("UUID={}", u)), Some(grub_prefix(&cfg))),
            Hint::Bls(text) => (bls_root(&text), None),
        };
        let Some(spec) = spec else { continue };
        let det = cache
            .entry(spec.clone())
            .or_insert_with(|| {
                let Some(prefix) = &prefix else { return detect_spec(parts, overrides, &spec) };
                let lookup = |s: &str| detect_spec(parts, overrides, s);
                let bare = spec.trim_start_matches("UUID=").to_string();
                if let Some(dir) = overrides.get(&bare) {
                    return detect_grub(dir, prefix, &lookup);
                }
                let p = find_part(parts, &spec)?;
                if p.mountpoint.as_deref() == Some("/") {
                    return detect_grub(Path::new("/"), prefix, &lookup);
                }
                let m = Mounted::open(p, false)?;
                detect_grub(&m.dir, prefix, &lookup)
            })
            .clone();
        if let Some(d) = det {
            out.insert(id, d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grub_stub_forms() {
        assert_eq!(grub_stub_uuid("search.fs_uuid 47d8a71c-c647 root \nset prefix=($root)'/grub'\n").as_deref(), Some("47d8a71c-c647"));
        assert_eq!(grub_stub_uuid("search --no-floppy --fs-uuid --set=root 2142a663-34a0-4b52\n").as_deref(), Some("2142a663-34a0-4b52"));
        assert_eq!(grub_stub_uuid("set root=hd0\n"), None);
    }

    #[test]
    fn grub_on_esp_follows_root() {
        let base = std::env::temp_dir().join(format!("bootpick-grub-{}", std::process::id()));
        let esp = base.join("esp");
        let root = base.join("root");
        fs::create_dir_all(esp.join("EFI/ubuntu/grub")).unwrap();
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::write(esp.join("EFI/ubuntu/grub/grub.cfg"), "menuentry 'Ubuntu' {\n\tlinux /vmlinuz root=UUID=F301 ro quiet\n}\n").unwrap();
        fs::write(root.join("etc/os-release"), "NAME=\"elementary OS\"\nID=elementary\nVERSION_ID=8\n").unwrap();
        let mut ov = RootOverride::new();
        ov.insert("F301".into(), root.clone());
        let prefix = grub_prefix("search.fs_uuid 4E38-0C72 root hd0,gpt6\nset prefix=($root)'/EFI/ubuntu/grub'\n");
        assert_eq!(prefix, "EFI/ubuntu/grub");
        let d = detect_grub(&esp, &prefix, &|s| detect_spec(&[], &ov, s)).unwrap();
        assert_eq!(d.name, "elementary OS");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn bls_root_forms() {
        assert_eq!(bls_root("title Pop!_OS\noptions root=UUID=2142a663 ro quiet\n").as_deref(), Some("UUID=2142a663"));
        assert_eq!(bls_root("options quiet root=/dev/sda3\n").as_deref(), Some("/dev/sda3"));
    }

    #[test]
    fn os_release() {
        let d = parse_os_release("NAME=\"elementary OS\"\nVERSION_ID=\"7.1\"\nID=elementary\n").unwrap();
        assert_eq!((d.name.as_str(), d.id.as_str(), d.version.as_str(), d.icon.as_str()), ("elementary OS", "elementary", "7.1", "os_elementary"));
        let d = parse_os_release("NAME=\"Pop!_OS\"\nID=pop\n").unwrap();
        assert_eq!(d.icon, "os_pop");
    }

    #[test]
    fn grub_title() {
        assert_eq!(grub_first_title("set x=1\nmenuentry 'Ubuntu' --class ubuntu {\n").as_deref(), Some("Ubuntu"));
        assert_eq!(grub_first_title("menuentry \"Fedora Linux\" {\n").as_deref(), Some("Fedora Linux"));
    }

    #[test]
    fn load_option_layout() {
        let guid = [0x11u8; 16];
        let o = load_option("bootpick", r"\EFI\bootpick\bootpick.efi", 1, 2048, 614400, guid);
        assert_eq!(parse_load_option(&o), Some((true, "bootpick".to_string())));
        let dp = &o[6 + 9 * 2..];
        assert_eq!(u16::from_le_bytes([o[4], o[5]]) as usize, dp.len());
        assert_eq!(&dp[..4], &[4, 1, 42, 0]);
        assert_eq!(u32::from_le_bytes(dp[4..8].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(dp[8..16].try_into().unwrap()), 2048);
        assert_eq!(&dp[24..40], &guid);
        assert_eq!(&dp[40..44], &[2, 2, 4, 4]);
        assert_eq!(&dp[dp.len() - 4..], &[0x7f, 0xff, 4, 0]);
    }

    #[test]
    fn this_machine_lsblk() {
        // Runs against the real lsblk; only checks the parse and labels.
        let parts = partitions();
        if parts.is_empty() {
            return;
        }
        for p in fat_volumes(&parts) {
            let l = disk_label(&p);
            assert!(!l.is_empty(), "{:?}", p);
            println!("{} {} {:?} esp={}", p.path, l, p.partuuid, is_esp(&p));
        }
    }
}
