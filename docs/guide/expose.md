# Open a web app from a sandbox

`kumoba expose` gives a port inside a sandbox a signed link that your browser can open. The link expires, and it works only for that sandbox and port.

Before you begin, [install kumoba and start `kumobad`](/getting-started), and have a started sandbox in `$id`.

1. Start a server in the sandbox. Use `setsid` so it keeps running after the command returns:

   ```sh
   kumoba exec "$id" --user kumoba -- sh -c 'cd /workspace && setsid python3 -m http.server 8000 > /dev/null 2>&1 < /dev/null &'
   ```

2. Open it:

   ```sh
   kumoba expose "$id" --port 8000 --ttl-seconds 600 --open
   ```

The link lasts `--ttl-seconds`, up to an hour. Links are served on kumobad's preview listener, `127.0.0.1:7789` by default, which is a separate origin from the API so a page from the sandbox can't call it. Redirects from the app stay inside the link.

To share links beyond your machine, put a reverse proxy in front of the preview listener and set `KUMOBAD_PREVIEW_URL` to its public address. See [Running as a service](/service).
