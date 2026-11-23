#!/usr/bin/env bash
# Builds the live view decoder to WebAssembly and writes it, base64-encoded, into the
# TypeScript SDK's browser client (sdk/typescript/src/view/codec.ts), then bundles that client
# into the engine's viewer page (crates/kumoba-view/viewer/view.js). Both outputs are
# committed so the engine and the npm package build without a wasm toolchain; CI runs this
# and fails when either differs from what is committed. The wasm build is reproducible.
#   scripts/build-view.sh
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_BUILD_TARGET \
  cargo build --release --locked -p kumoba-view-wasm --target wasm32-unknown-unknown
wasm=target/wasm32-unknown-unknown/release/kumoba_view_wasm.wasm
node -e '
const [wasm, out] = process.argv.slice(1);
const fs = require("node:fs");
fs.writeFileSync(out, `export const CODEC: string =\n  "${fs.readFileSync(wasm).toString("base64")}";\n`);
' "$wasm" sdk/typescript/src/view/codec.ts
cd sdk/typescript
[[ -d node_modules ]] || npm ci --no-fund --no-audit
npx esbuild src/view/index.ts --bundle --format=esm --target=es2022 --legal-comments=none \
  --log-level=warning --outfile=../../crates/kumoba-view/viewer/view.js
