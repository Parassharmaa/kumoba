# Run macOS in a sandbox

On an Apple silicon Mac, a sandbox can run macOS instead of Linux. Use it for work that needs a real Mac, such as building and testing with Apple's compilers, running a macOS build script or checking a command-line tool on macOS. The same CLI, SDKs, network rules and secrets apply.

macOS sandboxes are new in kumoba 0.4 and, like the rest of kumoba, alpha.

## Before you begin

- An Apple silicon Mac running macOS 27 or later, with kumoba 0.4 or later [installed](/getting-started).
- About 50 GB of free disk while the image builds. The finished image takes about 28 GB.

## Build the macOS image

1. Build it:

   ```sh
   kumobad macos-image
   ```

   This downloads Apple's macOS restore image for your Mac (about 26 GB), installs macOS, creates the `kumoba` user, and installs Apple's Command Line Tools and the kumoba guest agent. It takes about 30 minutes plus the download, and it deletes the restore image when the install is done. If it stops part way, run it again: it resumes where it stopped. Over a finished image it does nothing; delete `~/.kumoba/images/macos` to build a new one.

2. Restart `kumobad` so it finds the new image. `kumoba images` now lists `macos`.

## Use a macOS sandbox

```sh
id=$(kumoba create --image macos -q)
kumoba start "$id" -q                          # boots macOS, about 20 to 30 seconds
kumoba exec "$id" -- sw_vers
kumoba exec "$id" --user kumoba --cwd /workspace -- sh -c 'echo hi > note.txt; ls'
kumoba shell "$id" --user kumoba               # an interactive zsh
kumoba delete "$id"
```

Each sandbox is a copy-on-write clone of the image with its own machine identity, so creating one takes well under a second and uses almost no disk until it changes. Every macOS sandbox has the image's 4 CPUs and 4 GB of memory; the `cpu` and `memoryMib` you ask for are ignored.

Inside the sandbox:

- Commands run as root unless you pass `--user kumoba`. The `kumoba` user has passwordless `sudo`.
- `/workspace` exists and belongs to `kumoba`.
- The guest clock is set from your Mac's at every start, to within about a second.
- Apple's Command Line Tools are installed, so `git`, `clang`, `make` and `python3` work. Xcode itself is not included.

## Network and secrets

macOS sandboxes have no network card. All traffic leaves through kumobad's proxy, so [network rules](/guide/network), [secrets](/guide/secrets) and the access log work as they do on Linux.

Command-line tools that honour `HTTPS_PROXY`, such as `curl`, `pip` and `npm`, reach the network. Apps that ignore it, including Safari and most apps built on Apple's networking frameworks, have no network access.

## Limits

- **Two at a time.** Apple allows at most two macOS VMs running on a Mac at once, and that includes VMs from other apps such as UTM or Tart. Starting a third fails with `running sandbox capacity exhausted`. Linux sandboxes don't count.
- **Apple's license.** macOS may run in VMs only on Apple hardware, for software development, testing, macOS Server or personal non-commercial use. Read the macOS license before you build a service on it.
- **Not yet supported:** the desktop live view, host directory mounts and snapshots.
- **Local only.** Linux hosts and the release CI can't run macOS guests. Before a release, `scripts/macos-smoke.sh` checks macOS sandboxes on a Mac.

## Where things are

| Path | What it is |
| --- | --- |
| `~/.kumoba/images/macos/` | The image: disk, auxiliary storage and `machine.json` |
| `~/.kumoba/images/macos/password` | The `kumoba` user's password, readable only by you |
| `~/.kumoba/cache/` | The restore image while it downloads and installs |
