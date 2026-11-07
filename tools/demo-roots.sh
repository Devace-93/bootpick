#!/usr/bin/env bash
# Creates fake system roots for `bootpick-config --demo` (OS detection and
# GRUB / systemd-boot menu handling without root). Usage: tools/demo-roots.sh DIR
set -euo pipefail
D="${1:?usage: tools/demo-roots.sh DIR}"
HDR='if [ "${recordfail}" = 1 ] ; then
  set timeout=30
else
  if [ x$feature_timeout_style = xy ] ; then
    set timeout_style=menu
    set timeout=10
  else
    set timeout=10
  fi
fi'
CUSTOM='### BEGIN /etc/grub.d/41_custom ###
if [ -f  ${config_directory}/custom.cfg ]; then
  source ${config_directory}/custom.cfg
elif [ -z "${config_directory}" -a -f  $prefix/custom.cfg ]; then
  source $prefix/custom.cfg
fi
### END /etc/grub.d/41_custom ###'
mkdir -p "$D/elementary/etc" "$D/elementary/boot/grub" "$D/pop/etc" "$D/ubuntu-boot/grub"
printf 'NAME="elementary OS"\nVERSION_ID="7.1"\nID=elementary\n' > "$D/elementary/etc/os-release"
printf '%s\nmenuentry '"'"'elementary OS'"'"' {\n  linux /boot/vmlinuz root=UUID=DEMO-ELEMENTARY ro\n}\n%s\n' "$HDR" "$CUSTOM" > "$D/elementary/boot/grub/grub.cfg"
printf 'NAME="Pop!_OS"\nVERSION_ID="24.04"\nID=pop\n' > "$D/pop/etc/os-release"
printf '%s\nmenuentry '"'"'Ubuntu'"'"' {\n  linux /vmlinuz root=/dev/mapper/vg-root ro\n}\n%s\n' "$HDR" "$CUSTOM" > "$D/ubuntu-boot/grub/grub.cfg"
echo "demo roots in $D: --demo-root DEMO-ELEMENTARY=$D/elementary --demo-root test=$D/pop --demo-root DEMO-UBUNTU-BOOT=$D/ubuntu-boot"
