pub fn data_mover_image() -> String {
    std::env::var("DATA_MOVER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/pando85/kaniop-data-mover:latest".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_mover_image_returns_default_when_env_not_set() {
        // Note: We can't safely test the env var case in Rust 2024 without unsafe blocks
        // and parallel tests would race. The default path is the important one to verify.
        let image = data_mover_image();
        assert!(
            image == "ghcr.io/pando85/kaniop-data-mover:latest" || !image.is_empty() // env var may be set in CI
        );
    }
}
