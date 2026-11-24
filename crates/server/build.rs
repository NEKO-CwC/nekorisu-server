// Puts the commit into the binary, for the panel's footer (AGPL section 13:
// whoever uses the server over the network can see which source it runs).
// NEKORISU_COMMIT wins, for builds that have no .git (the Docker build).
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=NEKORISU_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let commit = std::env::var("NEKORISU_COMMIT")
        .ok()
        .filter(|c| !c.trim().is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=7", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=NEKORISU_COMMIT={commit}");
}
