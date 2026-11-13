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
printf '```sh\ncurl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | KUMOBA_VERSION=%s bash\nnpm install kumoba@%s\n```\n\n' "$version" "$version"
printf 'The installer puts the `kumobad` daemon, the `kumoba` CLI, `kumoba-worker` and the `default` image into `~/.kumoba`; `npx kumoba` gives the same CLI through npm; on macOS install libkrun first with `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd`. `cargo install kumobad@%s` builds the daemon and worker from crates.io instead, and the Rust library is `cargo add kumoba@%s`.\n\n' "$version" "$version"
printf 'The files below are the host tarballs (Linux ones bundle the patched libkrun and libkrunfw; the macOS worker is signed for Hypervisor.framework), the `default` and `test` guest images for each architecture, and `SHA256SUMS`. Guest images from an earlier release must be replaced when the protocol version changes.\n'
