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
#   sudo ./install.sh --keep          # already installed: keep its settings (default)
#   sudo ./install.sh --reset         # already installed: factory settings
#   sudo ./install.sh --uninstall     # remove everything
#
# When bootpick is already installed on any EFI partition (e.g. from another
# system on this computer), that copy is updated in place and you are asked
# whether to keep its settings (backgrounds, logos, choices) or reset them.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
ESP="/boot/efi"
ESP_GIVEN=0
MODE=""   # keep | reset (asked when empty and already installed)
BOOT_ENTRY=1
APP=1
UNINSTALL=0
LABEL="bootpick"
APP_BIN=/usr/local/bin/bootpick-config
APP_DESKTOP=/usr/share/applications/bootpick-settings.desktop
APP_ICON=/usr/share/icons/hicolor/scalable/apps/bootpick.svg
while [ $# -gt 0 ]; do
  case "$1" in
    --esp) ESP="$2"; ESP_GIVEN=1; shift 2 ;;
    --keep) MODE=keep; shift ;;
    --reset) MODE=reset; shift ;;
    --no-boot-entry) BOOT_ENTRY=0; shift ;;
    --no-app) APP=0; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
[ "$(id -u)" -eq 0 ] || { echo "run with sudo" >&2; exit 1; }

# An existing install anywhere on this computer: the partition the 'bootpick'
# boot entry points to, else any FAT partition holding \EFI\bootpick.
TMP_MNT=""
cleanup() { if [ -n "$TMP_MNT" ]; then umount "$TMP_MNT" 2>/dev/null || true; rmdir "$TMP_MNT" 2>/dev/null || true; fi; }
trap cleanup EXIT
has_bootpick() { # $1 = device; prints its mount point when bootpick is there
  local mp; mp="$(findmnt -no TARGET "$1" | head -1)"
  if [ -z "$mp" ]; then
    local t; t="$(mktemp -d)"
    mount "$1" "$t" 2>/dev/null || { rmdir "$t"; return 1; }
    if [ -f "$t/EFI/bootpick/bootpick.efi" ]; then echo "tmp:$t"; return 0; fi
    umount "$t"; rmdir "$t"; return 1
  fi
  [ -f "$mp/EFI/bootpick/bootpick.efi" ] && echo "$mp"
}
if [ "$ESP_GIVEN" = 0 ]; then
  ENTRY_PU="$(efibootmgr -v 2>/dev/null | grep -iE "^Boot[0-9A-F]{4}\*? ${LABEL}\b" | grep -oiE 'HD\([0-9]+,GPT,[0-9a-f-]+' | head -1 | cut -d, -f3)"
  CANDS=""
  [ -n "$ENTRY_PU" ] && CANDS="$(lsblk -nrpo NAME,PARTUUID | awk -v u="${ENTRY_PU,,}" 'tolower($2)==u {print $1}')"
  CANDS="$CANDS $(lsblk -nrpo NAME,FSTYPE | awk '$2=="vfat" {print $1}')"
  for dev in $CANDS; do
    if mp="$(has_bootpick "$dev")"; then
      case "$mp" in tmp:*) mp="${mp#tmp:}"; TMP_MNT="$mp" ;; esac
      FOUND_DEV="$dev"; ESP="$mp"
      break
    fi
  done
fi
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
FRESH=1
if [ -f "$DEST/bootpick.conf" ]; then
  FRESH=0
  echo "bootpick is already installed on ${FOUND_DEV:-$(findmnt -no SOURCE "$ESP")} ($DEST)."
  if [ -z "$MODE" ]; then
    MODE=keep
    if [ -r /dev/tty ]; then
      printf '  [k] keep its current settings (backgrounds, logos, choices)\n  [r] reset to factory settings\nChoice [k]: ' > /dev/tty
      read -r ans < /dev/tty || ans=""
      case "${ans,,}" in r|reset) MODE=reset ;; esac
    fi
  fi
  if [ "$MODE" = reset ]; then
    rm -rf "$DEST"
    FRESH=1
    echo "settings reset to factory defaults"
  else
    echo "keeping the current settings"
  fi
fi
mkdir -p "$DEST/icons" "$DEST/backgrounds" "$DEST/drivers" "$DEST/flags" "$DEST/fonts"
cp "$EFI" "$DEST/bootpick.efi"
cp "$ROOT"/esp/EFI/bootpick/icons/*.bmp "$DEST/icons/"
cp "$ROOT"/esp/EFI/bootpick/flags/*.bmp "$DEST/flags/"
cp "$ROOT"/esp/EFI/bootpick/fonts/*.ttf "$DEST/fonts/"
cp "$ROOT"/assets/drivers/*.efi "$DEST/drivers/" 2>/dev/null || true
if [ "$FRESH" = 1 ]; then
  # Bundled backgrounds only on a fresh install: the user may have removed some.
  cp "$ROOT"/esp/EFI/bootpick/backgrounds/*.bmp "$DEST/backgrounds/" 2>/dev/null || true
  cp "$ROOT/bootpick.conf.example" "$DEST/bootpick.conf"
fi
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
    "$APP_BIN" --upgrade-menus || true
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
