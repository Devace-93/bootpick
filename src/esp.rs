//! Helpers around UEFI simple file systems: opening volumes, reading files,
//! listing directories, and identifying partitions.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::fs::{FileSystem, Path};
use uefi::proto::device_path::{DevicePath, DevicePathNodeEnum};
use uefi::proto::device_path::media::PartitionSignature;
use uefi::proto::media::file::FileAttribute;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{CString16, Handle};

pub fn open_fs(handle: Handle) -> Option<FileSystem> {
    if let Ok(p) = boot::open_protocol_exclusive::<SimpleFileSystem>(handle) {
        return Some(FileSystem::new(p));
    }
    // Fall back to a shared open (e.g. the volume we were loaded from may be
    // held by the firmware).
    let params = OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
    // SAFETY: GetProtocol does not take ownership; we only read/write files.
    let p: ScopedProtocol<SimpleFileSystem> =
        unsafe { boot::open_protocol(params, OpenProtocolAttributes::GetProtocol) }.ok()?;
    Some(FileSystem::new(p))
}

pub fn device_path_of(handle: Handle) -> Option<Box<DevicePath>> {
    let params = OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
    // SAFETY: read-only access to the device path.
    let dp: ScopedProtocol<DevicePath> =
        unsafe { boot::open_protocol(params, OpenProtocolAttributes::GetProtocol) }.ok()?;
    Some(dp.to_boxed())
}

/// A stable key for a volume: its GPT partition GUID when available.
pub fn volume_key(handle: Handle, index: usize) -> String {
    if let Some(dp) = device_path_of(handle) {
        for node in dp.node_iter() {
            if let Ok(DevicePathNodeEnum::MediaHardDrive(hd)) = node.as_enum() {
                return match hd.partition_signature() {
                    PartitionSignature::Guid(g) => g.to_string().to_ascii_lowercase(),
                    PartitionSignature::Mbr(m) => format!("mbr-{:02x}{:02x}{:02x}{:02x}-{}", m[0], m[1], m[2], m[3], hd.partition_number()),
                    _ => format!("part-{}", hd.partition_number()),
                };
            }
        }
    }
    format!("vol-{}", index)
}

pub fn cpath(path: &str) -> Option<CString16> {
    CString16::try_from(path).ok()
}

pub fn exists(fs: &mut FileSystem, path: &str) -> bool {
    let Some(p) = cpath(path) else { return false };
    fs.try_exists(Path::new(&p)).unwrap_or(false)
}

pub fn read(fs: &mut FileSystem, path: &str) -> Option<Vec<u8>> {
    let p = cpath(path)?;
    fs.read(Path::new(&p)).ok()
}

pub fn read_string(fs: &mut FileSystem, path: &str) -> Option<String> {
    let bytes = read(fs, path)?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn write(fs: &mut FileSystem, path: &str, data: &[u8]) -> Result<(), String> {
    let p = cpath(path).ok_or("bad path")?;
    fs.write(Path::new(&p), data).map_err(|e| format!("{:?}", e))
}

/// Lists (name, is_dir) of a directory. Skips `.` and `..`.
pub fn list_dir(fs: &mut FileSystem, path: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let Some(p) = cpath(path) else { return out };
    let Ok(iter) = fs.read_dir(Path::new(&p)) else { return out };
    for e in iter.flatten() {
        let name = e.file_name().to_string();
        if name == "." || name == ".." {
            continue;
        }
        let is_dir = e.attribute().contains(FileAttribute::DIRECTORY);
        out.push((name, is_dir));
    }
    out.sort();
    out
}
