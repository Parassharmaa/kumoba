# kumoba-guest

[![crates.io](https://img.shields.io/crates/v/kumoba-guest)](https://crates.io/crates/kumoba-guest)
[![docs.rs](https://img.shields.io/docsrs/kumoba-guest)](https://docs.rs/kumoba-guest)
[![License](https://img.shields.io/crates/l/kumoba-guest)](https://github.com/Parassharmaa/kumoba/blob/main/LICENSE)

The agent that runs as PID 1 inside every kumoba microVM. It mounts the disk, applies the sandbox configuration, and serves commands, files, port forwarding and the optional desktop to the host over vsock.

Every release tarball carries a static build in `bin/kumoba-guest`. To build it yourself for the guest architecture, then copy it into your image as `/usr/local/bin/kumoba-guest`:

```sh
RUSTFLAGS='-C linker=rust-lld' cargo install kumoba-guest --target x86_64-unknown-linux-musl --root ./out
```

Its protocol version must match the `kumoba` crate on the host, so rebuild images when you upgrade. See [docs/images.md](https://github.com/Parassharmaa/kumoba/blob/main/docs/images.md) in the kumoba repository.
