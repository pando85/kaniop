use crate::controller::ControllerId;

use std::collections::HashMap;
use std::sync::Arc;

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use opentelemetry::{KeyValue, trace::TraceId};
use tokio::time::Instant;

#[derive(Clone)]
pub struct Metrics {
    pub controllers: HashMap<ControllerId, Arc<ControllerMetrics>>,
}

impl Metrics {
    pub fn new(meter: &Meter, controller_names: &[&'static str]) -> Self {
        let controllers = controller_names
            .iter()
            .map(|&id| (id, Arc::new(ControllerMetrics::new(id, meter))))
            .collect::<HashMap<ControllerId, Arc<ControllerMetrics>>>();

        Self { controllers }
    }
}

#[derive(Clone)]
pub struct ControllerMetrics {
    controller: String,
    pub reconcile: ReconcileMetrics,
    spec_replicas: Gauge<i64>,
    status_update_errors: Counter<u64>,
    triggered: Counter<u64>,
    watch_operations_failed: Counter<u64>,
    ready: Gauge<i64>,
}

impl ControllerMetrics {
    pub fn new(controller: &str, meter: &Meter) -> Self {
        let reconcile = ReconcileMetrics::new(meter);

        let spec_replicas = meter
            .i64_gauge("spec_replicas")
            .with_description("Number of expected replicas for the object")
            .build();

        let status_update_errors = meter
            .u64_counter("status_update_errors")
            .with_description(
                "Number of errors that occurred during update operations to status subresources",
            )
            .build();

        let triggered = meter
            .u64_counter("triggered")
            .with_description("Number of times a Kubernetes object applied or delete event triggered to reconcile an object")
            .build();

        let watch_operations_failed = meter
            .u64_counter("watch_operations_failed")
            .with_description("Total number of watch operations that failed")
            .build();

        let ready = meter
            .i64_gauge("ready")
            .with_description("1 when the controller is ready to reconcile resources, 0 otherwise")
            .build();

        Self {
            controller: controller.to_string(),
            reconcile,
            spec_replicas,
            status_update_errors,
            triggered,
            watch_operations_failed,
            ready,
        }
    }

    pub fn reconcile_failure_inc(&self) {
        self.reconcile
            .failures
            .add(1, &[KeyValue::new("controller", self.controller.clone())]);
    }

    pub fn reconcile_count_and_measure(&self, _trace_id: &TraceId) -> ReconcileMeasurer {
        self.reconcile
            .operations
            .add(1, &[KeyValue::new("controller", self.controller.clone())]);
        ReconcileMeasurer {
            start: Instant::now(),
            controller: self.controller.clone(),
            metric: self.reconcile.duration.clone(),
        }
    }

    pub fn reconcile_deploy_delete_create_inc(&self) {
        self.reconcile
            .deploy_delete_create
            .add(1, &[KeyValue::new("controller", self.controller.clone())]);
    }

    pub fn spec_replicas_set(&self, namespace: &str, name: &str, replicas: i32) {
        self.spec_replicas.record(
            replicas as i64,
            &[
                KeyValue::new("controller", self.controller.clone()),
                KeyValue::new("namespace", namespace.to_string()),
                KeyValue::new("name", name.to_string()),
            ],
        );
    }

    pub fn status_update_errors_inc(&self) {
        self.status_update_errors
            .add(1, &[KeyValue::new("controller", self.controller.clone())]);
    }

    pub fn triggered_inc(&self, action: Action, triggered_by: &str) {
        self.triggered.add(
            1,
            &[
                KeyValue::new("controller", self.controller.clone()),
                KeyValue::new("action", action.as_str()),
                KeyValue::new("triggered_by", triggered_by.to_string()),
            ],
        );
    }

    pub fn watch_operations_failed_inc(&self) {
        self.watch_operations_failed
            .add(1, &[KeyValue::new("controller", self.controller.clone())]);
    }

    pub fn ready_set(&self, status: i64) {
        self.ready.record(
            status,
            &[KeyValue::new("controller", self.controller.clone())],
        );
    }
}

#[derive(Clone)]
pub struct ReconcileMetrics {
    pub operations: Counter<u64>,
    pub failures: Counter<u64>,
    pub duration: Histogram<f64>,
    pub deploy_delete_create: Counter<u64>,
}

impl ReconcileMetrics {
    pub fn new(meter: &Meter) -> Self {
        let operations = meter
            .u64_counter("reconcile_operations")
            .with_description("Total number of reconcile operations")
            .build();

        let failures = meter
            .u64_counter("reconcile_failures")
            .with_description("Number of errors that occurred during reconcile operations")
            .build();

        let duration = meter
            .f64_histogram("reconcile_duration_seconds")
            .with_description("Histogram of reconcile operations")
            .build();

        let deploy_delete_create = meter
            .u64_counter("reconcile_deploy_delete_create")
            .with_description("Number of times that reconciling a deployment required deleting and re-creating it")
            .build();

        Self {
            operations,
            failures,
            duration,
            deploy_delete_create,
        }
    }
}

/// Smart function duration measurer
///
/// Relies on Drop to calculate duration and register the observation in the histogram
pub struct ReconcileMeasurer {
    start: Instant,
    controller: String,
    metric: Histogram<f64>,
}

impl Drop for ReconcileMeasurer {
    fn drop(&mut self) {
        let duration = self.start.elapsed().as_secs_f64();
        self.metric.record(
            duration,
            &[KeyValue::new("controller", self.controller.clone())],
        );
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Apply,
    Delete,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Apply => "apply",
            Action::Delete => "delete",
        }
    }
}
