#!/usr/bin/env sh
# Put the botroster runtime where Tauri's bundler expects it.
#
# The installer must not be a client with nothing to drive: BOTROSTER spawns
# `botroster`, so the bundle ships it. Tauri's `externalBin` wants the file named
# with the target triple, and copies it beside the app under its plain name —
# which is exactly where `sidecar()` in `botroster-app` looks at runtime.
#
# Run before `cargo tauri build`. CI does this on tags; see ci.yml.
set -eu

triple="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
root="$(cd "$(dirname "$0")/.." && pwd)"
ext=""
case "$triple" in *windows*) ext=".exe" ;; esac

cargo build --release -p botroster-cli --manifest-path "$root/Cargo.toml"

mkdir -p "$root/crates/botroster-app/binaries"
cp "$root/target/release/botroster$ext" \
   "$root/crates/botroster-app/binaries/botroster-$triple$ext"

echo "staged botroster-$triple$ext"
