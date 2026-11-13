# Control network access

A sandbox has no network device of its own. Its traffic goes through a proxy in kumobad, which enforces the sandbox's policy and logs every connection.

| Mode | The sandbox can reach |
| --- | --- |
| `open` (default) | The public internet |
| `allowlist` | Only the listed hosts, on ports 80 and 443. `*.example.com` matches subdomains |
| `blocked` | Nothing |

Private, link-local and cloud metadata addresses are never reachable, whatever the mode.

## Set the policy when you create a sandbox

```sh
kumoba create --image default --network.mode allowlist --network.hosts pypi.org,files.pythonhosted.org -q
```

`kumoba run` takes the same options.

## Change the policy of a sandbox

Stop the sandbox first. A running sandbox keeps the policy it booted with.

```sh
kumoba stop "$id"
kumoba update "$id" --network.mode blocked
kumoba start "$id"
```

A sandbox with secrets can't switch to `blocked`, because secrets are injected by the HTTPS proxy. Remove its secrets first with `kumoba set-secrets "$id" secrets='[]'`.

## See what a sandbox connected to

```sh
kumoba access-log
```

Each entry has the sandbox, host, port, whether the connection was allowed or denied, and the bytes in each direction. By default you get the last two hours; `--since-ms` and `--until-ms` choose another window.

## Reach an intranet or go through a corporate proxy

These are daemon settings, set in kumobad's environment:

- `KUMOBAD_ALLOW_PRIVATE_HOSTS=gitlab.corp.example,10.20.0.0/16` lets sandboxes reach those private hosts and ranges. Loopback, link-local and the host's own addresses stay blocked.
- `KUMOBAD_UPSTREAM_PROXY=http://proxy.corp.example:3128` sends outbound traffic through your proxy, except hosts in `KUMOBAD_NO_PROXY`.

See [Configuration](/configuration) for every setting.
