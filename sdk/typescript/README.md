# kumoba

[![npm](https://img.shields.io/npm/v/kumoba)](https://www.npmjs.com/package/kumoba)
[![CI](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml/badge.svg)](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml)
[![License](https://img.shields.io/npm/l/kumoba)](https://github.com/Parassharmaa/kumoba/blob/main/LICENSE)

TypeScript SDK and CLI for [kumoba](https://github.com/Parassharmaa/kumoba): fast sandboxes for AI agents, each a real VM that boots in under a second. Talks to a `kumobad` daemon over a Unix socket or loopback TCP. Depends only on `zod`; needs Node 20.3 or later.

> **Alpha.** APIs and formats may change between 0.x releases. Docs: [parassharmaa.github.io/kumoba](https://parassharmaa.github.io/kumoba/).

```sh
npm install kumoba
```

You also need a running `kumobad`. The one-line installer puts it and a guest image in `~/.kumoba`:

```sh
curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
~/.kumoba/bin/kumobad
```

```ts
import { Kumoba } from "kumoba";

const kumoba = new Kumoba();

await using sandbox = await kumoba.create({
  image: "default",
  network: { allow: ["api.github.com"] }, // "open" | "blocked" | { allow }
});

const result = await sandbox.run("python3 -c 'print(21 * 2)'");
console.log(result.stdout); // "42\n"

const once = await kumoba.run("uname -a"); // a fresh VM, deleted when the command ends

await sandbox.files.write("/workspace/notes.md", "# hello");
const link = await sandbox.expose(8080);
```

## CLI

The package also installs the `kumoba` command, the same CLI as the native one that ships with the daemon. It builds its commands from the daemon's catalog, so `kumoba` lists exactly what your daemon supports:

```sh
npx kumoba run -- python3 -c 'print(1)'
npx kumoba create --image default -q
npx kumoba exec <id> -- ls /workspace
npx kumoba desktop <id> --user kumoba --open
```

See [docs/cli.md](https://github.com/Parassharmaa/kumoba/blob/main/docs/cli.md).

## Live view in your own page

`kumoba/view` shows the live view of a desktop in a canvas of your page, for a sandbox or any machine running `kumoba-view`. It runs in the browser, with the decoder built in:

```ts
import { connectViewer } from "kumoba/view";

const viewer = connectViewer(canvas, "wss://desktop.example/stream?token=change-me");
addEventListener("keydown", (event) => viewer.key(event));
addEventListener("keyup", (event) => viewer.key(event));
```

See [Show it in your own page](https://parassharmaa.github.io/kumoba/guide/live-view-anywhere#show-it-in-your-own-page).

## Connecting

`new Kumoba()` with no options finds a zero-config `kumobad` through `~/.kumoba/kumobad.sock` and `~/.kumoba/token`. Each setting can be passed in or set in the environment:

| Option | Environment | Default |
| --- | --- | --- |
| `url` | `KUMOBA_URL` | `unix://$KUMOBA_HOME/kumobad.sock`; `http://127.0.0.1:<port>` also works |
| `token` | `KUMOBA_TOKEN` or `KUMOBA_TOKEN_FILE` | `$KUMOBA_HOME/token` |
| `publicUrl` | `KUMOBA_PUBLIC_URL` | the link `kumobad` returns; set it to where browsers reach the preview listener |
| `defaultUser` | `KUMOBA_USER` | the image's default user |
| | `KUMOBA_HOME` | `~/.kumoba` |

## What a sandbox gives you

- `run(command, { cwd, env, timeoutMs, onStdout })`: `/bin/sh -c` for a string, argv for an array. Never throws on a non-zero exit; check `result.ok`.
- `files.write / readText / read / stream / upload / list / remove`
- `setSecrets([...])`: values never enter the guest; they are injected into HTTPS requests to the hosts you name.
- `expose(port, { ttlSeconds })`: a signed, expiring link through `kumobad` to a port in the VM.
- `desktop.start / screenshot / click / type / hotkey` on images with the desktop installed, and `desktop.liveView()` for a link that shows the desktop in a browser. `liveView()` also works on macOS sandboxes.
- `start / stop / delete / update({ network })`, plus `kumoba.list` (every match), `kumoba.listPage` (a bounded page and whether more matched), `kumoba.get`, `kumoba.find` and `kumoba.accessLog()` for every proxied connection.

`await using` deletes the sandbox when the scope ends; it needs TypeScript 5.2 or later, or Node 24 or later for plain JavaScript. On older runtimes call `sandbox.delete()` in a `finally`. Errors are `KumobaError` with `code`, `status` and `retryable`. Every call takes `signal` and `timeoutMs` in its options; a call cancelled through its `signal` fails with code `aborted`, which is not retryable, and one that runs past `timeoutMs` fails with code `timeout`, which is. Neither is set by default.

The SDK version matches the `kumobad` release it was tested against; see the [changelog](https://github.com/Parassharmaa/kumoba/blob/main/CHANGELOG.md).
