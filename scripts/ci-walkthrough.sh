#!/usr/bin/env bash
# Called by .github/workflows/walkthrough.yml and release.yml on a fresh KVM runner: gives
# the runner user /dev/kvm, then runs the README walkthrough against the published release.
#   scripts/ci-walkthrough.sh <version>
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -e /dev/kvm ]] || { echo 'needs /dev/kvm' >&2; exit 2; }
sudo chmod 0666 /dev/kvm
command -v zstd >/dev/null || sudo apt-get install -y -qq zstd >/dev/null
bash scripts/walkthrough.sh "${1:?usage: ci-walkthrough.sh <version>}"
