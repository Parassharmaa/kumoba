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

## The live view of a macOS sandbox doesn't open

`kumoba desktop` on a macOS sandbox answers `the live view did not start` with the reason in brackets:

- **`kumoba-view-host is missing`**: the engine that shows a macOS sandbox's screen is not next to `kumobad`. The install script puts it there; a `kumobad` built with `cargo install` does not have it. [Build it from the repository](/install#build-the-live-view-engine-for-macos-sandboxes), or set `KUMOBAD_VIEW` to where it is.
- **`the live view needs kumobad to run inside a logged-in desktop session`**: `kumobad` was started over SSH or as a boot-time daemon. Start it from a terminal in your desktop session.
- **`too many live views are open on this sandbox`**: eight pages can watch one macOS sandbox at a time. Close some.

Paste, copy and characters outside the US layout need someone signed in to the sandbox's desktop. Before that they do nothing, and `~/.kumoba/state/runtime/sandboxes/<id>/worker.log` says `nobody is signed in to the sandbox's desktop`.

If the page opens and shows the macOS login window, that is the sandbox's screen: nobody is logged in yet. Sign in as `kumoba` with the password in `~/.kumoba/images/macos/password`.

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

## Output from a background process is missing

The command left a process running in the background that still holds its output open, as in `sh -c 'make; ./server &'`. `exec` keeps reading for a quarter of a second after the command ends, returns what has arrived by then, and stops. Whatever that process writes later is not returned, and is not counted as truncated. Send its output to a file, as in `./server > /tmp/server.log 2>&1 &`, and read the file instead.

## A command was cut off with status 141

The reader of `kumoba exec` or `kumoba run` output closed early, as in `kumoba exec <id> -- cat big.log | head`. That's the normal status for a closed pipe.

## `running sandbox capacity exhausted` when starting a macOS sandbox

Apple allows two macOS VMs running on a Mac at once, counting VMs from other apps such as UTM or Tart. Stop a macOS sandbox, or quit the other app's VM, and start again. See [Run macOS in a sandbox](/guide/macos#limits).

## A macOS sandbox won't start after upgrading kumoba

`this macOS sandbox's guest agent is from another kumoba release` means the sandbox's disk still has the agent it was cloned with. Run `kumobad macos-image`: over a finished image it installs the current agent and keeps everything else. Sandboxes made before that keep their old agent on their own disk, so copy out what you need and create them again. See [Run macOS in a sandbox](/guide/macos#update-the-image).

## `kumobad macos-image` stops part way

Run it again. It remembers the finished stages in `~/.kumoba/images/macos/build.json` and resumes, and an interrupted download continues where it stopped. `guest provisioning needs macOS 27 or later on the host` means the Mac itself must be updated first.

## Still stuck

Open an [issue](https://github.com/Parassharmaa/kumoba/issues) with the kumobad log, the output of `kumoba version`, and your OS and CPU. Report security problems privately as described in [Security](/security).
