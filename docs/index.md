---
layout: home
title: kumoba
titleTemplate: Local microVM sandboxes for AI agents

hero:
  name: kumoba
  text: Local microVM sandboxes for AI agents
  tagline: Real VMs with their own kernel, a zero-config daemon, a CLI and SDKs. Self-hosted, open source, alpha.
  image:
    src: /hero-terminal.svg
    alt: A terminal running kumoba run, create, exec and desktop
  actions:
    - theme: brand
      text: Get started
      link: /getting-started
    - theme: alt
      text: CLI
      link: /cli
    - theme: alt
      text: GitHub
      link: https://github.com/Parassharmaa/kumoba

features:
  - title: Real VMs, not containers
    details: Every sandbox is a libkrun microVM with its own kernel. KVM on Linux, Hypervisor.framework on Apple silicon. Boot, run and delete in under a second.
  - title: One command to start
    details: The installer fetches the daemon, the CLI and a ready image. kumobad needs no configuration, and the CLI and SDK find it on their own.
  - title: Made for agents
    details: The CLI builds its commands from the daemon's catalog, every command has a JSON Schema, and exec returns the guest's exit status.
  - title: Safe by default
    details: Egress allowlists, secrets that never enter the guest, signed expiring previews and a log of every connection.
  - title: A desktop you can watch
    details: The default image ships xfce, Chromium and screenshots and input for computer-use agents, with a live view in your browser.
  - title: Self-hosted
    details: Your laptop or your server. No account, no cloud, no telemetry. Apache-2.0.
---

<div class="quick-start">

## Quick start <span class="alpha-pill">alpha</span>

```sh
curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
export PATH="$HOME/.kumoba/bin:$PATH"
kumobad &
kumoba run -- python3 -c 'print(21 * 2)'
```

On macOS install libkrun first: `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd`. The [getting started guide](/getting-started) walks through the rest.

| | |
| --- | --- |
| Install | [`install.sh`](https://github.com/Parassharmaa/kumoba/blob/main/scripts/install.sh) · [Releases](https://github.com/Parassharmaa/kumoba/releases) |
| TypeScript | [`npm install kumoba`](https://www.npmjs.com/package/kumoba) |
| Rust | [`cargo install kumobad`](https://crates.io/crates/kumobad) · [`cargo add kumoba`](https://crates.io/crates/kumoba) · [docs.rs](https://docs.rs/kumoba) |
| Project | [GitHub](https://github.com/Parassharmaa/kumoba) · [Changelog](/changelog) · [Security](/security) |

</div>
