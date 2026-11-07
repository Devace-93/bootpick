# bootpick `beta`

> **Status: beta.** Tested in QEMU/OVMF and on one real machine so far: an
> AMD Ryzen 4000 laptop with Insyde UEFI firmware, NVMe + SATA disks, booting
> Pop!_OS (systemd-boot) and Ubuntu (shim/GRUB); mouse and touchpad
> work through the firmware's own pointer drivers. Try it from a USB stick
> before installing it on an internal disk, keep your existing boot loaders
> in place (the installer does), and know how to reach your firmware's boot
> menu. Mouse support depends on the firmware exposing a pointer device.


A mouse-driven, themeable boot picker for UEFI machines, written in Rust
(`no_std`, [uefi-rs](https://github.com/rust-osdev/uefi-rs)). It replaces the
GRUB / systemd-boot text menu with big icons, a wallpaper, and a settings
screen to rename, hide, and reorder entries without editing files.

![main screen](docs/main.png)

![settings screen](docs/settings.png)

*Screenshots taken in QEMU/OVMF at 1280x800 with the "Pop" theme and a NASA wallpaper.*

## What it does

- Finds everything bootable by itself: Windows (`bootmgfw.efi`), systemd-boot
  entries (`\loader\entries\*.conf`, started through systemd-boot with
  `LoaderEntryOneShot`), vendor loaders under `\EFI\<vendor>\` (shim/GRUB),
  loose tools, and the firmware's own `Boot####` options (started via
  `BootNext`).
- Shows only the main operating systems by default. Fallback, recovery,
  "old kernel", MokManager and firmware entries are hidden; Esc reveals them.
- Settings (gear icon or F2): checkboxes to show/hide, click a name to rename
  it, arrows to reorder, theme and wallpaper pickers with live preview,
  auto-start timeout, default entry. Saved to `EFI\bootpick\bootpick.conf`.
- Firmware setup, restart and shut down buttons.
- Mouse (Simple Pointer), touch (Absolute Pointer) and keyboard. Keyboard is
  always enough: arrows, Enter, Esc, F2 settings, F12 firmware setup, `d`
  shows a debug line with pointer stats.
- Scales with the screen resolution; works on any UEFI firmware. Secure Boot
  must be off, or the binary must be signed with your own key.

## Build

```sh
rustup target add x86_64-unknown-uefi
cargo build --release          # target/x86_64-unknown-uefi/release/bootpick.efi
uv run --with pillow --with cairosvg tools/build-assets.py   # icons + wallpapers (BMP) -> esp/EFI/bootpick/
cp assets/drivers/*.efi esp/EFI/bootpick/drivers/            # optional USB mouse drivers
```

## Try it in QEMU

```sh
tools/qemu.sh            # window; needs qemu-system-x86_64 with EDK2 firmware
tools/qemu.sh headless   # VNC :9 + monitor socket; drive with tools/qemu-ctl.py
```

The `esp/` folder is the test disk. `tools/qemu.sh` puts a dummy loader at the
Windows, systemd-boot and Ubuntu paths so chainloading can be exercised.
Stock OVMF/EDK2 firmware has no USB mouse driver. `assets/drivers/` ships
prebuilt `UsbMouseDxe.efi` and `UsbMouseAbsolutePointerDxe.efi` (EDK2
MdeModulePkg, BSD-2-Clause-Patent); they are copied to
`EFI\bootpick\drivers\` and bootpick loads anything found there before
scanning for pointer devices. The same mechanism helps on real firmware that
exposes USB but has no mouse driver. Rebuild them with:

```sh
git clone --depth 1 -b edk2-stable202505 https://github.com/tianocore/edk2 && cd edk2
git submodule update --init --depth 1 BaseTools/Source/C/BrotliCompress/brotli
make -C BaseTools && . edksetup.sh BaseTools
build -a X64 -t GCC -b RELEASE -p MdeModulePkg/MdeModulePkg.dsc -m MdeModulePkg/Bus/Usb/UsbMouseDxe/UsbMouseDxe.inf
build -a X64 -t GCC -b RELEASE -p MdeModulePkg/MdeModulePkg.dsc -m MdeModulePkg/Bus/Usb/UsbMouseAbsolutePointerDxe/UsbMouseAbsolutePointerDxe.inf
```
(needs `nasm`; the `.dec` parser wants the other submodule include dirs to exist, empty is fine).

In the QEMU monitor, `mouse_set 3` routes both motion and buttons to the USB
mouse; with the tablet active, buttons go to the absolute device instead.

## Install

Try it from a USB stick first (FAT32-formatted, mounted):

```sh
tools/make-usb.sh /media/$USER/MYUSB
```

Then boot the stick from the firmware boot menu. When happy:

```sh
sudo ./install.sh          # copies to /boot/efi/EFI/bootpick, adds a boot entry first in order
sudo ./install.sh --uninstall
```

GRUB, systemd-boot and the Windows boot manager stay where they are; the
firmware boot menu can still start them directly.

## Troubleshooting

- bootpick writes a log of every run to `EFI\bootpick\bootpick.log` on the
  partition it started from: firmware, volumes, entries, input devices and
  each launch step. If something hangs after you pick an entry, the log shows
  whether bootpick handed over to the loader.
- Some firmware (seen on an Insyde laptop with a Kingston DataTraveler Duo)
  hangs at the logo whenever that USB stick is plugged in, with or without
  bootpick. If testing from USB hangs only for some entries, try the same
  entry from the firmware's own boot menu with the stick plugged in.
- To get back to the previous boot loader: pick it from the firmware boot
  menu (F12 on many laptops), boot Linux and run `sudo ./install.sh --uninstall`.

## Customising

- Icons: 256x256 32-bit BMP files in `EFI\bootpick\icons\`. An entry's icon is
  chosen from its title (`os_win`, `os_pop`, `os_ubuntu`, …) and can be
  overridden with `icon = <name>` in its `[entry]` section.
- Wallpapers: 24-bit BMP files in `EFI\bootpick\backgrounds\` (any size, they
  are scaled to cover the screen). Add your own with
  `tools/build-assets.py` after dropping a JPEG/PNG in `assets/backgrounds/`.
- Themes are built in (`midnight`, `pop`, `nord`, `ember`, `light`); see
  `src/config.rs` to add one.

## Layout

| file | role |
| --- | --- |
| `src/main.rs` | entry point |
| `src/scan.rs` | discovers volumes, loaders, BLS entries, firmware options |
| `src/boot.rs` | chainloading, `LoaderEntryOneShot`, `BootNext`, firmware setup, drivers |
| `src/ui.rs` | main screen, settings screen, cursor, event loop |
| `src/input.rs` | keyboard, mice, absolute pointers, tick timer |
| `src/gfx.rs`, `src/font.rs`, `src/bmp.rs` | software renderer, TrueType text, BMP decoding |
| `src/config.rs` | `bootpick.conf` parser/writer and themes |
| `src/esp.rs` | file-system helpers |
| `src/bin/dummy.rs` | stand-in loader used by the QEMU tests |

## License

MIT. Fonts: DejaVu (Bitstream Vera license). Distribution logos belong to their
projects; the Pop!_OS and Ubuntu artwork comes from the Pop icon theme, some
generic icons from rEFInd (GPLv3).
