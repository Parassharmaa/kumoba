#!/usr/bin/env bash
# Called by hand during development: runs kumobad on a Unix socket and a loopback
# TCP port with the alpine test image (and images/default if it has been built),
# and writes the matching KUMOBA_* client variables to .local/serve/env.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
test_image=$(bash scripts/build-test-image.sh | tail -1)
images="test=$test_image"
default_image=${KUMOBA_IMAGE:-$PWD/.local/images/default.raw}
[[ -f "$default_image" ]] && images="default=$default_image,$images"
work=${KUMOBAD_WORK:-$PWD/.local/serve}
listen=${KUMOBAD_LISTEN:-127.0.0.1:7788}
mkdir -p "$work"
[[ -f "$work/token" ]] || head -c 48 /dev/urandom | base64 | tr -d '\n/+=' > "$work/token"
printf 'KUMOBA_URL=unix://%s/kumobad.sock\nKUMOBA_TOKEN_FILE=%s/token\nKUMOBA_PUBLIC_URL=http://%s\n' "$work" "$work" "$listen" > "$work/env"
echo "client env: $work/env" >&2
exec env KUMOBAD_STATE_DIR="$work/state" KUMOBAD_WORKER="$PWD/.local/bin/kumoba-worker" \
  KUMOBAD_IMAGES="$images" KUMOBAD_SOCKET="$work/kumobad.sock" KUMOBAD_LISTEN="$listen" \
  KUMOBAD_TOKEN_FILE="$work/token" .local/bin/kumobad
