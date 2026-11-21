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

   This downloads Apple's macOS restore image for your Mac (about 26 GB), installs macOS, creates the `kumoba` user, and installs Apple's Command Line Tools and the kumoba guest agent. It takes about 30 minutes plus the download, and it deletes the restore image when the install is done. If it stops part way, run it again: it resumes where it stopped. Over a finished image it updates that image instead (see below). To build a new image, delete `~/.kumoba/images/macos` first.

2. Restart `kumobad` so it finds the new image. `kumoba images` now lists `macos`.

## Update the image

macOS sandboxes need the guest agent from the same kumoba release, so run `kumobad macos-image` again after upgrading kumoba. Over a finished image it boots the image without a network card, installs this release's agent, applies the image settings again, checks the image and compacts its disk. That takes a few minutes, and no download.

- The image can't be used while it runs: creating a macOS sandbox fails until it finishes. Sandboxes that already exist keep running. If `kumobad` was started while it ran, restart `kumobad` afterwards so it finds the image again.
- It boots a macOS VM, so it needs one of the two VMs Apple allows on the Mac.
- Compacting rewrites the disk image in place, so it only runs when your Mac has free space about the size of the image (about 31 GB), because sandboxes made from the image can keep its old blocks in use. With less, it skips compacting and says so; the image still works, and new sandboxes just start more slowly. If it's interrupted, run it again and it resumes.
- Sandboxes made before the update keep their old agent on their own disk. Create them again.

Compacting matters because every sandbox starts as a copy-on-write clone of the image, and the first write to a clone costs time in proportion to how fragmented the image file is on your Mac's disk. The more free space your Mac has, the better it compacts; on a nearly full disk it can only help a little.

## Use a macOS sandbox

```sh
id=$(kumoba create --image macos -q)
kumoba start "$id" -q                          # boots macOS, about 10 to 25 seconds
kumoba exec "$id" -- sw_vers
kumoba exec "$id" --user kumoba --cwd /workspace -- sh -c 'echo hi > note.txt; ls'
kumoba shell "$id" --user kumoba               # an interactive zsh
kumoba delete "$id"
```

Each sandbox is a copy-on-write clone of the image with its own machine identity, so creating one takes well under a second and uses almost no disk until it changes. Every macOS sandbox has the image's 4 CPUs and 4 GB of memory; the `cpu` and `memoryMib` you ask for are ignored.

Inside the sandbox:

- Commands run as root unless you pass `--user kumoba`. The `kumoba` user has passwordless `sudo`.
- `/workspace` exists and belongs to `kumoba`.
- The guest clock is set from your Mac's at every start, to within about 50 ms.
- `start` returns once the guest agent answers, while macOS is still starting background services, so the first commands can take a few seconds longer than later ones.
- Nobody is logged in to the desktop: the image turns automatic login off, because the login session's apps slowed the first commands after start by up to 17 seconds. The `kumoba` user's login keychain stays locked, so tools that keep credentials there need `security unlock-keychain` first.
- Apple's Command Line Tools are installed, so `git`, `clang`, `make` and `python3` work. Xcode itself is not included.

## See the screen

```sh
kumoba desktop "$id" --open
```

This opens the sandbox's screen in your browser, with keyboard and mouse working. It is the same [live view](/guide/desktop) Linux sandboxes have. The picture is read on your Mac from the VM's display, so nothing is installed or granted inside macOS.

- From code, `await sandbox.desktop.liveView()` returns the same link. The other `sandbox.desktop` methods, such as `screenshot` and `click`, need the desktop helper of the Linux images and don't work here.
- The sandbox starts at the macOS login window, because nobody is logged in. Sign in as `kumoba` with the password in `~/.kumoba/images/macos/password`.
- Keys are sent as a US keyboard. Characters outside that layout, and text from an input method editor, are not typed yet.
- Copy and paste work inside the sandbox. Your Mac's clipboard and the sandbox's are not shared yet.
- The pointer shows as your browser's arrow, not the shape macOS sets.
- It needs `kumoba-view-host` next to `kumobad`. The install script puts it there; a `kumobad` built with `cargo install` does not have it.
- `kumobad` has to run in your logged-in desktop session. Started over SSH or as a boot-time daemon, it has no desktop session to draw the VM's display in, and the live view fails to open.

## Network and secrets

macOS sandboxes have no network card. All traffic leaves through kumobad's proxy, so [network rules](/guide/network), [secrets](/guide/secrets) and the access log work as they do on Linux.

Command-line tools that honour `HTTPS_PROXY`, such as `curl`, `pip` and `npm`, reach the network. Apps that ignore it, including Safari and most apps built on Apple's networking frameworks, have no network access.

## Limits

- **Two at a time.** Apple allows at most two macOS VMs running on a Mac at once, and that includes VMs from other apps such as UTM or Tart. Starting a third fails with `running sandbox capacity exhausted`. Linux sandboxes don't count.
- **Apple's license.** macOS may run in VMs only on Apple hardware, for software development, testing, macOS Server or personal non-commercial use. Read the macOS license before you build a service on it.
- **Not yet supported:** host directory mounts and snapshots.
- **Local only.** Linux hosts and the release CI can't run macOS guests. Before a release, `scripts/macos-smoke.sh` checks macOS sandboxes on a Mac.

## Where things are

| Path | What it is |
| --- | --- |
| `~/.kumoba/images/macos/` | The image: disk, auxiliary storage and `machine.json` |
| `~/.kumoba/images/macos/password` | The `kumoba` user's password, readable only by you |
| `~/.kumoba/cache/` | The restore image while it downloads and installs |
