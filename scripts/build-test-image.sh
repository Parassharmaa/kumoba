#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
bash scripts/build.sh
version=3.22.6
case "$alpine_arch" in
  aarch64) digest=821565fa8f3953eefd12497b166b4b50add2f7c57fb312e75862f5867e06fefe ;;
  x86_64) digest=27694aaa55fd7a9e3ef596e0ad4eb66802308bb20172b17030cd5f4d8ae9bac2 ;;
esac
archive=".local/downloads/alpine-minirootfs-${version}-${alpine_arch}.tar.gz"
mkdir -p .local/downloads
[[ -f "$archive" ]] || curl --fail --location --retry 2 "https://dl-cdn.alpinelinux.org/alpine/v3.22/releases/${alpine_arch}/alpine-minirootfs-${version}-${alpine_arch}.tar.gz" -o "$archive"
printf '%s  %s\n' "$digest" "$archive" | verify_sha256
staging=$(mktemp -d .local/rootfs.XXXXXX)
trap 'rm -rf "$staging" "${staging}.owners"' EXIT
tar -xzf "$archive" -C "$staging"
mkdir -p "$staging/usr/local/bin" "$staging/workspace" "$staging/home/kumoba"
chmod 1777 "$staging/tmp"
cp .local/bin/kumoba-guest .local/bin/http-fixture "$staging/usr/local/bin/"
printf '#!/bin/sh\nexec /bin/sh "$@"\n' > "$staging/bin/bash"
chmod 0755 "$staging/bin/bash"
printf 'kumoba:x:1000:1000::/home/kumoba:/bin/sh\n' >> "$staging/etc/passwd"
printf 'kumoba:x:1000:\n' >> "$staging/etc/group"
image=".local/test-${image_arch}.raw"
rm -f "$image"
truncate -s 512M "$image"
mkfs.ext4 -q -F -b 4096 -E root_owner=0:0 -d "$staging" "$image"
python3 scripts/root-ownership.py "$staging" > "${staging}.owners"
printf 'sif /home/kumoba uid 1000\nsif /home/kumoba gid 1000\n' >> "${staging}.owners"
debugfs -w -f "${staging}.owners" "$image" >/dev/null
printf '%s\n' "$PWD/$image"
