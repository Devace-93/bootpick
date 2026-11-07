//! Builds the core [`Scan`] from the firmware: every simple file system
//! volume, then the firmware's own Boot#### options.

use crate::esp;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use bootpick_core::scan::{parse_load_option, Scan};
use uefi::boot;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::runtime::{self, VariableVendor};
use uefi::{CString16, Handle};

/// Firmware handle for each `Scan::volumes` index.
pub struct Vol {
    pub handle: Handle,
    pub disk: String,
}

pub fn scan() -> (Scan, Vec<Vol>) {
    let mut s = Scan::default();
    let mut vols = Vec::new();
    let handles = boot::find_handles::<SimpleFileSystem>().unwrap_or_default();
    for (i, h) in handles.into_iter().enumerate() {
        let key = esp::volume_key(h, i);
        let disk = esp::disk_label(h);
        let Some(fs) = esp::open_fs(h) else { continue };
        let mut st = esp::FsStorage(fs);
        s.add_volume(&key, &disk, &mut st);
        vols.push(Vol { handle: h, disk });
    }
    s.finish_volumes();
    if let Ok((order, _)) = runtime::get_variable_boxed(uefi::cstr16!("BootOrder"), &VariableVendor::GLOBAL_VARIABLE) {
        for chunk in order.chunks_exact(2) {
            let n = u16::from_le_bytes([chunk[0], chunk[1]]);
            let Ok(name) = CString16::try_from(format!("Boot{:04X}", n).as_str()) else { continue };
            let Ok((data, _)) = runtime::get_variable_boxed(&name, &VariableVendor::GLOBAL_VARIABLE) else { continue };
            if let Some((true, desc)) = parse_load_option(&data) {
                s.add_firmware_option(n, &desc);
            }
        }
    }
    s.fw_setup_supported = os_indications_supported() & 1 != 0;
    (s, vols)
}

pub fn os_indications_supported() -> u64 {
    match runtime::get_variable_boxed(uefi::cstr16!("OsIndicationsSupported"), &VariableVendor::GLOBAL_VARIABLE) {
        Ok((d, _)) if d.len() >= 8 => u64::from_le_bytes(d[..8].try_into().unwrap()),
        _ => 0,
    }
}

/// The firmware's UI language (`PlatformLang`, e.g. "en-US"), if set.
pub fn platform_lang() -> String {
    match runtime::get_variable_boxed(uefi::cstr16!("PlatformLang"), &VariableVendor::GLOBAL_VARIABLE) {
        Ok((d, _)) => d.iter().take_while(|b| **b != 0).map(|b| *b as char).collect(),
        Err(_) => String::new(),
    }
}
