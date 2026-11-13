# Work in a sandbox

Create a sandbox when you need state between commands: installed packages, files, a running server. A sandbox keeps its disk until you delete it, across stops and daemon restarts.

Before you begin, [install kumoba and start `kumobad`](/getting-started).

## Create and start a sandbox

1. Create a sandbox from the `default` image. A label makes it easy to find again:

   ```sh
   id=$(kumoba create --image default --labels project=demo -q)
   ```

   `-q` prints only the id. A new sandbox is stopped.

2. Start it:

   ```sh
   kumoba start "$id"
   ```

3. Find it later by its label:

   ```sh
   kumoba list --label project:demo
   ```

## Run commands

For an interactive terminal, use [`kumoba shell`](/guide/shell). For single commands:

```sh
kumoba exec "$id" -- pip install requests
kumoba exec "$id" --cwd /workspace --env DEBUG=1 -- python3 app.py
kumoba exec "$id" --user kumoba -- whoami
```

Commands run as root unless you pass `--user`. The `default` image has a `kumoba` user, which owns the desktop.

::: tip Background processes
`exec` stops every process the command started when the command ends. To leave a server running, start it with `setsid`:

```sh
kumoba exec "$id" -- sh -c 'cd /workspace && setsid python3 -m http.server 8000 > /dev/null 2>&1 < /dev/null &'
```
:::

## Move files in and out

```sh
kumoba write-file "$id" --path /workspace/data.csv content=@./data.csv
echo 'print("hi")' | kumoba write-file "$id" --path /workspace/hi.py
kumoba list-files "$id" --path /workspace
kumoba read-file "$id" --path /workspace/out.png -o out.png
```

`content=@file` reads a local file. Without it, `write-file` reads stdin.

## Stop, restart and delete

```sh
kumoba stop "$id"      # the VM shuts down, the disk stays
kumoba start "$id"     # boots again with the same disk
kumoba delete "$id"    # removes the sandbox and its disk
```

A sandbox created with the CLI keeps running until you stop it. Pass `--auto-stop-minutes 15` to stop it after 15 idle minutes (the SDK's default), and `--auto-delete-minutes 60` to have kumobad delete it an hour after it stops.

To delete every sandbox:

```sh
for id in $(kumoba list -q); do kumoba delete "$id"; done
```
