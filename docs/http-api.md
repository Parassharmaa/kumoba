# kumobad HTTP API

kumobad speaks HTTP/1.1 on `~/.kumoba/kumobad.sock` and `127.0.0.1:7788`, and serves browser previews on `127.0.0.1:7789` ([Configuration](/configuration) changes these). Every route except `/healthz` and `/v1/preview/...` needs `authorization: Bearer <token>`, where the token is the content of `~/.kumoba/token`.

```sh
curl --unix-socket ~/.kumoba/kumobad.sock -H "authorization: Bearer $(cat ~/.kumoba/token)" http://kumobad/v1/images
```

## Commands

`GET /v1/commands` → `{"version": "0.2.2", "commands": [...]}` describes every route below in machine-readable form: `name`, `summary`, `method`, `path`, the `positional` path arguments, the `rest` argument that takes a trailing list, `input` (`none`, `json` or `bytes`), `output` (`json`, `events` or `bytes`), `in` (whether each argument goes in the path, query, JSON body or raw body) and `inputSchema`, a JSON Schema covering all of them. The `kumoba` CLI is built on this route alone, and the schemas can be handed to a model as tool definitions.

Errors are JSON: `{"error": "<code>", "message": "<detail>"}` with an HTTP status. Codes include `unauthorized` (401), `not_found` (404), `invalid_request` (400), `capacity` (429, `KUMOBAD_MAX_RUNNING` reached) and `guest_error` (422, the command or file operation failed inside the VM).

## Images

`GET /v1/images` → `{"images": ["default", "test"]}`

## Sandboxes

### Create

`POST /v1/sandboxes`

```json
{
  "image": "default",
  "labels": {"project": "demo"},
  "cpu": 2,
  "memoryMib": 4096,
  "autoStopMinutes": 15,
  "autoDeleteMinutes": null,
  "network": {"mode": "allowlist", "hosts": ["api.github.com"]}
}
```

Only `image` is required. `network.mode` is `open`, `allowlist` or `blocked`. In `allowlist` mode `hosts` holds exact host names and `*.domain` wildcards (which match subdomains, not the domain itself), reachable on ports 80 and 443. `autoStopMinutes: 0` disables auto-stop. `autoDeleteMinutes` removes a stopped sandbox after that many minutes; `null` keeps it.

The new sandbox is **stopped**. Returns a summary:

```json
{
  "id": "6a0b...",
  "image": "default",
  "labels": {"project": "demo"},
  "cpu": 2,
  "memoryMib": 4096,
  "autoStopMinutes": 15,
  "autoDeleteMinutes": null,
  "network": {"mode": "allowlist", "hosts": ["api.github.com"]},
  "createdAtMs": 1760000000000,
  "lastActivityAtMs": 1760000000000,
  "state": "stopped"
}
```

### List, get, update, delete

- `GET /v1/sandboxes?label=key:value&label=k2:v2&limit=100` → `{"sandboxes": [...], "truncated": false}`. Every `label` filter must match. Newest first.
- `GET /v1/sandboxes/{id}` → summary.
- `PATCH /v1/sandboxes/{id}` with `{"labels": {...}, "autoStopMinutes": 30, "network": {...}}` (all optional) → summary. `network` can only change while the sandbox is stopped (409 otherwise) and takes effect on the next start; the disk is kept.
- `DELETE /v1/sandboxes/{id}` → `{"deleted": true}`. Stops the VM if needed and removes its disk.

### Start and stop

- `POST /v1/sandboxes/{id}/start` → summary with `"state": "started"`. Boots the VM and waits for the guest agent.
- `POST /v1/sandboxes/{id}/stop` → summary with `"state": "stopped"`. The disk is kept.

Every exec, file or preview call counts as activity for auto-stop.

## Running commands

`POST /v1/sandboxes/{id}/exec`

```json
{
  "argv": ["/bin/sh", "-c", "echo hello"],
  "cwd": "/workspace",
  "env": {"A": "1"},
  "timeoutMs": 600000,
  "user": "kumoba"
}
```

A bare `argv[0]` such as `python3` is looked up on the guest's `PATH` through `/usr/bin/env`; an absolute path runs as is. `user` defaults to root; `cwd` defaults to the user's home. The response is `application/x-ndjson`, one event per line, streamed as the process runs:

```
{"type":"stdout","data":"aGVsbG8K"}
{"type":"stderr","data":"..."}
{"type":"exit","exitCode":0,"timedOut":false}
```

`data` is base64. The final event is either `exit` (`exitCode` is `null` and `timedOut` is `true` when the timeout hit; `signal` is set when a signal ended the process) or `{"type":"error","message":"..."}` when the command could not be started. Closing the connection kills the process group inside the guest.

### One-shot

`POST /v1/run` takes the exec fields plus the create fields (`image`, default `"default"`, `cpu`, `memoryMib`, `labels`, `network`). It creates a sandbox, starts it, streams the command's events exactly like exec, and deletes the sandbox when the command ends or the client disconnects. The stream starts with `{"type":"sandbox","id":"..."}`.

## Shell

`POST /v1/sandboxes/{id}/shell` opens an interactive terminal. Send it with `connection: upgrade` and `upgrade: kumoba-shell.v1`, and a JSON body:

```json
{"argv": [], "cwd": "/workspace", "env": {}, "user": "kumoba", "rows": 40, "cols": 120}
```

An empty `argv` starts the user's login shell. kumobad answers `101 Switching Protocols`; from then on both sides exchange frames of one type byte, a 4-byte big-endian length and a payload:

| Type | Direction | Payload |
| --- | --- | --- |
| 1 data | both | Terminal bytes |
| 2 resize | client to kumobad | Rows and columns, 2 bytes each, big-endian |
| 3 eof | client to kumobad | None; ends input as Ctrl-D does |
| 4 exit | kumobad to client | The shell's exit status, 4-byte signed big-endian; 128 + the signal for a signal |

Closing the connection ends the shell. A request without the upgrade gets `426 upgrade_required`, and a sandbox whose image predates kumoba 0.3 gets `422`.

## Files

- `PUT /v1/sandboxes/{id}/files?path=/abs/path[&user=name]` with the raw bytes as the body → `{"written": <bytes>}`. Parent directories must exist. The file is created `0644` as `user`.
- `GET /v1/sandboxes/{id}/files?path=/abs/path[&user=name]` → the raw bytes, `application/octet-stream`.
- `GET /v1/sandboxes/{id}/files/list?path=/abs/dir[&user=name]` → `{"entries": [{"name": "a.txt", "kind": "file", "size": 5}, ...]}`. `kind` is `file`, `directory` or `other`.

Reads and lists run with the user's permissions when `user` is given.

## Secrets

`PUT /v1/sandboxes/{id}/secrets`

```json
{
  "secrets": [
    {"name": "gh", "value": "ghp_...", "hosts": ["api.github.com"], "envVars": ["GITHUB_TOKEN"]}
  ]
}
```

Replaces the sandbox's secret set. Returns `{"mountedEnvVars": ["GITHUB_TOKEN"]}`. Inside the guest each listed variable holds a placeholder; HTTPS requests to a listed host have the placeholder swapped for the value in request headers, and responses from those hosts are scrubbed of the value. Values must be at least 8 bytes. Sending an empty list detaches everything.

`POST /v1/secrets/delete` with `{"names": ["gh"]}` removes secrets by name from every sandbox → `{"deletedNames": ["gh"]}`.

## Port previews

`POST /v1/sandboxes/{id}/preview` with `{"port": 8080, "ttlSeconds": 600}` → `{"url": "http://127.0.0.1:7789/v1/preview/<token>/", "path": "/v1/preview/<token>/", "expiresAtMs": ...}`. `url` is set when the daemon has a preview listener (`KUMOBAD_PREVIEW_LISTEN`, or `KUMOBAD_PREVIEW_URL` behind a proxy), and is `null` otherwise.

Previews on the preview listener render in a browser: the guest's `content-type`, `content-encoding` and `location` pass through. That listener is a separate origin from the API and answers nothing but previews, so a page from a sandbox cannot reach the API. The same preview path on the API listener is download-only: `content-disposition: attachment`, `content-security-policy: sandbox` and `application/octet-stream`.

## Desktop

`POST /v1/sandboxes/{id}/desktop` with `{"user": "kumoba", "ttlSeconds": 3600}` (both optional) starts Xvfb, xfce, x11vnc and the noVNC bridge in an image that has them, and → `{"url": "http://127.0.0.1:7789/v1/preview/<token>/vnc.html?autoconnect=1&resize=remote&path=...", "path": "...", "expiresAtMs": ..., "processes": {"xvfb": true, ...}}`. Opening `url` shows the desktop live, with keyboard and mouse. An image without a desktop answers 422.

Requests to `GET|POST|... /v1/preview/<token>/<anything>` are relayed to that port inside the guest, including WebSocket upgrades, without a bearer token. The token is an HMAC over the sandbox id, port and expiry, so it cannot be reused for another sandbox or after it expires. Hop-by-hop and host-identifying headers are stripped in both directions, and responses carry `content-disposition: attachment` for types a browser would otherwise render, so a page served from a sandbox cannot script against the daemon's origin.

## Access log

`GET /v1/access-log?sinceMs=<epoch ms>&untilMs=<epoch ms>&limit=<n>[&cursor=<token>]` → `{"entries": [...], "truncated": false, "nextCursor": null}`

Every connection the egress proxy handled, in the order it was recorded (close to completion time), for sandboxes of any state:

```json
{
  "sandboxId": "6a0b...",
  "labels": {"project": "demo"},
  "host": "api.github.com",
  "port": 443,
  "decision": "allowed",
  "bytesIn": 18233,
  "bytesOut": 912,
  "startedAtMs": 1760000000000,
  "completedAtMs": 1760000000412
}
```

`decision` is `denied` for allowlist misses and for attempts at private, link-local or metadata addresses; those carry zero bytes. A sandbox gets at most 10000 records per hour; beyond that, and whenever the daemon cannot keep up, the gap is reported as a `dropped` marker (`"decision": "dropped"`, empty host, port 0, `"dropped": <count>`) rather than lost silently. `bytesIn` is what reached the guest, `bytesOut` what it sent. Defaults: the last two hours, 10000 entries; `limit` caps at 50000. When `truncated` is true, repeat the call with `cursor=<nextCursor>` and the same window to get the next page; the cursor is a position in the log, so pages never skip or repeat entries. The cap is 10000 allowed and 10000 denied records per sandbox per hour, so a flood of permitted traffic cannot push denials out of the log. kumobad keeps 72 hours of entries as hourly files under `<state>/access`.

## Health

`GET /healthz` → `{"ok": true}`, no authentication.
