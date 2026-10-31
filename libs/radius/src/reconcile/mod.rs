//! Reconcile building blocks for the KanidmRadiusServer controller.
//!
//! Layout follows `intent/1119-kanidm-radius/plan.md`; the remaining modules
//! move in here as the old journal-based code is replaced.

/// Ownership-checked server-side apply, create-only Secret and delete.
pub mod apply;
/// Validated config rendering for the upstream Kanidm RADIUS service.
pub mod config;
/// Typed errors of the effectful executors.
pub mod error;
/// Kanidm account naming, ownership-checked ensure and delete.
pub mod identity;
/// Image inference and the verified runtime identity of the RADIUS image.
pub mod image;
/// Fail-closed input validation and public-material digest.
pub mod inputs;
/// Kanidm token mint, verify and revoke executors.
pub mod kanidm_token;
/// One module for every label, annotation and Secret key the operator uses.
pub mod meta;
/// Builds the planner input from Kanidm and Kubernetes lists.
pub mod observe;
/// Kanidm SDK call metrics wrapper.
mod sdk;
/// EAP TLS certificate and private key validation.
pub mod tls;
/// Pure token planner: which token to run, revoke and garbage-collect.
pub mod token;
/// Deployment, Service and content-addressed config Secret builders.
pub mod workload;

/// Fake Kanidm HTTP server for executor tests.
#[cfg(test)]
pub(crate) mod kanidm_mock;
/// Mock Kubernetes API server for executor tests.
#[cfg(test)]
pub(crate) mod kube_mock;
