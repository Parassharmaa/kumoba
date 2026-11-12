# Architecture

## Processes

**kumobad** is the long-running daemon. It owns a state directory, loads the images listed in `KUMOBAD_IMAGES`, serves the HTTP API, and runs a reaper every 30 seconds that stops idle sandboxes (`autoStopMinutes`) and deletes expired stopped ones (`autoDeleteMinutes`). It is a thin layer over the `kumoba` Rust crate.

**kumoba-worker** is spawned by the runtime once per running sandbox. It loads `libkrun`, creates the VM with the sandbox's disk, vCPUs and memory, and stays alive for the VM's lifetime. It also hosts the egress proxy for that VM. If the daemon dies, the worker notices the broken lock and shuts the VM down; if the worker dies, the daemon releases its capacity slot.

**kumoba-guest** is started as the first process inside every VM (`/usr/local/bin/kumoba-guest` in the image). It listens on a vsock port and executes the host's requests: run a command with streamed output, read, write or list files as a given user, open a TCP tunnel to a guest port. It also runs a loopback HTTP proxy on `127.0.0.1:3128` that forwards to the host's egress proxy, so ordinary `HTTP_PROXY`-aware tools and Chromium (via a managed policy) reach the network without a NIC.

## Wire protocol

`crates/kumoba-protocol` defines the frames exchanged over vsock: length-prefixed JSON with base64 payloads, capped at 8 MiB per frame and 1 MiB per data chunk. Each operation opens its own connection, so a long-running `exec` never blocks a concurrent file read. Protocol version 3.

## Disks

An image is a raw ext4 filesystem. Creating a sandbox copies the image into `<state>/runtime/sandboxes/<id>/root.raw`; on APFS, XFS and Btrfs the copy is a reflink and takes no time or space until the guest writes. Stopping a sandbox keeps its disk; `start` boots the same disk again. Snapshots (`Runtime::snapshot` in the Rust API) copy a stopped disk into `<state>/runtime/snapshots/`.

## Network

Guests have no network device. The worker exposes an HTTP CONNECT proxy to the guest over vsock, and the guest agent re-exposes it on loopback. The proxy applies the sandbox's policy:

- `blocked`: nothing leaves the VM.
- `open`: any public host on 80/443 or other ports via CONNECT. Private ranges (RFC 1918, link-local, loopback, the cloud metadata address) are always refused.
- `allowlist`: only the listed host names.

Operators can widen or reroute this per daemon: `KUMOBAD_ALLOW_PRIVATE_HOSTS` lists intranet host names (subdomains included) and IPv4 CIDRs that guests may reach despite being private, and `KUMOBAD_UPSTREAM_PROXY` makes the broker open every tunnel through a corporate HTTP proxy with `CONNECT` (Basic credentials from the URL), except for `KUMOBAD_NO_PROXY` and the allowed intranet hosts, which it dials directly. Loopback, link-local, the host's own interfaces and the metadata address are refused in every mode.

TLS is end to end for ordinary hosts: the guest sees the real certificate. Only hosts that have a secret scoped to them are terminated at the proxy with a per-runtime CA, which the agent installs into the guest's trust store and `SSL_CERT_FILE` / `NODE_EXTRA_CA_CERTS`, so the proxy can rewrite request headers and scrub responses. See [secrets.md](secrets.md).

## Access log

The broker reports every connection attempt (host, port, allowed or denied, bytes in each direction, start and end time) to the daemon, which stamps it with the sandbox id and labels and appends it to `<state>/access/<hour>.jsonl`. The reaper deletes files older than 72 hours. `GET /v1/access-log` reads them back, so an operator can attribute traffic per sandbox or per label without touching the host's own logs.

## Previews

`POST .../preview` signs `{sandbox, port, expiry}` with a key kept in `<state>/preview.key`. A request to `/v1/preview/<token>/...` verifies the token, opens a tunnel to the guest port through the agent, and relays the HTTP exchange, including WebSocket upgrades. Because the token is in the path, browsers can use these links directly; the daemon strips hop-by-hop headers and forces `content-disposition: attachment` on browser-renderable types so a sandbox cannot run scripts under the daemon's origin.

## State directory

```
<KUMOBAD_STATE_DIR>/
  meta/<id>.json            sandbox metadata (labels, limits, network, timestamps)
  preview.key               HMAC key for preview tokens
  runtime/
    owner.lock              held by the daemon
    sandboxes/<id>/         root.raw, worker socket, console log
    snapshots/<id>/         snapshot disks
    trash/                  disks awaiting deletion
```

Secrets live in memory in the daemon and are re-sent by clients after a restart.

## Platform notes

- **macOS** uses Hypervisor.framework through libkrun. The worker is ad-hoc signed with the `com.apple.security.hypervisor` entitlement at build time (`scripts/hvf-entitlements.plist`).
- **Linux** needs `/dev/kvm` and a patched libkrun 1.19.6 (`patches/libkrun-1.19.6`, built by `scripts/build-libkrun-closefix.sh`) plus libkrunfw 5.6.2. `scripts/prepare-linux-host.sh` does both and writes the resulting paths to `.local/linux-host.env`.
