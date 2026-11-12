#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
[[ "$host_os" == linux ]] || { echo 'Linux only' >&2; exit 2; }
sudo apt-get update -qq
sudo apt-get install -y -qq --no-install-recommends e2fsprogs patch curl build-essential zstd >/dev/null
rustup toolchain install 1.99.0 --profile minimal >/dev/null
bash scripts/build-libkrun-closefix.sh patched
case "$image_arch" in
  x86_64) fw_asset=libkrunfw-x86_64.tgz; fw_digest=016c32ddb2a28aa300382cab33352d63a41a901eb36ee8d1ca24b389791c8b91; fw_dir=x86_64-linux-gnu ;;
  arm64) fw_asset=libkrunfw-aarch64.tgz; fw_digest=dea7905a167eee17d482200ea2fe15871aceb293b0674ac825ed7ae69759399f; fw_dir=aarch64-linux-gnu ;;
esac
mkdir -p .local/downloads .local/libkrunfw
[[ -f ".local/downloads/$fw_asset" ]] || curl --fail --location --retry 2 "https://github.com/containers/libkrunfw/releases/download/v5.6.2/$fw_asset" -o ".local/downloads/$fw_asset"
printf '%s  %s\n' "$fw_digest" ".local/downloads/$fw_asset" | verify_sha256
tar -xzf ".local/downloads/$fw_asset" -C .local/libkrunfw
printf 'export KUMOBAD_LIBKRUN=%s\nexport KUMOBAD_LIBKRUNFW=%s\n' \
  "$PWD/.local/libkrun-closefix/patched/libkrun.so.1" \
  "$PWD/.local/libkrunfw/lib/$fw_dir/libkrunfw.so.5" > .local/linux-host.env
