#!/usr/bin/env bash
# Builds a release tarball with everything install.sh and make-usb.sh need,
# so users can install without a Rust toolchain:
#   tools/package.sh v0.2.0-beta   ->  dist/bootpick-v0.2.0-beta-x86_64-linux.tar.gz
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VER="${1:?version tag, e.g. v0.2.0-beta}"
cd "$ROOT"
tools/build.sh
NAME="bootpick-$VER-x86_64-linux"
STAGE="$(mktemp -d)/$NAME"
B="$STAGE/esp/EFI/bootpick"
mkdir -p "$B" "$STAGE/uefi/target/x86_64-unknown-uefi/release" "$STAGE/target/release" "$STAGE/tools" "$STAGE/assets"
cp -r esp/EFI/bootpick/{icons,flags,fonts,backgrounds} "$B/"
cp uefi/target/x86_64-unknown-uefi/release/bootpick.efi "$STAGE/uefi/target/x86_64-unknown-uefi/release/"
cp target/release/bootpick-config "$STAGE/target/release/"
cp -r assets/drivers "$STAGE/assets/"
cp -r packaging "$STAGE/"
cp install.sh bootpick.conf.example README.md LICENSE "$STAGE/"
cp tools/make-usb.sh "$STAGE/tools/"
mkdir -p dist
tar -C "$(dirname "$STAGE")" -czf "dist/$NAME.tar.gz" "$NAME"
rm -rf "$(dirname "$STAGE")"
( cd dist && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256" )
echo "dist/$NAME.tar.gz"
