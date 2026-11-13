# Rust

The [`kumoba`](https://crates.io/crates/kumoba) crate runs VMs directly, without the daemon. It needs the `kumoba-worker` binary, which the installer and `cargo install kumobad` provide.

```sh
cargo add kumoba
```

The repository's [`hello.rs`](https://github.com/Parassharmaa/kumoba/blob/main/crates/kumoba/examples/hello.rs) example boots an image, runs a command and exits:

```sh
cargo run --release -p kumoba --example hello -- ~/.kumoba/bin/kumoba-worker ~/.kumoba/images/default.raw
```

API documentation is on [docs.rs/kumoba](https://docs.rs/kumoba). To talk to a running daemon from Rust instead, use its [HTTP API](/http-api).
