# Changelog

All notable changes to kumoba are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The four crates (`kumoba`, `kumoba-protocol`, `kumoba-guest`, `kumobad`) and the `kumoba` npm package share one version and are released together.

## [Unreleased]

## [0.2.2] - 2026-11-13

### Changed

- The default image lets `pip install` write to the system Python (`/etc/pip.conf`), as the README shows. A sandbox is a disposable VM, so Debian's protection against mixing pip and apt packages only got in the way; non-root users fall back to a user install.

### Fixed

- `scripts/install.sh` says why an image could not be unpacked, with the free space it found, instead of exiting silently, and removes the partial image.

## [0.2.1] - 2026-11-13

### Fixed

- The `kumoba` CLI no longer panics when its output is piped into a command that exits early, such as `kumoba | head`. Help and JSON output stop quietly with status 0, and a streamed `exec` or `run` that is cut off exits 141, like a Unix process ended by a closed pipe. The npm CLI behaves the same.

### Added

- A README walkthrough (`scripts/walkthrough.sh`) runs on a fresh runner after every release, installing from the public installer and npm the way a new user does.

## [0.2.0] - 2026-11-13

### Added

- `kumoba` CLI, built for people and for agents. It has no commands of its own: it reads the daemon's catalog and builds every command, argument check and help page from it. Ships natively with the daemon (`install.sh`, release tarballs, `cargo install kumobad`) and through npm (`npx kumoba`). `exec` and `run` exit with the guest command's status.
- `GET /v1/commands`: every route with a JSON Schema for its input, so clients and agents can discover the API.
- `POST /v1/run` and `Kumoba.run()`: one command in a fresh sandbox that is deleted when it ends.
- `POST /v1/sandboxes/{id}/desktop`: starts the desktop and returns a link to a live noVNC view.
- Browser previews on a separate loopback listener (`KUMOBAD_PREVIEW_LISTEN`, default `127.0.0.1:7789`), so exposed web apps and the desktop render in a browser while staying off the API's origin. Preview and desktop responses carry a full `url`.

### Changed

- With no configuration `kumobad` also listens on `127.0.0.1:7788`, and on `127.0.0.1:7789` for previews; either is skipped if its port is taken.
- A bare program name in `exec` or `run` (`python3`) is looked up on the guest's `PATH` instead of being rejected.
- The SDK reports an unreachable daemon with the error code `unreachable`.
- Dependencies: `sha2` 0.11 with `hmac` 0.13, `base64` 0.23, `libloading` 0.9, TypeScript 7, vitest 5, and newer GitHub Actions.

### Fixed

- Previews no longer wait on a guest server that keeps its connection open: responses are read to their declared length, and plain requests reach the guest as one-shot HTTP/1.0. Page loads through a preview dropped from about 5 s per file to milliseconds.
- Redirects from a preview stay inside that preview: the guest's `location` is resolved with a URL parser and anything leaving the guest origin is dropped.
- The desktop's accessibility tree retries while the AT-SPI bus is still starting, instead of failing right after `desktop.start()`.

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

[Unreleased]: https://github.com/Parassharmaa/kumoba/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/Parassharmaa/kumoba/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/Parassharmaa/kumoba/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Parassharmaa/kumoba/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Parassharmaa/kumoba/releases/tag/v0.1.0
