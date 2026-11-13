#!/usr/bin/env bash
# Called by scripts/release.sh and .github/workflows/release.yml: confirms the tag, the
# Cargo workspace, the npm package and the changelog all name the same version.
#   scripts/release-check.sh <tag>      e.g. scripts/release-check.sh v0.1.0
set -euo pipefail
cd "$(dirname "$0")/.."
tag=${1:?usage: release-check.sh <tag>}
version=${tag#v}
[[ "$tag" == "v$version" ]] || { echo "tag must look like v0.1.0, got $tag" >&2; exit 2; }
cargo_version=$(perl -0ne 'print $1 if /^\[workspace\.package\]\n(?:.*\n)*?version = "([^"]+)"/m' Cargo.toml)
npm_version=$(node -p 'require("./sdk/typescript/package.json").version')
lock_version=$(node -p 'require("./sdk/typescript/package-lock.json").version')
status=0
check() { if [[ "$2" != "$version" ]]; then printf 'MISMATCH %s: %s (tag says %s)\n' "$1" "$2" "$version" >&2; status=1; else printf 'ok   %s %s\n' "$1" "$2"; fi; }
check Cargo.toml "$cargo_version"
check package.json "$npm_version"
check package-lock.json "$lock_version"
if grep -q "^## \[$version\] - [0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}$" CHANGELOG.md; then
  printf 'ok   CHANGELOG.md has %s\n' "$version"
else
  printf 'MISMATCH CHANGELOG.md: no "## [%s] - YYYY-MM-DD" section\n' "$version" >&2; status=1
fi
exit $status
