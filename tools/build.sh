#!/usr/bin/env bash
# Builds everything as a normal user (no root): the UEFI menu, the settings
# app and the generated assets. Then run: sudo ./install.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
rustup target list --installed | grep -q x86_64-unknown-uefi || rustup target add x86_64-unknown-uefi
cargo build --release --target x86_64-unknown-uefi --manifest-path uefi/Cargo.toml
cargo build --release -p bootpick-config
if [ ! -d esp/EFI/bootpick/fonts ] || [ ! -d esp/EFI/bootpick/flags ] || [ ! -d esp/EFI/bootpick/icons ]; then
  uv run --with pillow --with cairosvg --with fonttools tools/build-assets.py
fi
echo "built. Now: sudo ./install.sh"
