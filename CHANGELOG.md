# Changelog

All notable changes to kumoba are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The four crates (`kumoba`, `kumoba-protocol`, `kumoba-guest`, `kumobad`) and the `kumoba` npm package share one version and are released together.

## [Unreleased]

### Fixed

- `kumobad`: `GET /v1/sandboxes`, `kumoba list` and `kumoba.list()` return the newest sandboxes first, as documented. They used to return the oldest first, so with more matches than `limit` the sandboxes created most recently were the ones left out.
- `kumobad`: a `label` filter on `GET /v1/sandboxes` that is not `key:value`, such as `task=fix-tests`, is now refused with 400 `invalid_request`. It used to be ignored, so `kumoba list --label task=fix-tests` listed every sandbox. Both CLIs fail on it instead of listing (the Node.js CLI before it asks the daemon), and on an empty `--label`, `--label '[]'` or `--args '{"label":[]}'`, which used to send no filter at all. In the TypeScript SDK, `kumoba.list({ labels })` refuses an empty key or a key with `:`, which a filter cannot express.
- Live view: a client that connects to `kumoba-view` and goes quiet, or asks for the viewer script and never reads it, gives up its place sooner. A connection is let go once it has sent nothing for 2 seconds, whether it sent part of its request or nothing at all, and a client that keeps sending a byte at a time still has to finish its request within 5 seconds, as before. The reply has to be read within 10 seconds, which had no limit before. One address can hold at most 8 of the 64 connections that are still loading the page, so a few addresses cannot keep clients from other addresses out, and when no place is free the engine answers `503 Service Unavailable` instead of closing the connection without a reply.
- Writing a file to a path that is a named pipe (FIFO) fails at once with `guest operation failed: only regular files are supported` (HTTP 422, `guest_error`) instead of hanging. Each such write used to leave a guest connection stuck for good, and after 64 of them commands, file transfers, previews and the desktop on that sandbox stopped responding.
- A command that leaves a background process holding its output open, as in `sh -c 'build; server &'`, reports `truncated: true` when output over the limit was dropped. It used to report `truncated: false` in that case, so the cut output looked complete. Output that was already waiting to be read when `exec` stopped listening is now returned instead of being dropped.
- `kumobad`: `autoDeleteMinutes` now also deletes a sandbox whose VM stopped on its own, for example after `poweroff`, a kernel panic, running out of memory or a crashed worker. The clock starts when kumobad first sees it stopped. Such a sandbox used to show `stopped` forever and keep its disk.
- `kumobad`: listing files, opening a preview link and every preview request now count as activity for auto-stop, as the HTTP API guide says. A sandbox could be stopped while someone was only browsing its preview or file list, and a preview request that was still being answered could be cut off.

## [0.6.6] - 2026-11-25

### Fixed

- `kumoba/view`: a press in the empty bars around the desktop image no longer clicks the nearest edge of the desktop. Moves over the bars are ignored unless a button is held, a click lands on the pixel under the pointer instead of half a pixel off, a touch drag on the view drags on the desktop instead of scrolling the page while in control, and the page's own `touch-action` comes back afterwards. No button stays held on the desktop any more when the browser cancels a press, when a second button is pressed and released during a drag, or when the page switches to watching only mid-drag.

## [0.6.5] - 2026-11-23

The first release with these changes; the v0.6.4 tag was never published.

### Added

- `kumoba/view`, a browser client for the live view in the `kumoba` npm package: `connectViewer(canvas, url)` shows the stream of a `kumoba-view` engine in your own page, with the decoder built in. The engine's viewer page now runs the same client. See [Show it in your own page](https://parassharmaa.github.io/kumoba/guide/live-view-anywhere#show-it-in-your-own-page).

### Changed

- Live view: the engine uses about half as much CPU while a page scrolls and about a third less on animation. Static Linux builds compared pixels through musl's byte-by-byte `memcmp`; the codec now compares them itself. During a steady scroll it also checks last frame's shift before hashing rows to find a new one.
- Live view: a wheel notch scrolls one step for mice that report lines, instead of three, and characters typed with AltGr or Option reach the desktop without the held modifiers.

## [0.6.3] - 2026-11-23

### Added

- The live view engine on its own: from this release on, every release has `kumoba-view` for Linux on x86_64 and arm64, a static binary that shows any X display in the browser, as a replacement for VNC in Docker containers and other sandboxes. `--listen` on an address other than loopback protects the stream with a token, from `KUMOBA_VIEW_TOKEN` or `--token-file`, or made up and printed on a terminal or written to `~/.kumoba-view-token-PORT`. See [Use the live view in any container](https://parassharmaa.github.io/kumoba/guide/live-view-anywhere).

### Changed

- Dependency updates: `uuid` 1.29.3, `libloading` 0.9.1, `zeroize` 1.9.2, `bytes` 1.12.2 and `url` 2.5.9, which fixes RUSTSEC-2026-0592 (a stack overflow on some URLs).

## [0.6.2] - 2026-11-22

### Added

- Live view of macOS sandboxes: your clipboard and the sandbox's are shared. Paste with Cmd+V, or Ctrl+V from Windows or Linux, and copy with Cmd+C or Ctrl+C. Copied text reaches your clipboard only in the two seconds after a copy shortcut, as on Linux.
- Live view of macOS sandboxes: characters outside the US keyboard layout and text from an input method editor, such as Japanese, are typed. They go in through the sandbox's clipboard, whose earlier text is put back a second later.
- Live view of macOS sandboxes: the pointer takes the shape macOS sets, such as the text cursor over a text field.

## [0.6.1] - 2026-11-21

### Fixed

- TypeScript SDK: `desktop.liveView()` returns a working link for a macOS sandbox. It used to link to a port inside the sandbox, where macOS has no viewer. For an image without a desktop it now fails with kumobad's reason instead of returning a link that opens nothing.

## [0.6.0] - 2026-11-21

### Added

- A live view for macOS sandboxes. `kumoba desktop` on a macOS sandbox opens the same viewer Linux sandboxes have, with keyboard and mouse. The screen is read on your Mac from the VM's display, so nothing is installed or granted inside macOS. Not in it yet: a shared clipboard, characters outside the US keyboard layout, and the pointer shape macOS sets.
- Live view: paste with Ctrl+V or Cmd+V sends your clipboard's text to the sandbox and pastes it, with the terminal's own shortcut when a terminal has the focus. A copy shortcut pressed in the live view brings the sandbox's copied text to your clipboard; text leaves the sandbox only in the two seconds after that shortcut. On a Mac, Cmd+C and Cmd+X copy and cut in the sandbox.
- Live view: text from an input method editor, such as Japanese, is typed into the sandbox when you confirm it.
- Live view: the picture follows when the desktop changes size.

### Changed

- Live view: dragging a window is sent as a move, like scrolling. A scripted window drag took 2 to 4 Mbps where it took 18 Mbps before.
- Live view: video and other busy regions need about a third fewer bits for the same picture, from motion search at half-pixel steps and a tighter code for the compressed blocks.

### Fixed

- Live view: a character that the sandbox's keyboard layout lacks could be dropped the first time it was typed.
- Live view: a click could reach the sandbox before keys pressed just before it. Keyboard and pointer input now arrive in the order you made them.
- Live view: the keyboard follows a layout change made in the sandbox, such as `setxkbmap`.

## [0.5.0] - 2026-11-21

### Added

- A new desktop live view for Linux sandboxes. `kumoba desktop` now opens kumoba's own viewer instead of noVNC. It shows text and windows pixel for pixel, sends a scroll as a move plus the newly shown part, compresses regions that keep changing such as video, and returns them to exact pixels when they stop. It adapts to slow connections, shows the remote pointer, types the characters of any keyboard layout, and sends nothing while the screen is still or the tab is in the background. Text from an input method editor and pasted text aren't sent yet. Rebuild or update the `default` image to get it; older images keep noVNC.
- `kumoba desktop --viewer vnc`, and `viewer` in `POST /v1/sandboxes/{id}/desktop`, open noVNC on an image that has both viewers. The response's `viewer` field says which one the link opens.
- TypeScript SDK: `desktop.liveView()` links to the new viewer when the image has it, takes `{ viewer: "vnc" }` for the noVNC bridge, and returns which one it linked to as `viewer`. With the new viewer the link is the page itself, so code that appends `vnc.html` should check `viewer` first. `DESKTOP_STREAM_PORT` is the viewer's port in the guest.

### Changed

- The `default` image and the release tarballs include `kumoba-view`, and `scripts/build.sh` builds it. Building kumoba from source now needs the `wasm32-unknown-unknown` Rust target, which `rust-toolchain.toml` installs.

## [0.4.0] - 2026-11-19

### Added

- macOS sandboxes on Apple silicon Macs running macOS 27 or later. `kumobad macos-image` downloads Apple's restore image, installs macOS with a `kumoba` user, Apple's Command Line Tools and the guest agent, and adds a `macos` image; run it again after upgrading kumoba to update the agent in that image. `kumoba create --image macos` then works like any other image, including exec, files, shell, network rules and secrets. Sandboxes are copy-on-write clones with their own machine identity, and they take commands about 10 to 25 seconds after start. Apple allows two macOS VMs per Mac at once, so a third start reports capacity exhausted. The desktop live view, host mounts and snapshots don't support macOS yet.
- `scripts/macos-smoke.sh`: the local check for macOS sandboxes, since CI runners can't boot macOS guests.

### Changed

- The worker needs the `com.apple.security.virtualization` entitlement as well as `com.apple.security.hypervisor` on macOS. Release builds are signed with both; re-sign a `cargo install`ed worker as the install guide shows.
- The guest agent reports its operating system, and a sandbox's recorded CPU and memory are the ones it actually runs with.

### Fixed

- The egress proxy accepts `CONNECT ... HTTP/1.0`, which Python's `http.client` sends before Python 3.12. `pip install` with such a Python, including the one Apple's Command Line Tools ship, failed with `400 Bad Request` and the access log showed the allowed host as denied.

## [0.3.0] - 2026-11-13

### Added

- `kumoba shell <id>`: an interactive terminal inside a sandbox, with arrow keys, tab completion, Ctrl-C, full-screen programs and window resizing. Both the native and the npm CLI support it, and it exits with the shell's status. Piped input works too.
- `POST /v1/sandboxes/{id}/shell`: the HTTP route behind it, an upgrade to a small framed protocol documented in the HTTP API reference.

### Changed

- The guest agent reports whether it supports shells. Images from earlier releases keep working for everything else and answer a shell request with a clear error.

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

[Unreleased]: https://github.com/Parassharmaa/kumoba/compare/v0.6.6...HEAD
[0.6.6]: https://github.com/Parassharmaa/kumoba/compare/v0.6.5...v0.6.6
[0.6.5]: https://github.com/Parassharmaa/kumoba/compare/v0.6.3...v0.6.5
[0.6.3]: https://github.com/Parassharmaa/kumoba/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/Parassharmaa/kumoba/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/Parassharmaa/kumoba/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/Parassharmaa/kumoba/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/Parassharmaa/kumoba/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Parassharmaa/kumoba/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Parassharmaa/kumoba/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/Parassharmaa/kumoba/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/Parassharmaa/kumoba/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Parassharmaa/kumoba/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Parassharmaa/kumoba/releases/tag/v0.1.0
