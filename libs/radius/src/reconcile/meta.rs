//! Every label, annotation and Secret key the RADIUS operator reads or writes.
//!
//! Keeping them in one module prevents drift between the builders that write
//! them and the observers that read them. None of these values may carry
//! credential material: token ids and public-input digests only.

/// Standard name label; value is [`COMPONENT`].
pub const LABEL_NAME: &str = "app.kubernetes.io/name";
/// Standard managed-by label placed on every generated object.
pub const LABEL_MANAGED_BY: &str = "app.kubernetes.io/managed-by";
/// Immutable UID of the owning `KanidmRadiusServer`; part of the pod selector.
pub const LABEL_UID: &str = "kaniop.rs/radius-uid";

/// Value of [`LABEL_NAME`] and the container name.
pub const COMPONENT: &str = "kanidm-radius";
/// Value of [`LABEL_MANAGED_BY`].
pub const MANAGED_BY: &str = "kaniop-radius-server";

/// Legacy generation revision on the pod template (removed with the
/// generation journal in WP-5).
pub const ANNOTATION_REVISION: &str = "kaniop.rs/radius-revision";
/// Kanidm token id the pod template (and its config Secret) was rendered with.
pub const ANNOTATION_TOKEN_ID: &str = "kaniop.rs/radius-token-id";
/// Digest of the public certificate and CA bytes; rolls pods on TLS renewal.
pub const ANNOTATION_INPUTS_DIGEST: &str = "kaniop.rs/radius-inputs-digest";
/// Value of the CR rotate annotation recorded on the config Secret.
pub const ANNOTATION_ROTATION_REQUEST: &str = "kaniop.rs/radius-rotation-request";
/// CR annotation that requests a manual token rotation.
pub const ANNOTATION_ROTATE_TOKEN: &str = "kaniop.rs/radius-rotate-token";

/// Config Secret key holding the rendered `radius.toml`.
pub const SECRET_KEY_CONFIG: &str = "radius.toml";
/// Config Secret key holding the Kanidm bearer token.
pub const SECRET_KEY_TOKEN: &str = "auth_token";
