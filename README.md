# kumoba

[![CI](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml/badge.svg)](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Parassharmaa/kumoba?sort=semver)](https://github.com/Parassharmaa/kumoba/releases/latest)
[![npm](https://img.shields.io/npm/v/kumoba?label=npm)](https://www.npmjs.com/package/kumoba)
[![crates.io](https://img.shields.io/crates/v/kumobad?label=crates.io)](https://crates.io/crates/kumobad)
[![Status](https://img.shields.io/badge/status-alpha-orange)](https://parassharmaa.github.io/kumoba/getting-started)
[![Docs](https://img.shields.io/badge/docs-kumoba-0f766e)](https://parassharmaa.github.io/kumoba/)

**Fast sandboxes for AI agents.** Each sandbox is a real VM that boots in under a second, on your laptop, your servers or your cloud.

**[Documentation](https://parassharmaa.github.io/kumoba/)** · [日本語](https://parassharmaa.github.io/kumoba/ja/) · [Changelog](CHANGELOG.md)

> **Alpha.** Every release is installed and tested on real VMs, but the API, CLI and image format may change between 0.x releases.

- **Fast.** Create, boot, run and delete in about half a second on an Apple silicon Mac.
- **Real VMs.** Each sandbox has its own kernel, not a shared one.
- **Built for agents.** A CLI and SDKs where every command has a JSON Schema and returns the guest's exit code.
- **Safe by default.** Egress allowlists, secrets that never enter the VM, a log of every connection.
- **Live desktop.** Watch and drive a full desktop from your browser.
- **Runs anywhere.** Your laptop, your servers, your cloud. Apache-2.0.

## Getting started
<!-- #region getting-started -->

**1. Install.** On macOS, install libkrun first: `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd`. On Linux you need `/dev/kvm` and `zstd`.

```sh
curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
export PATH="$HOME/.kumoba/bin:$PATH"     # add this to your shell profile
```

The installer puts the `kumobad` daemon, the `kumoba` CLI and the `default` image (Debian with Python, Node, Chromium and a desktop) in `~/.kumoba`.

**2. Start the daemon.** It needs no configuration:

```sh
kumobad
```

Stop it with Ctrl-C. Sandboxes keep their disks across restarts.

**3. Run something.** In another terminal:

```sh
kumoba run -- python3 -c 'import platform; print(platform.platform())'

id=$(kumoba create --image default -q)       # a sandbox that stays around
kumoba start "$id" -q
kumoba exec "$id" -- pip install requests
echo 'print("hi")' | kumoba write-file "$id" --path /workspace/hi.py
kumoba exec "$id" --cwd /workspace -- python3 hi.py
kumoba desktop "$id" --user kumoba --open    # the desktop, live in your browser
kumoba delete "$id"
```

Run `kumoba` to list every command, and `kumoba <command> --help` for its options.

**4. Use it from code.**

```sh
npm install kumoba
```

```ts
import { Kumoba } from "kumoba";

const kumoba = new Kumoba();

const once = await kumoba.run("python3 --version");     // fresh VM, deleted afterwards
console.log(once.stdout);

const sandbox = await kumoba.create({ user: "kumoba" });
await sandbox.files.write("/workspace/a.txt", "hello");
console.log((await sandbox.run("cat /workspace/a.txt")).stdout);
await sandbox.delete();
```

`new Kumoba()` finds the daemon by itself.
<!-- #endregion getting-started -->

## Learn more

| | |
| --- | --- |
| Guides | [Run code once](https://parassharmaa.github.io/kumoba/guide/run-code) · [Work in a sandbox](https://parassharmaa.github.io/kumoba/guide/sandboxes) · [Live desktop](https://parassharmaa.github.io/kumoba/guide/desktop) · [Network access](https://parassharmaa.github.io/kumoba/guide/network) · [Secrets](https://parassharmaa.github.io/kumoba/guide/secrets) |
| Reference | [CLI](https://parassharmaa.github.io/kumoba/cli) · [TypeScript SDK](https://parassharmaa.github.io/kumoba/typescript) · [Rust](https://parassharmaa.github.io/kumoba/rust) · [HTTP API](https://parassharmaa.github.io/kumoba/http-api) · [Configuration](https://parassharmaa.github.io/kumoba/configuration) |
| Operate | [Install options](https://parassharmaa.github.io/kumoba/install) · [Run as a service](https://parassharmaa.github.io/kumoba/service) · [Security](https://parassharmaa.github.io/kumoba/security) · [Troubleshooting](https://parassharmaa.github.io/kumoba/troubleshooting) |
| Contribute | [Contributing](CONTRIBUTING.md) · [How it works](https://parassharmaa.github.io/kumoba/architecture) · [Releasing](docs/releasing.md) |

## License

Apache-2.0. libkrun is Apache-2.0; libkrunfw ships a GPL-2.0 Linux kernel. See [NOTICE](NOTICE).
