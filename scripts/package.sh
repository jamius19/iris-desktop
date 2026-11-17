#!/usr/bin/env bash
# Author: Jamius Siam
# Since: 16/11/2026
#
# Builds the Linux packages: a .deb (cargo-deb) and an AppImage (cargo-packager). Config: crates/iris/Cargo.toml.
set -euo pipefail

cd "$(dirname "$0")/.."

subcommands=$(cargo --list)
for tool in deb:cargo-deb packager:cargo-packager; do
    if ! grep -Eq "^ +${tool%%:*}( |\$)" <<<"$subcommands"; then
        echo "error: cargo ${tool%%:*} is missing. Install it with: cargo install ${tool#*:} --locked" >&2
        exit 1
    fi
done

metadata=$(cargo metadata --no-deps --format-version 1)
version=$(printf '%s' "$metadata" | grep -o '"name":"iris","version":"[^"]*"' | cut -d'"' -f8)
target_dir=$(printf '%s' "$metadata" | grep -o '"target_directory":"[^"]*"' | cut -d'"' -f4)
# cargo-packager has no target-dir placeholder: the AppImage config reads ../../target/release/iris-overlay.
if [ "$target_dir" != "$(pwd -P)/target" ]; then
    echo "error: the target dir is $target_dir; unset CARGO_TARGET_DIR (the AppImage needs $(pwd -P)/target)" >&2
    exit 1
fi

cargo build --release -p iris -p iris-overlay-helper

# build.rs refills transcribe-libs/ only when it reruns, and Windows builds share the directory.
if ! compgen -G "crates/iris/transcribe-libs/libtranscribe.so*" >/dev/null; then
    echo "error: crates/iris/transcribe-libs/ has no Linux libraries. Run 'touch crates/iris/build.rs' and retry." >&2
    exit 1
fi

cargo deb -p iris --no-build
# linuxdeploy runs its AppImage plugin, itself an AppImage: no FUSE needed this way.
APPIMAGE_EXTRACT_AND_RUN=1 cargo packager --release -p iris --formats appimage

deb="$target_dir/debian/iris_${version}-1_amd64.deb"
appimage="$target_dir/release/iris_${version}_x86_64.AppImage"
for file in "$deb" "$appimage"; do
    if [ ! -f "$file" ]; then
        echo "error: expected $file, but it wasn't produced" >&2
        exit 1
    fi
done
echo "Packages for Iris $version:"
echo "  $deb"
echo "  $appimage"
