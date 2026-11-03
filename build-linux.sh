#!/bin/bash
# Linux build: .deb + AppImage, plus files for the Arch package (packaging/arch).
# Needs: rust, tauri-cli 2, webkit2gtk-4.1 dev files (Ubuntu: libwebkit2gtk-4.1-dev,
# Arch: webkit2gtk-4.1). Output goes to update/linux/.
set -e
ROOT="$(cd "$(dirname "$0")" && pwd)"
VERSION="$(grep -m1 '"version"' "$ROOT/src-tauri/tauri.conf.json" | sed -E 's/.*"([0-9.]+)".*/\1/')"
cd "$ROOT/src-tauri"
# linuxdeploy's strip is too old for current distros' libraries
NO_STRIP=true cargo tauri build
OUT="$ROOT/update/linux"
mkdir -p "$OUT"
cp -f target/release/bundle/deb/SafeDrop_"$VERSION"_amd64.deb "$OUT/"
cp -f target/release/bundle/appimage/SafeDrop_"$VERSION"_amd64.AppImage "$OUT/"
# Arch: makepkg in a copy of packaging/arch next to the .deb
mkdir -p "$OUT/arch"
cp -f "$ROOT/packaging/arch/PKGBUILD" "$ROOT/LICENSE" "$OUT/arch/"
cp -f "$OUT/SafeDrop_${VERSION}_amd64.deb" "$OUT/arch/"
sed -i -E "s/^pkgver=.*/pkgver=$VERSION/" "$OUT/arch/PKGBUILD"
echo "OK: $OUT"
ls -la "$OUT"
