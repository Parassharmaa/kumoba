#!/usr/bin/env bash
# Called by .github/workflows/release.yml: prints the changelog section for one version,
# which becomes the body of the GitHub release.
#   scripts/release-notes.sh <version>      e.g. scripts/release-notes.sh 0.1.0
set -euo pipefail
cd "$(dirname "$0")/.."
version=${1:?usage: release-notes.sh <version>}
notes=$(awk -v v="$version" '
  $0 ~ "^## \\[" v "\\]" { on = 1; next }
  on && /^## \[/ { exit }
  on && /^\[.*\]: http/ { exit }
  on && !started && /^$/ { next }
  on { started = 1; print }
' CHANGELOG.md | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')
[[ -n "$notes" ]] || { echo "CHANGELOG.md has no section for $version" >&2; exit 1; }
printf '%s\n' "$notes"
printf '\n## Install\n\n'
printf '```sh\ncargo install kumobad@%s        # kumobad + kumoba-worker\nnpm install kumoba@%s\n```\n\n' "$version" "$version"
printf 'Linux tarballs below bundle the binaries with the patched libkrun and libkrunfw; the macOS tarball needs `brew install slp/krun/libkrun slp/krun/libkrunfw`. Guest images built for an earlier release must be rebuilt when the protocol version changes.\n'
