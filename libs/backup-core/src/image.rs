pub fn data_mover_image() -> String {
    std::env::var("DATA_MOVER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/pando85/kaniop-data-mover:latest".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_mover_image_returns_default_when_env_not_set() {
        let image = data_mover_image();
        assert_eq!(image, "ghcr.io/pando85/kaniop-data-mover:latest");
    }
}
