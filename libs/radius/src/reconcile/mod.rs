//! Reconcile building blocks for the KanidmRadiusServer controller.
//!
//! Layout follows `intent/1119-kanidm-radius/plan.md`; the remaining modules
//! move in here as the old journal-based code is replaced.

/// Validated config rendering for the upstream Kanidm RADIUS service.
pub mod config;
/// Image inference and the verified runtime identity of the RADIUS image.
pub mod image;
/// Fail-closed input validation and public-material digest.
pub mod inputs;
/// One module for every label, annotation and Secret key the operator uses.
pub mod meta;
/// EAP TLS certificate and private key validation.
pub mod tls;
/// Pure token planner: which token to run, revoke and garbage-collect.
pub mod token;
/// Deployment, Service and content-addressed config Secret builders.
pub mod workload;
