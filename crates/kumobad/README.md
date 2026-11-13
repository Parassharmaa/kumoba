# kumobad

[![crates.io](https://img.shields.io/crates/v/kumobad)](https://crates.io/crates/kumobad)
[![CI](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml/badge.svg)](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/kumobad)](https://github.com/Parassharmaa/kumoba/blob/main/LICENSE)

The [kumoba](https://github.com/Parassharmaa/kumoba) daemon. Serves fast VM sandboxes for AI agents over an HTTP API on a Unix socket or loopback TCP, and ships two more binaries: `kumoba`, the command-line client, and `kumoba-worker`, the per-VM process that loads libkrun and boots one sandbox.

> **Alpha.** APIs and formats may change between 0.x releases. Docs: [parassharmaa.github.io/kumoba](https://parassharmaa.github.io/kumoba/).

The quickest route is the installer, which also fetches a guest image and the libraries:

```sh
curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
~/.kumoba/bin/kumobad
```

From crates.io:

```sh
cargo install kumobad
kumobad --help
kumoba run -- echo hello        # once kumobad is running
```

With no configuration `kumobad` uses `~/.kumoba`: it boots every `images/<name>.raw`, creates a token and listens on `kumobad.sock`. Hosts need libkrun and libkrunfw, from Homebrew `slp/krun` on Apple silicon or `scripts/prepare-linux-host.sh` on Linux with `/dev/kvm`. On macOS a `cargo install`ed `kumoba-worker` must also be signed with the `com.apple.security.hypervisor` entitlement; the [install section](https://github.com/Parassharmaa/kumoba#install) has the commands.

Clients: the [`kumoba` npm package](https://www.npmjs.com/package/kumoba), or any HTTP client following [docs/http-api.md](https://github.com/Parassharmaa/kumoba/blob/main/docs/http-api.md).
