# TypeScript SDK

```sh
npm install kumoba
```

The package needs Node 20 or later and depends only on `zod`. It also installs the [`kumoba` CLI](/cli).

## Connect

```ts
import { Kumoba } from "kumoba";

const kumoba = new Kumoba();
```

With no options it finds the daemon through `~/.kumoba`. To reach another daemon, pass `url` and `token` or set `KUMOBA_URL` and `KUMOBA_TOKEN`:

```ts
const kumoba = new Kumoba({
  url: "unix:///path/to/kumobad.sock",          // or http://127.0.0.1:7788
  token: "...",
  publicUrl: "https://sandboxes.example.com",   // where browsers reach the preview listener
});
```

## Run code once

```ts
const result = await kumoba.run("python3 --version", { image: "default" });
result.ok; result.exitCode; result.stdout; result.stderr; result.timedOut;
```

A string runs through `/bin/sh -c`; an array is the program and its arguments. `run` never throws on a non-zero exit.

## Work in a sandbox

```ts
const sandbox = await kumoba.create({
  image: "default",
  cpu: 2,
  memoryMib: 4096,
  user: "kumoba",                          // default user for commands and files
  network: { allow: ["api.github.com"] },  // "open" | "blocked" | { allow: [...] }
  autoStopMinutes: 15,
  labels: { project: "demo" },
});

const r = await sandbox.run(["python3", "script.py"], {
  cwd: "/workspace",
  env: { PYTHONUNBUFFERED: "1" },
  timeoutMs: 60_000,
  onStdout: (text) => process.stdout.write(text),
});

await sandbox.files.write("/workspace/a.txt", "hi");
await sandbox.files.upload("./local.csv", "/workspace/data.csv");
const text = await sandbox.files.readText("/workspace/a.txt");

await sandbox.stop();
await sandbox.start();
await sandbox.delete();
```

`await using sandbox = await kumoba.create()` deletes the sandbox when the scope ends. It needs TypeScript 5.2 or later, or Node 24 for plain JavaScript.

## Ports, secrets and the desktop

```ts
const link = await sandbox.expose(3000, { ttlSeconds: 600 });

await sandbox.setSecrets([
  { name: "gh", value: token, hosts: ["api.github.com"], envVars: ["GITHUB_TOKEN"] },
]);

await sandbox.desktop.start();
const png = await sandbox.desktop.screenshot();
await sandbox.desktop.click(200, 300);
await sandbox.desktop.type("hello");
await sandbox.desktop.hotkey("ctrl+s");

const view = await sandbox.desktop.liveView({ ttlSeconds: 600 });
console.log(view.url, view.viewer);
```

`expose()` and `liveView()` return the link kumobad sends, on its preview listener. When browsers reach that listener at another address, for example through a reverse proxy, pass `publicUrl` and the links start with it instead. Never point `publicUrl` at the API address: it serves previews only as downloads. `url` is `null` when kumobad has no preview listener and `publicUrl` is not set; `path` is always there.

`liveView()` returns a link to the [live view](/guide/desktop) and which viewer it opens: `stream`, kumoba's own, or `vnc`, the noVNC bridge that older images have. On a macOS sandbox `liveView()` is the one `desktop` method that works: kumobad reads the screen on the host, and there is no desktop helper inside macOS for the others.

## Find sandboxes and read the access log

```ts
const sandboxes = await kumoba.list({ labels: { project: "demo" } });
const { sandboxes: newest, truncated } = await kumoba.listPage({ limit: 20 });
const one = await kumoba.get(id);

for await (const entry of kumoba.accessLog({ since: new Date(Date.now() - 3_600_000) })) {
  console.log(entry.sandboxId, entry.host, entry.decision);
}
```

`list()` without `limit` returns every matching sandbox, newest first, so a cleanup loop over it reaches them all. If the daemon still cuts the list it throws a `KumobaError` with code `list_truncated` instead of returning part of it. `list({ limit })` returns at most `limit` sandboxes and does not say whether more matched. `listPage({ labels, limit })` returns `{ sandboxes, truncated }`, at most `limit` sandboxes (the daemon's default of 1000 when `limit` is left out) and whether more matched.

`list()` and `listPage()` throw a `KumobaError` with code `invalid_request`, before asking the daemon, when a `labels` key is empty or contains `:`, since a filter cannot express it, or when `limit` is not a whole number of 0 or more.

## Errors

Failures throw `KumobaError` with an HTTP `status` and a `code`, such as `not_found` or `unreachable`.

`files.read()`, `files.readText()`, `files.stream()` and `files.download()` compare the bytes received with the file size the daemon reports. A download that ends early, for example because the sandbox stopped mid-transfer, throws a `KumobaError` with code `protocol` and `retryable: true` instead of returning a short file. A stream emits it as an `error`.
