#!/usr/bin/env bash
# Called by .github/workflows/walkthrough.yml and release.yml on a fresh KVM runner: gives
# the runner user /dev/kvm, waits until npm serves the new version (it lags a publish by a
# minute or so), then runs the README walkthrough against the published release.
#   scripts/ci-walkthrough.sh <version>
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -e /dev/kvm ]] || { echo 'needs /dev/kvm' >&2; exit 2; }
sudo chmod 0666 /dev/kvm
command -v zstd >/dev/null || sudo apt-get install -y -qq zstd >/dev/null
version=${1:?usage: ci-walkthrough.sh <version>}
for _ in $(seq 60); do
  npm view "kumoba@$version" version > /dev/null 2>&1 && break
  sleep 10
done
bash scripts/walkthrough.sh "$version"
