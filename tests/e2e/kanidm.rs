use toml::Value;

// Get Kanidm version from Cargo.toml
// This is used to test our code against current Kanidm version used in the project
// Also, if kaniop deployed in dev mode and version mismatch, it will fail automatically
pub fn get_dependency_version() -> Option<String> {
    let cargo_toml_content = include_str!("../../Cargo.toml");
    let cargo_toml: Value = cargo_toml_content.parse::<Value>().unwrap();
    cargo_toml
        .get("workspace")
        .and_then(|ws| ws.get("dependencies"))
        .and_then(|deps| deps.get("kanidm_client"))
        .and_then(|kc| kc.as_str())
        .map(|s| s.to_string())
}
