# Changelog

All notable changes to kumoba are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The four crates (`kumoba`, `kumoba-protocol`, `kumoba-guest`, `kumobad`) and the `kumoba` npm package share one version and are released together.

## [Unreleased]

## [0.1.0] - 2026-11-13

### Added

- libkrun microVM runtime (`kumoba`): boot, exec with streaming output, files, host mounts, snapshot images, secrets that stay on the host, egress allowlists with wildcard domains, intranet allowlists, corporate upstream proxies and an access log of every proxied connection.
- `kumobad` daemon: HTTP API over a Unix socket or loopback TCP with bearer auth, concurrency limit, auto-stop, restart reconciliation, signed preview links, network updates on stopped sandboxes and a paged access-log feed.
- `kumoba-guest`: PID 1 agent for guest images with command execution, files, port forwarding and an optional xfce desktop with screenshots, input and a VNC live view.
- TypeScript SDK (`kumoba` on npm): `Kumoba`, `Sandbox`, `Files`, `Desktop`, `accessLog()` async iterator and `await using` disposal.
- Default Debian guest image with Python, Node, Chromium and the desktop, plus an Alpine test image that needs no Docker.
- Sparse disk copies with reflinks where the filesystem supports them.
- Zero-config start: with no environment `kumobad` uses `~/.kumoba` (or `KUMOBA_HOME`) for images, state, a generated token and its socket, finds `kumoba-worker` next to itself and libkrun in `../lib`, and `new Kumoba()` connects to it with no options.
- `scripts/install.sh`: one-line install of the daemon, worker and a prebuilt `default` or `test` image, verified against the release checksums.
- Releases ship the four crates, the npm package, host tarballs for Linux x86_64, Linux arm64 and macOS arm64, and prebuilt guest images for both architectures. Every release is installed and booted before it is published.

[Unreleased]: https://github.com/Parassharmaa/kumoba/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Parassharmaa/kumoba/releases/tag/v0.1.0
