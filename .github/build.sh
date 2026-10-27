#!/bin/sh
# Baut boslook und boslook-desktop für eine Plattform und legt die Dateien für das Release in
# dist/ ab. Aufruf: .github/build.sh <target> <version>
#
# Ergebnis (Namen erwartet auch das Update-System, siehe src/update.rs):
#   dist/boslook-<target>[.exe]              einzelnes Programm, für Updates
#   dist/boslook-desktop-<target>[.exe]
#   dist/boslook-<version>-<target>.tar.gz   bzw. .zip unter Windows: beide Programme,
#                                            README und Beispielkonfigurationen
set -eu
target=$1
version=$2

# Linker und C-Compiler für das Zielsystem (ring, ALSA); ALSA kommt per Multiarch-Paket
case $target in
  aarch64-unknown-linux-gnu)
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
    export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
    export PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig
    ;;
  armv7-unknown-linux-gnueabihf)
    export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
    export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
    export PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_PATH=/usr/lib/arm-linux-gnueabihf/pkgconfig
    ;;
esac

exe=""
case $target in *windows*) exe=".exe" ;; esac

cargo build --release --locked --target "$target" --bin boslook
cargo build --release --locked --target "$target" --features desktop --bin boslook-desktop

mkdir -p dist
out="target/$target/release"
cp "$out/boslook$exe" "dist/boslook-$target$exe"
cp "$out/boslook-desktop$exe" "dist/boslook-desktop-$target$exe"

name="boslook-$version-$target"
pkg="target/package/$name"
rm -rf "$pkg"
mkdir -p "$pkg"
cp "$out/boslook$exe" "$out/boslook-desktop$exe" README.md boslook.example.toml boslook-client.example.toml "$pkg/"
if [ -n "$exe" ]; then
  (cd target/package && rm -f "$name.zip" && zip -qr "$name.zip" "$name")
  mv "target/package/$name.zip" dist/
else
  tar -czf "dist/$name.tar.gz" -C target/package "$name"
fi
ls -l dist
