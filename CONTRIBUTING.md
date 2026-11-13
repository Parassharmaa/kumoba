# Contributing

## Build and test locally

```sh
scripts/build.sh                 # Rust binaries into .local/bin
scripts/build-test-image.sh      # Alpine test image (no Docker needed)
cargo test --workspace --lib     # unit tests
scripts/test-vm.sh               # agent tests against a real VM
scripts/smoke-daemon.sh          # kumobad end to end with curl
cd sdk/typescript && npm install && npm test
scripts/ci-lint.sh               # everything the lint CI job checks
```

`scripts/ci-lint.sh` runs `cargo fmt --check`, clippy with warnings denied (the lint set is in the workspace `Cargo.toml`), rustdoc, `cargo deny check` for licenses, advisories and sources, shellcheck, actionlint, and Biome plus `tsc` for the SDK. It needs `cargo install cargo-deny`, shellcheck and actionlint. `cargo fmt --all` and `npm run format` in `sdk/typescript` fix most of what it reports. On macOS it skips the Linux-only guest code, which CI covers.

To run the SDK against a real daemon: `scripts/serve.sh` in one terminal, then in another `set -a; source .local/serve/env; set +a; cd sdk/typescript && npm run test:live`.

On Linux run `scripts/prepare-linux-host.sh` once and `source .local/linux-host.env` before the commands above. `scripts/ci-linux.sh` is what CI runs.

## Changing the host-guest protocol

Bump `VERSION` in `crates/kumoba-protocol/src/lib.rs` and rebuild images: the worker refuses a guest agent with a different version.

## Pull requests

CI boots real VMs on a KVM runner for every push, including building the default image and taking a desktop screenshot. Keep that green, add a test next to the code you touch, and keep commits focused.

A change users can notice gets a line under `## [Unreleased]` in `CHANGELOG.md`; it becomes the release notes. Releases are cut from `main` with `scripts/release.sh`, see [docs/releasing.md](docs/releasing.md).
