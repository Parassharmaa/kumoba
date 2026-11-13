# kumoba-protocol

The length-prefixed JSON wire protocol between the kumoba host (`kumoba`, `kumoba-worker`) and the guest agent (`kumoba-guest`). Requests, responses, limits and the protocol `VERSION` that both sides must agree on.

You only need this crate directly when writing an alternative guest agent or host. Everything else goes through the [`kumoba`](https://crates.io/crates/kumoba) crate or the [`kumobad`](https://crates.io/crates/kumobad) daemon.
