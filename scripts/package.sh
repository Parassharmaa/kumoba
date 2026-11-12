#!/usr/bin/env bash
# Called by hand on a prepared Linux host. Packs kumobad, kumoba-worker, the patched
# libkrun, libkrunfw, one disk image and the licences into a single tarball with a
# manifest of sha256 digests, ready to copy onto a server.
#   scripts/package.sh <version> [output-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
[[ "$host_os" == linux ]] || { echo 'Packages are built on Linux (GNU tar sparse support)' >&2; exit 2; }
version=${1:?usage: package.sh <version> [output-dir]}
output_dir=${2:-.local/dist}
image=${KUMOBA_IMAGE:-.local/images/default.raw}
: "${KUMOBAD_LIBKRUN:?KUMOBAD_LIBKRUN must point at the patched libkrun.so.1}"
: "${KUMOBAD_LIBKRUNFW:?KUMOBAD_LIBKRUNFW must point at libkrunfw.so.5}"
case "$image_arch" in
  x86_64) platform=linux-x86_64 ;;
  arm64) platform=linux-arm64 ;;
esac
stage=$(mktemp -d .local/package.XXXXXX)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin" "$stage/lib" "$stage/images" "$stage/licenses" "$output_dir"
install -m 0755 .local/bin/kumobad .local/bin/kumoba-worker "$stage/bin/"
install -m 0644 "$KUMOBAD_LIBKRUN" "$stage/lib/libkrun.so.1"
install -m 0644 "$KUMOBAD_LIBKRUNFW" "$stage/lib/libkrunfw.so.5"
ln "$image" "$stage/images/default.raw" 2>/dev/null || cp --sparse=always "$image" "$stage/images/default.raw"
tar -xzf .local/libkrun-closefix/source.tar.gz --wildcards --strip-components=1 -O '*/LICENSE' > "$stage/licenses/libkrun-LICENSE"
cp patches/libkrun-1.19.6/*.patch LICENSE NOTICE "$stage/licenses/"
python3 - "$stage" "$version" "$platform" <<'PY'
import hashlib, json, os, sys
stage, version, platform = sys.argv[1:4]
files = {}
for root, _, names in os.walk(stage):
    for name in sorted(names):
        full = os.path.join(root, name)
        digest = hashlib.sha256()
        with open(full, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
        files[os.path.relpath(full, stage)] = digest.hexdigest()
manifest = {"format": 1, "apiVersion": 1, "version": version, "platform": platform, "files": dict(sorted(files.items()))}
with open(os.path.join(stage, "manifest.json"), "w") as out:
    json.dump(manifest, out, indent=2)
PY
bundle="$output_dir/kumoba-$version-$platform.tar.zst"
tar --sparse --owner=0 --group=0 -C "$stage" -cf - . | zstd -T0 -10 -q -f -o "$bundle"
printf '%s\n' "$PWD/$bundle"
