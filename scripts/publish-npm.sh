#!/usr/bin/env bash
# Called by .github/workflows/release.yml with an OIDC token from npm trusted publishing.
# Skips a version npm already has, so a rerun after a failure later in the workflow is safe.
set -euo pipefail
cd "$(dirname "$0")/../sdk/typescript"
version=$(node -p 'require("./package.json").version')
if npm view "kumoba@$version" version >/dev/null 2>&1; then
  printf 'skip kumoba@%s: already on npm\n' "$version"
  exit 0
fi
npm ci --no-fund --no-audit
npm publish
