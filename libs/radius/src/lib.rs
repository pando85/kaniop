/// Ownership-verified Kubernetes RADIUS Deployment/Service apply.
pub mod apply;
/// Crash-safe first-generation RADIUS token/Secret bootstrap checkpoints.
pub mod bootstrap;
/// Ownership-verified Kubernetes RADIUS teardown and pod draining.
pub mod cleanup;
/// Validated config rendering for the upstream Kanidm RADIUS service.
pub mod config;
/// Namespace-authorized, fail-closed RADIUS resource controller.
pub mod controller;
/// Proposed KanidmRadiusServer Kubernetes API and status types.
#[rustfmt::skip]
pub mod crd;
/// Bounded UID-scoped immutable configuration generation names.
pub mod generation;
/// UID-scoped identity naming and ownership checks for RADIUS servers.
pub mod identity;
/// UID-aware Kanidm RADIUS service account and additive group provisioning.
pub mod identity_reconcile;
/// Conservative RADIUS component image selection.
pub mod image;
/// Fail-closed secret, CA and certificate preflight for RADIUS.
pub mod preflight;
/// Overlapping token rotation safety state machine.
pub mod rotation;
/// Immutable, UID-owned generated RADIUS configuration Secrets.
pub mod secret;
/// EAP TLS certificate and private key preflight validation.
pub mod tls;
/// Least-privilege RADIUS candidate token issuance.
pub mod token;
/// UID-owned Kubernetes RADIUS Deployment and UDP Service builders.
pub mod workload;
