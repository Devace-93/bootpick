# bootpick `beta`

Website: <https://bootpick.3m4.net>

> **Status: beta.** Tested in QEMU/OVMF and on one real machine so far: an
> AMD Ryzen 4000 laptop with Insyde UEFI firmware, NVMe + SATA disks, booting
> Windows, Pop!_OS (systemd-boot), Ubuntu and elementary OS (shim/GRUB, on
> separate disks); mouse and touchpad
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
  auto-start timeout (5 to 60 s), default entry ("Last selected" starts
  whatever was picked on the previous boot). Saved to
  `EFI\bootpick\bootpick.conf`.
- Firmware setup, restart and shut down buttons.
- Mouse (Simple Pointer), touch (Absolute Pointer) and keyboard. Keyboard is
  always enough: arrows, Enter, Esc, F2 settings, F12 firmware setup, `d`
  shows a debug line with pointer stats.
- Scales with the screen resolution; works on any UEFI firmware. Secure Boot
  must be off, or the binary must be signed with your own key.
- 34 languages (the same list as the 3m4 apps): a picker at the top right
  with flags and native names. Text is shaped with rustybuzz, so Hindi, Thai
  and Arabic (right to left) render correctly; CJK, Devanagari, Thai and
  Arabic glyphs come from Noto subsets of about 630 KiB in total.
- A desktop app, **bootpick settings**, with a live preview rendered by the
  same code as the boot menu: language, timeout, default entry, entry names,
  logos (import any image), backgrounds (add or remove), a theme editor with
  colour pickers, and buttons to disable or uninstall bootpick. It detects
  the real OS behind each loader (for example elementary OS installed in an
  `\EFI\ubuntu` folder) from `os-release`.


## Download

No Rust toolchain needed: grab the prebuilt package from the
[releases page](https://github.com/Devace-93/bootpick/releases) (x86_64; the
settings app needs glibc 2.39+, i.e. Ubuntu/Pop!_OS 24.04, Debian 13 or newer).

```sh
curl -LO https://github.com/Devace-93/bootpick/releases/download/v0.2.0-beta/bootpick-v0.2.0-beta-x86_64-linux.tar.gz
tar xzf bootpick-v0.2.0-beta-x86_64-linux.tar.gz
cd bootpick-v0.2.0-beta-x86_64-linux
sudo ./install.sh                   # or first: tools/make-usb.sh /media/$USER/MYUSB
```

Release tarballs are made with `tools/package.sh <tag>`.

On Windows the package is a zip made with `tools\package.ps1 <tag>`
(`bootpick-<tag>-x86_64-windows.zip`): extract it and double-click
`install.cmd`. See [Install from Windows](#install-from-windows).

## Build

```sh
tools/build.sh      # as your normal user: UEFI menu, settings app, assets
```

It runs `cargo build --release --target x86_64-unknown-uefi --manifest-path
uefi/Cargo.toml`, `cargo build --release -p bootpick-config` and, when the
generated assets are missing, `tools/build-assets.py` (icons, wallpapers,
flags and font subsets; needs `uv`, cairo and the Noto fonts).

On Windows, `tools\build.ps1` does the same from PowerShell. The generated
assets cannot be built there (cairo and the Noto fonts), so the script takes
them from a release. Any Rust toolchain for Windows works; without Visual
Studio, install `stable-x86_64-pc-windows-gnullvm` next to the default
`stable-x86_64-pc-windows-gnu` and the script handles the linker.

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
sudo ./install.sh           # boot menu + "bootpick settings" app, then opens the app
sudo ./install.sh --uninstall
```

The installer copies the menu to `/boot/efi/EFI/bootpick`, adds a firmware
boot entry first in the boot order, installs `bootpick-config` with a
desktop entry, and opens it. GRUB, systemd-boot and the Windows boot manager
stay where they are; the firmware boot menu can still start them directly.

If bootpick is already installed anywhere on the computer (for example from
another Linux on the same machine), the installer updates that copy in place
instead of adding a second one, and asks whether to keep its settings
(backgrounds, logos, choices) or reset them to the defaults. `--keep` and
`--reset` answer that question without asking. Install the app from each
Linux you want to change the settings from: all of them edit the same copy.

### Install from Windows

```bat
powershell -ExecutionPolicy Bypass -File tools\make-usb.ps1 E:
install.cmd
install.cmd -Uninstall
```

The first line is optional: it puts bootpick on a FAT32 USB stick to try it
before installing. `install.cmd` asks for administrator rights (UAC). It is a
wrapper around `install.ps1` (options `-Keep`, `-Reset`, `-NoBootEntry`,
`-NoApp`, `-Uninstall`) and does what `install.sh` does on Linux: it copies
the menu to `\EFI\bootpick` on the EFI System Partition, adds a firmware boot
entry first in the boot order, installs **bootpick settings** in
`C:\Program Files\bootpick` with a Start menu shortcut, and opens it. The
Windows boot manager stays where it is.

If bootpick is already installed on any EFI partition of the computer (for
example from Linux), the installer finds it, updates that copy in place and
keeps its settings; the app on Windows and the apps on Linux all edit the
same copy. `bootpick-config.exe --probe` (from an administrator prompt)
prints what the installer sees without changing anything.

Things that differ on Windows:

- With **BitLocker** on the system drive, starting Windows through a new
  first boot entry makes it ask for the recovery key. The installer warns
  and asks before continuing.
- The installer warns when **Secure Boot** is on (bootpick is not signed).
- Windows cannot read Linux file systems, so from Windows the app cannot
  hide a GRUB menu whose files are on ext4/btrfs (it is listed as not
  readable) and does not look up the real OS name behind a GRUB loader.
  Names and hidden menus set from Linux are kept. systemd-boot menus, and
  GRUB installations that keep their files on the EFI partition, work.
- Some firmware and some Windows updates put "Windows Boot Manager" back
  first in the boot order; open the app and press "Enable bootpick" again.

### The settings app

`bootpick-config` runs as your user and asks for authorization once
(`pkexec bootpick-config --helper`; on Windows a UAC prompt, the helper then
talks to the app over a named pipe). The helper copies `\EFI\bootpick` to a
private staging folder, the app edits that copy, and Save writes back only
`bootpick.conf`, `icons\*.bmp` and `backgrounds\*.bmp` (regular files, size
limited). "Disable bootpick" marks its firmware entry inactive so the next
loader in the boot order starts; nothing is deleted. "Uninstall" removes the
menu, its boot entry and the app.

**Other boot menus.** When bootpick starts Ubuntu or elementary OS, their
GRUB shows a second menu; systemd-boot (Pop!_OS) can too when the firmware
starts it directly. The app lists these menus and offers to hide them:

- GRUB: adds a marked block to `<grub dir>/custom.cfg` (sourced by
  `41_custom`, so it survives `update-grub`) that sets `timeout=0` and
  `timeout_style=hidden`. Per system, "Also hide it after a forced shutdown"
  (on by default) decides what happens when GRUB's `recordfail` flag is set
  (the previous start did not finish): ticked, the menu stays hidden; unticked,
  it appears so a recovery option can be chosen. Hold Shift or press Esc
  to see the menu anyway.
- systemd-boot: sets `timeout 0` in `\loader\loader.conf` and keeps the old
  line in a comment. Hold Space to see the menu anyway.

"Show menu again" removes exactly what bootpick added. Disabling or
uninstalling bootpick restores every menu it hid, so the next boot loader is
still a way to reach all systems.

`bootpick-config --demo DIR[:LABEL],DIR2[:LABEL]` runs without root against
plain folders (used for the screenshots above).

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
  `core/src/config.rs` to add one.

## Layout

| path | role |
| --- | --- |
| `core/` | `no_std` library shared by both programs: renderer (`gfx`), text shaping and font fallback (`text`), BMP, config and themes, i18n, scanning rules (`scan`), the menu state machine (`menu`) |
| `uefi/` | the boot menu: GOP output, mouse/keyboard input, volume and firmware scanning, chainloading, breadcrumb log |
| `config/` | the desktop settings app (egui) and its privileged helper; `sys_linux.rs` / `sys_windows.rs` hold everything platform specific, `install.rs` is the Windows installer |
| `assets/i18n/` | UI strings (`strings.json`) and locales (`langs.json`); `tools/gen-i18n.py` generates `core/src/i18n_data.rs` |
| `tools/` | build, asset generation, QEMU harness, USB stick helper (`.sh` for Linux, `.ps1` for Windows) |

## License

MIT. Fonts: DejaVu (Bitstream Vera license); Noto Sans CJK, Devanagari, Thai
and Arabic subsets (SIL OFL 1.1). Flags: lipis/flag-icons (MIT). UI and generic icons: Font
Awesome Free 6.7.2 (icons CC BY 4.0, see `assets/fa/LICENSE.txt`).
Distribution logos belong to their projects; the Pop!_OS, Ubuntu, elementary
and other distribution artwork comes from the Pop icon theme.
