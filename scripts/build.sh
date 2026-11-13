#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
rustup target add "$guest_target"
cargo build --release --locked -p kumobad
RUSTFLAGS='-C linker=rust-lld' cargo build --release --locked -p kumoba-guest --target "$guest_target"
stage_worker
cp "target/$guest_target/release/kumoba-guest" .local/bin/kumoba-guest.next
mv .local/bin/kumoba-guest.next .local/bin/kumoba-guest
cp "target/$guest_target/release/http-fixture" .local/bin/http-fixture.next
mv .local/bin/http-fixture.next .local/bin/http-fixture
cp target/release/kumobad .local/bin/kumobad.next
mv .local/bin/kumobad.next .local/bin/kumobad
