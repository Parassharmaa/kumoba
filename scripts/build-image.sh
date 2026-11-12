#!/usr/bin/env bash
# Called by hand and by .github/workflows/ci.yml. Builds images/<name>/Dockerfile for
# the host's architecture and exports it as an ext4 disk at .local/images/<name>.raw.
# Needs Docker. DISK_SIZE sets the sparse disk size (default 16G).
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
name=${1:-default}
[[ -f "images/$name/Dockerfile" ]] || { echo "no such image: images/$name" >&2; exit 2; }
case "$image_arch" in
  arm64) platform=linux/arm64 ;;
  x86_64) platform=linux/amd64 ;;
esac
bash scripts/build.sh
context=$(mktemp -d .local/image-context.XXXXXX)
container=""
cleanup() {
  [[ -n "$container" ]] && docker rm -f "$container" >/dev/null 2>&1
  rm -rf "$context"
}
trap cleanup EXIT
cp -R "images/$name/." "$context/"
cp .local/bin/kumoba-guest "$context/kumoba-guest"
docker build --platform "$platform" -t "kumoba-image:$name" "$context"
docker build --platform "$platform" -t kumoba-image:mkfs images/mkfs
mkdir -p .local/images
rm -f ".local/images/$name.raw.next"
truncate -s "${DISK_SIZE:-16G}" ".local/images/$name.raw.next"
container=$(docker create --platform "$platform" "kumoba-image:$name")
# mkfs.ext4 -d reads the tar through libarchive, which needs a UTF-8 locale for non-ASCII file names.
docker export "$container" | docker run --rm -i --platform "$platform" -e LC_ALL=C.UTF-8 -v "$PWD/.local/images:/out" kumoba-image:mkfs \
  mkfs.ext4 -q -F -b 4096 -E root_owner=0:0 -d - "/out/$name.raw.next"
mv ".local/images/$name.raw.next" ".local/images/$name.raw"
printf '%s\n' "$PWD/.local/images/$name.raw"
