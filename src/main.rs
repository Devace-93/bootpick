//! bootpick — a mouse-driven, themeable UEFI boot picker.

#![no_std]
#![no_main]

extern crate alloc;

mod bmp;
mod boot;
mod config;
mod esp;
mod font;
mod gfx;
mod input;
mod scan;
mod ui;

use uefi::prelude::*;

#[entry]
fn main() -> Status {
    let _ = uefi::helpers::init();
    // Firmware watchdogs reboot after ~5 minutes in a menu; disable it.
    let _ = uefi::boot::set_watchdog_timer(0, 0x1_0000, None);
    boot::load_drivers();
    let scan = scan::scan();
    log::info!("bootpick: {} volumes, {} entries", scan.volumes.len(), scan.entries.len());
    ui::run(&scan)
}
