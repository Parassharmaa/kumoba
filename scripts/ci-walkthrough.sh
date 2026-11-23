#!/usr/bin/env bash
# Called by .github/workflows/walkthrough.yml and release.yml on a fresh KVM runner: gives
# the runner user /dev/kvm, waits until npm serves the new version's package (it lags a
# publish by a few minutes, and the version shows up in npm view before the package can be
# downloaded), then runs the README walkthrough against the published release.
#   scripts/ci-walkthrough.sh <version>
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -e /dev/kvm ]] || { echo 'needs /dev/kvm' >&2; exit 2; }
sudo chmod 0666 /dev/kvm
command -v zstd >/dev/null || sudo apt-get install -y -qq zstd >/dev/null
version=${1:?usage: ci-walkthrough.sh <version>}
for _ in $(seq 60); do
  tarball=$(npm view "kumoba@$version" dist.tarball 2>/dev/null || true)
  [[ -n "$tarball" ]] && curl -fsI -o /dev/null "$tarball" && break
  sleep 10
done
bash scripts/walkthrough.sh "$version"
