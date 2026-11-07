//! bootpick-core: everything that is not tied to UEFI, so the boot menu and
//! the desktop settings app (live preview) share one implementation.
#![no_std]

extern crate alloc;

pub mod bmp;
pub mod config;
pub mod gfx;
pub mod i18n;
mod i18n_data;
pub mod langs;
pub mod menu;
pub mod scan;
pub mod storage;
pub mod text;

/// Paths on the ESP (backslash separated, as UEFI uses them).
pub mod paths {
    pub const DIR: &str = "\\EFI\\bootpick";
    pub const CONF: &str = "\\EFI\\bootpick\\bootpick.conf";
    pub const ICONS: &str = "\\EFI\\bootpick\\icons";
    pub const BACKGROUNDS: &str = "\\EFI\\bootpick\\backgrounds";
    pub const FONTS: &str = "\\EFI\\bootpick\\fonts";
    pub const FLAGS: &str = "\\EFI\\bootpick\\flags";
    pub const LAST: &str = "\\EFI\\bootpick\\last-entry";
}
