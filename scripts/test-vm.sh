#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
image=$(bash scripts/build-test-image.sh | tail -1)
KUMOBA_TEST_WORKER="$PWD/.local/bin/kumoba-worker" KUMOBA_TEST_IMAGE="$image" \
  cargo test --release --locked -p kumoba --test agent_vm -- --ignored --nocapture "$@"
