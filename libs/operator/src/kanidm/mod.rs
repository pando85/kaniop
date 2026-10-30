#[rustfmt::skip]
pub mod crd;
pub mod controller;
mod image;
pub mod reconcile;
pub mod restore;

/// Validated config rendering for the upstream Kanidm RADIUS service.
pub mod radius_config;
/// Proposed KanidmRadiusServer Kubernetes API and status types.
pub mod radius_crd;
/// UID-scoped identity naming and ownership checks for RADIUS servers.
pub mod radius_identity;
/// Conservative RADIUS component image selection.
pub mod radius_image;
/// EAP TLS certificate and private key preflight validation.
pub mod radius_tls;
/// UID-owned Kubernetes RADIUS Deployment and UDP Service builders.
pub mod radius_workload;
