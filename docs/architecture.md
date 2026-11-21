# How it works

```
your app or the kumoba CLI
        │ HTTP (Unix socket or loopback)
        ▼
     kumobad ── one worker per VM ──▶ kumoba-worker (libkrun)
        │                                   │ vsock
        │                                   ▼
        └─ egress proxy, secrets,       kumoba-guest (PID 1 in the VM)
           previews, access log
```

## Processes

- **kumobad** is the daemon. It keeps sandbox state, serves the HTTP API, and every 30 seconds stops idle sandboxes and deletes expired ones.
- **kumoba-worker** runs once per running VM. It loads libkrun, boots the VM and hosts that VM's egress proxy. If the daemon dies, the worker shuts its VM down.
- **kumoba-guest** is the first process inside each VM. It runs commands, reads and writes files, and opens tunnels to guest ports for the host. It also offers a proxy on `127.0.0.1:3128`, so tools inside the VM reach the network without a network card.

Host and guest talk over vsock with length-prefixed JSON frames. Each operation opens its own connection, so a long command never blocks a file read. The protocol is version 3; a guest with another version is refused.

## Disks

An image is a raw ext4 filesystem. Creating a sandbox copies it. On APFS, XFS and Btrfs the copy is a reflink that costs nothing until the guest writes; elsewhere only the used blocks are copied. A stopped sandbox keeps its disk.

## Network

A guest has no network device. Its traffic goes through the worker's proxy, which applies the sandbox's policy (`open`, `allowlist` or `blocked`) and always refuses private addresses. TLS stays end to end, except for hosts with a secret: the proxy terminates those with a per-daemon CA that the guest trusts, so it can add the secret to the request. See [Control network access](/guide/network) and [How secrets work](/secrets).

Every connection attempt, allowed or denied, goes to an access log under the state directory. kumobad keeps 72 hours of it.

## Previews

A preview link is a signed token for one sandbox, port and expiry. Requests to it are relayed to the guest port, WebSockets included. Browsers load previews from a separate listener so pages from a sandbox never share the API's origin.

## Live view

`kumoba-view` runs inside the guest next to the desktop and listens on loopback. A browser reaches it through a preview link, like any other guest port.

- It waits for the X server to report a change, then reads only the changed rows through shared memory.
- It keeps a copy of what the browser shows. Each frame is the difference from that copy: areas that moved, pixels coded exactly, and blocks of busy content coded with loss. Lossy blocks are sent exactly once they stop changing.
- The browser decodes frames with the same Rust code, built as WebAssembly, and draws one whole frame per screen refresh.
- The browser acknowledges every frame. From the delay of those acknowledgements the engine sets how many frames may be on the way and how many bytes a second it may send.

## State directory

```
<KUMOBAD_STATE_DIR>/
  meta/<id>.json        sandbox metadata
  preview.key           key that signs preview links
  access/               access log, one file per hour
  runtime/
    sandboxes/<id>/     disk, worker socket, console log
    snapshots/<id>/     snapshot disks
```

Secrets live only in the daemon's memory. Clients send them again after a restart.

## Platforms

- **macOS** uses Hypervisor.framework. The worker is signed with the hypervisor entitlement.
- **Linux** uses KVM with libkrun 1.19.6 plus two patches that kumoba ships, and libkrunfw 5.6.2. The release tarballs include both.
