# kumobad

The kumoba daemon. Serves microVM sandboxes over an HTTP API on a Unix socket or loopback TCP, and ships `kumoba-worker`, the per-VM process that loads libkrun and boots one sandbox.

```sh
cargo install kumobad
kumobad --help
```

Hosts need libkrun and libkrunfw: Homebrew `slp/krun` on Apple silicon, or the patched build from `scripts/prepare-linux-host.sh` on Linux with `/dev/kvm`. Release tarballs on GitHub bundle both binaries with the libraries.

The HTTP API, configuration and images are documented in the [kumoba repository](https://github.com/Parassharmaa/kumoba). Clients: the [`kumoba` npm package](https://www.npmjs.com/package/kumoba) for TypeScript, or any HTTP client.
