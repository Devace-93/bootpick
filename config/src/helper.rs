//! The privileged half (run through `pkexec bootpick-config --helper` on Linux,
//! elevated through UAC on Windows). It only
//! touches \EFI\bootpick on the ESP, the "bootpick" firmware boot entry,
//! the app's own installed files, and (on request) the menu settings of other
//! boot loaders: a marked block in GRUB's custom.cfg and the `timeout` line of
//! systemd-boot's loader.conf (see menus.rs). Files coming from the user's staging copy
//! are checked (regular files, allowed names and sizes) before they are
//! written.

use crate::host::{self, DirStorage, Mounted, Part, RootOverride};
use crate::menus::{self, Env};
use crate::proto::{LoadReply, Reply, Request, ScanJ};
use crate::sys::{self, Caller};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const MAX_CONF: u64 = 1 << 20;
const MAX_ICON: u64 = 4 << 20;
const MAX_BACKGROUND: u64 = 16 << 20;

/// The staging directory must be an absolute, real directory owned by the
/// user who started the app.
fn check_staging(staging: &str, caller: &Caller) -> Result<PathBuf, String> {
    let p = PathBuf::from(staging);
    if !p.is_absolute() {
        return Err("staging path must be absolute".into());
    }
    let meta = fs::symlink_metadata(&p).map_err(|e| format!("staging: {}", e))?;
    if !meta.is_dir() || !sys::owned_by(&meta, caller) {
        return Err("staging directory is not a directory owned by the caller".into());
    }
    Ok(p)
}

pub fn bootpick_entry() -> Option<(u16, bool, bool)> {
    let order = host::boot_order();
    host::all_boot_options().into_iter().find(|(_, _, d)| d == "bootpick").map(|(n, active, _)| (n, active, order.first() == Some(&n)))
}

pub fn find_esp() -> Result<(Part, Vec<Part>), String> {
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

/// Copies a directory tree of regular files (no symlinks), handing it to the caller.
fn copy_tree_to_user(src: &Path, dst: &Path, caller: &Caller) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    sys::give(dst, caller)?;
    for e in fs::read_dir(src)?.flatten() {
        let ft = e.file_type()?;
        let to = dst.join(e.file_name());
        if ft.is_dir() {
            copy_tree_to_user(&e.path(), &to, caller)?;
        } else if ft.is_file() {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.ends_with(".efi") {
                continue; // binaries are not edited from the app
            }
            fs::copy(e.path(), &to)?;
            sys::give(&to, caller)?;
        }
    }
    Ok(())
}

fn load(staging: &str, caller: &Caller) -> Result<LoadReply, String> {
    let staging = check_staging(staging, caller)?;
    let (esp, parts) = find_esp()?;
    {
        let m = Mounted::open(&esp, false).ok_or("cannot mount the EFI partition")?;
        let src = DirStorage::new(&m.dir).resolve("\\EFI\\bootpick");
        let dst = staging.join("EFI").join("bootpick");
        if dst.exists() {
            fs::remove_dir_all(&dst).map_err(|e| e.to_string())?;
        }
        copy_tree_to_user(&src, &dst, caller).map_err(|e| format!("copying settings: {}", e))?;
        let _ = sys::give(&staging.join("EFI"), caller);
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

fn save(staging: &str, caller: &Caller) -> Result<String, String> {
    let staging = check_staging(staging, caller)?.join("EFI").join("bootpick");
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
    sys::sync();
    Ok(format!("{} file(s) updated on {}", written, esp.path))
}

fn set_enabled(enabled: bool) -> Result<String, String> {
    let (n, _, _) = bootpick_entry().ok_or("the bootpick firmware entry was not found")?;
    sys::boot_set_active(n, enabled)?;
    if !enabled {
        // The firmware now starts another loader: its menu is the way to
        // reach the other systems again.
        menus::restore_all(&Env::Real { parts: &host::partitions() });
    }
    if enabled {
        let order: Vec<u16> = std::iter::once(n).chain(host::boot_order().into_iter().filter(|x| *x != n)).collect();
        let _ = sys::boot_set_order(&order);
    }
    Ok(if enabled { "bootpick enabled".into() } else { "bootpick disabled: the next entry in the firmware boot order starts instead".into() })
}

pub fn uninstall() -> Result<String, String> {
    menus::restore_all(&Env::Real { parts: &host::partitions() });
    if let Ok((esp, _)) = find_esp() {
        let m = Mounted::open(&esp, true).ok_or("cannot mount the EFI partition read-write")?;
        let dir = DirStorage::new(&m.dir).resolve("\\EFI\\bootpick");
        fs::remove_dir_all(&dir).map_err(|e| format!("removing {}: {}", dir.display(), e))?;
    }
    for (n, _, d) in host::all_boot_options() {
        if d == "bootpick" {
            let _ = sys::boot_delete(n);
        }
    }
    sys::remove_app();
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

pub fn run(args: &[String]) -> ! {
    let (caller, (reader, mut out)) = match sys::caller().and_then(|c| Ok((c, sys::helper_channel(args)?))) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(2);
        }
    };
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let reply = match serde_json::from_str::<Request>(&line) {
            Err(e) => Reply::Error { message: format!("bad request: {}", e) },
            Ok(Request::Quit) => break,
            Ok(Request::Load { staging }) => match load(&staging, &caller) {
                Ok(l) => Reply::Ok { load: Some(l), message: String::new(), menus: None },
                Err(e) => Reply::Error { message: e },
            },
            Ok(Request::Save { staging }) => match save(&staging, &caller) {
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
