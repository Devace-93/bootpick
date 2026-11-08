//! The privileged half (run through `pkexec bootpick-config --helper`). It only
//! touches \EFI\bootpick on the ESP, the "bootpick" firmware boot entry,
//! the app's own installed files, and (on request) the menu settings of other
//! boot loaders: a marked block in GRUB's custom.cfg and the `timeout` line of
//! systemd-boot's loader.conf (see menus.rs). Files coming from the user's staging copy
//! are checked (regular files, allowed names and sizes) before they are
//! written.

use crate::host::{self, DirStorage, Mounted, Part, RootOverride};
use crate::menus::{self, Env};
use crate::proto::{LoadReply, Reply, Request, ScanJ};
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::fs::{chown, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_CONF: u64 = 1 << 20;
const MAX_ICON: u64 = 4 << 20;
const MAX_BACKGROUND: u64 = 16 << 20;
/// Files installed with the app (removed by "Uninstall").
pub const APP_FILES: &[&str] = &[
    "/usr/local/bin/bootpick-config",
    "/usr/share/applications/bootpick-settings.desktop",
    "/usr/share/icons/hicolor/scalable/apps/bootpick.svg",
];
pub const APP_SHARE: &str = "/usr/local/share/bootpick";

fn caller_uid() -> Option<u32> {
    std::env::var("PKEXEC_UID").ok().and_then(|v| v.parse().ok())
}

/// The staging directory must be an absolute, real directory owned by the
/// user who started the app.
fn check_staging(staging: &str, uid: u32) -> Result<PathBuf, String> {
    let p = PathBuf::from(staging);
    if !p.is_absolute() {
        return Err("staging path must be absolute".into());
    }
    let meta = fs::symlink_metadata(&p).map_err(|e| format!("staging: {}", e))?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err("staging directory is not a directory owned by the caller".into());
    }
    Ok(p)
}

fn bootpick_entry() -> Option<(u16, bool, bool)> {
    let order = host::boot_order();
    host::all_boot_options().into_iter().find(|(_, _, d)| d == "bootpick").map(|(n, active, _)| (n, active, order.first() == Some(&n)))
}

fn find_esp() -> Result<(Part, Vec<Part>), String> {
    let parts = host::partitions();
    for p in host::fat_volumes(&parts) {
        if let Some(m) = Mounted::open(&p, false) {
            if DirStorage::new(&m.dir).resolve("\\EFI\\bootpick\\bootpick.efi").is_file() {
                return Ok((p, parts));
            }
        }
    }
    Err("bootpick is not installed on any EFI partition".into())
}

/// Copies a directory tree of regular files (no symlinks), chowning to `uid`.
fn copy_tree_to_user(src: &Path, dst: &Path, uid: u32) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    chown(dst, Some(uid), None)?;
    for e in fs::read_dir(src)?.flatten() {
        let ft = e.file_type()?;
        let to = dst.join(e.file_name());
        if ft.is_dir() {
            copy_tree_to_user(&e.path(), &to, uid)?;
        } else if ft.is_file() {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.ends_with(".efi") {
                continue; // binaries are not edited from the app
            }
            fs::copy(e.path(), &to)?;
            chown(&to, Some(uid), None)?;
        }
    }
    Ok(())
}

fn load(staging: &str, uid: u32) -> Result<LoadReply, String> {
    let staging = check_staging(staging, uid)?;
    let (esp, parts) = find_esp()?;
    {
        let m = Mounted::open(&esp, false).ok_or("cannot mount the EFI partition")?;
        let src = DirStorage::new(&m.dir).resolve("\\EFI\\bootpick");
        let dst = staging.join("EFI").join("bootpick");
        if dst.exists() {
            fs::remove_dir_all(&dst).map_err(|e| e.to_string())?;
        }
        copy_tree_to_user(&src, &dst, uid).map_err(|e| format!("copying settings: {}", e))?;
        let _ = chown(staging.join("EFI"), Some(uid), None);
    }
    let hs = host::scan_host(&parts, &RootOverride::new());
    menus::upgrade(&Env::Real { parts: &parts });
    Ok(LoadReply {
        esp_label: host::disk_label(&esp),
        esp_device: esp.path.clone(),
        scan: ScanJ::from(&hs.scan),
        detections: hs.detections,
        boot_entry: bootpick_entry(),
        menus: menus::scan(&Env::Real { parts: &parts }),
    })
}

fn allowed(rel_dir: &str, name: &str) -> Option<u64> {
    let l = name.to_ascii_lowercase();
    let safe = !name.contains(['/', '\\']) && !name.starts_with('.') && name.len() <= 120;
    if !safe {
        return None;
    }
    match rel_dir {
        "" if l == "bootpick.conf" => Some(MAX_CONF),
        "icons" if l.ends_with(".bmp") => Some(MAX_ICON),
        "backgrounds" if l.ends_with(".bmp") => Some(MAX_BACKGROUND),
        _ => None,
    }
}

fn save(staging: &str, uid: u32) -> Result<String, String> {
    let staging = check_staging(staging, uid)?.join("EFI").join("bootpick");
    let (esp, _) = find_esp()?;
    let m = Mounted::open(&esp, true).ok_or("cannot mount the EFI partition read-write")?;
    let dest = DirStorage::new(&m.dir).resolve("\\EFI\\bootpick");
    let mut written = 0;
    for rel in ["", "icons", "backgrounds"] {
        let sdir = if rel.is_empty() { staging.clone() } else { staging.join(rel) };
        let ddir = if rel.is_empty() { dest.clone() } else { dest.join(rel) };
        fs::create_dir_all(&ddir).map_err(|e| e.to_string())?;
        let mut wanted = Vec::new();
        for e in fs::read_dir(&sdir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(max) = allowed(rel, &name) else { continue };
            let meta = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.len() > max {
                continue;
            }
            wanted.push(name.to_ascii_lowercase());
            let data = fs::read(e.path()).map_err(|e| e.to_string())?;
            let target = ddir.join(&name);
            if fs::read(&target).ok().as_deref() == Some(&data[..]) {
                continue;
            }
            let tmp = ddir.join(format!(".{}.tmp", name));
            fs::write(&tmp, &data).map_err(|e| format!("{}: {}", name, e))?;
            fs::rename(&tmp, &target).map_err(|e| format!("{}: {}", name, e))?;
            written += 1;
        }
        // Remove backgrounds and custom icons the user deleted.
        if rel == "backgrounds" || rel == "icons" {
            for e in fs::read_dir(&ddir).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let l = name.to_ascii_lowercase();
                let removable = rel == "backgrounds" || l.starts_with("custom-");
                if removable && l.ends_with(".bmp") && !wanted.contains(&l) {
                    let _ = fs::remove_file(e.path());
                    written += 1;
                }
            }
        }
    }
    let _ = Command::new("sync").status();
    Ok(format!("{} file(s) updated on {}", written, esp.path))
}

fn set_enabled(enabled: bool) -> Result<String, String> {
    let (n, _, _) = bootpick_entry().ok_or("the bootpick firmware entry was not found")?;
    let num = format!("{:04X}", n);
    let ok = Command::new("efibootmgr").args(["-q", "-b", &num, if enabled { "-a" } else { "-A" }]).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return Err("efibootmgr failed".into());
    }
    if !enabled {
        // The firmware now starts another loader: its menu is the way to
        // reach the other systems again.
        menus::restore_all(&Env::Real { parts: &host::partitions() });
    }
    if enabled {
        let order: Vec<String> = std::iter::once(n).chain(host::boot_order().into_iter().filter(|x| *x != n)).map(|x| format!("{:04X}", x)).collect();
        let _ = Command::new("efibootmgr").args(["-q", "-o", &order.join(",")]).status();
    }
    Ok(if enabled { "bootpick enabled".into() } else { "bootpick disabled: the next entry in the firmware boot order starts instead".into() })
}

fn uninstall() -> Result<String, String> {
    menus::restore_all(&Env::Real { parts: &host::partitions() });
    if let Ok((esp, _)) = find_esp() {
        let m = Mounted::open(&esp, true).ok_or("cannot mount the EFI partition read-write")?;
        let dir = DirStorage::new(&m.dir).resolve("\\EFI\\bootpick");
        fs::remove_dir_all(&dir).map_err(|e| format!("removing {}: {}", dir.display(), e))?;
    }
    for (n, _, d) in host::all_boot_options() {
        if d == "bootpick" {
            let _ = Command::new("efibootmgr").args(["-q", "-b", &format!("{:04X}", n), "-B"]).status();
        }
    }
    for f in APP_FILES {
        let _ = fs::remove_file(f);
    }
    let _ = fs::remove_dir_all(APP_SHARE);
    Ok("bootpick was removed. Your previous boot loader starts again.".into())
}

/// `bootpick-config --upgrade-menus` (root, run by install.sh): rewrites
/// GRUB blocks written by an older bootpick.
pub fn upgrade_menus() -> ! {
    for (name, r) in menus::upgrade(&Env::Real { parts: &host::partitions() }) {
        match r {
            Ok(()) => println!("updated the hidden GRUB menu of {}", name),
            Err(e) => println!("could not update the hidden GRUB menu of {}: {}", name, e),
        }
    }
    std::process::exit(0)
}

pub fn run() -> ! {
    let uid = match caller_uid() {
        Some(u) => u,
        None => {
            eprintln!("bootpick-config --helper must be started through pkexec");
            std::process::exit(2);
        }
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let reply = match serde_json::from_str::<Request>(&line) {
            Err(e) => Reply::Error { message: format!("bad request: {}", e) },
            Ok(Request::Quit) => break,
            Ok(Request::Load { staging }) => match load(&staging, uid) {
                Ok(l) => Reply::Ok { load: Some(l), message: String::new(), menus: None },
                Err(e) => Reply::Error { message: e },
            },
            Ok(Request::Save { staging }) => match save(&staging, uid) {
                Ok(m) => Reply::Ok { load: None, message: m, menus: None },
                Err(e) => Reply::Error { message: e },
            },
            Ok(Request::SetEnabled { enabled }) => match set_enabled(enabled) {
                Ok(m) => Reply::Ok { load: None, message: m, menus: Some(menus::scan(&Env::Real { parts: &host::partitions() })) },
                Err(e) => Reply::Error { message: e },
            },
            Ok(Request::SetMenu { id, visible, always }) => {
                let parts = host::partitions();
                let env = Env::Real { parts: &parts };
                match menus::set(&env, &id, visible, always) {
                    Ok(()) => Reply::Ok { load: None, message: String::new(), menus: Some(menus::scan(&env)) },
                    Err(e) => Reply::Error { message: e },
                }
            }
            Ok(Request::Uninstall) => match uninstall() {
                Ok(m) => Reply::Ok { load: None, message: m, menus: None },
                Err(e) => Reply::Error { message: e },
            },
        };
        let _ = writeln!(out, "{}", serde_json::to_string(&reply).unwrap_or_default());
        let _ = out.flush();
    }
    std::process::exit(0)
}
