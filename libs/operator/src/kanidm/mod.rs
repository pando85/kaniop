#[rustfmt::skip]
pub mod crd;
pub mod controller;
mod image;
pub mod reconcile;
pub mod restore;

/// Namespace-authorized, fail-closed RADIUS resource controller.
pub mod radius_controller;
/// Ownership-verified Kubernetes RADIUS Deployment/Service apply.
pub mod radius_apply;
/// Ownership-verified Kubernetes RADIUS teardown and pod draining.
pub mod radius_cleanup;
/// Validated config rendering for the upstream Kanidm RADIUS service.
pub mod radius_config;
/// Proposed KanidmRadiusServer Kubernetes API and status types.
pub mod radius_crd;
/// Bounded UID-scoped immutable configuration generation names.
pub mod radius_generation;
/// UID-scoped identity naming and ownership checks for RADIUS servers.
pub mod radius_identity;
/// UID-aware Kanidm RADIUS service account and additive group provisioning.
pub mod radius_identity_reconcile;
/// Conservative RADIUS component image selection.
pub mod radius_image;
/// Fail-closed secret, CA and certificate preflight for RADIUS.
pub mod radius_preflight;
/// Overlapping token rotation safety state machine.
pub mod radius_rotation;
/// Immutable, UID-owned generated RADIUS configuration Secrets.
pub mod radius_secret;
/// Least-privilege RADIUS candidate token issuance.
pub mod radius_token;
/// EAP TLS certificate and private key preflight validation.
pub mod radius_tls;
/// UID-owned Kubernetes RADIUS Deployment and UDP Service builders.
pub mod radius_workload;
