# Troubleshooting

## The installer says libkrun is missing (macOS)

Install it from Homebrew, then run the installer again:

```sh
brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd
```

## `/dev/kvm` is not writable (Linux)

kumobad needs KVM. Add your user to the `kvm` group and log in again:

```sh
sudo usermod -aG kvm "$USER"
```

On a cloud VM, check that the instance type supports nested virtualization.

## The installer could not unpack the image

The disk is too full. The `default` image needs about 2 GB while it unpacks. Free some space and run the installer again; it removes the partial file itself.

## `kumoba: Cannot reach kumobad`

kumobad isn't running, or the CLI is looking in the wrong place. Start it with `kumobad`. If you run kumobad with custom settings, point the CLI at it with `KUMOBA_URL` and `KUMOBA_TOKEN` (see [Configuration](/configuration)). The CLI exits with status 3 in this case.

## `No kumobad token`

kumobad writes `~/.kumoba/token` the first time it starts. Start it once, or set `KUMOBA_TOKEN`.

## The desktop link doesn't open, or kumobad logs `preview_listen_skipped`

Another program uses port 7789, so kumobad couldn't serve browser links. Free the port, or choose another one: `KUMOBAD_PREVIEW_LISTEN=127.0.0.1:7790 kumobad`. kumobad also logs `listen_skipped` when port 7788 is taken; the CLI and SDK keep working through the socket.

## `stop the sandbox before changing its network`

Network changes apply at boot. Run `kumoba stop <id>`, then `kumoba update`, then `kumoba start <id>`.

## `this sandbox's image has no shell support`

The sandbox was created from an image older than kumoba 0.3. [Upgrade](/guide/upgrade), then create a new sandbox from the new image.

## `shell is interactive: use the kumoba CLI 0.3 or later`

Your CLI predates `kumoba shell`. Install the current version with the installer or `npm install -g kumoba@latest`.

## `pip install` fails with `externally-managed-environment`

Your image predates kumoba 0.2.2. Upgrade (see [Upgrade or uninstall](/guide/upgrade)) to get an image where `pip install` works directly.

## A server started with `exec` stops right away

`exec` stops the processes a command started when the command ends. Start long-running processes with `setsid`, as in [Open a web app](/guide/expose).

## A command was cut off with status 141

The reader of `kumoba exec` or `kumoba run` output closed early, as in `kumoba exec <id> -- cat big.log | head`. That's the normal status for a closed pipe.

## Still stuck

Open an [issue](https://github.com/Parassharmaa/kumoba/issues) with the kumobad log, the output of `kumoba version`, and your OS and CPU. Report security problems privately as described in [Security](/security).
