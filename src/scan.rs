//! Discovers bootable things: EFI loaders on every FAT volume, systemd-boot
//! (BLS) entries, and the firmware's own Boot#### options.

use crate::esp;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use uefi::boot;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::runtime::{self, VariableVendor};
use uefi::{CString16, Handle};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Chainload an EFI application from a volume.
    Efi { vol: usize, path: String },
    /// A systemd-boot (Boot Loader Specification) entry: boot it via
    /// systemd-boot with `LoaderEntryOneShot`.
    LinuxBls { vol: usize, sd_boot: String, entry: String },
    /// A firmware boot option (Boot####): set `BootNext` and reset.
    BootNext(u16),
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub icon: String,
    pub kind: Kind,
    pub hidden_default: bool,
}

#[allow(dead_code)]
pub struct Volume {
    pub handle: Handle,
    pub key: String,
}

pub struct Scan {
    pub volumes: Vec<Volume>,
    pub entries: Vec<Entry>,
    pub fw_setup_supported: bool,
}

const HIDE_HINTS: &[&str] = &[
    "oldkern", "old kernel", "-old", "recovery", "fallback", "debug", "safe", "rescue", "memtest", "previous",
];

fn looks_secondary(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    HIDE_HINTS.iter().any(|h| l.contains(h))
}

pub fn icon_for(title: &str) -> &'static str {
    let l = title.to_ascii_lowercase();
    let table: &[(&str, &str)] = &[
        ("windows", "os_win"),
        ("pop", "os_pop"),
        ("ubuntu", "os_ubuntu"),
        ("debian", "os_debian"),
        ("fedora", "os_fedora"),
        ("arch", "os_arch"),
        ("mint", "os_mint"),
        ("manjaro", "os_manjaro"),
        ("refind", "os_refind"),
        ("systemd", "os_systemd"),
        ("shell", "tool_shell"),
        ("memtest", "tool_memtest"),
        ("pxe", "os_network"),
        ("network", "os_network"),
        ("usb", "vol_external"),
        ("dvd", "vol_optical"),
        ("cd", "vol_optical"),
        ("linux", "os_linux"),
        ("vmlinuz", "os_linux"),
    ];
    for (k, v) in table {
        if l.contains(k) {
            return v;
        }
    }
    "os_unknown"
}

pub fn scan() -> Scan {
    let mut volumes = Vec::new();
    let mut entries = Vec::new();

    let handles = boot::find_handles::<SimpleFileSystem>().unwrap_or_default();
    for (i, h) in handles.into_iter().enumerate() {
        let key = esp::volume_key(h, i);
        let vol = volumes.len();
        volumes.push(Volume { handle: h, key: key.clone() });
        let Some(mut fs) = esp::open_fs(h) else { continue };
        scan_volume(&mut fs, vol, &key, &mut entries);
    }

    scan_firmware_options(&mut entries);
    let fw_setup_supported = os_indications_supported() & 1 != 0;

    Scan { volumes, entries, fw_setup_supported }
}

fn scan_volume(fs: &mut uefi::fs::FileSystem, vol: usize, key: &str, out: &mut Vec<Entry>) {
    let mk_id = |path: &str| format!("{}:{}", key, path.to_ascii_lowercase());
    let short = key.split('-').next().unwrap_or(key).to_string();
    let detail_of = |path: &str| format!("{}  ·  volume {}", path, short);

    // Windows
    let win = "\\EFI\\Microsoft\\Boot\\bootmgfw.efi";
    if esp::exists(fs, win) {
        out.push(Entry {
            id: mk_id(win),
            title: "Windows".into(),
            detail: detail_of(win),
            icon: "os_win".into(),
            kind: Kind::Efi { vol, path: win.into() },
            hidden_default: false,
        });
    }

    // systemd-boot + BLS entries
    let sd_boot = "\\EFI\\systemd\\systemd-bootx64.efi";
    let has_sd = esp::exists(fs, sd_boot);
    let mut sd_entries = 0;
    if has_sd {
        for (name, is_dir) in esp::list_dir(fs, "\\loader\\entries") {
            if is_dir || !name.to_ascii_lowercase().ends_with(".conf") {
                continue;
            }
            let path = format!("\\loader\\entries\\{}", name);
            let Some(text) = esp::read_string(fs, &path) else { continue };
            let stem = &name[..name.len() - 5];
            let mut title = stem.replace('_', " ").replace('-', " ");
            let mut version = String::new();
            let mut is_efi = None;
            for line in text.lines() {
                let line = line.trim();
                let mut it = line.splitn(2, char::is_whitespace);
                let k = it.next().unwrap_or("").to_ascii_lowercase();
                let v = it.next().unwrap_or("").trim().to_string();
                match k.as_str() {
                    "title" => title = v,
                    "version" => version = v,
                    "efi" => is_efi = Some(v),
                    _ => {}
                }
            }
            let hidden = looks_secondary(stem) || looks_secondary(&title);
            let detail = if version.is_empty() { detail_of(&name) } else { format!("{} · {}  ·  volume {}", name, version, short) };
            let icon = icon_for(&title).to_string();
            let kind = match is_efi {
                Some(p) if !p.is_empty() => Kind::Efi { vol, path: p.replace('/', "\\") },
                _ => Kind::LinuxBls { vol, sd_boot: sd_boot.into(), entry: stem.into() },
            };
            out.push(Entry { id: mk_id(&path), title, detail, icon, kind, hidden_default: hidden });
            sd_entries += 1;
        }
        out.push(Entry {
            id: mk_id(sd_boot),
            title: "systemd-boot menu".into(),
            detail: detail_of(sd_boot),
            icon: "os_systemd".into(),
            kind: Kind::Efi { vol, path: sd_boot.into() },
            hidden_default: true,
        });
    }
    let _ = sd_entries;

    // Other vendor directories under \EFI
    for (dir, is_dir) in esp::list_dir(fs, "\\EFI") {
        if !is_dir {
            continue;
        }
        let ld = dir.to_ascii_lowercase();
        if ld == "microsoft" || ld == "systemd" || ld == "bootpick" {
            continue;
        }
        let dir_path = format!("\\EFI\\{}", dir);
        let files = esp::list_dir(fs, &dir_path);
        let efis: Vec<String> = files
            .iter()
            .filter(|(n, d)| !d && n.to_ascii_lowercase().ends_with(".efi"))
            .map(|(n, _)| n.clone())
            .collect();
        if efis.is_empty() {
            continue;
        }
        // Prefer shim, then grub, then anything else; the rest become hidden extras.
        let pick = |pat: &str| efis.iter().find(|n| n.to_ascii_lowercase().starts_with(pat)).cloned();
        let primary = pick("shim").or_else(|| pick("grub")).or_else(|| pick("refind")).or_else(|| efis.first().cloned());
        for f in &efis {
            let lf = f.to_ascii_lowercase();
            if lf.starts_with("mm") || lf.starts_with("fb") || lf.contains("fwupd") {
                continue; // MokManager / fallback helpers
            }
            let path = format!("{}\\{}", dir_path, f);
            let is_primary = Some(f) == primary.as_ref();
            let is_fallback_dir = ld == "boot";
            let title = if is_fallback_dir {
                "Fallback loader".to_string()
            } else if is_primary {
                pretty_vendor(&dir)
            } else {
                format!("{} ({})", pretty_vendor(&dir), f)
            };
            let icon = if is_fallback_dir { "os_uefi".to_string() } else { icon_for(&dir).to_string() };
            out.push(Entry {
                id: mk_id(&path),
                title,
                detail: detail_of(&path),
                icon,
                kind: Kind::Efi { vol, path },
                hidden_default: !is_primary || is_fallback_dir || ld == "tools",
            });
        }
    }

    // Loose tools at the root.
    for (name, is_dir) in esp::list_dir(fs, "\\") {
        if is_dir || !name.to_ascii_lowercase().ends_with(".efi") {
            continue;
        }
        let path = format!("\\{}", name);
        out.push(Entry {
            id: mk_id(&path),
            title: name.trim_end_matches(".efi").trim_end_matches(".EFI").to_string(),
            detail: path.clone(),
            icon: icon_for(&name).to_string(),
            kind: Kind::Efi { vol, path },
            hidden_default: true,
        });
    }
}

fn pretty_vendor(dir: &str) -> String {
    let l = dir.to_ascii_lowercase();
    match l.as_str() {
        "ubuntu" => "Ubuntu".into(),
        "debian" => "Debian".into(),
        "fedora" => "Fedora".into(),
        "arch" => "Arch Linux".into(),
        "refind" => "rEFInd".into(),
        "boot" => "Fallback loader".into(),
        _ => {
            let mut c = dir.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => dir.into(),
            }
        }
    }
}

fn scan_firmware_options(out: &mut Vec<Entry>) {
    let Ok((order, _)) = runtime::get_variable_boxed(uefi::cstr16!("BootOrder"), &VariableVendor::GLOBAL_VARIABLE) else {
        return;
    };
    for chunk in order.chunks_exact(2) {
        let n = u16::from_le_bytes([chunk[0], chunk[1]]);
        let Ok(name) = CString16::try_from(format!("Boot{:04X}", n).as_str()) else { continue };
        let Ok((data, _)) = runtime::get_variable_boxed(&name, &VariableVendor::GLOBAL_VARIABLE) else { continue };
        if data.len() < 8 {
            continue;
        }
        let attrs = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if attrs & 1 == 0 {
            continue; // inactive
        }
        let mut desc = String::new();
        let mut i = 6;
        while i + 1 < data.len() {
            let c = u16::from_le_bytes([data[i], data[i + 1]]);
            if c == 0 {
                break;
            }
            desc.push(char::from_u32(c as u32).unwrap_or('?'));
            i += 2;
        }
        if desc.trim().is_empty() {
            desc = format!("Boot{:04X}", n);
        }
        out.push(Entry {
            id: format!("fw:{:04x}", n),
            title: desc.clone(),
            detail: format!("Firmware boot option Boot{:04X}", n),
            icon: icon_for(&desc).to_string(),
            kind: Kind::BootNext(n),
            hidden_default: true,
        });
    }
}

pub fn os_indications_supported() -> u64 {
    match runtime::get_variable_boxed(uefi::cstr16!("OsIndicationsSupported"), &VariableVendor::GLOBAL_VARIABLE) {
        Ok((d, _)) if d.len() >= 8 => u64::from_le_bytes(d[..8].try_into().unwrap()),
        _ => 0,
    }
}
