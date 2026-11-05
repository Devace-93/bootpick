//! Stand-in boot loader for QEMU tests: prints what it was asked to do and
//! returns to the caller when a key is pressed.
#![no_std]
#![no_main]
extern crate alloc;

use core::fmt::Write;
use uefi::boot;
use uefi::prelude::*;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::device_path::text::{AllowShortcuts, DisplayOnly};

#[entry]
fn main() -> Status {
    let _ = uefi::helpers::init();
    uefi::system::with_stdout(|o| {
        let _ = o.reset(false);
        let _ = o.write_str("\r\n  DUMMY LOADER\r\n\r\n");
    });
    if let Ok(li) = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()) {
        if let Some(fp) = li.file_path() {
            if let Ok(s) = fp.to_string16(DisplayOnly(true), AllowShortcuts(false)) {
                log::info!("file path: {}", s);
            }
        }
        if let Ok(opts) = li.load_options_as_cstr16() {
            log::info!("load options: {}", opts);
        }
    }
    log::info!("press any key to return to the menu");
    uefi::system::with_stdin(|i| {
        let _ = i.reset(false);
        loop {
            if let Ok(Some(_)) = i.read_key() {
                break;
            }
            boot::stall(core::time::Duration::from_millis(20));
        }
    });
    Status::SUCCESS
}
