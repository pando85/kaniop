use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::jiff::Timestamp;
use kube::api::PartialObjectMeta;

use std::collections::BTreeMap;

/// Annotation key for tracking the last rotation timestamp.
pub const ROTATION_LAST_TIME_ANNOTATION: &str = "kaniop.rs/last-rotation-time";

/// Annotation key for indicating that rotation is enabled on a secret.
pub const ROTATION_ENABLED_ANNOTATION: &str = "kaniop.rs/rotation-enabled";

/// Annotation key for storing the rotation period in days.
pub const ROTATION_PERIOD_DAYS_ANNOTATION: &str = "kaniop.rs/rotation-period-days";

/// Add rotation annotations to a secret's metadata based on rotation configuration.
///
/// This function should be called when creating or rotating a secret. It adds annotations
/// that track when the secret was last rotated and what the rotation policy is.
///
/// # Arguments
/// * `annotations` - The annotations map to modify
/// * `enabled` - Whether rotation is enabled
/// * `period_days` - The rotation period in days
pub fn add_rotation_annotations(
    annotations: &mut BTreeMap<String, String>,
    enabled: bool,
    period_days: u32,
) {
    if enabled {
        annotations.insert(ROTATION_ENABLED_ANNOTATION.to_string(), "true".to_string());
        annotations.insert(
            ROTATION_PERIOD_DAYS_ANNOTATION.to_string(),
            period_days.to_string(),
        );
        annotations.insert(
            ROTATION_LAST_TIME_ANNOTATION.to_string(),
            Timestamp::now().to_string(),
        );
    }
}

/// Check if a secret needs rotation based on its annotations and the current rotation policy.
///
/// A secret needs rotation if:
/// - Rotation is enabled in the config but the secret doesn't have rotation annotations
/// - The rotation period has elapsed since the last rotation
///
/// # Arguments
/// * `secret` - The secret metadata to check
/// * `rotation_enabled` - Whether rotation is currently enabled in the config
/// * `period_days` - The current rotation period in days
///
/// # Returns
/// `true` if the secret needs to be rotated, `false` otherwise
pub fn needs_rotation(
    secret: &PartialObjectMeta<Secret>,
    rotation_enabled: bool,
    period_days: u32,
) -> bool {
    // If rotation is not enabled, no rotation needed
    if !rotation_enabled {
        return false;
    }

    // Check if the secret has rotation annotations
    let annotations = match &secret.metadata.annotations {
        Some(a) => a,
        None => return true, // No annotations means never rotated, needs rotation
    };

    // Check if rotation is enabled in annotations
    if annotations
        .get(ROTATION_ENABLED_ANNOTATION)
        .map(String::as_str)
        != Some("true")
    {
        return true; // Rotation was enabled but secret doesn't have annotation, needs rotation
    }

    // Get last rotation time
    let last_rotation_time = match annotations.get(ROTATION_LAST_TIME_ANNOTATION) {
        Some(time_str) => match time_str.parse::<Timestamp>() {
            Ok(time) => time,
            Err(_) => return true, // Invalid timestamp, needs rotation
        },
        None => return true, // No timestamp, needs rotation
    };

    // Check if period has passed
    // Convert days to seconds for Timestamp arithmetic
    let rotation_seconds = (period_days as i64) * 24 * 60 * 60;
    let rotation_period = k8s_openapi::jiff::Span::new().seconds(rotation_seconds);
    let next_rotation_time = last_rotation_time
        .checked_add(rotation_period)
        .unwrap_or(last_rotation_time);
    let now = Timestamp::now();

    now >= next_rotation_time
}

#[cfg(test)]
mod tests {
    use super::*;

    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn create_secret_metadata(
        annotations: Option<BTreeMap<String, String>>,
    ) -> PartialObjectMeta<Secret> {
        PartialObjectMeta {
            metadata: ObjectMeta {
                name: Some("test-secret".to_string()),
                namespace: Some("default".to_string()),
                annotations,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_needs_rotation_disabled() {
        let secret = create_secret_metadata(None);
        assert!(!needs_rotation(&secret, false, 90));
    }

    #[test]
    fn test_needs_rotation_no_annotations() {
        let secret = create_secret_metadata(None);
        assert!(needs_rotation(&secret, true, 90));
    }

    #[test]
    fn test_needs_rotation_missing_enabled_annotation() {
        let annotations = BTreeMap::from([(
            ROTATION_LAST_TIME_ANNOTATION.to_string(),
            Timestamp::now().to_string(),
        )]);
        let secret = create_secret_metadata(Some(annotations));
        assert!(needs_rotation(&secret, true, 90));
    }

    #[test]
    fn test_needs_rotation_missing_timestamp() {
        let annotations =
            BTreeMap::from([(ROTATION_ENABLED_ANNOTATION.to_string(), "true".to_string())]);
        let secret = create_secret_metadata(Some(annotations));
        assert!(needs_rotation(&secret, true, 90));
    }

    #[test]
    fn test_needs_rotation_invalid_timestamp() {
        let annotations = BTreeMap::from([
            (ROTATION_ENABLED_ANNOTATION.to_string(), "true".to_string()),
            (
                ROTATION_LAST_TIME_ANNOTATION.to_string(),
                "invalid-date".to_string(),
            ),
        ]);
        let secret = create_secret_metadata(Some(annotations));
        assert!(needs_rotation(&secret, true, 90));
    }

    #[test]
    fn test_needs_rotation_period_not_elapsed() {
        let now = Timestamp::now();
        let annotations = BTreeMap::from([
            (ROTATION_ENABLED_ANNOTATION.to_string(), "true".to_string()),
            (ROTATION_LAST_TIME_ANNOTATION.to_string(), now.to_string()),
            (
                ROTATION_PERIOD_DAYS_ANNOTATION.to_string(),
                "90".to_string(),
            ),
        ]);
        let secret = create_secret_metadata(Some(annotations));
        assert!(!needs_rotation(&secret, true, 90));
    }

    #[test]
    fn test_needs_rotation_period_elapsed() {
        // 100 days in seconds
        let span_100_days = k8s_openapi::jiff::Span::new().seconds(100 * 24 * 60 * 60);
        let past = Timestamp::now().checked_sub(span_100_days).unwrap();
        let annotations = BTreeMap::from([
            (ROTATION_ENABLED_ANNOTATION.to_string(), "true".to_string()),
            (ROTATION_LAST_TIME_ANNOTATION.to_string(), past.to_string()),
            (
                ROTATION_PERIOD_DAYS_ANNOTATION.to_string(),
                "90".to_string(),
            ),
        ]);
        let secret = create_secret_metadata(Some(annotations));
        assert!(needs_rotation(&secret, true, 90));
    }

    #[test]
    fn test_add_rotation_annotations() {
        let mut annotations = BTreeMap::new();
        add_rotation_annotations(&mut annotations, true, 30);

        assert_eq!(
            annotations.get(ROTATION_ENABLED_ANNOTATION),
            Some(&"true".to_string())
        );
        assert_eq!(
            annotations.get(ROTATION_PERIOD_DAYS_ANNOTATION),
            Some(&"30".to_string())
        );
        assert!(annotations.contains_key(ROTATION_LAST_TIME_ANNOTATION));
    }

    #[test]
    fn test_add_rotation_annotations_disabled() {
        let mut annotations = BTreeMap::new();
        add_rotation_annotations(&mut annotations, false, 30);

        assert!(annotations.is_empty());
    }
}
