#!/usr/bin/env bash
# Called by scripts/release-assets.sh on the release runners and by hand on a prepared host.
# Packs kumobad, kumoba-worker and kumoba-guest with the licences and a manifest of sha256
# digests into one tarball. Linux tarballs also carry the patched libkrun and libkrunfw;
# macOS hosts get both from Homebrew. KUMOBA_IMAGE=/path.raw adds that disk image (Linux).
#   scripts/package.sh <version> [output-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
version=${1:?usage: package.sh <version> [output-dir]}
output_dir=${2:-.local/dist}
image=${KUMOBA_IMAGE:-}
platform="$host_os-$image_arch"
if [[ "$host_os" == linux ]]; then
  : "${KUMOBAD_LIBKRUN:?KUMOBAD_LIBKRUN must point at the patched libkrun.so.1}"
  : "${KUMOBAD_LIBKRUNFW:?KUMOBAD_LIBKRUNFW must point at libkrunfw.so.5}"
elif [[ -n "$image" ]]; then
  echo 'Images are packaged on Linux (GNU tar sparse support)' >&2; exit 2
fi
protocol=$(perl -ne 'print $1 if /^pub const VERSION: u32 = (\d+);/' crates/kumoba-protocol/src/lib.rs)
stage=$(mktemp -d .local/package.XXXXXX)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin" "$stage/licenses" "$output_dir"
install -m 0755 .local/bin/kumobad .local/bin/kumoba-worker .local/bin/kumoba-guest "$stage/bin/"
cp LICENSE NOTICE "$stage/licenses/"
if [[ "$host_os" == linux ]]; then
  mkdir -p "$stage/lib"
  install -m 0644 "$KUMOBAD_LIBKRUN" "$stage/lib/libkrun.so.1"
  install -m 0644 "$KUMOBAD_LIBKRUNFW" "$stage/lib/libkrunfw.so.5"
  tar -xzf .local/libkrun-closefix/source.tar.gz --wildcards --strip-components=1 -O '*/LICENSE' > "$stage/licenses/libkrun-LICENSE"
  cp patches/libkrun-1.19.6/*.patch "$stage/licenses/"
fi
if [[ -n "$image" ]]; then
  mkdir -p "$stage/images"
  ln "$image" "$stage/images/default.raw" 2>/dev/null || cp --sparse=always "$image" "$stage/images/default.raw"
fi
python3 - "$stage" "$version" "$platform" "$protocol" <<'PY'
import hashlib, json, os, sys
stage, version, platform, protocol = sys.argv[1:5]
files = {}
for root, _, names in os.walk(stage):
    for name in sorted(names):
        full = os.path.join(root, name)
        digest = hashlib.sha256()
        with open(full, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
        files[os.path.relpath(full, stage)] = digest.hexdigest()
manifest = {"format": 2, "apiVersion": 1, "protocolVersion": int(protocol), "version": version, "platform": platform, "files": dict(sorted(files.items()))}
with open(os.path.join(stage, "manifest.json"), "w") as out:
    json.dump(manifest, out, indent=2)
    out.write("\n")
PY
bundle="$output_dir/kumoba-$version-$platform.tar.zst"
if [[ "$host_os" == linux ]]; then
  tar --sparse --owner=0 --group=0 -C "$stage" -cf - . | zstd -T0 -10 -q -f -o "$bundle"
else
  tar --uid 0 --gid 0 -C "$stage" -cf - . | zstd -T0 -10 -q -f -o "$bundle"
fi
printf '%s\n' "$(cd "$(dirname "$bundle")" && pwd)/$(basename "$bundle")"
