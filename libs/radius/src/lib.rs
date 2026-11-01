/// Controller wiring: watches, stores and the `run` entry point.
pub mod controller;
/// KanidmRadiusServer Kubernetes API and status types.
#[rustfmt::skip]
pub mod crd;
/// World-derived reconciliation: inputs, identity, token plan, workload,
/// status and finalizer.
pub mod reconcile;
