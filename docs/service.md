# Run kumobad as a service

On a server, give each setting an explicit path instead of relying on `~/.kumoba`. [Configuration](/configuration) lists every setting.

1. Unpack the release tarball for your platform into `/opt/kumoba`. It contains `bin/` (`kumobad`, `kumoba`, `kumoba-worker`, `kumoba-guest`), `lib/` (libkrun and its firmware), `licenses/` and a `manifest.json` of checksums.
2. Put an image in `/opt/kumoba/images/default.raw`.
3. Create a systemd unit:

   ```ini
   [Service]
   User=kumoba
   Environment=KUMOBAD_STATE_DIR=/var/lib/kumoba/state
   Environment=KUMOBAD_IMAGES=default=/opt/kumoba/images/default.raw
   Environment=KUMOBAD_TOKEN_FILE=/etc/kumoba/token
   Environment=KUMOBAD_SOCKET=/run/kumoba/kumobad.sock
   Environment=KUMOBAD_PREVIEW_LISTEN=127.0.0.1:7789
   Environment=KUMOBAD_PREVIEW_URL=https://previews.example.com
   Environment=KUMOBAD_MAX_RUNNING=4
   ExecStart=/opt/kumoba/bin/kumobad
   ```

4. Put a reverse proxy in front of the preview listener if browsers on other machines need `expose` links or the desktop live view, and set `KUMOBAD_PREVIEW_URL` to its public address. Preview links carry their own signature, so they need no token.

## Intranets and corporate proxies

```ini
Environment=KUMOBAD_ALLOW_PRIVATE_HOSTS=gitlab.corp.example,10.20.0.0/16
Environment=KUMOBAD_UPSTREAM_PROXY=http://proxy.corp.example:3128
Environment=KUMOBAD_NO_PROXY=.corp.example
```

See [Control network access](/guide/network) for what each one does.
