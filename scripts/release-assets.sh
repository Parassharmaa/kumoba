#!/usr/bin/env bash
# Called by .github/workflows/release.yml on each release runner (Linux x86_64, Linux arm64,
# macOS arm64): builds the binaries for this host and packs them with scripts/package.sh.
#   scripts/release-assets.sh <version> [output-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
version=${1:?usage: release-assets.sh <version> [output-dir]}
output_dir=${2:-.local/dist}
if [[ "$host_os" == linux ]]; then
  bash scripts/prepare-linux-host.sh
  source .local/linux-host.env
else
  command -v zstd >/dev/null || brew install zstd
fi
bash scripts/build.sh
bash scripts/package.sh "$version" "$output_dir"
