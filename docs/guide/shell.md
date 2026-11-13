# Open a shell in a sandbox

`kumoba shell` gives you an interactive terminal inside a sandbox, like `docker exec -it`. Arrow keys, tab completion, Ctrl-C and full-screen terminal programs work, and the terminal follows your window size.

Before you begin, [install kumoba and start `kumobad`](/getting-started), and have a started sandbox in `$id`.

1. Open a shell:

   ```sh
   kumoba shell "$id"
   ```

   You get the login shell of the guest user, in `/workspace`. Commands see the same network policy and secrets as `kumoba exec`.

2. Leave with `exit` or Ctrl-D. `kumoba shell` exits with the shell's own status.

## Choose the user, directory or program

```sh
kumoba shell "$id" --user kumoba                 # log in as the image's kumoba user
kumoba shell "$id" --cwd /tmp                    # start somewhere else
kumoba shell "$id" --env DEBUG=1                 # add environment variables
kumoba shell "$id" -- python3                    # run a program instead of a shell
```

## Use it from a script

When stdin isn't a terminal, `kumoba shell` sends the input as typed lines and ends the session when the input ends:

```sh
printf 'cd /workspace && ls\nexit\n' | kumoba shell "$id"
```

For scripts, `kumoba exec` is usually the better choice: it keeps stdout and stderr apart and doesn't echo your input.

## Requirements

The sandbox's image must come from kumoba 0.3 or later. An older image answers with `this sandbox's image has no shell support`; [upgrade](/guide/upgrade) to get a new image. The CLI must be 0.3 or later too.
