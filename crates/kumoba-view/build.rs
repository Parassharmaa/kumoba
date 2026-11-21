use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let workspace = manifest.join("../..");
    for path in [
        "../kumoba-view-codec/src",
        "../kumoba-view-wasm/src",
        "../kumoba-view-wasm/Cargo.toml",
    ] {
        println!("cargo:rerun-if-changed={}", manifest.join(path).display());
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(&workspace)
        .args(["build", "--release", "--locked", "-p", "kumoba-view-wasm"])
        .args(["--target", "wasm32-unknown-unknown", "--target-dir"])
        .arg(out.join("wasm"))
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .expect("cargo runs");
    assert!(
        status.success(),
        "building the viewer decoder failed; run `rustup target add wasm32-unknown-unknown`"
    );
    let built = out.join("wasm/wasm32-unknown-unknown/release/kumoba_view_wasm.wasm");
    std::fs::copy(&built, out.join("codec.wasm")).expect("the decoder was built");
}
