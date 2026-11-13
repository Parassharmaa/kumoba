#!/usr/bin/env bash
# Called by .github/workflows/release.yml on the Linux release runners: builds the test and
# default disk images for this architecture and compresses them next to the tarballs, so
# scripts/install.sh can download a ready-to-boot image.
#   scripts/release-images.sh <version> [output-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
[[ "$host_os" == linux ]] || { echo 'Images are built on Linux' >&2; exit 2; }
version=${1:?usage: release-images.sh <version> [output-dir]}
output_dir=${2:-.local/dist}
mkdir -p "$output_dir"
bash scripts/prepare-linux-host.sh
test_image=$(bash scripts/build-test-image.sh | tail -1)
default_image=$(bash scripts/build-image.sh default | tail -1)
docker image prune -af >/dev/null 2>&1 || true
for pair in "test=$test_image" "default=$default_image"; do
  name=${pair%%=*}
  out="$output_dir/kumoba-image-$name-$version-$image_arch.raw.zst"
  zstd -T0 -12 -q -f "${pair#*=}" -o "$out"
  printf '%s %s\n' "$(du -h "$out" | cut -f1)" "$out"
done
