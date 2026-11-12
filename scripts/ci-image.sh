#!/usr/bin/env bash
# Called by .github/workflows/ci.yml on a KVM-capable Linux runner. Builds the default
# image, boots it under kumobad and drives the desktop through the TypeScript SDK,
# leaving a screenshot at .local/ci-image/desktop.png.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
[[ "$host_os" == linux && -e /dev/kvm ]] || { echo 'needs Linux with /dev/kvm' >&2; exit 2; }
sudo chmod 0666 /dev/kvm
bash scripts/prepare-linux-host.sh
source .local/linux-host.env
bash scripts/build-image.sh default
docker image prune -af >/dev/null 2>&1 || true
work=$PWD/.local/ci-image
rm -rf "$work"
mkdir -p "$work"
KUMOBAD_WORK="$work" bash scripts/serve.sh 2> "$work/kumobad.log" &
daemon=$!
trap 'kill $daemon 2>/dev/null; wait $daemon 2>/dev/null' EXIT
for _ in $(seq 100); do [[ -S "$work/kumobad.sock" ]] && break; sleep 0.1; done
[[ -S "$work/kumobad.sock" ]] || { cat "$work/kumobad.log" >&2; exit 1; }
set -a; source "$work/env"; set +a
export KUMOBA_SCREENSHOT="$work/desktop.png"
cd sdk/typescript
npm ci --no-fund --no-audit
KUMOBA_DESKTOP_LIVE=1 npx vitest run test/desktop.live.test.ts
