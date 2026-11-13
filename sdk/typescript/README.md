# kumoba

[![npm](https://img.shields.io/npm/v/kumoba)](https://www.npmjs.com/package/kumoba)
[![CI](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml/badge.svg)](https://github.com/Parassharmaa/kumoba/actions/workflows/ci.yml)
[![License](https://img.shields.io/npm/l/kumoba)](https://github.com/Parassharmaa/kumoba/blob/main/LICENSE)

TypeScript SDK for [kumoba](https://github.com/Parassharmaa/kumoba): local microVM sandboxes for AI agents and untrusted code. Talks to a `kumobad` daemon over a Unix socket or loopback TCP. Depends only on `zod`; needs Node 20 or later.

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

await sandbox.files.write("/workspace/notes.md", "# hello");
const link = await sandbox.expose(8080);
```

## Connecting

`new Kumoba()` with no options finds a zero-config `kumobad` through `~/.kumoba/kumobad.sock` and `~/.kumoba/token`. Each setting can be passed in or set in the environment:

| Option | Environment | Default |
| --- | --- | --- |
| `url` | `KUMOBA_URL` | `unix://$KUMOBA_HOME/kumobad.sock`; `http://127.0.0.1:<port>` also works |
| `token` | `KUMOBA_TOKEN` or `KUMOBA_TOKEN_FILE` | `$KUMOBA_HOME/token` |
| `publicUrl` | `KUMOBA_PUBLIC_URL` | the `http://` URL, for full `expose()` links |
| `defaultUser` | `KUMOBA_USER` | the image's default user |
| | `KUMOBA_HOME` | `~/.kumoba` |

## What a sandbox gives you

- `run(command, { cwd, env, timeoutMs, onStdout })`: `/bin/sh -c` for a string, argv for an array. Never throws on a non-zero exit; check `result.ok`.
- `files.write / readText / read / stream / upload / list / remove`
- `setSecrets([...])`: values never enter the guest; they are injected into HTTPS requests to the hosts you name.
- `expose(port, { ttlSeconds })`: a signed, expiring link through `kumobad` to a port in the VM.
- `desktop.start / screenshot / click / type / hotkey / liveView` on images with the desktop installed.
- `start / stop / delete / update({ network })`, plus `kumoba.list`, `kumoba.get`, `kumoba.find` and `kumoba.accessLog()` for every proxied connection.

`await using` deletes the sandbox when the scope ends. Errors are `KumobaError` with `status` and `code`.

The SDK version matches the `kumobad` release it was tested against; see the [changelog](https://github.com/Parassharmaa/kumoba/blob/main/CHANGELOG.md).
