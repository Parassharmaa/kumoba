# Install options

The [getting started guide](/getting-started) uses the installer. These are all the ways to get kumoba.

| What | How |
| --- | --- |
| Daemon, CLI and an image | `curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh \| bash` |
| A specific version | Add `KUMOBA_VERSION=0.2.2` before `bash` |
| A small test image instead | Add `KUMOBA_IMAGE=test` (5 MB Alpine) |
| The files by hand | A tarball and an image from the [latest release](https://github.com/Parassharmaa/kumoba/releases/latest), checked against `SHA256SUMS` |
| Daemon and CLI from source | `cargo install kumobad` |
| CLI through npm | `npm install -g kumoba`, or `npx kumoba <command>` |
| TypeScript SDK | `npm install kumoba` |
| Rust library | `cargo add kumoba` |

## Requirements

| Host | Needs |
| --- | --- |
| macOS 14 or later, Apple silicon | `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd` |
| Linux, x86_64 or arm64 | Write access to `/dev/kvm`, and `zstd`. The release includes the libkrun build kumoba needs. |

You need about 2 GB of free disk for the `default` image. `scripts/check-host.sh` in the repository checks a machine and estimates how many sandboxes fit in its memory.

## Sign the worker after `cargo install` (macOS)

The worker needs the Hypervisor.framework entitlement. The release build already has it; a `cargo install`ed one needs signing:

```sh
cat > hvf.plist <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>
PLIST
codesign --force --sign - --entitlements hvf.plist "$(which kumoba-worker)"
```
