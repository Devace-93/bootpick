//! The Windows installer (`bootpick-config --install`, elevated; started by
//! install.ps1 or by the installer window in setup.rs). The counterpart of
//! install.sh: copies the boot menu to an EFI System Partition, makes it the
//! first firmware boot entry and installs the settings app. Existing loaders
//! (the Windows boot manager, GRUB, systemd-boot) are left untouched.
//!
//! The files to install are read from a folder (a release package or a built
//! source tree) or from this program itself: `--make-setup OUT.exe` writes a
//! copy of the program with the files packed at its end, a single-file
//! installer.
//!
//! When bootpick is already installed on any FAT partition of this computer
//! (for example from a Linux on the same machine), that copy is updated in
//! place and its settings are kept unless `--reset` is given.

use crate::helper;
use crate::host::{self, DirStorage, Mounted, Part};
use crate::menus::{self, Env};
use crate::sys;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

const LABEL: &str = "bootpick";
const LOADER: &str = "\\EFI\\bootpick\\bootpick.efi";

pub struct Opts {
    pub source: Option<PathBuf>,
    /// Some(true) = reset, Some(false) = keep, None = ask.
    pub reset: Option<bool>,
    pub boot_entry: bool,
    pub app: bool,
    pub open_app: bool,
    pub uninstall: bool,
}

pub fn run(args: &[String]) -> ! {
    sys::attach_console();
    let result = if args.iter().any(|a| a == "--vars") {
        dump_vars()
    } else if args.iter().any(|a| a == "--probe") {
        probe()
    } else if let Some(i) = args.iter().position(|a| a == "--make-setup") {
        make_setup(args.get(i + 1).map(Path::new), args.iter().position(|a| a == "--source").and_then(|i| args.get(i + 1)).map(Path::new))
    } else {
        parse(args).and_then(install)
    };
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

/// The files to install, by their path below `\EFI\bootpick` (with `/`):
/// `bootpick.efi`, `bootpick.conf` (the factory settings) and the files of
/// the folders in `FOLDERS`.
type Source = Vec<(String, Vec<u8>)>;

/// (folder, file extension)
const FOLDERS: [(&str, &str); 5] = [("icons", ".bmp"), ("flags", ".bmp"), ("fonts", ".ttf"), ("backgrounds", ".bmp"), ("drivers", ".efi")];

fn load_folder(out: &mut Source, dir: &Path, folder: &str, ext: &str) -> Result<(), String> {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) && name.to_ascii_lowercase().ends_with(ext) {
            out.push((format!("{}/{}", folder, name), fs::read(e.path()).map_err(|e| format!("{}: {}", name, e))?));
        }
    }
    Ok(())
}

/// From the folder given, from this program when it is a single-file
/// installer, or from the folder the program is in.
fn load_source(given: Option<&Path>) -> Result<Source, String> {
    if given.is_none() && payload_span().is_some() {
        return unpack();
    }
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
            let mut src: Source = vec![
                ("bootpick.efi".into(), fs::read(&efi).map_err(|e| format!("bootpick.efi: {}", e))?),
                ("bootpick.conf".into(), fs::read(root.join("bootpick.conf.example")).map_err(|e| format!("bootpick.conf.example: {}", e))?),
            ];
            for (folder, ext) in FOLDERS {
                let dir = if folder == "drivers" { root.join("assets").join("drivers") } else { assets.join(folder) };
                load_folder(&mut src, &dir, folder, ext)?;
            }
            return Ok(src);
        }
    }
    Err("bootpick.efi not found: run this from the release package or build first (tools\\build.ps1)".into())
}

// ----- the single-file installer ----------------------------------------------------------
//
// program | gzip(files) | length of the gzip part (u64, little endian) | MAGIC
// files: for each one, name length (u16), name, data length (u32), data.

const MAGIC: &[u8; 16] = b"bootpick-setup\x01\n";
const TRAILER: u64 = 24;
const DAMAGED: &str = "the installer file is damaged: download it again";

/// (length of the program itself, length of the packed files) when this
/// program is a single-file installer.
fn payload_span() -> Option<(u64, u64)> {
    span_of(&std::env::current_exe().ok()?)
}

fn span_of(file: &Path) -> Option<(u64, u64)> {
    let mut f = fs::File::open(file).ok()?;
    let total = f.metadata().ok()?.len();
    if total < TRAILER {
        return None;
    }
    f.seek(SeekFrom::End(-(TRAILER as i64))).ok()?;
    let mut tail = [0u8; TRAILER as usize];
    f.read_exact(&mut tail).ok()?;
    if &tail[8..] != MAGIC {
        return None;
    }
    let len = u64::from_le_bytes(tail[..8].try_into().ok()?);
    (len <= total - TRAILER).then(|| (total - TRAILER - len, len))
}

/// Whether this program carries the files to install.
pub fn is_setup() -> bool {
    payload_span().is_some()
}

/// This program without the packed files: what is installed as the settings app.
fn program_bytes() -> Result<Vec<u8>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut bytes = fs::read(&exe).map_err(|e| format!("{}: {}", exe.display(), e))?;
    if let Some((n, _)) = payload_span() {
        bytes.truncate(n as usize);
    }
    Ok(bytes)
}

/// Only the names the installer writes: no path can leave `\EFI\bootpick`.
fn valid_name(name: &str) -> bool {
    if name == "bootpick.efi" || name == "bootpick.conf" {
        return true;
    }
    match name.split_once('/') {
        Some((folder, file)) => {
            FOLDERS.iter().any(|(f, ext)| *f == folder && file.to_ascii_lowercase().ends_with(ext)) && !file.is_empty() && !file.starts_with('.') && !file.contains(['/', '\\', ':'])
        }
        None => false,
    }
}

fn unpack() -> Result<Source, String> {
    unpack_file(&std::env::current_exe().map_err(|e| e.to_string())?)
}

fn unpack_file(file: &Path) -> Result<Source, String> {
    let (start, len) = span_of(file).ok_or("this program carries no files to install")?;
    let mut f = fs::File::open(file).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut packed = Vec::new();
    f.take(len).read_to_end(&mut packed).map_err(|e| e.to_string())?;
    unpack_bytes(&packed)
}

fn unpack_bytes(packed: &[u8]) -> Result<Source, String> {
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(packed).read_to_end(&mut data).map_err(|_| DAMAGED)?;
    let mut rest = &data[..];
    let mut src = Source::new();
    while !rest.is_empty() {
        let n = rest.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or(DAMAGED)?;
        let name = rest.get(2..2 + n).and_then(|b| std::str::from_utf8(b).ok()).ok_or(DAMAGED)?;
        rest = &rest[2 + n..];
        let size = rest.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or(DAMAGED)?;
        let body = rest.get(4..4 + size).ok_or(DAMAGED)?;
        rest = &rest[4 + size..];
        if !valid_name(name) {
            return Err(DAMAGED.into());
        }
        src.push((name.to_string(), body.to_vec()));
    }
    if !src.iter().any(|(n, _)| n == "bootpick.efi") {
        return Err(DAMAGED.into());
    }
    Ok(src)
}

fn pack(src: &Source) -> Result<Vec<u8>, String> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    for (name, data) in src {
        if !valid_name(name) {
            return Err(format!("{}: not a file the installer accepts", name));
        }
        let mut put = |b: &[u8]| enc.write_all(b).map_err(|e| e.to_string());
        put(&(name.len() as u16).to_le_bytes())?;
        put(name.as_bytes())?;
        put(&(data.len() as u32).to_le_bytes())?;
        put(data)?;
    }
    enc.finish().map_err(|e| e.to_string())
}

/// `--make-setup OUT.exe [--source DIR]`: a copy of this program with the
/// files to install packed at its end.
fn make_setup(out: Option<&Path>, source: Option<&Path>) -> Result<(), String> {
    let out = out.ok_or("--make-setup needs the name of the file to write")?;
    let src = load_source(source)?;
    let packed = pack(&src)?;
    let mut bytes = program_bytes()?;
    let program = bytes.len();
    bytes.extend_from_slice(&packed);
    bytes.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    bytes.extend_from_slice(MAGIC);
    fs::write(out, &bytes).map_err(|e| format!("{}: {}", out.display(), e))?;
    // Read back the way the installer will.
    if unpack_file(out)? != src {
        return Err(format!("{}: the packed files do not read back", out.display()));
    }
    println!("{}: {} files, {} MB (program {} MB, files {} MB)", out.display(), src.len(), bytes.len() >> 20, program >> 20, packed.len() >> 20);
    Ok(())
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

/// PowerShell without a console window of its own.
fn powershell(script: &str) -> std::process::Command {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut c = std::process::Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]).creation_flags(CREATE_NO_WINDOW);
    c
}

/// What the installer window shows before anything is changed.
pub struct Survey {
    /// The EFI partition bootpick goes to.
    pub target: String,
    /// bootpick is there already: it can be updated (settings kept or
    /// reset) or removed.
    pub existing: bool,
    /// The settings app is installed in this Windows.
    pub app_installed: bool,
    pub secure_boot: bool,
    /// BitLocker protects the Windows volume: a new first boot entry makes
    /// Windows ask for the recovery key.
    pub bitlocker: bool,
}

pub fn survey() -> Result<Survey, String> {
    if !sys::is_elevated() {
        return Err("administrator rights are needed".into());
    }
    if host::efivar("BootOrder").is_none() {
        return Err("the firmware boot entries cannot be read: bootpick needs a computer started in UEFI mode".into());
    }
    let (esp, existing) = match helper::find_esp() {
        Ok((p, _)) => (p, true),
        Err(_) => (pick_esp(&host::partitions())?, false),
    };
    let bitlocker = powershell("try { if ((Get-BitLockerVolume -MountPoint $env:SystemDrive -ErrorAction Stop).ProtectionStatus -eq 'On') { exit 3 } } catch { }; exit 0").status().ok().and_then(|s| s.code()) == Some(3);
    Ok(Survey { target: describe(&esp), existing, app_installed: sys::app_exe().is_file(), secure_boot: secure_boot(), bitlocker })
}

/// Removes the settings app and its Start menu shortcut from this Windows.
/// The boot menu on the EFI partition and its boot entry stay.
pub fn remove_app() -> Result<(), String> {
    if !sys::is_elevated() {
        return Err("administrator rights are needed".into());
    }
    sys::remove_app();
    Ok(())
}

/// Opens the installed settings app through the shell, so that it starts
/// without administrator rights.
pub fn open_app() -> bool {
    std::process::Command::new("explorer.exe").arg(sys::app_exe()).spawn().is_ok()
}

pub fn install(o: Opts) -> Result<(), String> {
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
    let src = load_source(o.source.as_deref())?;
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
        let needed = src.iter().map(|(_, d)| d.len() as u64).sum::<u64>() + (1 << 20);
        if let Some(free) = sys::free_space(&m.dir) {
            if free < needed {
                return Err(format!("not enough free space on {}: {} MB needed, {} MB free", describe(&esp), needed.div_ceil(1 << 20), free >> 20));
            }
        }
    }
    fs::create_dir_all(dest.join("backgrounds")).map_err(|e| format!("{}: {}", dest.display(), e))?;
    for (name, data) in &src {
        // Settings and bundled backgrounds only on a fresh install: the user
        // may have changed the first and removed some of the second.
        if !fresh && (name == "bootpick.conf" || name.starts_with("backgrounds/")) {
            continue;
        }
        let path = name.split('/').fold(dest.clone(), |p, c| p.join(c));
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        }
        fs::write(&path, data).map_err(|e| format!("{}: {}", name, e))?;
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
            // Without the packed files when this is the single-file installer.
            fs::write(&target, program_bytes()?).map_err(|e| format!("{}: {} (close bootpick settings and try again)", target.display(), e))?;
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
            if open_app() {
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
    powershell(script)
        .env("BOOTPICK_LNK", lnk)
        .env("BOOTPICK_EXE", target)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && lnk.is_file()
}

/// `--probe --vars`: every firmware variable (vendor, name, size, start of
/// the data). For working out how a firmware keeps its boot order.
fn dump_vars() -> Result<(), String> {
    let names = sys::variable_names().ok_or("the firmware variables cannot be listed (run elevated)")?;
    for (vendor, name) in names {
        match sys::efivar_vendor(&name, &vendor) {
            Some(d) => {
                let hex: String = d.iter().take(160).map(|b| format!("{:02x}", b)).collect();
                println!("{} {} [{}] {}{}", vendor, name, d.len(), hex, if d.len() > 160 { "..." } else { "" });
            }
            None => println!("{} {} [unreadable]", vendor, name),
        }
    }
    Ok(())
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
    if let Some(list) = sys::hp_os_order() {
        println!("firmware's own loader list (HP HUOOL): {}", list);
    }
    if let Some(d) = sys::efivar_vendor("PhysicalBootOrder", "59d1c24f-50f1-401a-b101-f33e0daed443") {
        println!("firmware's own order (Insyde PhysicalBootOrder): {}", d.chunks_exact(2).map(|c| format!("{:04X}", u16::from_le_bytes([c[0], c[1]]))).collect::<Vec<_>>().join(","));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_files_round_trip() {
        let src: Source = vec![("bootpick.efi".into(), vec![1, 2, 3]), ("bootpick.conf".into(), b"theme = dark".to_vec()), ("icons/os_windows.bmp".into(), vec![0; 5000]), ("drivers/ext4_x64.efi".into(), Vec::new())];
        let packed = pack(&src).unwrap();
        assert_eq!(unpack_bytes(&packed).unwrap(), src);
        assert!(unpack_bytes(&packed[..packed.len() / 2]).is_err());
    }

    #[test]
    fn packed_names_stay_inside_the_bootpick_folder() {
        for bad in ["icons/../../x.bmp", "..\\x.bmp", "icons/a\\b.bmp", "C:/x.bmp", "icons/x.exe", "other/x.bmp", "x.efi", "icons/", "fonts/.ttf"] {
            assert!(!valid_name(bad), "{}", bad);
            assert!(pack(&vec![(bad.to_string(), Vec::new())]).is_err(), "{}", bad);
        }
        assert!(valid_name("backgrounds/Aurora.BMP"));
    }
}
