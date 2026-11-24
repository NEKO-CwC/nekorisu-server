// Compiles cloud.proto from the client repository, which owns it: both sides
// share one copy. protox is pure Rust, so building
// needs no protoc.
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=NEKORISU_PROTO_DIR");
    let dir = match std::env::var("NEKORISU_PROTO_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("../../../nekorisu/proto"),
    };
    println!("cargo:rerun-if-changed={}", dir.display());
    let descriptors = protox::compile(["nekorisu/cloud/v1/cloud.proto"], [&dir])
        .map_err(|e| format!("cannot compile cloud.proto from {}: {e}", dir.display()))?;
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(descriptors)?;
    Ok(())
}
