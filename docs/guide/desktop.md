# Open the desktop live view

The `default` image includes an xfce desktop with Chromium. You can watch it and use it from your browser, while an agent drives it through screenshots and input.

Before you begin:

- [Install kumoba and start `kumobad`](/getting-started).
- Make sure port 7789 on your machine is free. kumobad serves browser links there.

## Open the live view

1. Create and start a sandbox from the `default` image:

   ```sh
   id=$(kumoba create --image default -q)
   kumoba start "$id"
   ```

2. Start the desktop and open it in your browser:

   ```sh
   kumoba desktop "$id" --user kumoba --open
   ```

   The desktop appears with keyboard and mouse working. **Applications → Terminal Emulator** gives you a shell inside the sandbox.

The link expires after one hour. Run the same command again for a new one. Without `--open`, the command prints the link as `url`.

## What the live view does

The live view is kumoba's own viewer. It sends the desktop to your browser only when the screen changes, so a still desktop uses no bandwidth and almost no CPU.

- **Text and windows are exact.** Every pixel matches the desktop. Scrolling sends only the newly shown part.
- **Video and animations are compressed.** A region that keeps changing, such as a playing video, is sent at lower quality so it stays smooth. It becomes exact again about a second after it stops.
- **Slow connections get a lower rate.** The viewer measures the connection and lowers the quality of moving regions before it lowers the frame rate.
- **The pointer follows whoever drives.** While an agent moves the pointer, you see it move. While you move it, the page shows the desktop's pointer shape under your own pointer.

A tab in the background receives nothing until you return to it.

The keyboard sends the characters you type, so it works with any keyboard layout on your side. Text from an input method editor and pasted text aren't sent yet; use `kumoba exec` or the SDK's `desktop.type` for those.

## Use the noVNC viewer instead

Images built for kumoba 0.4 or earlier don't include the viewer, so `kumoba desktop` opens noVNC for them. To get noVNC on a newer image, ask for it:

```sh
kumoba desktop "$id" --user kumoba --viewer vnc --open
```

The output's `viewer` field says which one the link opens: `stream` or `vnc`.

## Open the live view of a remote machine

If kumobad runs on a server, forward its preview port from your laptop, then open the printed link:

```sh
ssh -N -L 7789:127.0.0.1:7789 my-server &
```

On the server, run `kumoba desktop <id> --user kumoba` and open the `url` it prints in your local browser.

## Drive the desktop from code

```ts
const sandbox = await new Kumoba().create({ image: "default", user: "kumoba" });
await sandbox.desktop.start();
const png = await sandbox.desktop.screenshot();
await sandbox.desktop.click(200, 300);
await sandbox.desktop.type("hello");
```

See the [TypeScript SDK](/typescript) for every desktop method.
