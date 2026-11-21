#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
rustup target add "$guest_target"
cargo build --release --locked -p kumobad
RUSTFLAGS='-C linker=rust-lld' cargo build --release --locked -p kumoba-guest --target "$guest_target"
RUSTFLAGS='-C linker=rust-lld' cargo build --release --locked -p kumoba-view --target "$guest_target"
stage_worker
if [[ "$host_os" == macos ]]; then
  cargo build --release --locked -p kumoba-guest
  cp target/release/kumoba-guest .local/bin/kumoba-guest-macos.next
  mv .local/bin/kumoba-guest-macos.next .local/bin/kumoba-guest-macos
  cargo build --release --locked -p kumoba-view
  cp target/release/kumoba-view .local/bin/kumoba-view-host.next
  mv .local/bin/kumoba-view-host.next .local/bin/kumoba-view-host
fi
cp "target/$guest_target/release/kumoba-guest" .local/bin/kumoba-guest.next
mv .local/bin/kumoba-guest.next .local/bin/kumoba-guest
cp "target/$guest_target/release/kumoba-view" .local/bin/kumoba-view.next
mv .local/bin/kumoba-view.next .local/bin/kumoba-view
cp "target/$guest_target/release/http-fixture" .local/bin/http-fixture.next
mv .local/bin/http-fixture.next .local/bin/http-fixture
cp target/release/kumobad .local/bin/kumobad.next
mv .local/bin/kumobad.next .local/bin/kumobad
cp target/release/kumoba .local/bin/kumoba.next
mv .local/bin/kumoba.next .local/bin/kumoba
