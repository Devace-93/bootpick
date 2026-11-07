#!/usr/bin/env bash
# Installs bootpick onto the mounted EFI System Partition and makes it the
# first firmware boot entry. Existing loaders (GRUB, systemd-boot, Windows)
# are left untouched, so you can always pick them from the firmware boot menu.
#
#   sudo ./install.sh                 # ESP autodetected from /boot/efi
#   sudo ./install.sh --esp /mnt/esp  # another mounted ESP
#   sudo ./install.sh --no-boot-entry # copy files only
#   sudo ./install.sh --uninstall     # remove files + boot entry
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
ESP="/boot/efi"
BOOT_ENTRY=1
UNINSTALL=0
LABEL="bootpick"
while [ $# -gt 0 ]; do
  case "$1" in
    --esp) ESP="$2"; shift 2 ;;
    --no-boot-entry) BOOT_ENTRY=0; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }
mountpoint -q "$ESP" || { echo "$ESP is not a mount point" >&2; exit 1; }
# Boot#### numbers whose label is exactly $LABEL. efibootmgr prints
# "Boot0008* bootpick<TAB>HD(...)" (path after a tab) or just the label.
entry_ids() {
  efibootmgr | sed -nE "s/^Boot([0-9A-Fa-f]{4})\*? ${LABEL}(\t.*)?$/\1/p"
}
EFI="$ROOT/uefi/target/x86_64-unknown-uefi/release/bootpick.efi"
DEST="$ESP/EFI/bootpick"

if [ "$UNINSTALL" = 1 ]; then
  rm -rf "$DEST"
  for n in $(entry_ids); do
    efibootmgr -q -b "$n" -B
  done
  echo "removed $DEST and the '$LABEL' boot entry"
  exit 0
fi

[ -f "$EFI" ] || { echo "build first: cargo build --release" >&2; exit 1; }
mkdir -p "$DEST/icons" "$DEST/backgrounds" "$DEST/drivers"
cp "$EFI" "$DEST/bootpick.efi"
cp "$ROOT"/esp/EFI/bootpick/icons/*.bmp "$DEST/icons/"
cp "$ROOT"/esp/EFI/bootpick/backgrounds/*.bmp "$DEST/backgrounds/" 2>/dev/null || true
cp "$ROOT"/assets/drivers/*.efi "$DEST/drivers/" 2>/dev/null || true
[ -f "$DEST/bootpick.conf" ] || cp "$ROOT/bootpick.conf.example" "$DEST/bootpick.conf"
echo "files installed in $DEST"

if [ "$BOOT_ENTRY" = 1 ]; then
  SRC="$(findmnt -no SOURCE "$ESP")"
  DISK="/dev/$(lsblk -no PKNAME "$SRC")"
  PART="$(lsblk -no PARTN "$SRC")"
  # Drop stale entries with the same label, then create a new one first in order.
  for n in $(entry_ids); do
    efibootmgr -q -b "$n" -B
  done
  efibootmgr -q --create --disk "$DISK" --part "$PART" --label "$LABEL" --loader '\EFI\bootpick\bootpick.efi'
  NEW="$(entry_ids | head -1)"
  [ -n "$NEW" ] || { echo "could not find the new boot entry; boot order unchanged" >&2; exit 1; }
  ORDER="$(efibootmgr | awk '/^BootOrder:/ {print $2}')"
  efibootmgr -q -o "$NEW,$(echo "$ORDER" | sed "s/\b$NEW\b,\?//; s/,$//")"
  echo "boot entry Boot$NEW '$LABEL' created on $DISK partition $PART and placed first"
  efibootmgr | sed -n '1,4p'
fi
