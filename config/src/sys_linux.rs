//! Linux platform layer: partitions (lsblk / blkid), temporary mounts,
//! firmware variables (efivarfs, efibootmgr), the pkexec helper channel and
//! the installed app files.

use crate::host::Part;
use crate::proto::HelperIo;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{chown, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const EFIVARS: &str = "/sys/firmware/efi/efivars";

/// Files installed with the app (removed by "Uninstall").
const APP_FILES: &[&str] = &[
    "/usr/local/bin/bootpick-config",
    "/usr/share/applications/bootpick-settings.desktop",
    "/usr/share/icons/hicolor/scalable/apps/bootpick.svg",
];
const APP_SHARE: &str = "/usr/local/share/bootpick";

// ----- partitions ---------------------------------------------------------------------

#[derive(Deserialize)]
struct Lsblk {
    blockdevices: Vec<Part>,
}

/// PARTN is not requested: lsblk older than util-linux 2.39 rejects the whole
/// call for an unknown column. `fill_missing` reads it from sysfs instead.
pub fn partitions() -> Vec<Part> {
    let out = Command::new("lsblk")
        .args(["-J", "-l", "-o", "NAME,PATH,PKNAME,TYPE,TRAN,PARTUUID,PARTTYPE,FSTYPE,UUID,MOUNTPOINT"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    let mut parts: Vec<Part> = serde_json::from_slice::<Lsblk>(&out.stdout).map(|l| l.blockdevices).unwrap_or_default();
    for p in &mut parts {
        fill_missing(p);
    }
    // Partitions inherit the transport of their disk.
    let trans: BTreeMap<String, String> = parts.iter().filter_map(|p| Some((p.name.clone(), p.tran.clone()?))).collect();
    for p in &mut parts {
        if p.tran.is_none() {
            if let Some(t) = p.pkname.as_ref().and_then(|k| trans.get(k)) {
                p.tran = Some(t.clone());
            }
        }
    }
    parts
}

/// lsblk leaves fields empty when the udev database is not readable; probe
/// sysfs and the device itself (`blkid -p`, needs root) for what is missing.
fn fill_missing(p: &mut Part) {
    if p.kind.as_deref() != Some("part") {
        return;
    }
    if p.partn.is_none() {
        p.partn = fs::read_to_string(format!("/sys/class/block/{}/partition", p.name)).ok().and_then(|s| s.trim().parse().ok());
    }
    if p.partuuid.is_some() && p.fstype.is_some() && p.uuid.is_some() && p.parttype.is_some() {
        return;
    }
    let Ok(out) = Command::new("blkid").args(["-p", "-o", "export", &p.path]).output() else { return };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = Some(v.to_string());
        match k {
            "PART_ENTRY_UUID" if p.partuuid.is_none() => p.partuuid = v,
            "PART_ENTRY_TYPE" if p.parttype.is_none() => p.parttype = v,
            "TYPE" if p.fstype.is_none() => p.fstype = v,
            "UUID" if p.uuid.is_none() => p.uuid = v,
            _ => {}
        }
    }
}

// ----- mounting -----------------------------------------------------------------------

/// Gives access to a partition's files: its current mount point, or a
/// temporary read-only mount (needs root). `writable` mounts read-write.
pub struct Mounted {
    pub dir: PathBuf,
    temp: bool,
}

/// Temporary mounts made by this process: device -> (dir, users, writable).
/// A partition opened twice (an ESP that also holds GRUB's files) reuses the
/// first mount instead of failing on a second one.
static MOUNTS: std::sync::Mutex<BTreeMap<String, (PathBuf, usize, bool)>> = std::sync::Mutex::new(BTreeMap::new());

impl Mounted {
    pub fn open(p: &Part, writable: bool) -> Option<Mounted> {
        if let Some(mp) = p.mountpoint.as_ref().filter(|m| !m.is_empty()) {
            return Some(Mounted { dir: PathBuf::from(mp), temp: false });
        }
        let mut reg = MOUNTS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((dir, users, rw)) = reg.get_mut(&p.path) {
            if writable && !*rw {
                let ok = Command::new("mount").args(["-o", "remount,rw"]).arg(&*dir).status().map(|s| s.success()).unwrap_or(false);
                if !ok {
                    return None;
                }
                *rw = true;
            }
            *users += 1;
            return Some(Mounted { dir: dir.clone(), temp: true });
        }
        let fstype = p.fstype.as_deref()?;
        if !matches!(fstype, "vfat" | "ext4" | "ext3" | "ext2" | "btrfs" | "xfs" | "f2fs") {
            return None;
        }
        let dir = PathBuf::from(format!("/run/bootpick/mnt-{}", p.name));
        fs::create_dir_all(&dir).ok()?;
        let mut opts = String::from(if writable { "rw" } else { "ro" });
        opts.push_str(",nosuid,nodev,noexec");
        if !writable && fstype.starts_with("ext") {
            opts.push_str(",noload"); // never replay a journal from here
        }
        let ok = Command::new("mount").args(["-o", &opts, &p.path]).arg(&dir).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            let _ = fs::remove_dir(&dir);
            return None;
        }
        reg.insert(p.path.clone(), (dir.clone(), 1, writable));
        Some(Mounted { dir, temp: true })
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        if self.temp {
            let mut reg = MOUNTS.lock().unwrap_or_else(|e| e.into_inner());
            let key = reg.iter().find(|(_, v)| v.0 == self.dir).map(|(k, _)| k.clone());
            if let Some(k) = key {
                let last = {
                    let e = reg.get_mut(&k).unwrap();
                    e.1 -= 1;
                    e.1 == 0
                };
                if !last {
                    return;
                }
                reg.remove(&k);
            }
            let _ = Command::new("umount").arg(&self.dir).status();
            let _ = fs::remove_dir(&self.dir);
        }
    }
}

pub fn sync() {
    let _ = Command::new("sync").status();
}

// ----- firmware variables ---------------------------------------------------------------

/// The data of a firmware variable (without the attribute word).
pub fn efivar_vendor(name: &str, vendor: &str) -> Option<Vec<u8>> {
    let data = fs::read(Path::new(EFIVARS).join(format!("{}-{}", name, vendor))).ok()?;
    (data.len() >= 4).then(|| data[4..].to_vec())
}

/// The number of every Boot#### variable present (also those not in BootOrder).
pub fn boot_numbers() -> Vec<u16> {
    let suffix = format!("-{}", crate::host::EFI_GLOBAL);
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(EFIVARS) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(hex) = name.strip_prefix("Boot").and_then(|r| r.strip_suffix(&suffix)) {
                if hex.len() == 4 {
                    if let Ok(n) = u16::from_str_radix(hex, 16) {
                        v.push(n);
                    }
                }
            }
        }
    }
    v.sort();
    v
}

fn efibootmgr(args: &[&str]) -> Result<(), String> {
    let ok = Command::new("efibootmgr").args(args).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err("efibootmgr failed".into())
    }
}

pub fn boot_set_active(n: u16, active: bool) -> Result<(), String> {
    efibootmgr(&["-q", "-b", &format!("{:04X}", n), if active { "-a" } else { "-A" }])
}

pub fn boot_set_order(order: &[u16]) -> Result<(), String> {
    let list: Vec<String> = order.iter().map(|x| format!("{:04X}", x)).collect();
    efibootmgr(&["-q", "-o", &list.join(",")])
}

pub fn boot_delete(n: u16) -> Result<(), String> {
    efibootmgr(&["-q", "-b", &format!("{:04X}", n), "-B"])
}

/// A timeout set with `bootctl set-timeout` overrides loader.conf: clear it.
pub fn clear_sd_timeout() {
    let _ = Command::new("bootctl").args(["set-timeout", ""]).status();
}

// ----- the helper and its caller ----------------------------------------------------------

/// The unprivileged user who started the helper.
pub type Caller = u32;

pub fn caller() -> Result<Caller, String> {
    std::env::var("PKEXEC_UID").ok().and_then(|v| v.parse().ok()).ok_or_else(|| "bootpick-config --helper must be started through pkexec".to_string())
}

pub fn owned_by(meta: &fs::Metadata, caller: &Caller) -> bool {
    meta.uid() == *caller
}

/// Hands a file or directory created by the helper to the caller.
pub fn give(path: &Path, caller: &Caller) -> std::io::Result<()> {
    chown(path, Some(*caller), None)
}

/// The helper's side of the channel: its own stdin and stdout.
pub fn helper_channel(_args: &[String]) -> Result<(Box<dyn BufRead>, Box<dyn Write>), String> {
    Ok((Box::new(BufReader::new(std::io::stdin())), Box::new(std::io::stdout())))
}

/// Starts the privileged helper (asks for authorization through pkexec).
pub fn spawn_helper() -> Result<HelperIo, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new("pkexec").arg(exe).arg("--helper").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("could not start pkexec: {}", e))?;
    let stdin = child.stdin.take().ok_or("no helper stdin")?;
    let stdout = BufReader::new(child.stdout.take().ok_or("no helper stdout")?);
    Ok(HelperIo {
        reader: Box::new(stdout),
        writer: Box::new(stdin),
        wait: Some(Box::new(move || {
            let _ = child.wait();
        })),
    })
}

pub fn remove_app() {
    for f in APP_FILES {
        let _ = fs::remove_file(f);
    }
    let _ = fs::remove_dir_all(APP_SHARE);
}

// ----- desktop integration ------------------------------------------------------------------

/// Where the app keeps its staging copy.
pub fn staging_base() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir())
}

/// The user's locale as a BCP 47-like tag ("es-MX").
pub fn locale() -> String {
    let sys = std::env::var("LC_ALL").or_else(|_| std::env::var("LC_MESSAGES")).or_else(|_| std::env::var("LANG")).unwrap_or_default();
    sys.split('.').next().unwrap_or("").replace('_', "-")
}

/// System fonts for scripts egui's defaults lack: (name, path, face index, language).
pub fn script_fonts() -> Vec<(&'static str, String, u32, &'static str)> {
    let cjk = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
    vec![
        ("noto-cjk-jp", cjk.to_string(), 0, "ja"),
        ("noto-cjk-kr", cjk.to_string(), 1, "ko"),
        ("noto-cjk-sc", cjk.to_string(), 2, "zh"),
        ("noto-deva", "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf".to_string(), 0, "hi"),
        ("noto-thai", "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf".to_string(), 0, "th"),
        ("noto-arab", "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf".to_string(), 0, "ar"),
    ]
}
