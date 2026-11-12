#!/usr/bin/env bash
set -euo pipefail

# Called by .github/workflows/ci.yml before building the default image. Hosted
# runners ship toolchains this repo never uses; dropping them makes room for the
# Docker layers plus the exported ext4 image.
echo "Disk usage before cleanup:"
df -h /

dirs=(
  /usr/share/dotnet
  /usr/local/.ghcup
  /opt/ghc
  /usr/share/swift
  /usr/local/share/powershell
  /opt/hostedtoolcache/CodeQL
)
# /usr/local/lib/android only on request: deleting it alone takes ~75s, and the rest frees enough elsewhere.
if [[ "${1:-}" == "--android" ]]; then
  dirs+=(/usr/local/lib/android)
fi

for dir in "${dirs[@]}"; do
  if [[ -d "$dir" ]]; then
    echo "Removing $dir"
    sudo rm -rf "$dir"
  fi
done

echo "Disk usage after cleanup:"
df -h /
