use toml::Value;

// Get Kanidm version from Cargo.lock
// This is used to test our code against current Kanidm version used in the project
// Also, if kaniop deployed in dev mode and version mismatch, it will fail automatically
pub fn get_dependency_version() -> Option<String> {
    let cargo_lock_content = include_str!("../../Cargo.lock");
    let cargo_lock: Value = cargo_lock_content.parse::<Value>().unwrap();
    if let Some(packages) = cargo_lock.get("package").and_then(|p| p.as_array()) {
        for package in packages {
            if let Some(name) = package.get("name").and_then(|n| n.as_str()) {
                if name == "kanidm_client" {
                    return package
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }
    }
    None
}
