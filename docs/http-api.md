# kumobad HTTP API

`kumobad` speaks HTTP/1.1 over a Unix socket (`KUMOBAD_SOCKET`) and/or loopback TCP (`KUMOBAD_LISTEN`). Every route except `/healthz` and `/v1/preview/...` requires `authorization: Bearer <token>`, where the token is the content of `KUMOBAD_TOKEN_FILE`.

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

Only `image` is required. `network.mode` is `open`, `allowlist` or `blocked`. `autoStopMinutes: 0` disables auto-stop. `autoDeleteMinutes` removes a stopped sandbox after that many minutes; `null` keeps it.

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
- `PATCH /v1/sandboxes/{id}` with `{"labels": {...}, "autoStopMinutes": 30}` (both optional) → summary.
- `DELETE /v1/sandboxes/{id}` → `{"deleted": true}`. Stops the VM if needed and removes its disk.

### Start and stop

- `POST /v1/sandboxes/{id}/start` → summary with `"state": "started"`. Boots the VM and waits for the guest agent.
- `POST /v1/sandboxes/{id}/stop` → summary with `"state": "stopped"`. The disk is kept.

Every exec, file or preview call counts as activity for auto-stop.

## Commands

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

`argv[0]` must be an absolute path. `user` defaults to root; `cwd` defaults to the user's home. The response is `application/x-ndjson`, one event per line, streamed as the process runs:

```
{"type":"stdout","data":"aGVsbG8K"}
{"type":"stderr","data":"..."}
{"type":"exit","exitCode":0,"timedOut":false}
```

`data` is base64. The final event is either `exit` (`exitCode` is `null` and `timedOut` is `true` when the timeout hit; `signal` is set when a signal ended the process) or `{"type":"error","message":"..."}` when the command could not be started. Closing the connection kills the process group inside the guest.

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

`POST /v1/sandboxes/{id}/preview` with `{"port": 8080, "ttlSeconds": 600}` → `{"path": "/v1/preview/<token>/", "expiresAtMs": ...}`.

Requests to `GET|POST|... /v1/preview/<token>/<anything>` are relayed to that port inside the guest, including WebSocket upgrades, without a bearer token. The token is an HMAC over the sandbox id, port and expiry, so it cannot be reused for another sandbox or after it expires. Hop-by-hop and host-identifying headers are stripped in both directions, and responses carry `content-disposition: attachment` for types a browser would otherwise render, so a page served from a sandbox cannot script against the daemon's origin.

## Health

`GET /healthz` → `{"ok": true}`, no authentication.
