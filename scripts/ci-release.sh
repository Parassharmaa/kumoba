#!/usr/bin/env bash
# Called by .github/workflows/release.yml before anything is published: the version in the
# tag must match the manifests and the changelog, and both registries must accept a dry run.
#   scripts/ci-release.sh [tag]      no tag on a manual dry run
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
tag=${1:-}
version=$(bash scripts/version.sh)
if [[ -n "$tag" ]]; then
  bash scripts/release-check.sh "$tag"
  bash scripts/release-notes.sh "$version" > /dev/null
fi
cargo test --workspace --lib --locked
cargo publish --workspace --dry-run --locked
cd sdk/typescript
npm ci --no-fund --no-audit
npm run typecheck
npm run build
npm test
npm pack --dry-run
