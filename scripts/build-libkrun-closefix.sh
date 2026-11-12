#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Linux ]] || { echo 'Run this builder inside the Linux test host.' >&2; exit 2; }
mode=${1:-patched}
[[ "$mode" == stock || "$mode" == patched ]] || { echo 'usage: build-libkrun-closefix.sh [stock|patched]' >&2; exit 2; }
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CC_LINUX=gcc
for tool in cargo curl tar patch flock sha256sum readelf gcc; do
  command -v "$tool" >/dev/null || { printf 'Missing prerequisite: %s\n' "$tool" >&2; exit 2; }
done
patches="$PWD/patches/libkrun-1.19.6"
cache="$PWD/.local/libkrun-closefix"
mkdir -p "$cache"
exec 9>"$cache/build.lock"
flock -n 9 || { echo 'Another closefix build is running.' >&2; exit 1; }
sha256sum -c "$patches/SHA256SUMS"
archive="$cache/source.tar.gz"
if [[ ! -f "$archive" ]]; then
  curl --fail --location --retry 2 https://api.github.com/repos/libkrun/libkrun/tarball/v1.19.6 -o "$archive.partial"
  mv "$archive.partial" "$archive"
fi
printf '%s  %s\n' 3e80db0928be7b00e2b94910ef782613aad40ede692c629e7d909b7d73dc891d "$archive" | sha256sum -c -
source_dir=$(mktemp -d "$cache/source-$mode.XXXXXX")
tar -xzf "$archive" --strip-components=1 -C "$source_dir"
patch -d "$source_dir" -p1 < "$patches/0001-close-regressions.patch"
if [[ "$mode" == patched ]]; then
  patch -d "$source_dir" -p1 < "$patches/0002-drain-before-reset.patch"
fi
export CARGO_TARGET_DIR="$cache/target-$mode"
cd "$source_dir"
cargo +1.99.0 test -p krun-devices --locked --features blk close_regressions -- --nocapture --test-threads=1
cargo +1.99.0 test -p krun-devices --locked --features blk
cargo +1.99.0 build -p libkrun --release --locked --features blk
output="$cache/$mode"
mkdir -p "$output"
cp "$CARGO_TARGET_DIR/release/libkrun.so" "$output/libkrun.so.1.next"
mv "$output/libkrun.so.1.next" "$output/libkrun.so.1"
{
  printf 'Upstream commit: 227b2de6ed323fe180e02f871c5f325a90c13cc2\nMode: %s\nFeatures: default init-blob, blk\nSource: %s\n' "$mode" "$source_dir"
  rustc +1.99.0 -Vv
  "$CC_LINUX" --version | head -1
  sha256sum "$archive" "$patches/0001-close-regressions.patch" "$patches/0002-drain-before-reset.patch" Cargo.lock "$output/libkrun.so.1"
  readelf -d "$output/libkrun.so.1" | grep SONAME
} > "$output/build-manifest.txt"
printf 'Built trusted override: %s\nManifest: %s\n' "$output/libkrun.so.1" "$output/build-manifest.txt"
