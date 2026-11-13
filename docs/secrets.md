# How secrets work

Agents need API keys, but code in a sandbox shouldn't be able to read or leak them. kumoba keeps the values on the host and lets the guest use them without seeing them. To set one up, see [Use a secret token](/guide/secrets).

## How a secret is used

1. The client attaches a secret to a sandbox: a `name`, the `value`, the `hosts` allowed to receive it and the `envVars` that should carry it.
2. The guest agent exports each listed variable with a **placeholder**, a random token that is unique to that secret. `echo $GITHUB_TOKEN` inside the VM prints the placeholder.
3. When a process in the guest makes an HTTPS request to one of the `hosts`, the worker's egress proxy terminates TLS for that connection, replaces the placeholder with the real value in request headers, and forwards the request upstream with the real certificate chain validated.
4. Responses from those hosts are scanned, and any literal occurrence of the secret value is replaced with the placeholder before the bytes reach the guest.

Requests to any other host travel through a plain CONNECT tunnel. They are not decrypted, and because the real value was never sent to those hosts there is nothing to scrub.

## Guarantees and limits

- The guest never holds the value: not in environment variables, files, memory or process listings.
- Values shorter than 8 bytes are rejected, because scrubbing them from responses would cause false positives.
- Placeholders are only substituted in request **headers**. A value in a request body or URL is not rewritten; design clients to send tokens in `authorization` or similar headers.
- Only hosts that have at least one secret scoped to them are TLS-terminated. Everything else keeps end-to-end TLS, so browsers and tools see the origin's certificate.
- The per-runtime CA that signs the intercepted connections is generated when the daemon starts and installed into the guest at boot (`/tmp/kumoba-ca.pem`, `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`, Chromium's NSS store).
- Secrets are held in daemon memory. After a restart, re-attach them with `PUT /v1/sandboxes/{id}/secrets`.

## API

```ts
await sandbox.setSecrets([
  { name: "openai", value: process.env.OPENAI_API_KEY!, hosts: ["api.openai.com"], envVars: ["OPENAI_API_KEY"] },
]);
await kumoba.deleteSecrets(["openai"]);
```

Equivalent HTTP routes are in [http-api.md](http-api.md#secrets).
