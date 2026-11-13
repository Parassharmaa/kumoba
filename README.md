# kumoba

[![CI](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml/badge.svg)](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Parassharmaa/kumoba?sort=semver)](https://github.com/Parassharmaa/kumoba/releases/latest)
[![npm](https://img.shields.io/npm/v/kumoba?label=npm%20kumoba)](https://www.npmjs.com/package/kumoba)
[![kumobad](https://img.shields.io/crates/v/kumobad?label=crates.io%20kumobad)](https://crates.io/crates/kumobad)
[![docs.rs](https://img.shields.io/docsrs/kumoba?label=docs.rs%20kumoba)](https://docs.rs/kumoba)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Status](https://img.shields.io/badge/status-alpha-orange)](#status)
[![Docs](https://img.shields.io/badge/docs-github.io-0f766e)](https://parassharmaa.github.io/kumoba/)

Local microVM sandboxes for AI agents and untrusted code. One small daemon, a CLI, an HTTP API, and SDKs for TypeScript and Rust.

> **Alpha.** kumoba works end to end and every release is installed and tested on real VMs, but the HTTP API, CLI, SDK and image format may still change between 0.x releases. Pin a version for anything you depend on. Docs: **[parassharmaa.github.io/kumoba](https://parassharmaa.github.io/kumoba/)**.

- **Real VMs, not containers.** Every sandbox is a [libkrun](https://github.com/containers/libkrun) microVM with its own kernel. KVM on Linux, Hypervisor.framework on Apple silicon.
- **Fast.** Create, boot, run a command and delete in about 0.4 s on an M-series Mac.
- **Self-hosted.** Runs on your laptop or your server. No account, no cloud, no telemetry.
- **Batteries included.** Commands with streaming output, files, time-limited port previews, egress allowlists, secrets that never enter the guest, and an xfce desktop you can watch live in the browser or drive from a computer-use agent.

```sh
kumoba run -- python3 -c 'print(21 * 2)'      # a fresh VM, the command, then gone
```

```ts
import { Kumoba } from "kumoba";

await using sandbox = await new Kumoba().create();
const result = await sandbox.run("python3 -c 'print(21 * 2)'");
console.log(result.stdout); // "42\n"
```

## Getting started
<!-- #region getting-started -->

**1. Install.** On macOS, get libkrun from Homebrew first: `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd`. Linux needs `/dev/kvm` and `zstd`; the release already carries the patched libkrun.

```sh
curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
export PATH="$HOME/.kumoba/bin:$PATH"     # add this to your shell profile
```

This puts the `kumobad` daemon, the `kumoba` CLI, `kumoba-worker` and the `default` image (Debian with Python, Node, Chromium and an xfce desktop) into `~/.kumoba`, checking every download against the release's `SHA256SUMS`. `KUMOBA_IMAGE=test` fetches a 5 MB Alpine image instead, and `KUMOBA_VERSION=0.2.2` pins a release.

**2. Start the daemon.** No configuration needed:

```sh
kumobad
```

It boots every image in `~/.kumoba/images`, writes a bearer token to `~/.kumoba/token`, listens on `~/.kumoba/kumobad.sock` and `127.0.0.1:7788` for the API, and serves browser links such as the desktop live view on `127.0.0.1:7789`. `kumobad --help` lists the environment variables that override each default. Stop it with Ctrl-C, or `pkill -x kumobad` from another terminal: running sandboxes stop and keep their disks, so `kumoba start <id>` picks them up again later.

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

`kumoba` with no arguments lists every command, and `kumoba <command> --help` shows its arguments. The command list comes from the daemon itself, so the CLI always matches the daemon it talks to. [docs/cli.md](docs/cli.md) covers the argument syntax, output and exit codes.

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

`new Kumoba()` finds the socket and token in `~/.kumoba` by itself. Pass `{ url, token }` or set `KUMOBA_URL` and `KUMOBA_TOKEN` to reach a daemon elsewhere. Agents with a shell can use the CLI directly: `kumoba commands --json` prints every command with its JSON Schema.
<!-- #endregion getting-started -->

## Install
<!-- #region install -->

| What | How |
| --- | --- |
| Daemon, CLI, worker and images | `scripts/install.sh` as above, or a tarball and image from the [latest release](https://github.com/Parassharmaa/kumoba/releases/latest). Linux tarballs include the patched libkrun and libkrunfw; the macOS worker is signed for Hypervisor.framework. |
| Daemon and CLI from source | `cargo install kumobad` installs `kumobad`, `kumoba` and `kumoba-worker` side by side. On macOS sign the worker as below. |
| CLI only, through npm | `npm install -g kumoba`, or `npx kumoba <command>`; the same commands as the native CLI |
| TypeScript SDK | `npm install kumoba` |
| Rust library | `cargo add kumoba` |
| Guest agent for your own images | `bin/kumoba-guest` in the release tarball, or `cargo install kumoba-guest --target x86_64-unknown-linux-musl` with `RUSTFLAGS='-C linker=rust-lld'` |

On macOS a `cargo install`ed worker cannot use Hypervisor.framework until it carries the entitlement. The release worker already does:

```sh
cat > hvf.plist <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>
EOF
codesign --force --sign - --entitlements hvf.plist "$(which kumoba-worker)"
```

`scripts/check-host.sh` tells you whether a machine qualifies and how many sandboxes fit in its memory. Everything is released together under one version; see [CHANGELOG.md](CHANGELOG.md).
<!-- #endregion install -->

## How it works
<!-- #region how-it-works -->

```
your app, the kumoba CLI ──HTTP──▶ kumobad ──spawns──▶ kumoba-worker (one per VM, libkrun)
                      │                        │ vsock
                      │                        ▼
                      │                  kumoba-guest (PID 1 helper inside the VM)
                      └── egress proxy, secrets, previews, auto-stop
```

`kumobad` keeps sandbox metadata and disks in a state directory, boots VMs through a worker process per sandbox, and talks to a small agent inside each guest over vsock. Guests have no network device; outbound HTTP(S) goes through a proxy in the worker that enforces the sandbox's allowlist and injects secrets only for the hosts they are scoped to. See [docs/architecture.md](docs/architecture.md).
<!-- #endregion how-it-works -->

## Building from source
<!-- #region building -->

| Host | Needs |
| --- | --- |
| macOS 14+ on Apple silicon | `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw e2fsprogs rustup` |
| Linux x86_64 or arm64 | `/dev/kvm` (bare metal, or a VM with nested virtualization), Rust 1.99, `scripts/prepare-linux-host.sh` builds the patched libkrun and fetches libkrunfw |

```sh
git clone https://github.com/Parassharmaa/kumoba
cd kumoba
scripts/build.sh                    # kumobad, kumoba-worker, kumoba-guest into .local/bin
scripts/build-image.sh default      # needs Docker; into .local/images/default.raw
scripts/serve.sh                    # kumobad on a Unix socket and 127.0.0.1:7788
```

`serve.sh` writes the client settings to `.local/serve/env`; `set -a; source .local/serve/env; set +a` points `new Kumoba()` at it. Without Docker, `scripts/build-test-image.sh` produces the Alpine `test` image. [CONTRIBUTING.md](CONTRIBUTING.md) covers the tests.
<!-- #endregion building -->

## CLI

`kumoba` is a thin client for the daemon's command catalog (`GET /v1/commands`): every command, argument, type and help line comes from the daemon it talks to. The native build ships with the daemon; `npm install -g kumoba` or `npx kumoba` gives the same CLI on Node.

```sh
kumoba                                      # what this daemon can do
kumoba run --network.mode blocked -- python3 -c 'print(1)'
kumoba exec "$id" --cwd /workspace --env DEBUG=1 -- make test; echo "exit $?"
kumoba desktop "$id" --user kumoba --open   # live desktop in the browser
kumoba commands --json                      # JSON Schemas, for agents
```

`exec` and `run` exit with the guest command's own status. [docs/cli.md](docs/cli.md) has the argument syntax, output kinds and exit codes.

## TypeScript SDK
<!-- #region typescript-sdk -->

The package lives in [`sdk/typescript`](sdk/typescript). It depends only on `zod` and talks to `kumobad` over a Unix socket or loopback TCP.

```ts
const kumoba = new Kumoba({
  url: "unix:///path/to/kumobad.sock",   // default: KUMOBA_URL, then ~/.kumoba/kumobad.sock
  token: "...",                           // default: KUMOBA_TOKEN, KUMOBA_TOKEN_FILE, then ~/.kumoba/token
  publicUrl: "https://sandboxes.example.com", // optional, makes expose() return full URLs
});

const sandbox = await kumoba.create({
  image: "default",
  cpu: 2,
  memoryMib: 4096,
  user: "kumoba",                         // run commands as this guest user
  network: { allow: ["api.github.com"] }, // "open" | "blocked" | { allow }
  autoStopMinutes: 15,
  labels: { project: "demo" },
});

// Commands: string → /bin/sh -c, array → argv. Never throws on non-zero exit.
const r = await sandbox.run(["python3", "script.py"], {
  cwd: "/workspace",
  env: { PYTHONUNBUFFERED: "1" },
  timeoutMs: 60_000,
  onStdout: (text) => process.stdout.write(text),
});
r.ok; r.exitCode; r.stdout; r.stderr; r.timedOut;

// Files
await sandbox.files.write("/workspace/a.txt", "hi");
await sandbox.files.upload("./local.csv", "/workspace/data.csv");
const text = await sandbox.files.readText("/workspace/a.txt");
const stream = await sandbox.files.stream("/workspace/big.bin");

// Secrets: the guest sees a placeholder in $GITHUB_TOKEN; the real value is
// injected only into HTTPS requests to api.github.com.
await sandbox.setSecrets([
  { name: "gh", value: token, hosts: ["api.github.com"], envVars: ["GITHUB_TOKEN"] },
]);

// Ports: a signed, expiring link through kumobad to a port inside the VM.
const link = await sandbox.expose(3000, { ttlSeconds: 600 });

// Desktop (images built from images/default)
await sandbox.desktop.start();
const png = await sandbox.desktop.screenshot();
await sandbox.desktop.click(200, 300);
await sandbox.desktop.type("hello");
await sandbox.desktop.hotkey("ctrl+s");
const view = await sandbox.desktop.liveView(); // noVNC websocket link

// One-shot: a fresh VM for one command, deleted afterwards
const once = await kumoba.run(["python3", "-c", "print(42)"], { image: "default" });

// Lifecycle
await sandbox.stop(); await sandbox.start(); await sandbox.delete();
await kumoba.list({ labels: { project: "demo" } });
await kumoba.get(id); await kumoba.find(id);

// Network activity: every proxied connection, allowed or denied, with bytes
for await (const entry of kumoba.accessLog({ since: new Date(Date.now() - 3_600_000) })) {
  console.log(entry.sandboxId, entry.host, entry.decision, entry.bytesIn);
}
```

`await using` deletes the sandbox when the scope ends (TypeScript 5.2+, or Node 24+ for plain JavaScript; otherwise call `sandbox.delete()` in a `finally`). Errors are `KumobaError` with `status` and `code`.
<!-- #endregion typescript-sdk -->

## Rust
<!-- #region rust -->

The `kumoba` crate drives VMs directly, without the daemon; it needs a `kumoba-worker` binary from the release or `cargo install kumobad`. [`crates/kumoba/examples/hello.rs`](crates/kumoba/examples/hello.rs) boots an image, runs a command and exits:

```sh
cargo run --release -p kumoba --example hello -- ~/.kumoba/bin/kumoba-worker ~/.kumoba/images/default.raw
```

API documentation is on [docs.rs/kumoba](https://docs.rs/kumoba).
<!-- #endregion rust -->

## HTTP API

Everything the SDK does is a plain HTTP call with a bearer token; see [docs/http-api.md](docs/http-api.md).

```sh
curl --unix-socket ~/.kumoba/kumobad.sock -H "authorization: Bearer $(cat ~/.kumoba/token)" \
  -X POST http://kumobad/v1/sandboxes -H 'content-type: application/json' \
  -d '{"image":"default","cpu":2,"memoryMib":2048,"network":{"mode":"blocked"}}'
```

## Running it as a service
<!-- #region service -->

On a server, give each setting an explicit path rather than relying on `~/.kumoba`. `kumobad` is configured through environment variables (`kumobad --help` lists them). A minimal unit:

```ini
[Service]
User=kumoba
Environment=KUMOBAD_STATE_DIR=/var/lib/kumoba/state
Environment=KUMOBAD_WORKER=/opt/kumoba/bin/kumoba-worker
Environment=KUMOBAD_IMAGES=default=/opt/kumoba/images/default.raw
Environment=KUMOBAD_TOKEN_FILE=/etc/kumoba/token
Environment=KUMOBAD_SOCKET=/run/kumoba/kumobad.sock
Environment=KUMOBAD_PREVIEW_LISTEN=127.0.0.1:7789
Environment=KUMOBAD_PREVIEW_URL=https://previews.example.com
Environment=KUMOBAD_MAX_RUNNING=4
Environment=KUMOBAD_ALLOW_PRIVATE_HOSTS=gitlab.corp.example,10.20.0.0/16
Environment=KUMOBAD_UPSTREAM_PROXY=http://proxy.corp.example:3128
Environment=KUMOBAD_NO_PROXY=.corp.example
Environment=KUMOBAD_UPSTREAM_PROXY_CONNECT=address
Environment=KUMOBAD_LIBKRUN=/opt/kumoba/lib/libkrun.so.1
Environment=KUMOBAD_LIBKRUNFW=/opt/kumoba/lib/libkrunfw.so.5
ExecStart=/opt/kumoba/bin/kumobad
```

The release tarballs (`kumoba-<version>-linux-<arch>.tar.zst`) have exactly this layout: `bin/` with `kumobad`, `kumoba-worker` and `kumoba-guest`, `lib/` with the libraries, `licenses/` and a `manifest.json` of checksums. `KUMOBA_IMAGE=path.raw scripts/package.sh <version>` builds the same tarball with an image under `images/`. Put a reverse proxy in front of `KUMOBAD_PREVIEW_LISTEN` if browsers on other machines need `expose()` links or the desktop live view, and set `KUMOBAD_PREVIEW_URL` to its public address; the preview routes are self-authenticating and need no token.
<!-- #endregion service -->

## Security model
<!-- #region security -->

- Guests run in a hardware-isolated VM with their own kernel; the only channel to the host is the vsock agent and the egress proxy.
- Private, link-local and metadata addresses are never reachable from a guest, whatever the sandbox's allowlist says. An operator can open specific intranet hosts or CIDRs with `KUMOBAD_ALLOW_PRIVATE_HOSTS`; loopback, link-local and the host's own addresses stay blocked even then.
- Behind a corporate proxy, set `KUMOBAD_UPSTREAM_PROXY` and the daemon tunnels through it with `CONNECT` to the address it resolved and checked itself, or to the host name with `KUMOBAD_UPSTREAM_PROXY_CONNECT=host` (hosts in `KUMOBAD_NO_PROXY` and allowed intranet hosts go direct). The guest's TLS session rides inside the tunnel, so secrets and certificate checks behave the same.
- Secret values are stored on the host, scrubbed from proxied responses and injected only into HTTPS request headers for the hosts they are scoped to. The guest only ever holds a placeholder.
- Preview links are HMAC-signed, bound to one sandbox and port, and expire. Browsers load them from a separate preview listener (`127.0.0.1:7789` by default) that serves nothing else, so a page from a sandbox runs on an origin without access to the API, and redirects it sends stay inside its own preview path. On the API listener the same links are download-only. All sandboxes' previews share that one origin: a page can't load another sandbox's preview without its token, but browser storage such as `localStorage` is shared between them, so don't open untrusted previews in the same browser profile as previews whose storage matters.
- The Unix socket is created `0600`; the TCP listener accepts loopback addresses only.
<!-- #endregion security -->

## Status
<!-- #region status -->

Alpha. The runtime and SDK are exercised on every push by real VMs in CI (Linux x86_64 with KVM), and every release is installed with `scripts/install.sh` and booted before it is published. Expect the API to change before 1.0; [docs/releasing.md](docs/releasing.md) says what a version bump promises.
<!-- #endregion status -->

## Layout

| Path | What |
| --- | --- |
| `crates/kumoba` | Host runtime library ([crates.io](https://crates.io/crates/kumoba)) |
| `crates/kumoba-guest` | Agent that runs inside every VM ([crates.io](https://crates.io/crates/kumoba-guest)) |
| `crates/kumoba-protocol` | Host-guest wire protocol ([crates.io](https://crates.io/crates/kumoba-protocol)) |
| `crates/kumobad` | The daemon, its HTTP API and the `kumoba-worker` VMM process ([crates.io](https://crates.io/crates/kumobad)) |
| `sdk/typescript` | `kumoba` npm package ([npm](https://www.npmjs.com/package/kumoba)) |
| `images/` | Guest image recipes (`default`, plus the `mkfs` helper) |
| `patches/` | libkrun patches this project needs |
| `scripts/` | Build, test, image, package, install, release and CI entry points |

## License

Apache-2.0. libkrun is Apache-2.0; libkrunfw ships a GPL-2.0 Linux kernel. See [NOTICE](NOTICE).
