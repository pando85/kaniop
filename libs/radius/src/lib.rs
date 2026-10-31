/// Ownership-verified Kubernetes RADIUS Deployment/Service apply.
pub mod apply;
/// Crash-safe first-generation RADIUS token/Secret bootstrap checkpoints.
pub mod bootstrap;
/// Ownership-verified Kubernetes RADIUS teardown and pod draining.
pub mod cleanup;
/// Namespace-authorized, fail-closed RADIUS resource controller.
pub mod controller;
/// Proposed KanidmRadiusServer Kubernetes API and status types.
#[rustfmt::skip]
pub mod crd;
/// Bounded UID-scoped immutable configuration generation names.
pub mod generation;
/// UID-aware Kanidm RADIUS service account and additive group provisioning.
pub mod identity_reconcile;
/// Pure builders and reconcile-time helpers (restructured incrementally).
pub mod reconcile;
/// Overlapping token rotation safety state machine.
pub mod rotation;
/// Immutable, UID-owned generated RADIUS configuration Secrets.
pub mod secret;
/// Least-privilege RADIUS candidate token issuance.
pub mod token;
