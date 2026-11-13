# Run code once

Use `kumoba run` when you want an answer, not a machine: it creates a sandbox, runs one command, prints its output and deletes the sandbox. Nothing is left behind.

Before you begin, [install kumoba and start `kumobad`](/getting-started).

1. Run a command:

   ```sh
   kumoba run -- python3 -c 'print("hello from a fresh VM")'
   ```

   Everything after `--` is the program and its arguments. A bare name such as `python3` is looked up on the guest's `PATH`.

2. Check the exit status. `kumoba run` exits with the command's own status, so scripts and agents can branch on it:

   ```sh
   kumoba run -- sh -c 'exit 7'; echo $?      # 7
   ```

## Run without network access

By default a sandbox can reach the public internet. To run untrusted code with no network at all:

```sh
kumoba run --network.mode blocked -- python3 untrusted.py
```

To allow only some hosts, list them. `*.example.com` matches subdomains:

```sh
kumoba run --network.mode allowlist --network.hosts example.com -- curl -sS https://example.com
```

## Limit time and resources

```sh
kumoba run --timeout-ms 30000 --cpu 1 --memory-mib 1024 -- python3 job.py
```

A command that runs past its timeout is killed and `kumoba run` exits with status 124.

## From code

```ts
import { Kumoba } from "kumoba";

const result = await new Kumoba().run("python3 --version");
console.log(result.exitCode, result.stdout);
```

## Next steps

- To keep a sandbox between commands, see [Work in a sandbox](/guide/sandboxes).
- For every option, run `kumoba run --help` or see the [CLI reference](/cli).
