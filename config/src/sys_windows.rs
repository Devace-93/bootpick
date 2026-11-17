//! Windows platform layer: volumes and partitions (volume GUID paths and
//! storage IOCTLs, nothing is mounted), firmware variables (the Win32 firmware
//! environment API, needs elevation), the UAC-elevated helper and its named
//! pipe, and the installed app files.

use crate::host::{self, Part};
use crate::proto::HelperIo;
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
use windows_sys::Win32::Security::{AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, TokenElevation, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, FlushFileBuffers, GetDiskFreeSpaceExW, GetFileType, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, MoveFileExW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TYPE_UNKNOWN, MOVEFILE_DELAY_UNTIL_REBOOT, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Ioctl::{DISK_GEOMETRY, PARTITION_INFORMATION_EX, PARTITION_STYLE_GPT, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, WaitForSingleObject, INFINITE};
use windows_sys::Win32::System::WindowsProgramming::{GetFirmwareEnvironmentVariableExW, SetFirmwareEnvironmentVariableExW};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

// All of these accept a handle opened without any access right.
const IOCTL_DISK_GET_DRIVE_GEOMETRY: u32 = 0x0007_0000;
const IOCTL_DISK_GET_PARTITION_INFO_EX: u32 = 0x0007_0048;
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const FILE_DEVICE_DISK: u32 = 7;

/// Non-volatile, boot service and runtime access: what Boot#### variables use.
const VAR_ATTRS: u32 = 7;

const PIPE_PREFIX: &str = r"\\.\pipe\bootpick-helper-";
const APP_EXE: &str = "bootpick-config.exe";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateFileW / OpenProcessToken and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_device(path: &str, access: u32) -> Option<Handle> {
    let p = wide(path);
    // SAFETY: `p` is a NUL-terminated UTF-16 string that outlives the call.
    let h = unsafe { CreateFileW(p.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut()) };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Handle(h))
}

/// Runs a device control request that fills one fixed-size structure.
fn ioctl_out<T: Copy>(h: &Handle, code: u32, input: &[u8]) -> Option<T> {
    // SAFETY: T is a plain C structure from the Windows headers (all-zero is
    // valid); the kernel writes at most size_of::<T>() bytes into it.
    unsafe {
        let mut out: T = std::mem::zeroed();
        let mut returned = 0u32;
        let inp = if input.is_empty() { null() } else { input.as_ptr().cast::<c_void>() };
        let ok = DeviceIoControl(h.0, code, inp, input.len() as u32, (&mut out as *mut T).cast(), std::mem::size_of::<T>() as u32, &mut returned, null_mut());
        // Variable-length results (the storage descriptor) report "more data"
        // while still filling the fixed header.
        (ok != 0 || returned as usize >= std::mem::size_of::<T>()).then_some(out)
    }
}

fn guid_string(g: &GUID) -> String {
    let d = g.data4;
    format!("{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7])
}

/// The bytes of a GUID as firmware device paths store it (the in-memory layout).
fn guid_bytes(g: &GUID) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..4].copy_from_slice(&g.data1.to_le_bytes());
    b[4..6].copy_from_slice(&g.data2.to_le_bytes());
    b[6..8].copy_from_slice(&g.data3.to_le_bytes());
    b[8..].copy_from_slice(&g.data4);
    b
}

// ----- partitions ---------------------------------------------------------------------

/// Every volume as `\\?\Volume{guid}\`.
fn volumes() -> Vec<String> {
    let mut out = Vec::new();
    let mut name = [0u16; 260];
    // SAFETY: `name` is a writable buffer of the stated length; the find
    // handle is closed below.
    unsafe {
        let h = FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32);
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return out;
        }
        loop {
            out.push(from_wide(&name));
            if FindNextVolumeW(h, name.as_mut_ptr(), name.len() as u32) == 0 {
                break;
            }
        }
        FindVolumeClose(h);
    }
    out
}

/// (file system name, serial number) of a volume root; fails without
/// elevation for an EFI System Partition.
fn volume_fs(root: &str) -> Option<(String, u32)> {
    let r = wide(root);
    let mut fs_name = [0u16; 64];
    let mut serial = 0u32;
    // SAFETY: all pointers are valid for the lengths given, or null where optional.
    let ok = unsafe { GetVolumeInformationW(r.as_ptr(), null_mut(), 0, &mut serial, null_mut(), null_mut(), fs_name.as_mut_ptr(), fs_name.len() as u32) };
    (ok != 0).then(|| (from_wide(&fs_name), serial))
}

struct VolInfo {
    info: PARTITION_INFORMATION_EX,
    disk: Option<u32>,
    bus: Option<&'static str>,
    sector: u64,
}

fn volume_info(root: &str) -> Option<VolInfo> {
    let h = open_device(root.trim_end_matches('\\'), 0)?;
    let info: PARTITION_INFORMATION_EX = ioctl_out(&h, IOCTL_DISK_GET_PARTITION_INFO_EX, &[])?;
    // Optical drives and the like also answer; only disk partitions count.
    if info.PartitionNumber == 0 {
        return None;
    }
    let disk = ioctl_out::<STORAGE_DEVICE_NUMBER>(&h, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[]).filter(|d| d.DeviceType == FILE_DEVICE_DISK).map(|d| d.DeviceNumber);
    // StorageDeviceProperty, PropertyStandardQuery.
    let query = [0u8; std::mem::size_of::<STORAGE_PROPERTY_QUERY>()];
    let bus = ioctl_out::<STORAGE_DEVICE_DESCRIPTOR>(&h, IOCTL_STORAGE_QUERY_PROPERTY, &query).and_then(|d| match d.BusType {
        1 => Some("scsi"),
        2 | 3 => Some("ata"),
        7 => Some("usb"),
        11 => Some("sata"),
        12 | 13 => Some("mmc"),
        17 => Some("nvme"),
        _ => None,
    });
    let sector = ioctl_out::<DISK_GEOMETRY>(&h, IOCTL_DISK_GET_DRIVE_GEOMETRY, &[]).map(|g| g.BytesPerSector as u64).filter(|b| *b > 0).unwrap_or(512);
    Some(VolInfo { info, disk, bus, sector })
}

/// Volumes that are partitions of a disk. `mountpoint` is the volume GUID
/// path, so the files are reachable without mounting anything; `uuid` is the
/// FAT serial number in the form GRUB and Linux use ("4E38-0C72"). Linux file
/// systems have no Windows volume and are not listed.
pub fn partitions() -> Vec<Part> {
    let mut out = Vec::new();
    for root in volumes() {
        let Some(v) = volume_info(&root) else { continue };
        let gpt = v.info.PartitionStyle == PARTITION_STYLE_GPT;
        // SAFETY: the Gpt member of the union is the valid one for a GPT partition.
        let (partuuid, parttype) = if gpt { unsafe { (Some(guid_string(&v.info.Anonymous.Gpt.PartitionId)), Some(guid_string(&v.info.Anonymous.Gpt.PartitionType))) } } else { (None, None) };
        let fs = volume_fs(&root);
        let fstype = fs.as_ref().map(|(name, _)| match name.to_ascii_uppercase().as_str() {
            "FAT" | "FAT12" | "FAT16" | "FAT32" => "vfat".to_string(),
            other => other.to_ascii_lowercase(),
        });
        let is_fat = fstype.as_deref() == Some("vfat");
        let n = v.info.PartitionNumber;
        let disk = v.disk.map(|d| format!("disk{}", d));
        out.push(Part {
            name: format!("{}p{}", disk.as_deref().unwrap_or("disk"), n),
            path: match v.disk {
                Some(d) => format!("Disk {} partition {}", d, n),
                None => format!("partition {}", n),
            },
            pkname: disk,
            kind: Some("part".into()),
            tran: v.bus.map(str::to_string),
            partn: Some(n),
            partuuid,
            parttype,
            uuid: fs.filter(|_| is_fat).map(|(_, serial)| format!("{:04X}-{:04X}", serial >> 16, serial & 0xffff)),
            fstype,
            mountpoint: Some(root),
        });
    }
    out.sort_by(|a, b| (&a.pkname, a.partn).cmp(&(&b.pkname, b.partn)));
    out
}

/// The volume GUID path of the Windows system drive.
pub fn system_volume() -> Option<String> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let mp = wide(&format!("{}\\", drive));
    let mut name = [0u16; 260];
    // SAFETY: both buffers are valid for the lengths given.
    let ok = unsafe { GetVolumeNameForVolumeMountPointW(mp.as_ptr(), name.as_mut_ptr(), name.len() as u32) };
    (ok != 0).then(|| from_wide(&name))
}

/// Gives access to a partition's files. Windows volumes are always reachable
/// through their volume GUID path, so nothing is mounted or unmounted.
pub struct Mounted {
    pub dir: PathBuf,
}

impl Mounted {
    pub fn open(p: &Part, _writable: bool) -> Option<Mounted> {
        let mp = p.mountpoint.as_ref().filter(|m| !m.is_empty())?;
        Some(Mounted { dir: PathBuf::from(mp) })
    }
}

/// Bytes available on the volume that holds `dir`.
pub fn free_space(dir: &Path) -> Option<u64> {
    let d = wide(&dir.display().to_string());
    let mut free = 0u64;
    // SAFETY: `d` is NUL-terminated; `free` receives the result.
    let ok = unsafe { GetDiskFreeSpaceExW(d.as_ptr(), &mut free, null_mut(), null_mut()) };
    (ok != 0).then_some(free)
}

/// Flushes the FAT volumes, so a restart right after Save loses nothing.
pub fn sync() {
    for root in volumes() {
        let fat = volume_fs(&root).map(|(n, _)| n.to_ascii_uppercase().starts_with("FAT")).unwrap_or(false);
        if !fat {
            continue;
        }
        if let Some(h) = open_device(root.trim_end_matches('\\'), GENERIC_READ | GENERIC_WRITE) {
            // SAFETY: a valid volume handle with write access.
            unsafe { FlushFileBuffers(h.0) };
        }
    }
}

// ----- privileges -----------------------------------------------------------------------

fn process_token(access: u32) -> Option<Handle> {
    let mut tok: HANDLE = null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle; `tok` receives a real one.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut tok) };
    (ok != 0).then_some(Handle(tok))
}

pub fn is_elevated() -> bool {
    let Some(tok) = process_token(TOKEN_QUERY) else { return false };
    let mut el = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `el` is a writable TOKEN_ELEVATION of the size given.
    let ok = unsafe { GetTokenInformation(tok.0, TokenElevation, (&mut el as *mut TOKEN_ELEVATION).cast(), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len) };
    ok != 0 && el.TokenIsElevated != 0
}

/// Firmware variables need SeSystemEnvironmentPrivilege, which an elevated
/// administrator holds but has to enable.
fn enable_firmware_privilege() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(tok) = process_token(TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY) else { return };
        let name = wide("SeSystemEnvironmentPrivilege");
        // SAFETY: plain structures and valid pointers; failure only means the
        // later variable calls report "access denied".
        unsafe {
            let mut tp: TOKEN_PRIVILEGES = std::mem::zeroed();
            if LookupPrivilegeValueW(null(), name.as_ptr(), &mut tp.Privileges[0].Luid) == 0 {
                return;
            }
            tp.PrivilegeCount = 1;
            tp.Privileges[0] = LUID_AND_ATTRIBUTES { Luid: tp.Privileges[0].Luid, Attributes: SE_PRIVILEGE_ENABLED };
            AdjustTokenPrivileges(tok.0, 0, &tp, 0, null_mut(), null_mut());
        }
    });
}

// ----- firmware variables ---------------------------------------------------------------

/// The data of a firmware variable (None without elevation, on BIOS systems,
/// or when it does not exist).
pub fn efivar_vendor(name: &str, vendor: &str) -> Option<Vec<u8>> {
    efivar_attrs(name, vendor).map(|(data, _)| data)
}

/// The data and the attributes of a firmware variable.
fn efivar_attrs(name: &str, vendor: &str) -> Option<(Vec<u8>, u32)> {
    enable_firmware_privilege();
    let n = wide(name);
    let g = wide(&format!("{{{}}}", vendor));
    let mut buf = vec![0u8; 32 * 1024];
    // SAFETY: `buf` is writable for the length given; the strings are NUL-terminated.
    let mut attrs = 0u32;
    let len = unsafe { GetFirmwareEnvironmentVariableExW(n.as_ptr(), g.as_ptr(), buf.as_mut_ptr().cast(), buf.len() as u32, &mut attrs) };
    if len == 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some((buf, attrs))
}

/// Writes a firmware variable; empty data deletes it.
fn set_efivar(name: &str, vendor: &str, data: &[u8]) -> Result<(), String> {
    set_efivar_attrs(name, vendor, data, VAR_ATTRS)
}

fn set_efivar_attrs(name: &str, vendor: &str, data: &[u8], attrs: u32) -> Result<(), String> {
    enable_firmware_privilege();
    let n = wide(name);
    let g = wide(&format!("{{{}}}", vendor));
    let ptr = if data.is_empty() { null() } else { data.as_ptr().cast::<c_void>() };
    // SAFETY: `ptr` is valid for `data.len()` bytes (or null with length 0).
    let ok = unsafe { SetFirmwareEnvironmentVariableExW(n.as_ptr(), g.as_ptr(), ptr, data.len() as u32, attrs) };
    if ok != 0 {
        Ok(())
    } else {
        // SAFETY: reads the calling thread's last error code.
        Err(format!("writing the firmware variable {} failed (error {})", name, unsafe { GetLastError() }))
    }
}

#[link(name = "ntdll")]
extern "system" {
    fn NtEnumerateSystemEnvironmentValuesEx(information_class: u32, buffer: *mut c_void, buffer_length: *mut u32) -> i32;
}

/// Names of the firmware variables of the EFI global vendor.
fn global_variable_names() -> Option<Vec<String>> {
    Some(variable_names()?.into_iter().filter(|(vendor, _)| vendor == host::EFI_GLOBAL).map(|(_, name)| name).collect())
}

/// (vendor GUID, name) of every firmware variable.
pub fn variable_names() -> Option<Vec<(String, String)>> {
    enable_firmware_privilege();
    const NAMES: u32 = 1; // VARIABLE_INFORMATION_NAMES
    let mut len = 0u32;
    // SAFETY: a null buffer with length 0 only queries the size needed.
    unsafe { NtEnumerateSystemEnvironmentValuesEx(NAMES, null_mut(), &mut len) };
    if len == 0 {
        return None;
    }
    // u32 storage keeps the entries (which start with a u32) aligned.
    let mut buf = vec![0u32; (len as usize).div_ceil(4) + 1];
    let mut len = (buf.len() * 4) as u32;
    // SAFETY: `buf` is writable for `len` bytes.
    if unsafe { NtEnumerateSystemEnvironmentValuesEx(NAMES, buf.as_mut_ptr().cast(), &mut len) } < 0 {
        return None;
    }
    // SAFETY: the same allocation viewed as bytes, limited to what was written.
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), (len as usize).min(buf.len() * 4)) };
    let mut names = Vec::new();
    let mut at = 0usize;
    // Each entry: u32 offset of the next one (0 = last), 16-byte vendor GUID, UTF-16 name.
    while at + 20 <= bytes.len() {
        let next = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        let end = if next == 0 { bytes.len() } else { (at + next).min(bytes.len()) };
        let g = &bytes[at + 4..at + 20];
        let vendor = GUID { data1: u32::from_le_bytes([g[0], g[1], g[2], g[3]]), data2: u16::from_le_bytes([g[4], g[5]]), data3: u16::from_le_bytes([g[6], g[7]]), data4: [g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]] };
        let name: Vec<u16> = bytes[at + 20..end].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
        names.push((guid_string(&vendor), String::from_utf16_lossy(&name)));
        if next == 0 {
            break;
        }
        at += next;
    }
    Some(names)
}

/// The number of every Boot#### variable present (also those not in BootOrder).
pub fn boot_numbers() -> Vec<u16> {
    let mut v: Vec<u16> = match global_variable_names() {
        Some(names) => names.iter().filter_map(|n| n.strip_prefix("Boot").filter(|h| h.len() == 4).and_then(|h| u16::from_str_radix(h, 16).ok())).collect(),
        // Enumeration unavailable: BootOrder plus the range firmware normally uses.
        None => host::boot_order().into_iter().chain(0..0x200).filter(|n| host::efivar(&format!("Boot{:04X}", n)).is_some()).collect(),
    };
    v.sort();
    v.dedup();
    v
}

pub fn boot_set_active(n: u16, active: bool) -> Result<(), String> {
    let name = format!("Boot{:04X}", n);
    let mut data = host::efivar(&name).filter(|d| d.len() >= 4).ok_or_else(|| format!("the firmware entry {} could not be read", name))?;
    if active {
        data[0] |= 1;
    } else {
        data[0] &= !1;
    }
    set_efivar(&name, host::EFI_GLOBAL, &data)
}

pub fn boot_set_order(order: &[u16]) -> Result<(), String> {
    let data: Vec<u8> = order.iter().flat_map(|n| n.to_le_bytes()).collect();
    set_efivar("BootOrder", host::EFI_GLOBAL, &data)?;
    set_physical_order(order)?;
    match order.first() {
        Some(first) => hp_promote(*first),
        None => Ok(()),
    }
}

const HP_VENDOR: &str = "30fef3a5-ba29-4767-9f11-7c7fd606ffa6";

/// HP's list of operating system loaders ("OS Boot Manager" in the firmware
/// setup), in the order it starts them: `Name (disk model);Name (disk model)`.
pub fn hp_os_order() -> Option<String> {
    let (data, _) = efivar_attrs("HUOOL", HP_VENDOR)?;
    let text: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&text).trim_end_matches('\0').to_string())
}

/// The hard drive node of a load option's device path, when it has one.
fn hd_node(option: &[u8]) -> Option<&[u8]> {
    option.windows(4).position(|w| w == [0x04, 0x01, 42, 0]).and_then(|i| option.get(i..i + 42))
}

/// HP firmware orders the loaders on the internal disk by its own list of
/// their names (the `HUOOL` variable) and rewrites BootOrder and
/// PhysicalBootOrder from it at every start. When the list exists, the entry
/// that was just made first is moved to (or added at) its front.
fn hp_promote(n: u16) -> Result<(), String> {
    let Some((raw, attrs)) = efivar_attrs("HUOOL", HP_VENDOR) else { return Ok(()) };
    // Only loaders on a disk are in the list (not "EFI USB Device" and the like).
    let Some(option) = host::efivar(&format!("Boot{:04X}", n)) else { return Ok(()) };
    let (Some(node), Some((_, name))) = (hd_node(&option), bootpick_core::scan::parse_load_option(&option)) else { return Ok(()) };
    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let nuls = units.iter().rev().take_while(|c| **c == 0).count();
    let text = String::from_utf16_lossy(&units[..units.len() - nuls]);
    let trailing = text.ends_with(';');
    let items: Vec<&str> = text.split(';').filter(|i| !i.is_empty()).collect();
    let name_of = |item: &str| item.rsplit_once(" (").map(|(a, _)| a.to_string()).unwrap_or_else(|| item.to_string());
    if items.first().map(|i| name_of(i)) == Some(name.clone()) {
        return Ok(());
    }
    // " (disk model)": taken from the entry itself when the firmware already
    // listed it, else from another loader on the same partition, else from
    // the first item.
    let same_partition: Vec<String> = boot_numbers().into_iter().filter_map(|b| host::efivar(&format!("Boot{:04X}", b))).filter(|d| hd_node(d) == Some(node)).filter_map(|d| bootpick_core::scan::parse_load_option(&d).map(|(_, desc)| desc)).collect();
    let suffix_of = |item: &&str| item.rfind(" (").map(|i| item[i..].to_string());
    let suffix = items.iter().find(|i| name_of(i) == name).and_then(|i| suffix_of(i)).or_else(|| items.iter().find(|i| same_partition.contains(&name_of(i))).and_then(|i| suffix_of(i))).or_else(|| items.iter().find_map(|i| suffix_of(i)));
    let Some(suffix) = suffix else { return Ok(()) };
    let mut new: Vec<String> = vec![format!("{}{}", name, suffix)];
    new.extend(items.iter().filter(|i| name_of(i) != name).map(|i| i.to_string()));
    let mut out = new.join(";");
    if trailing {
        out.push(';');
    }
    let data: Vec<u8> = out.encode_utf16().chain(std::iter::repeat(0).take(nuls)).flat_map(u16::to_le_bytes).collect();
    let written = set_efivar_attrs("HUOOL", HP_VENDOR, &data, attrs);
    if written.is_ok() && efivar_attrs("HUOOL", HP_VENDOR).map(|(d, _)| d) == Some(data) {
        Ok(())
    } else {
        Err("this HP firmware keeps its own list of boot loaders and it could not be changed: move the entry up in the firmware setup (Boot Options, OS Boot Manager)".into())
    }
}

/// Insyde firmware keeps the order it really uses in its own
/// `PhysicalBootOrder` and rebuilds BootOrder from it at every start, so an
/// entry put first only in BootOrder is back at the end after a restart.
/// When the variable exists, it gets the same order: its own entries in the
/// new sequence, plus the entry that is now first.
fn set_physical_order(order: &[u16]) -> Result<(), String> {
    const INSYDE_GENERIC: &str = "59d1c24f-50f1-401a-b101-f33e0daed443";
    let read = || efivar_attrs("PhysicalBootOrder", INSYDE_GENERIC).map(|(d, attrs)| (d.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<u16>>(), attrs));
    let Some((old, attrs)) = read() else { return Ok(()) };
    let new: Vec<u16> = order.iter().copied().filter(|n| old.contains(n) || order.first() == Some(n)).collect();
    if new == old {
        return Ok(());
    }
    let data: Vec<u8> = new.iter().flat_map(|n| n.to_le_bytes()).collect();
    let written = set_efivar_attrs("PhysicalBootOrder", INSYDE_GENERIC, &data, attrs);
    // Read back: this is the variable that decides what starts.
    if written.is_ok() && read().map(|(now, _)| now) == Some(new) {
        Ok(())
    } else {
        Err("the firmware keeps its own boot order (PhysicalBootOrder) and it could not be changed: set the order in the firmware setup".into())
    }
}

/// Deletes Boot#### and takes it out of BootOrder.
pub fn boot_delete(n: u16) -> Result<(), String> {
    let order = host::boot_order();
    if order.contains(&n) {
        let rest: Vec<u16> = order.into_iter().filter(|x| *x != n).collect();
        boot_set_order(&rest)?;
    }
    set_efivar(&format!("Boot{:04X}", n), host::EFI_GLOBAL, &[])
}

/// The firmware load option that starts `loader` from the partition `part`
/// (GPT only).
pub fn boot_option(part: &Part, label: &str, loader: &str) -> Result<Vec<u8>, String> {
    let root = part.mountpoint.as_deref().ok_or("the partition has no volume")?;
    let v = volume_info(root).ok_or("the partition could not be queried")?;
    if v.info.PartitionStyle != PARTITION_STYLE_GPT {
        return Err("the disk is not GPT partitioned".into());
    }
    // SAFETY: the Gpt member of the union is the valid one for a GPT partition.
    let guid = unsafe { guid_bytes(&v.info.Anonymous.Gpt.PartitionId) };
    Ok(host::load_option(label, loader, v.info.PartitionNumber, v.info.StartingOffset as u64 / v.sector, v.info.PartitionLength as u64 / v.sector, guid))
}

/// Creates a Boot#### entry that starts `loader` from the partition `part`
/// and returns its number. BootOrder is left to the caller.
pub fn boot_create(part: &Part, label: &str, loader: &str) -> Result<u16, String> {
    let data = boot_option(part, label, loader)?;
    let used = boot_numbers();
    let n = (0..0x200u16).find(|n| !used.contains(n)).ok_or("no free firmware boot entry number")?;
    set_efivar(&format!("Boot{:04X}", n), host::EFI_GLOBAL, &data)?;
    Ok(n)
}

/// A timeout stored by `bootctl set-timeout` overrides loader.conf: delete it.
pub fn clear_sd_timeout() {
    let _ = set_efivar("LoaderConfigTimeout", "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f", &[]);
}

// ----- the helper and its caller ----------------------------------------------------------

/// The helper's caller. UAC already asked the user; there is no separate
/// identity to track as on Linux.
pub struct Caller;

pub fn caller() -> Result<Caller, String> {
    if is_elevated() {
        Ok(Caller)
    } else {
        Err("bootpick-config --helper must run elevated (the settings app starts it)".into())
    }
}

/// The caller's directories are not told apart by owner on Windows; the
/// checks on what is read back from them still apply.
pub fn owned_by(_meta: &fs::Metadata, _caller: &Caller) -> bool {
    true
}

/// Files the helper creates in the user's staging folder inherit its
/// permissions, so the user can already edit them.
pub fn give(_path: &Path, _caller: &Caller) -> std::io::Result<()> {
    Ok(())
}

/// The helper's side of the channel: the named pipe given with `--pipe`.
pub fn helper_channel(args: &[String]) -> Result<(Box<dyn BufRead>, Box<dyn Write>), String> {
    let name = args.iter().position(|a| a == "--pipe").and_then(|i| args.get(i + 1)).ok_or("--helper needs --pipe NAME")?;
    if !name.starts_with(PIPE_PREFIX) {
        return Err("not a bootpick helper pipe".into());
    }
    let pipe = OpenOptions::new().read(true).write(true).open(name).map_err(|e| format!("{}: {}", name, e))?;
    let reader = pipe.try_clone().map_err(|e| e.to_string())?;
    Ok((Box::new(BufReader::new(reader)), Box::new(pipe)))
}

/// Starts the privileged helper elevated (a UAC prompt) and connects to it
/// through a named pipe that only this process serves.
pub fn spawn_helper() -> Result<HelperIo, String> {
    const CANCELLED: &str = "Authorization was cancelled or the helper stopped.";
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let name = format!("{}{}-{:08x}", PIPE_PREFIX, std::process::id(), nonce);
    let wname = wide(&name);
    // SAFETY: `wname` is NUL-terminated; default security (creator and administrators).
    let pipe = unsafe { CreateNamedPipeW(wname.as_ptr(), PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, 1, 1 << 16, 1 << 16, 0, null()) };
    if pipe == INVALID_HANDLE_VALUE || pipe.is_null() {
        return Err("could not create the helper pipe".into());
    }
    // SAFETY: `pipe` is a valid handle owned from here on by `file`.
    let file = unsafe { File::from_raw_handle(pipe as _) };

    let verb = wide("runas");
    let wexe = wide(&exe.display().to_string());
    let params = wide(&format!("--helper --pipe {}", name));
    // SAFETY: SHELLEXECUTEINFOW is a plain structure; the strings outlive the call.
    let process = unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = wexe.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE as i32;
        if ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
            return Err(CANCELLED.into());
        }
        info.hProcess as usize
    };
    // When the helper ends: unblock a server that still waits for it to connect.
    let poke = name.clone();
    let watcher = std::thread::spawn(move || {
        // SAFETY: the process handle is owned by this thread and closed once.
        unsafe {
            WaitForSingleObject(process as HANDLE, INFINITE);
            CloseHandle(process as HANDLE);
        }
        let _ = OpenOptions::new().read(true).write(true).open(&poke);
    });
    // SAFETY: `pipe` is the valid server end; blocks until the helper connects.
    let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) != 0 || GetLastError() == ERROR_PIPE_CONNECTED };
    if !connected {
        return Err(CANCELLED.into());
    }
    let reader = file.try_clone().map_err(|e| e.to_string())?;
    Ok(HelperIo {
        reader: Box::new(BufReader::new(reader)),
        writer: Box::new(file),
        wait: Some(Box::new(move || {
            let _ = watcher.join();
        })),
    })
}

// ----- the installed app ------------------------------------------------------------------

/// `%ProgramFiles%\bootpick`
pub fn app_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into())).join("bootpick")
}

pub fn app_exe() -> PathBuf {
    app_dir().join(APP_EXE)
}

/// The Start menu shortcut for all users.
pub fn shortcut_path() -> PathBuf {
    PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())).join(r"Microsoft\Windows\Start Menu\Programs\bootpick settings.lnk")
}

fn delete_at_restart(path: &Path) {
    let p = wide(&path.display().to_string());
    // SAFETY: `p` is NUL-terminated; a null target means "delete".
    unsafe { MoveFileExW(p.as_ptr(), null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
}

/// Removes the app. The program file is in use while the app (or this
/// helper) runs from it, so Windows deletes it at the next restart.
pub fn remove_app() {
    let _ = fs::remove_file(shortcut_path());
    let exe = app_exe();
    if exe.exists() && fs::remove_file(&exe).is_err() {
        delete_at_restart(&exe);
    }
    let dir = app_dir();
    if dir.exists() && fs::remove_dir(&dir).is_err() {
        delete_at_restart(&dir);
    }
}

// ----- desktop integration ------------------------------------------------------------------

/// Where the app keeps its staging copy.
pub fn staging_base() -> PathBuf {
    std::env::temp_dir()
}

/// The user's locale as a BCP 47 tag ("es-MX").
pub fn locale() -> String {
    let mut buf = [0u16; 85];
    // SAFETY: `buf` is writable for the length given.
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if n > 0 {
        from_wide(&buf)
    } else {
        String::new()
    }
}

/// System fonts for scripts egui's defaults lack: (name, path, face index,
/// language). Several candidates per name; the first that exists is used.
pub fn script_fonts() -> Vec<(&'static str, String, u32, &'static str)> {
    let dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    let f = |file: &str| dir.join(file).display().to_string();
    vec![
        ("win-cjk-jp", f("YuGothR.ttc"), 0, "ja"),
        ("win-cjk-jp", f("msgothic.ttc"), 0, "ja"),
        ("win-cjk-kr", f("malgun.ttf"), 0, "ko"),
        ("win-cjk-sc", f("msyh.ttc"), 0, "zh"),
        ("win-cjk-sc", f("simsun.ttc"), 0, "zh"),
        ("win-deva", f("Nirmala.ttc"), 0, "hi"),
        ("win-deva", f("Nirmala.ttf"), 0, "hi"),
        ("win-thai", f("LeelawUI.ttf"), 0, "th"),
        ("win-thai", f("tahoma.ttf"), 0, "th"),
        ("win-arab", f("segoeui.ttf"), 0, "ar"),
        ("win-arab", f("arial.ttf"), 0, "ar"),
    ]
}

/// The program has no console of its own (GUI subsystem). For the command
/// line modes: keep redirected output as it is, otherwise write to the
/// console of the process that started this one.
pub fn attach_console() {
    // SAFETY: console and handle calls with valid arguments; the CON files
    // are opened once and stay open for the life of the process.
    unsafe {
        let valid = |h: HANDLE| !h.is_null() && h != INVALID_HANDLE_VALUE && GetFileType(h) != FILE_TYPE_UNKNOWN;
        let (out, err, inp) = (GetStdHandle(STD_OUTPUT_HANDLE), GetStdHandle(STD_ERROR_HANDLE), GetStdHandle(STD_INPUT_HANDLE));
        let (need_out, need_err, need_in) = (!valid(out), !valid(err), !valid(inp));
        if !(need_out || need_err || need_in) {
            return;
        }
        AttachConsole(ATTACH_PARENT_PROCESS);
        // Attaching may point all three handles at the console: put back the
        // ones that were redirected to a file or a pipe.
        if !need_out {
            SetStdHandle(STD_OUTPUT_HANDLE, out);
        }
        if !need_err {
            SetStdHandle(STD_ERROR_HANDLE, err);
        }
        if !need_in {
            SetStdHandle(STD_INPUT_HANDLE, inp);
        }
        let open = |name: &str| {
            let n = wide(name);
            CreateFileW(n.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut())
        };
        if need_out || need_err {
            let out = open("CONOUT$");
            if valid(out) {
                if need_out {
                    SetStdHandle(STD_OUTPUT_HANDLE, out);
                }
                if need_err {
                    SetStdHandle(STD_ERROR_HANDLE, out);
                }
            }
        }
        if need_in {
            let inp = open("CONIN$");
            if valid(inp) {
                SetStdHandle(STD_INPUT_HANDLE, inp);
            }
        }
    }
}
