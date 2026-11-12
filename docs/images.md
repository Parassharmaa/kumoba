# Guest images

An image is a raw ext4 disk. `kumobad` boots whatever `KUMOBAD_IMAGES` points at, so you can ship your own.

## The default image

`images/default/Dockerfile` builds a Debian 13 system with:

- `bash`, `git`, `curl`, `jq`, `sudo` (passwordless for the `kumoba` user)
- Python 3 with `pip` and `venv`, Node.js with `npm`
- Chromium, fonts for Latin and CJK text
- An xfce desktop on Xvfb with x11vnc and noVNC, plus `scrot`, `xdotool` and AT-SPI for the `desktop` API
- The `kumoba` user (uid 1000) and a `/workspace` it owns

Build it with Docker:

```sh
scripts/build-image.sh default          # → .local/images/default.raw (16 GB sparse)
DISK_SIZE=32G scripts/build-image.sh default
```

The script builds the Dockerfile for the host architecture, exports the container filesystem and writes it into an ext4 image with `mkfs.ext4 -d` inside a helper container (`images/mkfs`), so no root privileges are needed on the host.

## The test image

`scripts/build-test-image.sh` produces a 512 MB Alpine image without Docker. It has `kumoba-guest`, a `kumoba` user and a tiny HTTP server fixture, and is what the test suites use.

## Writing your own image

Any Linux root filesystem works as long as:

1. `/usr/local/bin/kumoba-guest` exists and is executable. The worker runs it as the VM's init process; it writes `/etc/hosts`, creates `/workspace`, then serves the host over vsock. Build it with `scripts/build.sh` (static musl binary in `.local/bin/kumoba-guest`).
2. `/bin/sh` exists. The SDK runs string commands through it.
3. Programs that should reach the network honour `HTTP_PROXY`/`HTTPS_PROXY` or Chromium's managed proxy policy. The agent sets the proxy variables for every command it runs; `images/default/rootfs/etc/profile.d/kumoba-proxy.sh` covers interactive shells.
4. For `sandbox.desktop`: `images/default/rootfs/usr/local/bin/kumoba-desktop` plus `xvfb`, an xfce session, `x11vnc`, `websockify` with `novnc`, `scrot`, `xdotool`, `python3-pil` and `python3-pyatspi`.

Put your Dockerfile and any `rootfs/` overlay in `images/<name>/` and run `scripts/build-image.sh <name>`. The script copies `kumoba-guest` into the build context for you.

## Sizing

The `.raw` file is sparse; its apparent size is the guest's disk size and its actual size is what the image uses. Sandbox disks are reflink copies on APFS, XFS and Btrfs, so each new sandbox costs only what it writes. On ext4 every sandbox copies the whole used portion of the image, which `scripts/check-host.sh` warns about.
