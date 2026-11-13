# Open the desktop live view

The `default` image includes an xfce desktop with Chromium. You can watch it and use it from your browser, while an agent drives it through screenshots and input.

Before you begin:

- [Install kumoba and start `kumobad`](/getting-started).
- Make sure port 7789 on your machine is free. kumobad serves browser links there.

## Open the live view

1. Create and start a sandbox from the `default` image:

   ```sh
   id=$(kumoba create --image default -q)
   kumoba start "$id"
   ```

2. Start the desktop and open it in your browser:

   ```sh
   kumoba desktop "$id" --user kumoba --open
   ```

   The desktop appears with keyboard and mouse working. **Applications → Terminal Emulator** gives you a shell inside the sandbox.

The link expires after one hour. Run the same command again for a new one. Without `--open`, the command prints the link as `url`.

## Open the live view of a remote machine

If kumobad runs on a server, forward its preview port from your laptop, then open the printed link:

```sh
ssh -N -L 7789:127.0.0.1:7789 my-server &
```

On the server, run `kumoba desktop <id> --user kumoba` and open the `url` it prints in your local browser.

## Drive the desktop from code

```ts
const sandbox = await new Kumoba().create({ image: "default", user: "kumoba" });
await sandbox.desktop.start();
const png = await sandbox.desktop.screenshot();
await sandbox.desktop.click(200, 300);
await sandbox.desktop.type("hello");
```

See the [TypeScript SDK](/typescript) for every desktop method.
