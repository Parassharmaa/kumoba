# Configuration

kumoba works without configuration. Each setting below overrides one default.

## Daemon (`kumobad`)

Set these in kumobad's environment. `kumobad --help` prints the same list.

| Variable | Default | Meaning |
| --- | --- | --- |
| `KUMOBA_HOME` | `~/.kumoba` | Base directory for the defaults below |
| `KUMOBAD_IMAGES` | every `$KUMOBA_HOME/images/*.raw` | Images to serve, as `name=/path/to/image.raw,...` |
| `KUMOBAD_STATE_DIR` | `$KUMOBA_HOME/state` | Sandbox disks, metadata and keys |
| `KUMOBAD_TOKEN_FILE` | `$KUMOBA_HOME/token`, created on first start | Bearer token, 32 bytes or more |
| `KUMOBAD_WORKER` | next to `kumobad` | The `kumoba-worker` binary |
| `KUMOBAD_SOCKET` | `$KUMOBA_HOME/kumobad.sock` unless `KUMOBAD_LISTEN` is set | Unix socket for the API, mode 0600 |
| `KUMOBAD_LISTEN` | `127.0.0.1:7788` when neither it nor `KUMOBAD_SOCKET` is set | Loopback address for the API |
| `KUMOBAD_PREVIEW_LISTEN` | `127.0.0.1:7789` when nothing else is set | Loopback address that serves previews and the desktop to browsers |
| `KUMOBAD_PREVIEW_URL` | `http://<preview listen address>` | Public base URL of that listener, behind a reverse proxy |
| `KUMOBAD_MAX_RUNNING` | `4` | Sandboxes running at once |
| `KUMOBAD_ALLOW_PRIVATE_HOSTS` | none | Intranet host names and IPv4 CIDRs sandboxes may reach |
| `KUMOBAD_UPSTREAM_PROXY` | none | `http://[user:pass@]host[:port]` proxy for outbound traffic |
| `KUMOBAD_NO_PROXY` | none | Host names reached directly instead of through the proxy |
| `KUMOBAD_UPSTREAM_PROXY_CONNECT` | `address` | What the proxy `CONNECT` names: the checked `address`, or the `host` name |
| `KUMOBAD_LIBKRUN`, `KUMOBAD_LIBKRUNFW` | `../lib` next to `kumobad`, then the system | libkrun and its firmware |

## Clients (CLI and SDK)

| Variable | Default | Meaning |
| --- | --- | --- |
| `KUMOBA_URL` | `unix://$KUMOBA_HOME/kumobad.sock` | The daemon: `unix:///path` or `http://127.0.0.1:port` |
| `KUMOBA_TOKEN` | contents of `KUMOBA_TOKEN_FILE` | Bearer token |
| `KUMOBA_TOKEN_FILE` | `$KUMOBA_HOME/token` | File holding the token |
| `KUMOBA_PUBLIC_URL` | none | SDK only: base URL for `expose()` links when the daemon doesn't return one |
| `KUMOBA_USER` | the image's default user | SDK only: guest user for commands and files |

## Installer (`install.sh`)

| Variable | Default | Meaning |
| --- | --- | --- |
| `KUMOBA_VERSION` | the latest release | Release to install, such as `0.2.2` |
| `KUMOBA_HOME` | `~/.kumoba` | Install directory |
| `KUMOBA_IMAGE` | `default` | `default`, `test` or `none` |
