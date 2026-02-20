use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();
    let cargo_lock_path = workspace_root.join("Cargo.lock");

    let kanidm_client_version = fs::read_to_string(&cargo_lock_path)
        .expect("Failed to read Cargo.lock")
        .lines()
        .skip_while(|line| !line.contains("name = \"kanidm_client\""))
        .nth(1)
        .and_then(|line| line.strip_prefix("version = "))
        .map(|v| v.trim_matches('"').to_string())
        .expect("Failed to find kanidm_client version in Cargo.lock");

    println!(
        "cargo:rustc-env=KANIDM_CLIENT_VERSION={}",
        kanidm_client_version
    );
    println!("cargo:rerun-if-changed={}", cargo_lock_path.display());
}
