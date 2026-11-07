#!/usr/bin/env bash
# Installs bootpick: the boot menu onto the mounted EFI System Partition (made
# the first firmware boot entry) and the "bootpick settings" desktop app, then
# opens the app. Existing loaders (GRUB, systemd-boot, Windows) are left
# untouched, so you can always pick them from the firmware boot menu.
#
# Build first as your normal user:  tools/build.sh
#
#   sudo ./install.sh                 # ESP autodetected from /boot/efi
#   sudo ./install.sh --esp /mnt/esp  # another mounted ESP
#   sudo ./install.sh --no-boot-entry # copy files only
#   sudo ./install.sh --no-app        # boot menu only, no desktop app
#   sudo ./install.sh --uninstall     # remove everything
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
ESP="/boot/efi"
BOOT_ENTRY=1
APP=1
UNINSTALL=0
LABEL="bootpick"
APP_BIN=/usr/local/bin/bootpick-config
APP_DESKTOP=/usr/share/applications/bootpick-settings.desktop
APP_ICON=/usr/share/icons/hicolor/scalable/apps/bootpick.svg
while [ $# -gt 0 ]; do
  case "$1" in
    --esp) ESP="$2"; shift 2 ;;
    --no-boot-entry) BOOT_ENTRY=0; shift ;;
    --no-app) APP=0; shift ;;
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
CONFIG_BIN="$ROOT/target/release/bootpick-config"
DEST="$ESP/EFI/bootpick"

if [ "$UNINSTALL" = 1 ]; then
  rm -rf "$DEST"
  for n in $(entry_ids); do
    efibootmgr -q -b "$n" -B
  done
  rm -f "$APP_BIN" "$APP_DESKTOP" "$APP_ICON"
  rm -rf /usr/local/share/bootpick
  command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
  echo "removed $DEST, the '$LABEL' boot entry and the settings app"
  exit 0
fi

[ -f "$EFI" ] || { echo "build first (as your user): tools/build.sh" >&2; exit 1; }
for d in icons flags fonts; do
  [ -d "$ROOT/esp/EFI/bootpick/$d" ] || { echo "assets missing: run tools/build.sh first" >&2; exit 1; }
done
mkdir -p "$DEST/icons" "$DEST/backgrounds" "$DEST/drivers" "$DEST/flags" "$DEST/fonts"
cp "$EFI" "$DEST/bootpick.efi"
cp "$ROOT"/esp/EFI/bootpick/icons/*.bmp "$DEST/icons/"
cp "$ROOT"/esp/EFI/bootpick/flags/*.bmp "$DEST/flags/"
cp "$ROOT"/esp/EFI/bootpick/fonts/*.ttf "$DEST/fonts/"
cp "$ROOT"/esp/EFI/bootpick/backgrounds/*.bmp "$DEST/backgrounds/" 2>/dev/null || true
cp "$ROOT"/assets/drivers/*.efi "$DEST/drivers/" 2>/dev/null || true
[ -f "$DEST/bootpick.conf" ] || cp "$ROOT/bootpick.conf.example" "$DEST/bootpick.conf"
sync
echo "boot menu installed in $DEST"

if [ "$BOOT_ENTRY" = 1 ]; then
  SRC="$(findmnt -no SOURCE "$ESP")"
  DISK="/dev/$(lsblk -no PKNAME "$SRC")"
  PART="$(cat "/sys/class/block/$(basename "$SRC")/partition")"
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

if [ "$APP" = 1 ]; then
  if [ ! -f "$CONFIG_BIN" ]; then
    echo "settings app not built (tools/build.sh); skipping it"
  else
    install -m 0755 "$CONFIG_BIN" "$APP_BIN"
    install -m 0644 "$ROOT/packaging/bootpick-settings.desktop" "$APP_DESKTOP"
    install -D -m 0644 "$ROOT/packaging/bootpick.svg" "$APP_ICON"
    command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q /usr/share/icons/hicolor 2>/dev/null || true
    echo "settings app installed: open \"bootpick settings\" from your applications menu"
    # Open it in the invoking user's desktop session.
    if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
      if systemd-run --user --machine="$SUDO_USER@" --quiet --collect "$APP_BIN" 2>/dev/null; then
        echo "opening bootpick settings…"
      else
        echo "(could not open it automatically; start \"bootpick settings\" yourself)"
      fi
    fi
  fi
fi
