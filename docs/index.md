---
layout: home
title: kumoba
titleTemplate: Fast sandboxes for AI agents

hero:
  name: kumoba
  text: Fast sandboxes for AI agents
  tagline: Real VMs that boot in under a second. Run them on your laptop, your servers or your cloud.
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
  - title: Fast
    details: Create, boot, run and delete in about half a second.
  - title: Real VMs
    details: Each sandbox has its own kernel, not a shared one.
  - title: Built for agents
    details: A CLI and SDKs where every command has a JSON Schema.
  - title: Safe by default
    details: Egress allowlists, hidden secrets, a log of every connection.
  - title: Live desktop
    details: Watch and drive a full desktop from your browser.
  - title: Runs anywhere
    details: Your laptop, your servers, your cloud. Apache-2.0.
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
