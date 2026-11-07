#!/bin/sh
# Cross-check the whole workspace for x86_64-unknown-linux-gnu (AGENTS.md).
#
# The Tauri Linux dependencies are pkg-config crates (gtk, webkit2gtk, ...), so
# a Linux target needs that target's .pc metadata. This script provisions a
# small, gitignored sysroot under target/ (once, with Docker), stages the Linux
# sidecar the Tauri build script expects, exports the target-scoped pkg-config
# variables and then runs the required command unchanged.
#
# Nothing is installed globally and nothing outside target/ and the gitignored
# app/src-tauri/binaries is written.
#
# Usage: scripts/linux-cross-check.sh [extra cargo-zigbuild arguments...]
set -eu

repo_root=$(cd "$(dirname "$0")/.." && pwd)
triple=x86_64-unknown-linux-gnu
sysroot="$repo_root/target/linux-sysroot"
sidecar="$repo_root/app/src-tauri/binaries/agendar-$triple"

if [ ! -d "$sysroot/usr/lib/x86_64-linux-gnu/pkgconfig" ]; then
    echo "linux-cross-check: provisioning $sysroot (needs Docker)" >&2
    mkdir -p "$sysroot"
    docker run --rm --platform linux/amd64 -v "$sysroot:/out" ubuntu:24.04 bash -lc '
        set -e
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends \
            libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
            librsvg2-dev libxdo-dev libssl-dev ca-certificates
        tar -C / -cf - usr/include usr/share/pkgconfig \
            usr/lib/x86_64-linux-gnu/pkgconfig usr/lib/pkgconfig | tar -C /out -xf -'
fi

if [ ! -f "$sidecar" ]; then
    echo "linux-cross-check: staging the Linux sidecar" >&2
    (cd "$repo_root" && AGENDAR_BUNDLE_ZIGBUILD=1 pnpm -C app bundle:cli --target "$triple")
fi

PKG_CONFIG_ALLOW_CROSS=1
PKG_CONFIG_SYSROOT_DIR="$sysroot"
PKG_CONFIG_LIBDIR="$sysroot/usr/lib/x86_64-linux-gnu/pkgconfig:$sysroot/usr/share/pkgconfig:$sysroot/usr/lib/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_LIBDIR

cd "$repo_root"
exec cargo-zigbuild check --workspace --all-targets \
    --target "$triple" --target-dir target/cross "$@"
