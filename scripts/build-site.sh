#!/usr/bin/env bash
# Called by .github/workflows/pages.yml and scripts/ci-lint.sh, and by hand: builds the
# VitePress site from docs/ (which pulls sections of README.md, CHANGELOG.md, SECURITY.md and
# CONTRIBUTING.md in) into docs/.vitepress/dist. Fails on dead links.
set -euo pipefail
cd "$(dirname "$0")/../docs"
npm ci --no-fund --no-audit
npm run build
