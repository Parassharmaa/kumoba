# kumoba

TypeScript SDK for [kumoba](https://github.com/Parassharmaa/kumoba): local microVM sandboxes for AI agents and untrusted code. Talks to a `kumobad` daemon over a Unix socket or loopback TCP. Depends only on `zod`.

```sh
npm install kumoba
```

```ts
import { Kumoba } from "kumoba";

const kumoba = new Kumoba({
  url: "unix:///path/to/kumobad.sock",   // or http://127.0.0.1:7788
  token: "...",                           // contents of KUMOBAD_TOKEN_FILE
});

await using sandbox = await kumoba.create({
  image: "default",
  user: "kumoba",
  network: { allow: ["api.github.com"] }, // "open" | "blocked" | { allow }
});

const result = await sandbox.run("python3 -c 'print(21 * 2)'");
console.log(result.stdout); // "42\n"

await sandbox.files.write("/workspace/notes.md", "# hello");
const link = await sandbox.expose(8080);
```

`Kumoba.fromEnv()` reads `KUMOBA_URL`, `KUMOBA_TOKEN` and `KUMOBA_PUBLIC_URL`.

What a `Sandbox` gives you:

- `run(command, { cwd, env, timeoutMs, onStdout })`: `/bin/sh -c` for a string, argv for an array. Never throws on a non-zero exit.
- `files.write / readText / read / stream / upload / list / remove`
- `setSecrets([...])`: values never enter the guest; they are injected into HTTPS requests to the hosts you name.
- `expose(port, { ttlSeconds })`: a signed, expiring link through `kumobad` to a port in the VM.
- `desktop.start / screenshot / click / type / hotkey / liveView` on images with the desktop installed.
- `start / stop / delete / update({ network })`, plus `kumoba.list`, `kumoba.get`, `kumoba.find` and `kumoba.accessLog()` for every proxied connection.

Errors are `KumobaError` with `status` and `code`. The SDK version tracks the `kumobad` release it was tested against; see the [changelog](https://github.com/Parassharmaa/kumoba/blob/main/CHANGELOG.md).
