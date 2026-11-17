# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

bootpick is a mouse-driven UEFI boot picker in Rust, plus a desktop settings app (Linux and Windows) that renders a live preview with the same code as the boot menu. Each OS has its own scripts: `tools/*.sh` + `install.sh` on Linux, `tools/*.ps1` + `install.cmd`/`install.ps1` on Windows. The QEMU harness and asset generation (`build-assets.py`: cairo, Noto fonts) are Linux-only.

## Commands

```sh
tools/build.sh                      # everything: UEFI menu, settings app, generated assets (if stale)
cargo build --release --target x86_64-unknown-uefi --manifest-path uefi/Cargo.toml   # UEFI menu only
cargo build --release -p bootpick-config                                             # settings app only

cargo test                          # host workspace (core + config); the uefi crate has no tests
cargo test -p bootpick-core <name>  # single test
cargo test -p bootpick-config <name>

tools/qemu.sh                       # build + boot esp/ under OVMF in a window
tools/qemu.sh headless              # VNC :9 + monitor socket in .run/
tools/qemu-ctl.py shot out.png | key f2 | mouse <dx> <dy> [1] | raw '<monitor cmd>'

python3 tools/gen-i18n.py           # regenerate i18n Rust tables after editing assets/i18n/*.json
uv run --with pillow --with cairosvg --with fonttools tools/build-assets.py   # icons, flags, wallpapers, font subsets -> esp/EFI/bootpick/
tools/package.sh <tag>              # release tarball in dist/
```

On Windows:

```powershell
tools\build.ps1                     # UEFI menu + settings app; fetches the generated assets from a release if missing
target\release\bootpick-config.exe --probe   # from an elevated prompt: what the installer sees (read-only)
.\install.cmd [-Keep|-Reset|-NoBootEntry|-NoApp|-Uninstall]   # self-elevates
tools\package.ps1 <tag>             # release zip in dist\
```

Windows toolchain notes: MSVC works with plain `cargo`. Without Visual Studio, `stable-x86_64-pc-windows-gnu` builds `core` and the UEFI menu but **not** `bootpick-config` (its `dlltool` needs `as.exe`, which rustup does not ship). `build.ps1` then builds the app with `+stable-x86_64-pc-windows-gnullvm` and `RUSTFLAGS="-Clinker-flavor=ld.lld -Clinker=rust-lld -Clink-self-contained=yes -Lnative=<repo>/target/winlibs"` (a copy of `opengl32.dll` there stands in for the missing import library). Use the same toolchain and flags for `cargo test` on the host workspace.

Running the settings app without root or a real ESP:

```sh
tools/demo-roots.sh /tmp/roots      # fake OS roots; prints the matching --demo-root flags
target/release/bootpick-config --demo esp[:LABEL][,esp2[:LABEL]] [--demo-root UUID=DIR]...
target/release/bootpick-config --demo esp --screenshot out.png [--preview-settings | --open-theme-editor | ...]
```

The full flag list is in the header comment of `config/src/main.rs`.

## Two separate cargo builds

- The root workspace contains `core` and `config` only. `uefi/` is **excluded**: it has its own `Cargo.lock`, `target/` dir, release profile (`panic = "abort"`, full LTO) and `uefi/.cargo/config.toml` that sets the target. From the repo root, always pass `--manifest-path uefi/Cargo.toml --target x86_64-unknown-uefi`.
- `uefi/src/bin/dummy.rs` builds `dummy.efi`, which `tools/qemu.sh` copies to the Windows / systemd-boot / Ubuntu loader paths in `esp/` so chainloading can be tested without real operating systems.
- A dependency change in `core` affects both lockfiles.

## Architecture

The central design rule: **everything not tied to UEFI lives in `core` (`no_std` + `alloc`) so the boot menu and the desktop app share one implementation.** Platform code is a thin shell on each side.

- `core/src/storage.rs` — the `Storage` trait (read / write / list_dir on ESP-style backslash paths such as `\EFI\bootpick\...`; constants in `core::paths`). Implemented by `uefi/src/esp.rs` (`FsStorage` over SimpleFileSystem) and `config/src/host.rs` (`DirStorage`, a directory with FAT-like case-insensitive lookup).
- `core/src/scan.rs` — the rules that turn files on a volume into boot entries (Windows, BLS/systemd-boot entries, vendor loaders, tools, firmware `Boot####`). Platform code only supplies volumes and firmware options (`uefi/src/scan.rs`, `config/src/host.rs`). Entry ids must come out identical on both sides, because `bootpick.conf` written by the desktop app is keyed by them.
- `core/src/menu.rs` — the whole UI as a state machine (main picker, settings screen, language popup). It takes `Key`/pointer events and `tick()` calls, draws into a software `Canvas` (`gfx.rs`, text via `text.rs` with rustybuzz shaping and font fallback), and returns an `Outcome` (launch, firmware, reboot, shutdown). `MenuOptions::preview` disables the countdown and writes for the desktop preview.
- `uefi/src/shell.rs` — drives the `Menu`: GOP blits (full frame, or cursor-rect only), input translation (`input.rs`: Simple Pointer, Absolute Pointer, keyboard), frame timing, then acts on the `Outcome` through `boot.rs` (chainload, `LoaderEntryOneShot`, `BootNext`, loading drivers from `EFI\bootpick\drivers\`). `trace.rs` writes the breadcrumb log `EFI\bootpick\bootpick.log`.
- `config/` — one binary, two roles:
  - the egui app (`app.rs`), running as the user;
  - the privileged helper (`helper.rs`), speaking one JSON object per line (`proto.rs`): started as `pkexec bootpick-config --helper` over stdin/stdout on Linux; on Windows elevated through UAC (`ShellExecuteExW` "runas") and connected over a named pipe the app serves.
  The helper copies `\EFI\bootpick` to a user-owned staging dir; the app edits only that copy; Save writes back an allow-list (`bootpick.conf`, `icons\*.bmp`, `backgrounds\*.bmp`; regular files, size-capped). Keep that validation intact when touching the helper — it is the privilege boundary.
  - `menus.rs` hides/restores other loaders' menus: a marked block in GRUB's `custom.cfg`, and the `timeout` line of systemd-boot's `loader.conf`. "Show again", disable and uninstall must remove exactly what bootpick added.
  - `host.rs` also detects the real OS behind a loader via `os-release`.
  - Everything OS-specific goes through `mod sys` (`sys_linux.rs` / `sys_windows.rs`, selected by `#[cfg_attr(..., path)]` in `main.rs`) with the same function names on both sides: `partitions()`, `Mounted`, firmware variables and `boot_*` entry operations, the helper channel, app files, locale, fonts. Keep `host.rs`, `helper.rs`, `menus.rs` and `app.rs` free of `cfg` and OS calls.
  - Linux: `lsblk`/`blkid`, temporary mounts, efivarfs for reads and `efibootmgr` for writes. Windows: volumes are reached by `\\?\Volume{guid}\` paths (nothing is mounted; build sub-paths with `host::sub`, since verbatim paths reject `/`), firmware variables through `Get/SetFirmwareEnvironmentVariable` (needs elevation), and Boot#### entries are built by `host::load_option`.
  - `install.rs` (Windows only) is the installer behind `--install` / `--probe`; it reuses `helper::find_esp`, so an existing install on any FAT partition is updated in place. On Linux the installer is `install.sh`.
  - Insyde firmware (HP and others) rebuilds `BootOrder` at every start from its own `PhysicalBootOrder` variable (vendor `59d1c24f-50f1-401a-b101-f33e0daed443`). `sys_windows::boot_set_order` writes both and reads the second back; the Linux side (`efibootmgr`) writes only `BootOrder`. `bootpick-config --probe --vars` (elevated) dumps every firmware variable.
  - Windows cannot read ext4/btrfs: GRUB menus whose files live there show `Problem::Unreadable` and OS detection behind GRUB returns nothing. That is expected, not a bug.
  - The Windows binary is GUI-subsystem; CLI modes call `sys::attach_console()`. PowerShell does not wait for GUI programs: use `Start-Process -Wait -NoNewWindow`.

## Generated and ignored files

- `core/src/i18n_data.rs` and `config/src/i18n_app.rs` are generated by `tools/gen-i18n.py` and committed. Edit `assets/i18n/strings.json` (boot menu), `app.json` (settings app) or `langs.json`, then regenerate. Each JSON has a `_keys` array; every language's array must match it in length and order.
- `esp/` is the QEMU test disk. `esp/EFI/bootpick/{icons,backgrounds,flags,fonts,drivers}` are generated (by `build-assets.py` / `qemu.sh`) and git-ignored; sources are under `assets/`. Icons are 256x256 32-bit BMP, wallpapers 24-bit BMP.
- QEMU's vvfat mishandles repeated guest writes; to test file writes (saving settings, the log) use a real FAT image via `BOOTPICK_IMG=/path/esp.img tools/qemu.sh`. An `esp2/` directory, if present, is attached as a second ESP on a virtual NVMe disk.
- In the QEMU monitor, `mouse_set 3` routes motion and buttons to the USB mouse (otherwise buttons go to the tablet).

## Other

- Themes are built in, defined in `core/src/config.rs`. `bootpick.conf.example` documents the config format.
- `site/` is the landing page (bootpick.3m4.net), deployed to GitHub Pages by `.github/workflows/pages.yml` on pushes to `main` touching `site/**`. There is no CI for the Rust code.
- Version `0.2.0` is repeated in the three `Cargo.toml` files; the release tag and download URL in `README.md` and the site also carry it.
