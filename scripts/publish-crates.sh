#!/usr/bin/env bash
# Called by .github/workflows/release.yml with CARGO_REGISTRY_TOKEN set by crates.io trusted
# publishing. Publishes the crates in dependency order and skips versions crates.io already
# has, so a rerun after a failure later in the workflow is safe.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
version=$(bash scripts/version.sh)
for crate in kumoba-protocol kumoba-guest kumoba kumobad; do
  status=$(curl -sS -o /dev/null -w '%{http_code}' -A 'kumoba-release (https://github.com/Parassharmaa/kumoba)' "https://crates.io/api/v1/crates/$crate/$version")
  if [[ "$status" == 200 ]]; then
    printf 'skip %s@%s: already on crates.io\n' "$crate" "$version"
    continue
  fi
  cargo publish -p "$crate" --locked --no-verify
done
