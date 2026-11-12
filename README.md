# kumoba

Local microVM sandboxes for AI agents and untrusted code. One small daemon, an HTTP API, and SDKs for TypeScript and Rust.

- **Real VMs, not containers.** Every sandbox is a [libkrun](https://github.com/containers/libkrun) microVM with its own kernel. KVM on Linux, Hypervisor.framework on Apple silicon.
- **Fast.** Create, boot, run a command and delete in about 0.4 s on an M-series Mac.
- **Self-hosted.** Runs on your laptop or your server. No account, no cloud, no telemetry.
- **Batteries included.** Commands with streaming output, files, time-limited port previews, egress allowlists, secrets that never enter the guest, and an optional xfce desktop with screenshots and input for computer-use agents.

```ts
import { Kumoba } from "kumoba";

const kumoba = Kumoba.fromEnv();
await using sandbox = await kumoba.create({ image: "default", user: "kumoba" });

const result = await sandbox.run("python3 -c 'print(21 * 2)'");
console.log(result.stdout); // "42\n"

await sandbox.files.write("/workspace/notes.md", "# hello");
console.log(await sandbox.files.list("/workspace"));

const link = await sandbox.expose(8080);
console.log(link.url);
```

## How it works

```
your app ──HTTP──▶ kumobad ──spawns──▶ kumoba-worker (one per VM, libkrun)
                      │                        │ vsock
                      │                        ▼
                      │                  kumoba-guest (PID 1 helper inside the VM)
                      └── egress proxy, secrets, previews, auto-stop
```

`kumobad` keeps sandbox metadata and disks in a state directory, boots VMs through a worker process per sandbox, and talks to a small agent inside each guest over vsock. Guests have no network device; outbound HTTP(S) goes through a proxy in the worker that enforces the sandbox's allowlist and injects secrets only for the hosts they are scoped to. See [docs/architecture.md](docs/architecture.md).

## Requirements

| Host | Needs |
| --- | --- |
| macOS 14+ on Apple silicon | `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw e2fsprogs rustup` |
| Linux x86_64 or arm64 | `/dev/kvm` (bare metal, or a VM with nested virtualization), Rust 1.99, `scripts/prepare-linux-host.sh` builds the patched libkrun and fetches libkrunfw |

`scripts/check-host.sh` tells you whether a machine qualifies and how many sandboxes fit in its memory.

Docker is needed only to build the default guest image.

## Quickstart

```sh
git clone https://github.com/Parassharmaa/kumoba
cd kumoba
scripts/build.sh                    # kumobad, kumoba-worker, kumoba-guest into .local/bin
scripts/build-image.sh default      # Debian + Python + Node + Chromium + xfce, into .local/images/default.raw
scripts/serve.sh                    # kumobad on a Unix socket and 127.0.0.1:7788
```

`serve.sh` writes the client settings to `.local/serve/env`:

```sh
set -a; source .local/serve/env; set +a
cd sdk/typescript && npm install && npm run build
node --input-type=module -e '
  import { Kumoba } from "./dist/index.js";
  const sandbox = await Kumoba.fromEnv().create({ user: "kumoba" });
  console.log((await sandbox.run("uname -a")).stdout);
  await sandbox.delete();
'
```

Without Docker, `scripts/build-test-image.sh` produces a 512 MB Alpine image named `test` that is enough to try the API.

## TypeScript SDK

The package lives in [`sdk/typescript`](sdk/typescript). It depends only on `zod` and talks to `kumobad` over a Unix socket or loopback TCP.

```ts
const kumoba = new Kumoba({
  url: "unix:///path/to/kumobad.sock",   // or http://127.0.0.1:7788
  token: "...",                           // contents of KUMOBAD_TOKEN_FILE
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

// Lifecycle
await sandbox.stop(); await sandbox.start(); await sandbox.delete();
await kumoba.list({ labels: { project: "demo" } });
await kumoba.get(id); await kumoba.find(id);
```

`await using` deletes the sandbox when the scope ends. Errors are `KumobaError` with `status` and `code`.

## Rust

The `kumoba` crate drives VMs directly, without the daemon. [`crates/kumoba/examples/hello.rs`](crates/kumoba/examples/hello.rs) boots an image, runs a command and exits:

```sh
scripts/build.sh && scripts/build-test-image.sh
cargo run --release -p kumoba --example hello -- .local/bin/kumoba-worker .local/test-arm64.raw
```

## HTTP API

Everything the SDK does is a plain HTTP call with a bearer token; see [docs/http-api.md](docs/http-api.md).

```sh
curl --unix-socket .local/serve/kumobad.sock -H "authorization: Bearer $(cat .local/serve/token)" \
  -X POST http://kumobad/v1/sandboxes -H 'content-type: application/json' \
  -d '{"image":"default","cpu":2,"memoryMib":2048,"network":{"mode":"blocked"}}'
```

## Running it as a service

`kumobad` is configured through environment variables (`kumobad --help` lists them). A minimal unit:

```ini
[Service]
User=kumoba
Environment=KUMOBAD_STATE_DIR=/var/lib/kumoba/state
Environment=KUMOBAD_WORKER=/opt/kumoba/bin/kumoba-worker
Environment=KUMOBAD_IMAGES=default=/opt/kumoba/images/default.raw
Environment=KUMOBAD_TOKEN_FILE=/etc/kumoba/token
Environment=KUMOBAD_SOCKET=/run/kumoba/kumobad.sock
Environment=KUMOBAD_MAX_RUNNING=4
Environment=KUMOBAD_LIBKRUN=/opt/kumoba/lib/libkrun.so.1
Environment=KUMOBAD_LIBKRUNFW=/opt/kumoba/lib/libkrunfw.so.5
ExecStart=/opt/kumoba/bin/kumobad
```

`scripts/package.sh <version>` produces a `kumoba-<version>-linux-<arch>.tar.zst` with the binaries, libraries, an image and a manifest of checksums for exactly this layout. Put a reverse proxy in front of `KUMOBAD_LISTEN` if browsers need to reach `expose()` links; the preview routes are self-authenticating and need no token.

## Security model

- Guests run in a hardware-isolated VM with their own kernel; the only channel to the host is the vsock agent and the egress proxy.
- Private, link-local and metadata addresses are never reachable from a guest, whatever the allowlist says.
- Secret values are stored on the host, scrubbed from proxied responses and injected only into HTTPS request headers for the hosts they are scoped to. The guest only ever holds a placeholder.
- Preview links are HMAC-signed, bound to one sandbox and port, and expire.
- The Unix socket is created `0600`; the TCP listener accepts loopback addresses only.

## Status

Experimental. The runtime and SDK are exercised on every push by real VMs in CI (Linux x86_64 with KVM) and by hand on macOS. Expect the API to change before 1.0.

## Layout

| Path | What |
| --- | --- |
| `crates/kumoba` | Host runtime library and the `kumoba-worker` VMM process |
| `crates/kumoba-guest` | Agent that runs inside every VM |
| `crates/kumoba-protocol` | Host-guest wire protocol |
| `crates/kumobad` | The daemon and HTTP API |
| `sdk/typescript` | `kumoba` npm package |
| `images/` | Guest image recipes (`default`, plus the `mkfs` helper) |
| `patches/` | libkrun patches this project needs |
| `scripts/` | Build, test, image, package and CI entry points |

## License

Apache-2.0. libkrun is Apache-2.0; libkrunfw ships a GPL-2.0 Linux kernel. See [NOTICE](NOTICE).
