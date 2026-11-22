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

A macOS sandbox has a live view too, opened with the same command. [See the screen](/guide/macos#see-the-screen) lists what differs there.

## What the live view does

The live view is kumoba's own viewer. It sends the desktop to your browser only when the screen changes, so a still desktop uses no bandwidth and almost no CPU.

- **Text and windows are exact.** Every pixel matches the desktop. Scrolling and dragging a window send only the newly shown part.
- **Video and animations are compressed.** A region that keeps changing, such as a playing video, is sent at lower quality so it stays smooth. It becomes exact again about a second after it stops.
- **Slow connections get a lower rate.** The viewer measures the connection and lowers the quality of moving regions before it lowers the frame rate.
- **The pointer follows whoever drives.** While an agent moves the pointer, you see it move. While you move it, the page shows the desktop's pointer shape under your own pointer.

A tab in the background receives nothing until you return to it.

If the desktop changes size, the live view follows.

## Type, paste and copy

- **Typing.** The keyboard sends the characters you type, so it works with any keyboard layout on your side. Text from an input method editor, such as Japanese, arrives when you confirm it. A character that the sandbox's own keyboard layout lacks takes a moment longer the first time.
- **Paste.** Press Ctrl+V, or Cmd+V on a Mac. Your clipboard's text goes to the sandbox's clipboard, and the live view pastes it. It uses the terminal's own paste shortcut when a terminal has the focus.
- **Copy.** On a Mac, press Cmd+C or Cmd+X; the live view sends the copy shortcut of the app that has the focus. On other systems the keys go to the sandbox as you press them: Ctrl+C in most apps, Ctrl+Shift+C in a terminal. The copied text then goes to your clipboard.

In a macOS sandbox the copy and paste shortcuts are always sent as Cmd+C, Cmd+X and Cmd+V, whatever system you are on. From Windows or Linux that means Ctrl+C, Ctrl+X and Ctrl+V never reach the sandbox as Control keys, so Control+C can't stop a command in Terminal from there.

Text leaves the sandbox only in the two seconds after you press a copy shortcut in the live view. Your browser must allow the page to write to the clipboard, which needs `https` or `localhost`.

Only text is exchanged, up to 200,000 bytes.

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

`await sandbox.desktop.liveView()` returns the live view's link, so code can hand a person the page to watch. See the [TypeScript SDK](/typescript) for every desktop method. These methods drive the desktop of a Linux image; on a macOS sandbox only `liveView()` works.
