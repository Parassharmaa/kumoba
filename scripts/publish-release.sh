#!/usr/bin/env bash
# Called by .github/workflows/release.yml last: writes SHA256SUMS next to the tarballs and
# images, and creates the GitHub release with the changelog section followed by the
# generated notes. scripts/install.sh verifies every download against SHA256SUMS.
#   scripts/publish-release.sh <tag> <assets-dir>
set -euo pipefail
cd "$(dirname "$0")/.."
tag=${1:?usage: publish-release.sh <tag> <assets-dir>}
assets=${2:?usage: publish-release.sh <tag> <assets-dir>}
version=${tag#v}
bash scripts/release-notes.sh "$version" > "$assets/notes.md"
(cd "$assets" && sha256sum kumoba-*.tar.zst kumoba-image-*.raw.zst > SHA256SUMS)
if gh release view "$tag" >/dev/null 2>&1; then
  printf 'release %s exists; uploading assets\n' "$tag"
  gh release upload "$tag" --clobber "$assets"/kumoba-*.tar.zst "$assets"/kumoba-image-*.raw.zst "$assets/SHA256SUMS"
  exit 0
fi
gh release create "$tag" --verify-tag --title "kumoba $tag" --notes-file "$assets/notes.md" --generate-notes \
  "$assets"/kumoba-*.tar.zst "$assets"/kumoba-image-*.raw.zst "$assets/SHA256SUMS"
