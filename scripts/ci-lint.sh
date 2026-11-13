#!/usr/bin/env bash
# Called by .github/workflows/ci.yml and scripts/ci-release.sh, and by hand before a pull
# request: formatting, clippy with warnings denied (host crates and the musl guest), rustdoc,
# the cargo-deny supply-chain checks, shellcheck, actionlint, the SDK's Biome checks and a
# build of the docs site, which fails on dead links.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
if [[ "$host_os" == linux ]]; then
  rustup target add "$guest_target" >/dev/null
  RUSTFLAGS='-C linker=rust-lld' cargo clippy -p kumoba-guest --target "$guest_target" --all-targets --locked -- -D warnings
fi
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
cargo deny --log-level error check
shellcheck -x -S warning scripts/*.sh
actionlint
cd sdk/typescript
npm ci --no-fund --no-audit
npm run lint
npm run typecheck
cd ../..
bash scripts/build-site.sh
