//! The Windows installer (`bootpick-config --install`, elevated; started by
//! install.ps1). The counterpart of install.sh: copies the boot menu to an
//! EFI System Partition, makes it the first firmware boot entry and installs
//! the settings app. Existing loaders (the Windows boot manager, GRUB,
//! systemd-boot) are left untouched.
//!
//! When bootpick is already installed on any FAT partition of this computer
//! (for example from a Linux on the same machine), that copy is updated in
//! place and its settings are kept unless `--reset` is given.

use crate::helper;
use crate::host::{self, DirStorage, Mounted, Part};
use crate::menus::{self, Env};
use crate::sys;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const LABEL: &str = "bootpick";
const LOADER: &str = "\\EFI\\bootpick\\bootpick.efi";

struct Opts {
    source: Option<PathBuf>,
    /// Some(true) = reset, Some(false) = keep, None = ask.
    reset: Option<bool>,
    boot_entry: bool,
    app: bool,
    open_app: bool,
    uninstall: bool,
}

pub fn run(args: &[String]) -> ! {
    sys::attach_console();
    let result = if args.iter().any(|a| a == "--probe") { probe() } else { parse(args).and_then(install) };
    let _ = std::io::stdout().flush();
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1)
        }
    }
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { source: None, reset: None, boot_entry: true, app: true, open_app: true, uninstall: false };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--install" => {}
            "--source" => {
                o.source = Some(PathBuf::from(args.get(i + 1).ok_or("--source needs a folder")?));
                i += 1;
            }
            "--keep" => o.reset = Some(false),
            "--reset" => o.reset = Some(true),
            "--no-boot-entry" => o.boot_entry = false,
            "--no-app" => o.app = false,
            "--no-open" => o.open_app = false,
            "--uninstall" => o.uninstall = true,
            other => return Err(format!("unknown option: {}", other)),
        }
        i += 1;
    }
    Ok(o)
}

/// The files to install: a release package or a built source tree.
struct Source {
    efi: PathBuf,
    assets: PathBuf,
    drivers: PathBuf,
    conf: PathBuf,
}

fn find_source(given: Option<&Path>) -> Result<Source, String> {
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    let mut roots: Vec<PathBuf> = Vec::new();
    match given {
        Some(g) => roots.push(g.to_path_buf()),
        None => {
            // Next to the program (package), or two levels up (target\release in a source tree).
            if let Some(d) = &exe_dir {
                roots.push(d.clone());
                if let Some(up) = d.parent().and_then(Path::parent) {
                    roots.push(up.to_path_buf());
                }
            }
        }
    }
    for root in &roots {
        let assets = root.join("esp").join("EFI").join("bootpick");
        let efi = [root.join("bootpick.efi"), root.join("uefi").join("target").join("x86_64-unknown-uefi").join("release").join("bootpick.efi")].into_iter().find(|p| p.is_file());
        if let Some(efi) = efi {
            for d in ["icons", "flags", "fonts"] {
                if !assets.join(d).is_dir() {
                    return Err(format!("assets missing in {} (run tools\\build.ps1 first)", assets.display()));
                }
            }
            return Ok(Source { efi, assets, drivers: root.join("assets").join("drivers"), conf: root.join("bootpick.conf.example") });
        }
    }
    Err("bootpick.efi not found: run this from the release package or build first (tools\\build.ps1)".into())
}

/// Total size of the regular files directly in `dir` and one level below.
fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => fs::read_dir(e.path()).into_iter().flatten().flatten().filter_map(|f| f.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum(),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

fn copy_files(src: &Path, dst: &Path, ext: &str) -> Result<usize, String> {
    fs::create_dir_all(dst).map_err(|e| format!("{}: {}", dst.display(), e))?;
    let mut n = 0;
    for e in fs::read_dir(src).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) && name.to_ascii_lowercase().ends_with(ext) {
            fs::copy(e.path(), dst.join(&name)).map_err(|e| format!("{}: {}", name, e))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Where a fresh install goes: an EFI System Partition, the one on the disk
/// Windows runs from when there are several.
fn pick_esp(parts: &[Part]) -> Result<Part, String> {
    let esps: Vec<Part> = host::fat_volumes(parts).into_iter().filter(host::is_esp).collect();
    let system = sys::system_volume();
    let system_disk = parts.iter().find(|p| p.mountpoint.is_some() && p.mountpoint == system).and_then(|p| p.pkname.clone());
    esps.iter().find(|p| system_disk.is_some() && p.pkname == system_disk).or_else(|| esps.first()).cloned().ok_or_else(|| "no EFI System Partition was found (is this a UEFI computer with a GPT disk?)".to_string())
}

fn describe(p: &Part) -> String {
    let label = host::disk_label(p);
    if label.is_empty() {
        p.path.clone()
    } else {
        format!("{} ({})", p.path, label)
    }
}

fn secure_boot() -> bool {
    host::efivar("SecureBoot").and_then(|d| d.first().copied()) == Some(1)
}

fn ask_keep() -> bool {
    print!("  [k] keep its current settings (backgrounds, logos, choices)\n  [r] reset to factory settings\nChoice [k]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return true;
    }
    !matches!(line.trim().to_ascii_lowercase().as_str(), "r" | "reset")
}

fn install(o: Opts) -> Result<(), String> {
    if !sys::is_elevated() {
        return Err("administrator rights are needed: start install.cmd (or install.ps1), which asks for them".into());
    }
    if host::efivar("BootOrder").is_none() {
        return Err("the firmware boot entries cannot be read: bootpick needs a computer started in UEFI mode".into());
    }
    if o.uninstall {
        let found = helper::find_esp().is_ok();
        println!("{}", helper::uninstall()?);
        if !found {
            println!("(no boot menu files were found on any EFI partition; the boot entry and the app were removed)");
        }
        return Ok(());
    }
    let src = find_source(o.source.as_deref())?;
    let parts = host::partitions();
    let existing = helper::find_esp().ok().map(|(p, _)| p);
    let esp = match &existing {
        Some(p) => p.clone(),
        None => pick_esp(&parts)?,
    };
    let m = Mounted::open(&esp, true).ok_or("cannot open the EFI partition")?;
    let dest = DirStorage::new(&m.dir).resolve("\\EFI\\bootpick");

    let mut fresh = true;
    if dest.join("bootpick.conf").is_file() {
        fresh = false;
        println!("bootpick is already installed on {} ({}).", describe(&esp), dest.display());
        let keep = match o.reset {
            Some(reset) => !reset,
            None => ask_keep(),
        };
        if keep {
            println!("keeping the current settings");
        } else {
            fs::remove_dir_all(&dest).map_err(|e| format!("removing {}: {}", dest.display(), e))?;
            fresh = true;
            println!("settings reset to factory defaults");
        }
    }
    if fresh {
        // EFI partitions are small: check before filling one half way.
        let needed = dir_size(&src.assets) + dir_size(&src.drivers) + fs::metadata(&src.efi).map(|m| m.len()).unwrap_or(0) + (1 << 20);
        if let Some(free) = sys::free_space(&m.dir) {
            if free < needed {
                return Err(format!("not enough free space on {}: {} MB needed, {} MB free", describe(&esp), needed.div_ceil(1 << 20), free >> 20));
            }
        }
    }
    fs::create_dir_all(dest.join("backgrounds")).map_err(|e| format!("{}: {}", dest.display(), e))?;
    fs::copy(&src.efi, dest.join("bootpick.efi")).map_err(|e| format!("bootpick.efi: {}", e))?;
    copy_files(&src.assets.join("icons"), &dest.join("icons"), ".bmp")?;
    copy_files(&src.assets.join("flags"), &dest.join("flags"), ".bmp")?;
    copy_files(&src.assets.join("fonts"), &dest.join("fonts"), ".ttf")?;
    copy_files(&src.drivers, &dest.join("drivers"), ".efi")?;
    if fresh {
        // Bundled backgrounds only on a fresh install: the user may have removed some.
        copy_files(&src.assets.join("backgrounds"), &dest.join("backgrounds"), ".bmp")?;
        fs::copy(&src.conf, dest.join("bootpick.conf")).map_err(|e| format!("bootpick.conf: {}", e))?;
    }
    sys::sync();
    println!("boot menu installed in {} on {}", dest.display(), describe(&esp));

    if o.boot_entry {
        // Drop stale entries with the same label, then create a new one first in order.
        for (n, _, d) in host::all_boot_options() {
            if d == LABEL {
                sys::boot_delete(n)?;
            }
        }
        let new = sys::boot_create(&esp, LABEL, LOADER)?;
        let order: Vec<u16> = std::iter::once(new).chain(host::boot_order().into_iter().filter(|x| *x != new)).collect();
        sys::boot_set_order(&order)?;
        println!("boot entry Boot{:04X} '{}' created on {} and placed first", new, LABEL, describe(&esp));
    }

    if o.app {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let target = sys::app_exe();
        if exe != target {
            fs::create_dir_all(sys::app_dir()).map_err(|e| format!("{}: {}", sys::app_dir().display(), e))?;
            fs::copy(&exe, &target).map_err(|e| format!("{}: {} (close bootpick settings and try again)", target.display(), e))?;
        }
        for (name, r) in menus::upgrade(&Env::Real { parts: &parts }) {
            match r {
                Ok(()) => println!("updated the hidden GRUB menu of {}", name),
                Err(e) => println!("could not update the hidden GRUB menu of {}: {}", name, e),
            }
        }
        if make_shortcut(&sys::shortcut_path(), &target) {
            println!("settings app installed: open \"bootpick settings\" from the Start menu");
        } else {
            println!("settings app installed in {} (the Start menu shortcut could not be created)", target.display());
        }
        if o.open_app {
            // Through the shell, so the app starts without administrator rights.
            if std::process::Command::new("explorer.exe").arg(&target).spawn().is_ok() {
                println!("opening bootpick settings...");
            }
        }
    }
    if secure_boot() {
        println!();
        println!("WARNING: Secure Boot is on. The firmware will refuse to start bootpick and");
        println!("continue with the next boot entry. Turn Secure Boot off in the firmware");
        println!("setup, or sign bootpick.efi with your own key.");
    }
    Ok(())
}

fn make_shortcut(lnk: &Path, target: &Path) -> bool {
    let script = "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:BOOTPICK_LNK); $s.TargetPath = $env:BOOTPICK_EXE; $s.Description = 'Configure the bootpick boot menu'; $s.Save()";
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .env("BOOTPICK_LNK", lnk)
        .env("BOOTPICK_EXE", target)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && lnk.is_file()
}

/// `--probe`: what the installer sees on this computer. Changes nothing.
fn probe() -> Result<(), String> {
    let elevated = sys::is_elevated();
    println!("elevated: {}", elevated);
    let parts = host::partitions();
    println!("partitions:");
    for p in &parts {
        println!(
            "  {:<22} {:<8} fs={:<6} type={} id={} uuid={}{}",
            p.path,
            host::disk_label(p),
            p.fstype.as_deref().unwrap_or("?"),
            p.parttype.as_deref().unwrap_or("-"),
            p.partuuid.as_deref().unwrap_or("-"),
            p.uuid.as_deref().unwrap_or("-"),
            if host::is_esp(p) { "  [ESP]" } else { "" }
        );
    }
    if !elevated {
        println!("not elevated: EFI partitions and firmware variables cannot be read; run from an administrator prompt for the rest");
        return Ok(());
    }
    let target = match helper::find_esp() {
        Ok((p, _)) => {
            println!("existing bootpick: {}", describe(&p));
            Some(p)
        }
        Err(_) => match pick_esp(&parts) {
            Ok(p) => {
                println!("existing bootpick: none; a fresh install would go to {}", describe(&p));
                Some(p)
            }
            Err(e) => {
                println!("existing bootpick: none; {}", e);
                None
            }
        },
    };
    if let Some(p) = &target {
        if let Some(free) = p.mountpoint.as_deref().and_then(|m| sys::free_space(Path::new(m))) {
            println!("free space there: {} MB", free >> 20);
        }
        // The partition part of the boot entry that would be written,
        // compared with the entries the firmware already has for it.
        match sys::boot_option(p, LABEL, LOADER) {
            Ok(opt) => {
                let node = &opt[6 + (LABEL.len() + 1) * 2..][..42];
                let same: Vec<String> = sys::boot_numbers().into_iter().filter(|n| host::efivar(&format!("Boot{:04X}", n)).map(|d| d.windows(42).any(|w| w == node)).unwrap_or(false)).map(|n| format!("Boot{:04X}", n)).collect();
                println!("boot entry check: the partition path {}", if same.is_empty() { "matches no existing entry".to_string() } else { format!("is identical to the one in {}", same.join(", ")) });
            }
            Err(e) => println!("boot entry check: {}", e),
        }
    }
    println!("secure boot: {}", if secure_boot() { "on" } else { "off" });
    let order = host::boot_order();
    println!("boot order: {}", order.iter().map(|n| format!("{:04X}", n)).collect::<Vec<_>>().join(","));
    for (n, active, desc) in host::all_boot_options() {
        println!("  Boot{:04X}{} {}", n, if active { "*" } else { " " }, desc);
    }
    let hs = host::scan_host(&parts, &host::RootOverride::new());
    println!("entries the boot menu will show:");
    for e in &hs.scan.entries {
        println!("  {}{}  [{}]", e.title, if e.hidden_default { " (hidden)" } else { "" }, e.id);
    }
    for m in menus::scan(&Env::Real { parts: &parts }) {
        println!("other boot menu: {:?} {} @ {} visible={} managed={} problem={:?}", m.kind, m.name, m.location, m.visible, m.managed, m.problem);
    }
    Ok(())
}
