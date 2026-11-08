//! bootpick: a mouse-driven, themeable UEFI boot picker.

#![no_std]
#![no_main]

extern crate alloc;

mod boot;
mod esp;
mod input;
mod scan;
mod shell;
mod trace;

use alloc::string::String;
use uefi::prelude::*;

#[entry]
fn main() -> Status {
    let _ = uefi::helpers::init();
    // Firmware watchdogs reboot after ~5 minutes in a menu; disable it.
    let _ = uefi::boot::set_watchdog_timer(0, 0x1_0000, None);
    shell::splash(0);
    trace::line("==== bootpick start");
    trace::line(&alloc::format!(
        "firmware: {} rev {:#x}, UEFI {}",
        uefi::system::firmware_vendor(),
        uefi::system::firmware_revision(),
        uefi::system::uefi_revision()
    ));
    boot::load_drivers();
    shell::splash(1);
    let (scan, vols) = scan::scan();
    shell::splash(2);
    trace::line(&alloc::format!("scan: {} volumes, {} entries", scan.volumes.len(), scan.entries.len()));
    for (i, v) in scan.volumes.iter().enumerate() {
        trace::line(&alloc::format!("  volume {}: {} ({})", i, v.key, v.disk));
    }
    for e in &scan.entries {
        trace::line(&alloc::format!(
            "  entry: {}{} [{:?}]{}",
            e.title,
            if e.location.is_empty() { String::new() } else { alloc::format!(" @ {}", e.location) },
            e.kind,
            if e.hidden_default { " hidden" } else { "" }
        ));
    }
    trace::flush();
    shell::run(scan, vols)
}
