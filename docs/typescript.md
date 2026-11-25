# TypeScript SDK

```sh
npm install kumoba
```

The package needs Node 20.3 or later and depends only on `zod`. It also installs the [`kumoba` CLI](/cli).

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

## Cancel a call or give it a time limit

Every call that asks kumobad for something takes `signal` and `timeoutMs` in its options object, the last argument; the desktop calls with positional arguments, such as `desktop.drag(from, to, button, options)`, take it last too.

```ts
const sandbox = await kumoba.create({ timeoutMs: 600_000 });
const data = await sandbox.files.read("/workspace/out.json", { signal: controller.signal });
await sandbox.desktop.click(200, 300, { timeoutMs: 10_000 });
```

- `signal` aborts the call: it throws a `KumobaError` with code `aborted` and `cause` set to `signal.reason`, and the request to kumobad is closed.
- `timeoutMs` throws a `KumobaError` with code `timeout` when that many milliseconds pass before the call's promise settles. The clock covers every request the call makes, so for `create()` it counts creating the sandbox and the whole boot. It must be above 0 and at most 2147483647; anything else is `invalid_request`.
- Neither is set by default and calls wait as long as kumobad takes. kumobad bounds a boot itself, at 2 minutes for Linux and 5 for macOS.
- `files.stream()` resolves once the download starts, so `timeoutMs` covers only opening it; `signal` still stops it partway, and the stream then emits the `aborted` error. `files.read()`, `files.download()` and `files.write()` count the whole transfer.
- `kumoba.accessLog()` counts its time limit across every page, from the first one to the last.
- When `create()` is aborted or times out while the sandbox boots, the SDK asks kumobad to delete the sandbox before it throws, and waits up to 10 seconds for that. `desktop.screenshot()` and `desktop.accessibilityTree()` remove their temporary file in the guest the same way. When `create()` stops before kumobad has replied to the create request itself, kumobad may have made a sandbox whose id the SDK never received; find it with `list()` and delete it.
- `run()` already takes `signal`. Its `timeoutMs` is different: kumobad kills the command when it passes, and `run()` resolves with `timedOut: true` instead of throwing.

## Errors

Failures throw `KumobaError` with a `code`, a `retryable` flag and an HTTP-style `status`: the daemon's own when it answered with an error, 502 or 503 for a failure the SDK detected in its reply or connection, and 0 when no request was made or the call was aborted or timed out. The exceptions are errors thrown by your own `onStdout` and `onStderr` callbacks, local file errors in `files.upload()` and `files.download()` such as a missing local path, and errors from a stream you pass to `files.write()`; those reach you unchanged.

| `code` | When | `retryable` |
| --- | --- | --- |
| `aborted` | The `signal` you passed was aborted. `cause` is `signal.reason`. | `false` |
| `timeout` | The `timeoutMs` you passed ran out before the call finished. | `true` |
| `unreachable` | kumobad could not be reached, or the connection broke before the reply was complete, for example because kumobad restarted. | `true` |
| `protocol` | kumobad sent something the SDK does not understand, a download ended early, or a command's output ended without an exit status. | `true` when a download or a command's output ended early, otherwise `false` |
| `daemon_error` | kumobad started the request but could not run the command, for example because the guest agent did not answer. | `false` |
| `command_failed` | A shell helper such as `files.mkdir()`, `files.remove()` or a `desktop` method exited non-zero. For `mkdir` and `remove`, `result` holds the command's `RunResult`. | `false` |
| `invalid_request` | The SDK refused the arguments before asking kumobad, such as an empty command. | `false` |

Other codes, such as `not_found`, `capacity` or `guest_error`, are the daemon's own and come with its HTTP status (see the [HTTP API](/http-api)). An error reply without a code is `daemon_error`. Errors from the daemon are retryable when the status is 429 or 5xx.

A `run()` that fails with `unreachable` or `protocol` after kumobad accepted it may have started the command, so retry only commands that are safe to run twice. The same holds for `timeout`: kumobad may have done what the call asked, such as a desktop click, before the time ran out.

`files.read()`, `files.readText()`, `files.stream()` and `files.download()` compare the bytes received with the file size the daemon reports. A download that ends early, for example because the sandbox stopped mid-transfer, throws a `KumobaError` with code `protocol` and `retryable: true` instead of returning a short file. A stream emits it as an `error`.
