# Give a sandbox a token without revealing it

A secret lets code in a sandbox call an API without ever seeing the credential. The guest gets a placeholder in an environment variable, and kumobad swaps in the real value only in HTTPS requests to the hosts you allow.

Before you begin, have a started sandbox in `$id` with network mode `open` or `allowlist`.

1. Set the secret:

   ```sh
   kumoba set-secrets "$id" secrets='[{"name":"gh","value":"ghp_your_token","hosts":["api.github.com"],"envVars":["GITHUB_TOKEN"]}]'
   ```

   To keep the token out of your shell history, put the JSON in a file and pass `secrets=@secrets.json`.

2. Use it from the sandbox like a normal token:

   ```sh
   kumoba exec "$id" -- sh -c 'curl -sS -H "Authorization: Bearer $GITHUB_TOKEN" https://api.github.com/user'
   ```

   Inside the guest, `$GITHUB_TOKEN` holds a placeholder such as `kmb_secret_…`. The request to `api.github.com` leaves kumobad with the real token. A request to any other host carries only the placeholder.

`set-secrets` replaces the sandbox's whole set; `secrets='[]'` removes them. `kumoba delete-secrets --names gh` removes a secret from every sandbox.

[How secrets work](/secrets) explains the guarantees and limits.
