# Contributing

## Build from source

| Host | Needs |
| --- | --- |
| macOS 14 or later, Apple silicon | `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw e2fsprogs rustup` |
| Linux, x86_64 or arm64 | `/dev/kvm`, Rust 1.99, and `scripts/prepare-linux-host.sh`, which builds the patched libkrun |

```sh
git clone https://github.com/Parassharmaa/kumoba
cd kumoba
scripts/build.sh                 # binaries into .local/bin
scripts/build-image.sh default   # the default image; needs Docker
scripts/serve.sh                 # kumobad with the built images
```

`scripts/build-test-image.sh` builds the small Alpine `test` image without Docker.

## Repository layout

| Path | What |
| --- | --- |
| `crates/kumoba` | Runtime library |
| `crates/kumobad` | Daemon, HTTP API, `kumoba` CLI and `kumoba-worker` |
| `crates/kumoba-guest` | Agent inside every VM |
| `crates/kumoba-protocol` | Host-guest wire protocol |
| `sdk/typescript` | `kumoba` npm package: SDK and CLI |
| `images/` | Guest image recipes |
| `docs/` | This site |
| `patches/` | libkrun patches |
| `scripts/` | Build, test, release and CI entry points |

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

## Writing docs

The site at [parassharmaa.github.io/kumoba](https://parassharmaa.github.io/kumoba/) is built with VitePress from `docs/`; `scripts/build-site.sh` builds it and fails on dead links, and CI runs it. Several pages include sections of `README.md` through `<!-- #region name -->` markers, so edit the README rather than copying text.

- Put a page where its reader needs it: the tutorial (`getting-started.md`) teaches by doing, how-to guides (`docs/guide/`) solve one task, reference pages list facts, and explanation pages say why.
- Write to the reader as "you", in the active voice, present tense. Put conditions before instructions ("To keep a server running, start it with `setsid`").
- Number the steps of a procedure, one action per step, and state what the reader needs before the first step.
- Every command must run as written on a fresh install. Run it before you commit it.
- Japanese pages live in `docs/ja/` and follow the JTF style: です/ます form, half-width letters and digits with no spaces around them, and commands, flags and code left in English. When an English page changes, update its Japanese page or mark the difference.
