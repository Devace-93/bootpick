#!/usr/bin/env bash
# Boots the esp/ directory as a FAT drive under OVMF (EDK2) in QEMU.
#   tools/qemu.sh            interactive window (GTK/SDL)
#   tools/qemu.sh headless   VNC on :9 + monitor socket for scripted screenshots
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUN="${BOOTPICK_RUN_DIR:-$ROOT/.run}"
mkdir -p "$RUN"
QEMU_SHARE="$(dirname "$(command -v qemu-system-x86_64)")/../share/qemu"
CODE="$QEMU_SHARE/edk2-x86_64-code.fd"
VARS="$RUN/ovmf-vars.fd"
[ -f "$VARS" ] || cp "$QEMU_SHARE/edk2-i386-vars.fd" "$VARS"

cargo build --release --target x86_64-unknown-uefi --manifest-path "$ROOT/uefi/Cargo.toml" >/dev/null
mkdir -p "$ROOT/esp/EFI/BOOT"
cp "$ROOT/uefi/target/x86_64-unknown-uefi/release/bootpick.efi" "$ROOT/esp/EFI/BOOT/BOOTX64.EFI"
# Stand-in loaders so chainloading can be exercised without real operating systems.
D="$ROOT/uefi/target/x86_64-unknown-uefi/release/dummy.efi"
for f in EFI/Microsoft/Boot/bootmgfw.efi EFI/systemd/systemd-bootx64.efi EFI/ubuntu/shimx64.efi EFI/ubuntu/grubx64.efi EFI/ubuntu/mmx64.efi EFI/tools/memtest.efi; do
  mkdir -p "$(dirname "$ROOT/esp/$f")"; cp "$D" "$ROOT/esp/$f"
done
[ -f "$ROOT/esp/EFI/bootpick/bootpick.conf" ] || printf 'theme = midnight\nbackground = none\ntimeout = 0\n' > "$ROOT/esp/EFI/bootpick/bootpick.conf"

DISPLAY_ARGS=(-display gtk)
if [ "${1:-}" = "headless" ]; then
  DISPLAY_ARGS=(-display none -vnc :9 -monitor "unix:$RUN/monitor.sock,server,nowait")
fi
# BOOTPICK_IMG=/path/esp.img uses a real FAT image instead of QEMU's vvfat
# (vvfat mishandles repeated guest writes, so use an image to test file writes).
# Optional second ESP on a virtual NVMe disk (tests duplicate loaders across disks).
EXTRA=()
if [ -d "$ROOT/esp2" ]; then
  EXTRA=(-drive "if=none,id=esp2,format=raw,file=fat:rw:$ROOT/esp2" -device nvme,drive=esp2,serial=bootpick2)
fi
ACCEL=(-cpu max)
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=(-enable-kvm -cpu host); fi
exec qemu-system-x86_64 \
  -machine q35 -m 1024 "${ACCEL[@]}" -smp 2 \
  -serial "file:$RUN/serial.log" \
  -drive if=pflash,format=raw,readonly=on,file="$CODE" \
  -drive if=pflash,format=raw,file="$VARS" \
  -drive format=raw,file="${BOOTPICK_IMG:-fat:rw:$ROOT/esp}" \
  "${EXTRA[@]}" \
  -device qemu-xhci -device usb-mouse -device usb-tablet -device usb-kbd \
  -vga std -net none \
  "${DISPLAY_ARGS[@]}"
