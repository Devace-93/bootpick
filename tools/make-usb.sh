#!/usr/bin/env bash
# Copies bootpick onto an already FAT-formatted, mounted USB stick so it can be
# tried from the firmware's one-time boot menu before touching the internal ESP.
#   tools/make-usb.sh /media/$USER/MYUSB
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
USB="${1:?mount point of the USB stick}"
mountpoint -q "$USB" || { echo "$USB is not a mount point" >&2; exit 1; }
# Release tarballs ship the built menu and have no Rust toolchain.
if command -v cargo >/dev/null; then cargo build --release --target x86_64-unknown-uefi --manifest-path "$ROOT/uefi/Cargo.toml"; fi
mkdir -p "$USB/EFI/BOOT" "$USB/EFI/bootpick/icons" "$USB/EFI/bootpick/backgrounds" "$USB/EFI/bootpick/drivers"
cp "$ROOT/uefi/target/x86_64-unknown-uefi/release/bootpick.efi" "$USB/EFI/BOOT/BOOTX64.EFI"
cp "$ROOT"/esp/EFI/bootpick/icons/*.bmp "$USB/EFI/bootpick/icons/"
cp "$ROOT"/esp/EFI/bootpick/backgrounds/*.bmp "$USB/EFI/bootpick/backgrounds/" 2>/dev/null || true
mkdir -p "$USB/EFI/bootpick/flags" "$USB/EFI/bootpick/fonts"
cp "$ROOT"/esp/EFI/bootpick/flags/*.bmp "$USB/EFI/bootpick/flags/"
cp "$ROOT"/esp/EFI/bootpick/fonts/*.ttf "$USB/EFI/bootpick/fonts/"
cp "$ROOT"/assets/drivers/*.efi "$USB/EFI/bootpick/drivers/" 2>/dev/null || true
[ -f "$USB/EFI/bootpick/bootpick.conf" ] || cp "$ROOT/bootpick.conf.example" "$USB/EFI/bootpick/bootpick.conf"
sync
echo "done: boot the stick from the firmware boot menu (settings are saved on the stick)"
