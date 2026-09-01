use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let cargo_toml_path = Path::new(&manifest_dir).join("Cargo.toml");

    let content = fs::read_to_string(&cargo_toml_path).expect("Failed to read Cargo.toml");

    let version = extract_version_from_lock(&manifest_dir)
        .or_else(|| extract_version_from_toml(&content))
        .expect("Failed to find kanidm_client version");

    println!("cargo:rustc-env=KANIDM_CLIENT_VERSION={}", version);
    println!("cargo:rerun-if-changed=Cargo.toml");
}

fn extract_version_from_lock(manifest_dir: &str) -> Option<String> {
    let workspace_root = Path::new(manifest_dir).parent().and_then(|p| p.parent())?;
    let cargo_lock_path = workspace_root.join("Cargo.lock");
    let content = fs::read_to_string(&cargo_lock_path).ok()?;
    content
        .lines()
        .skip_while(|line| !line.contains("name = \"kanidm_client\""))
        .nth(1)
        .and_then(|line| line.strip_prefix("version = "))
        .map(|v| v.trim_matches('"').to_string())
}

fn extract_version_from_toml(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut in_kanidm_client_section = false;

    for line in lines.iter() {
        let trimmed = line.trim();

        if trimmed == "[dependencies.kanidm_client]" {
            in_kanidm_client_section = true;
            continue;
        }

        if in_kanidm_client_section {
            if trimmed.starts_with('[') {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("version") {
                if let Some(v) = rest.split('"').nth(1) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}
