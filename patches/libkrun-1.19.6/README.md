# libkrun 1.19.6 Unix-vsock close fix

Local patch for the non-browser Linux secret/streaming validation failure. The library ABI stays at 1 and firmware stays at 5.6.2. No system library is replaced. This is not an upstream release or a published fork.

## Provenance

- Upstream: [libkrun/libkrun, v1.19.6](https://github.com/libkrun/libkrun/tree/v1.19.6).
- Commit: `227b2de6ed323fe180e02f871c5f325a90c13cc2`.
- Archive URL: `https://api.github.com/repos/libkrun/libkrun/tarball/v1.19.6`.
- Archive SHA-256: `3e80db0928be7b00e2b94910ef782613aad40ede692c629e7d909b7d73dc891d`.
- Cargo.lock SHA-256: `a73d2ee0e4f2d2b63c7b4a00d2639076e4f7884a40cec495d5d82eed14dcd873`.
- Upstream licensing: [Apache 2.0](LICENSE.upstream). The patches retain upstream context and add the close regression/fix described here.
- Patch checksums: [SHA256SUMS](SHA256SUMS).

`0001-close-regressions.patch` adds tests only. `0002-drain-before-reset.patch` changes the Unix proxy receive/close handling. Both apply to the pinned archive in order.

## Evidence and fix

A real Unix socket peer writes final bytes and closes **before** epoll is polled. The tests observe `IN | HANG_UP` together, then pass that actual readiness event to `UnixProxy` with guest-memory virtqueue buffers. There are no sleeps. Stock 1.19.6 handles hang-up first, emits reset and returns before reading the queued bytes.

The fix routes connected hang-ups through the receive path. A nonblocking peek distinguishes queued data, EOF and would-block before requiring guest RX descriptors or credit. Data is copied only within the existing descriptor and credit limits. Empty EOF closes promptly even with zero credit. Reset follows the final data packets, and the closed proxy is scheduled for deferred reclamation.

Zero-credit replies leave a credit-blocked proxy paused. RX-descriptor exhaustion retains the existing readable-socket polling behavior and does not consume bytes or reset the connection; posting buffers allows the next readiness processing to continue. No extra payload buffer, larger window, delayed close, or altered shutdown flags are introduced. Existing connecting/listening hang-up handling is unchanged.

Six regression cases cover combined readable/hang-up delivery, zero credit, no RX descriptors, empty EOF, partial delivery followed by forwarded credit, and the existing data-then-reset behavior on a write-half close. On stock, four fail and two pass. With the fix, all six pass. The final-byte case also requires deferred cleanup.

## Build and reproduce

Run inside the prepared Linux host, from the Kumoba root. Prerequisites are Rust/rustup 1.99.0, GCC, static glibc development files, curl, tar, patch, util-linux `flock`, coreutils and binutils. On the Fedora test host, missing build tools were installed with `dnf install glibc-static patch`; `libkrunfw-devel` 5.6.2 was also installed during initial setup. The runtime firmware binary remains unchanged.

```sh
# Expected nonzero test exit: 4 failed, 2 passed. A compiler/setup failure is not the expected result.
bash scripts/build-libkrun-closefix.sh stock

# Apply the same regression tests plus the correctness fix; test and build locally.
bash scripts/build-libkrun-closefix.sh patched
```

The [builder](../../scripts/build-libkrun-closefix.sh) checks archive and patch hashes, extracts fresh sources, uses the upstream lockfile, serializes builds, and isolates stock/patched target directories under `.local/libkrun-closefix`. Sources and binaries remain ignored. It records the compiler, source, lockfile, patch and binary hashes in `patched/build-manifest.txt`. These are pinned build inputs and a reproducible procedure, not a promise of identical binaries across toolchains or source paths.

The local build enables default `init-blob` plus `blk`, the features needed for Kumoba's block-disk/vsock workloads. It does not enable optional GPU, sound, input or guest NIC devices. It is not a full-feature replacement for the distribution package.

## Use the trusted override

```sh
export KUMOBAD_LIBKRUN="$PWD/.local/libkrun-closefix/patched/libkrun.so.1"
export KUMOBAD_LIBKRUNFW=/usr/lib64/libkrunfw.so.5
cargo run -p kumoba --example host-info --locked
bash scripts/test-local.sh --lib real_guest_network_secrets_are_host_only_and_scoped
```

Keep these paths in trusted host configuration. `RuntimeConfig::for_host` and the existing test script honor them. Unsetting the override returns to the stock library, whose close failure remains. No default SDK selection or global installation was changed.

See the [validation record](../../docs/history/linux-vsock-closefix-validation.md) for exact binary identity, before/after outcomes, repeated real-guest checks and remaining platform limits.
