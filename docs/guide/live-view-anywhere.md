# Use the live view in any container

kumoba's live view engine, `kumoba-view`, also works on its own, as a replacement for VNC on any Linux desktop with an X display: a Docker container, a VM, or another company's sandbox. It is one static binary with no dependencies. It serves the viewer page itself, so the browser needs nothing installed, and there is no websockify or noVNC to run next to it.

## Get it

Releases from 0.6.3 on have the engine for Linux on x86_64 and arm64, with its checksum in `SHA256SUMS`:

```sh
arch=$(uname -m | sed 's/aarch64/arm64/')
curl -fsSL -o /usr/local/bin/kumoba-view \
  "https://github.com/Parassharmaa/kumoba/releases/latest/download/kumoba-view-linux-$arch"
chmod +x /usr/local/bin/kumoba-view
```

## Run it

Start it next to the X server, with `DISPLAY` naming the display to show:

```sh
DISPLAY=:0 kumoba-view --listen 0.0.0.0:6090
```

It prints the address to open, ending in `?token=` and a token it made up. When it doesn't run in a terminal, for example under `docker run -d`, it writes the token to `~/.kumoba-view-token-6090` (the port at the end) instead of printing it. Under `docker run -t` it does print it, and the token stays in `docker logs`:

```text
kumoba-view listening on 0.0.0.0:6090; open http://<this host>:6090/?token=4f0c…
```

Open that address in your browser, with the host name or port you reach the machine at. Keyboard, mouse and scrolling work as described in [the live view guide](/guide/desktop#type-paste-and-copy). Paste and copy work in apps that use the usual clipboard shortcuts; plain `xterm`, as in the Docker example below, doesn't.

- **The token.** Anyone with the token can see the screen and use the keyboard and mouse. Set `KUMOBA_VIEW_TOKEN`, or pass `--token-file` with a file that holds it, to choose it yourself, for example to put it in a link you build. A token is letters, digits, `.`, `_`, `~` and `-`. The engine has no option to take it on its command line, where other users of the machine could see it, and it doesn't print a token you choose. A token you choose is needed on loopback too. Listening on `127.0.0.1` needs no token, because only that machine can connect.
- **Encryption.** The engine serves plain HTTP. To reach it over a network you don't trust, put it behind an HTTPS reverse proxy that forwards WebSocket upgrades. Browsers also allow paste and copy only on `https` pages or on `localhost`.
- **What it needs.** An X server on the same machine at depth 24, with the MIT-SHM, DAMAGE, XFIXES and XTEST extensions. Xvfb, Xorg and Xvnc all have them; start Xvfb with a screen like `1920x1080x24`, since `x16` is not shown. It shows the whole screen of that display, which can be at most 4096 pixels wide and 4096 tall, so a wider multi-monitor Xorg is not shown either. When the engine cannot show the display, the viewer page says why.
- **Options.** `--fps` sets the most frames a second (60 by default) and `--max-mbps` the most bandwidth (30 Mbit/s by default). `kumoba-view --help` lists them.

## Try it in Docker

This image runs a small desktop, Xvfb with the Openbox window manager and a terminal, and shows it with the live view:

```dockerfile
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends xvfb openbox xterm curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
RUN arch=$(uname -m | sed 's/aarch64/arm64/') \
 && curl -fsSL -o /usr/local/bin/kumoba-view \
    "https://github.com/Parassharmaa/kumoba/releases/latest/download/kumoba-view-linux-$arch" \
 && chmod +x /usr/local/bin/kumoba-view
ENV DISPLAY=:0
CMD Xvfb :0 -screen 0 1280x800x24 & while [ ! -e /tmp/.X11-unix/X0 ]; do sleep 0.1; done; openbox & xterm & exec kumoba-view --listen 0.0.0.0:6090
```

```sh
docker build -t desktop .
docker run --rm --init -p 6090:6090 -e KUMOBA_VIEW_TOKEN=change-me desktop
```

Then open `http://localhost:6090/?token=change-me`. `--init` lets Ctrl+C and `docker stop` end the container at once. Install your own desktop and apps in place of Openbox and xterm, for example `xfce4` or a browser.

## Show it in your own page

The viewer page is one way to watch the stream. To put the desktop inside your own web app, use the browser client from the `kumoba` npm package. It is the client the viewer page runs, with the decoder built in, so there is no file to host next to it:

```sh
npm install kumoba
```

```ts
import { connectViewer } from "kumoba/view";

const canvas = document.querySelector("canvas")!;
const viewer = connectViewer(canvas, "wss://desktop.example/stream?token=change-me", {
  onConnect: () => console.log("the first frame is on screen"),
  onClose: ({ connected, error }) => console.log("the stream ended", connected, error),
  onClipboard: (text) => navigator.clipboard.writeText(text),
});
addEventListener("keydown", (event) => viewer.key(event));
addEventListener("keyup", (event) => viewer.key(event));
```

- **Versions.** Use the `kumoba` package from the same release as the engine. The stream format can change between releases.
- **Size.** The canvas takes the desktop's size in pixels. Size it with CSS; with `object-fit: contain` it keeps its shape and pointer positions still land where you point. A press in the empty bars around the desktop belongs to your page and is not sent.
- **Input.** Mouse buttons and the wheel come from the canvas, keys from what you pass to `viewer.key`. `typeText` types text, such as what an input method commits, and `paste` puts text on the desktop's clipboard and presses the paste shortcut. `releaseHeldInput` lets go of keys and buttons still held, for example when your page loses focus. While in control, the canvas sets `touch-action: none`, so a touch drag drags on the desktop instead of scrolling your page; your own value comes back when you watch only or close.
- **When it ends.** `onClose` gets `connected`, whether a frame was on screen, and an `error` when there is a reason. When the engine cannot show the desktop, such as a display at depth 16 or one over 4096 pixels, the error is the engine's reason. Without an `error`, the connection dropped; reconnect to go on.
- **Watching only.** `viewOnly: true` shows the stream and sends no input; `setViewOnly` changes that later.
- **Content Security Policy.** The decoder is WebAssembly, so a page with a policy needs `'wasm-unsafe-eval'` in `script-src`.
