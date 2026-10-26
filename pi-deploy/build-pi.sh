#!/bin/sh
# Baut BosLook für den Raspberry Pi „bwpimitte“ (armv7, Debian 10 Buster, glibc 2.28)
# und spielt die Binary per ssh ein.
#
# Voraussetzungen auf dem Laptop:
#   rustup target add armv7-unknown-linux-gnueabihf
#   cargo install cargo-zigbuild --locked
#   uv tool install ziglang        (oder zig im PATH)
#
# musl geht nicht: cpal braucht libasound, die es nicht statisch für musl gibt. Daher
# glibc-Build gegen genau die glibc (2.28) und libasound (1.1.8) des Pi.
#
# Aufruf: pi-deploy/build-pi.sh [user@host]
set -eu

PI=${1:-jan@10.10.2.21}
GLIBC=2.28
TARGET=armv7-unknown-linux-gnueabihf
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SYSROOT=${XDG_CACHE_HOME:-$HOME/.cache}/boslook-pi-sysroot
DEBS="https://archive.debian.org/debian/pool/main/a/alsa-lib"

# zig: aus dem PATH oder aus der uv-Tool-Installation von ziglang
if ! command -v zig >/dev/null 2>&1; then
    ZIG=$(find "$HOME" -path '*ziglang/zig' -type f 2>/dev/null | head -1)
    [ -n "$ZIG" ] || { echo "zig nicht gefunden (uv tool install ziglang)" >&2; exit 1; }
    PATH="$(dirname "$ZIG"):$PATH"
fi

# libasound aus Buster als Sysroot (nur zum Linken, auf dem Pi wird die eigene geladen)
if [ ! -e "$SYSROOT/usr/lib/arm-linux-gnueabihf/pkgconfig/alsa.pc" ]; then
    mkdir -p "$SYSROOT"
    tmp=$(mktemp -d)
    for deb in libasound2_1.1.8-1_armhf.deb libasound2-dev_1.1.8-1_armhf.deb; do
        curl -fsSL -o "$tmp/$deb" "$DEBS/$deb"
        (cd "$tmp" && ar x "$deb" && tar -xf data.tar.* -C "$SYSROOT" && rm -f data.tar.* control.tar.* debian-binary)
    done
    rm -rf "$tmp"
fi

cd "$ROOT"
PKG_CONFIG_ALLOW_CROSS=1 \
PKG_CONFIG_SYSROOT_DIR="$SYSROOT" \
PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/arm-linux-gnueabihf/pkgconfig" \
    cargo zigbuild --release --target "$TARGET.$GLIBC"

BIN=target/$TARGET/release/boslook
need=$(readelf -V "$BIN" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -1)
echo "Höchste benötigte glibc: $need"
[ "$(printf '%s\n%s\n' "$need" "$GLIBC" | sort -V | tail -1)" = "$GLIBC" ] \
    || { echo "Binary braucht glibc $need, Pi hat $GLIBC" >&2; exit 1; }

# Deploy: Binary immer, boslook.toml nur, wenn auf dem Pi noch keine liegt
# (sonst würden Server und Token überschrieben)
ssh "$PI" 'mkdir -p ~/boslook'
tar -C "target/$TARGET/release" -cf - boslook | ssh "$PI" 'tar -xf - -C ~/boslook'
ssh "$PI" 'test -e ~/boslook/boslook.toml' \
    || ssh "$PI" 'cat > ~/boslook/boslook.toml' < pi-deploy/boslook.example.toml

local_md5=$(md5sum "$BIN" | cut -d' ' -f1)
remote_md5=$(ssh "$PI" 'md5sum ~/boslook/boslook' | cut -d' ' -f1)
echo "md5 lokal:  $local_md5"
echo "md5 Pi:     $remote_md5"
[ "$local_md5" = "$remote_md5" ] || { echo "md5 stimmt nicht überein" >&2; exit 1; }

ssh "$PI" '~/boslook/boslook --help'
echo "Fertig. Läuft der Dienst schon: ssh $PI 'systemctl --user restart boslook'"
