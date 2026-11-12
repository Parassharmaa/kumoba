#!/usr/bin/env bash
# Called by .github/workflows/ci.yml after ci-linux.sh. Typechecks and unit-tests the
# TypeScript SDK, then runs its live suite against a real kumobad with the test image.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
[[ -f .local/linux-host.env ]] && source .local/linux-host.env
work=$PWD/.local/ci-sdk
rm -rf "$work"
KUMOBAD_WORK="$work" bash scripts/serve.sh 2> "$work.log" &
daemon=$!
trap 'kill $daemon 2>/dev/null; wait $daemon 2>/dev/null' EXIT
for _ in $(seq 100); do [[ -S "$work/kumobad.sock" ]] && break; sleep 0.1; done
[[ -S "$work/kumobad.sock" ]] || { cat "$work.log" >&2; exit 1; }
set -a; source "$work/env"; set +a
cd sdk/typescript
npm ci --no-fund --no-audit
npm run typecheck
npm run build
npm test
KUMOBA_LIVE=1 npx vitest run test/live.test.ts
