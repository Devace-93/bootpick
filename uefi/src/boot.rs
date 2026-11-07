//! Launching entries and system actions.

use crate::esp;
use crate::scan::Vol as Volume;
use bootpick_core::scan::Kind;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::boot::{self, LoadImageSource};
use uefi::proto::BootPolicy;
use uefi::proto::device_path::build::{self, DevicePathBuilder};
use uefi::runtime::{self, ResetType, VariableAttributes, VariableVendor};
use uefi::{guid, CString16, Status};

const SD_LOADER_VENDOR: VariableVendor = VariableVendor(guid!("4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"));

/// Starts the entry. Returns only if launching failed.
pub fn launch(kind: &Kind, volumes: &[Volume]) -> Result<(), String> {
    crate::trace::line(&format!("launch: {:?}", kind));
    crate::trace::flush();
    match kind {
        Kind::Efi { vol, path } => chainload(volumes, *vol, path),
        Kind::LinuxBls { vol, sd_boot, entry } => {
            // Tell systemd-boot which entry to boot, and to boot it now.
            let mut v: Vec<u8> = entry.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            v.extend_from_slice(&[0, 0]);
            let volatile = VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
            runtime::set_variable(uefi::cstr16!("LoaderEntryOneShot"), &SD_LOADER_VENDOR, volatile, &v)
                .map_err(|e| format!("LoaderEntryOneShot: {:?}", e.status()))?;
            crate::trace::line(&format!("  LoaderEntryOneShot = {}", entry));
            // No LoaderConfigTimeoutOneShot: systemd-boot treats it as "force
            // the menu" (with that timeout), so its menu would always appear.
            chainload(volumes, *vol, sd_boot)
        }
        Kind::BootNext(n) => {
            let attrs = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
            runtime::set_variable(uefi::cstr16!("BootNext"), &VariableVendor::GLOBAL_VARIABLE, attrs, &n.to_le_bytes())
                .map_err(|e| format!("BootNext: {:?}", e.status()))?;
            runtime::reset(ResetType::WARM, Status::SUCCESS, None);
        }
    }
}

fn chainload(volumes: &[Volume], vol: usize, path: &str) -> Result<(), String> {
    let volume = volumes.get(vol).ok_or("volume vanished")?;
    let base = esp::device_path_of(volume.handle).ok_or("no device path for volume")?;
    let cpath = CString16::try_from(path).map_err(|_| "bad path")?;
    let mut buf = Vec::new();
    let file_node = DevicePathBuilder::with_vec(&mut buf)
        .push(&build::media::FilePath { path_name: &cpath })
        .map_err(|e| format!("{:?}", e))?
        .finalize()
        .map_err(|e| format!("{:?}", e))?;
    let full = base.append_path(file_node).map_err(|e| format!("{:?}", e))?;
    crate::trace::line(&format!("  chainload {} from volume {} ({})", path, vol, volume.disk));
    let image = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromDevicePath { device_path: &full, boot_policy: BootPolicy::BootSelection },
    )
    .map_err(|e| format!("load {}: {:?}", path, e.status()))?;
    crate::trace::line("  image loaded, starting it (anything after this is the loader's own doing)");
    crate::trace::flush();
    match boot::start_image(image) {
        Ok(()) => Err(format!("{} returned to the menu", path)),
        Err(e) => Err(format!("{}: {:?}", path, e.status())),
    }
}

pub fn firmware_setup() -> Result<(), String> {
    let cur = match runtime::get_variable_boxed(uefi::cstr16!("OsIndications"), &VariableVendor::GLOBAL_VARIABLE) {
        Ok((d, _)) if d.len() >= 8 => u64::from_le_bytes(d[..8].try_into().unwrap()),
        _ => 0,
    };
    let attrs = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    runtime::set_variable(uefi::cstr16!("OsIndications"), &VariableVendor::GLOBAL_VARIABLE, attrs, &(cur | 1).to_le_bytes())
        .map_err(|e| format!("OsIndications: {:?}", e.status()))?;
    runtime::reset(ResetType::WARM, Status::SUCCESS, None);
}

pub fn reboot() -> ! {
    runtime::reset(ResetType::WARM, Status::SUCCESS, None)
}

pub fn shutdown() -> ! {
    runtime::reset(ResetType::SHUTDOWN, Status::SUCCESS, None)
}

/// Loads optional UEFI drivers from `\EFI\bootpick\drivers\*.efi` on the
/// volume bootpick was started from (e.g. UsbMouseDxe.efi on firmware that
/// ships without a mouse driver), then connects them to their devices.
pub fn load_drivers() {
    let Ok(fs_proto) = boot::get_image_file_system(boot::image_handle()) else { return };
    let mut fs = uefi::fs::FileSystem::new(fs_proto);
    let dir = "\\EFI\\bootpick\\drivers";
    let files = esp::list_dir(&mut fs, dir);
    drop(fs);
    let Ok(li) = boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(boot::image_handle()) else { return };
    let Some(dev) = li.device() else { return };
    drop(li);
    let Some(base) = esp::device_path_of(dev) else { return };
    let mut loaded = 0;
    for (name, is_dir) in files {
        if is_dir || !name.to_ascii_lowercase().ends_with(".efi") {
            continue;
        }
        let path = format!("{}\\{}", dir, name);
        let Ok(cpath) = CString16::try_from(path.as_str()) else { continue };
        let mut buf = Vec::new();
        let Ok(node) = DevicePathBuilder::with_vec(&mut buf).push(&build::media::FilePath { path_name: &cpath }).and_then(|b| b.finalize()) else { continue };
        let Ok(full) = base.append_path(node) else { continue };
        match boot::load_image(boot::image_handle(), LoadImageSource::FromDevicePath { device_path: &full, boot_policy: BootPolicy::ExactMatch }) {
            Ok(img) => match boot::start_image(img) {
                Ok(()) => {
                    loaded += 1;
                    crate::trace::line(&format!("driver loaded: {}", name));
                }
                Err(e) => crate::trace::line(&format!("driver {} failed: {:?}", name, e.status())),
            },
            Err(e) => crate::trace::line(&format!("could not load driver {}: {:?}", name, e.status())),
        }
    }
    if loaded > 0 {
        if let Ok(all) = boot::locate_handle_buffer(boot::SearchType::AllHandles) {
            for h in all.iter() {
                let _ = boot::connect_controller(*h, &[], None, true);
            }
        }
    }
}
