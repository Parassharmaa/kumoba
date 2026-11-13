#!/usr/bin/env bash
# Called by .github/workflows/release.yml to decide whether the crates.io job needs a token:
# prints true when any crate is missing at the workspace version. Sourced with --lib by
# scripts/publish-crates.sh for crate_published.
set -euo pipefail
crate_published() {
  local status
  status=$(curl -sS -o /dev/null -w '%{http_code}' -A 'kumoba-release (https://github.com/Parassharmaa/kumoba)' "https://crates.io/api/v1/crates/$1/$2")
  [[ "$status" == 200 ]]
}
[[ "${1:-}" == --lib ]] && return 0
cd "$(dirname "$0")/.."
version=$(bash scripts/version.sh)
for crate in kumoba-protocol kumoba-guest kumoba kumobad; do
  if ! crate_published "$crate" "$version"; then
    echo true
    exit 0
  fi
done
echo false
