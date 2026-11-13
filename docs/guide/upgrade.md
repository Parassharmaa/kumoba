# Upgrade or uninstall

## Upgrade

1. Stop kumobad: press Ctrl-C in its terminal, or run `pkill -x kumobad`.
2. Run the installer again:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
   ```

   It replaces the binaries and downloads the new image when the version changed. Your sandboxes stay.
3. Start kumobad again.
4. Update the SDK in your projects with `npm install kumoba@latest`.

Read the [changelog](/changelog) first. While kumoba is alpha, a minor version (0.2 to 0.3) may change the API, and a protocol change needs new images.

## Install a specific version

```sh
curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | KUMOBA_VERSION=0.2.2 bash
```

## Uninstall

1. Stop kumobad.
2. Remove its directory, which holds the binaries, images, sandboxes and token:

   ```sh
   rm -rf ~/.kumoba
   ```

3. Remove the `~/.kumoba/bin` line from your shell profile.
