pub const KANIDM_CLIENT_VERSION: &str = env!("KANIDM_CLIENT_VERSION");

pub fn is_version_compatible(image_tag: &str) -> bool {
    let client_parts = parse_semver(KANIDM_CLIENT_VERSION);
    let image_parts = parse_semver(image_tag);

    match (client_parts, image_parts) {
        (Some((c_major, c_minor, _)), Some((i_major, i_minor, _))) => {
            i_major == c_major && i_minor <= c_minor
        }
        _ => true,
    }
}

fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim_start_matches('v');
    let parts: Vec<&str> = version.split('.').collect();

    if parts.len() >= 3 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts[2].split('-').next()?.parse().ok()?;
        Some((major, minor, patch))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_semver() {
        assert_eq!(parse_semver("1.8.6"), Some((1, 8, 6)));
        assert_eq!(parse_semver("v1.8.6"), Some((1, 8, 6)));
        assert_eq!(parse_semver("1.8.6-dev"), Some((1, 8, 6)));
        assert_eq!(parse_semver("invalid"), None);
    }

    #[test]
    fn test_is_version_compatible() {
        assert!(is_version_compatible("1.8.6"));
        assert!(is_version_compatible("1.8.0"));
        assert!(is_version_compatible("1.7.9"));
        assert!(!is_version_compatible("11.9.0"));
    }
}
