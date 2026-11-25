#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
[[ "$host_os" == linux && -e /dev/kvm ]] || { echo 'Linux CI needs /dev/kvm' >&2; exit 2; }
sudo chmod 0666 /dev/kvm
bash scripts/prepare-linux-host.sh
source .local/linux-host.env
cargo test --locked --workspace --lib --bins
# Only the test binary runs as root, so target/ stays owned by the CI user for the steps below.
host_triple=$(rustc -vV | sed -n 's/^host: //p')
runner_variable="CARGO_TARGET_$(tr '[:lower:]-' '[:upper:]_' <<<"$host_triple")_RUNNER"
env "$runner_variable=sudo -E" cargo test --locked -p kumoba-guest --bins -- --ignored
bash scripts/test-vm.sh
bash scripts/smoke-daemon.sh
