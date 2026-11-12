#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
[[ "$host_os" == linux && -e /dev/kvm ]] || { echo 'Linux CI needs /dev/kvm' >&2; exit 2; }
sudo chmod 0666 /dev/kvm
bash scripts/prepare-linux-host.sh
source .local/linux-host.env
cargo test --locked --workspace --lib
bash scripts/test-vm.sh
bash scripts/smoke-daemon.sh
