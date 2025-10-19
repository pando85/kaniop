use crate::controller::ControllerId;
use kaniop_k8s_util::error::Error;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use opentelemetry::trace::TraceId;
use prometheus_client::encoding::{EncodeLabelSet, EncodeLabelValue};
use prometheus_client::metrics::{
    counter::Counter, exemplar::HistogramWithExemplars, family::Family, gauge::Gauge,
};
use prometheus_client::registry::{Registry, Unit};
use tokio::time::Instant;

#[derive(Clone)]
pub struct Metrics {
    pub controllers: HashMap<ControllerId, Arc<ControllerMetrics>>,
    pub registry: Arc<Registry>,
}

impl Metrics {
    pub fn new(mut registry: Registry, controller_names: &[&'static str]) -> Self {
        let controllers = controller_names
            .iter()
            .map(|&id| {
                (
                    id,
                    Arc::new(ControllerMetrics::new(id).register(&mut registry)),
                )
            })
            .collect::<HashMap<ControllerId, Arc<ControllerMetrics>>>();

        Self {
            registry: Arc::new(registry),
            controllers,
        }
    }
}

#[derive(Clone, Default)]
pub struct ControllerMetrics {
    controller: String,
    pub reconcile: ReconcileMetrics,
    pub spec_replicas: Family<ResourceLabels, Gauge>,
    pub status_update_errors: Family<ControllerLabels, Counter>,
    pub triggered: Family<TriggeredLabels, Counter>,
    pub watch_operations_failed: Family<ControllerLabels, Counter>,
    pub ready: Family<ControllerLabels, Gauge>,
}

impl ControllerMetrics {
    pub fn new(controller: &str) -> Self {
        Self {
            controller: controller.to_string(),
            ..Default::default()
        }
    }

    /// Register API metrics to start tracking them.
    pub fn register(self, r: &mut Registry) -> Self {
        r.register(
            "reconcile_operations",
            "Total number of reconcile operations",
            self.reconcile.operations.clone(),
        );
        r.register(
            "reconcile_failures",
            "Number of errors that occurred during reconcile operations",
            self.reconcile.failures.clone(),
        );
        r.register_with_unit(
            "reconcile_duration",
            "Histogram of reconcile operations",
            Unit::Seconds,
            self.reconcile.duration.clone(),
        );
        r.register(
            "reconcile_deploy_delete_create",
            "Number of times that reconciling a deployment required deleting and re-creating it",
            self.reconcile.deploy_delete_create.clone(),
        );
        r.register(
            "spec_replicas",
            "Number of expected replicas for the object",
            self.spec_replicas.clone(),
        );
        r.register(
            "status_update_errors",
            "Number of errors that occurred during update operations to status subresources",
            self.status_update_errors.clone(),
        );
        r.register(
            "triggered",
            "Number of times a Kubernetes object applied or delete event triggered to reconcile an object",
            self.triggered.clone(),
        );
        r.register(
            "watch_operations_failed",
            "Total number of watch operations that failed",
            self.watch_operations_failed.clone(),
        );
        r.register(
            "ready",
            "1 when the controller is ready to reconcile resources, 0 otherwise",
            self.ready.clone(),
        );
        self
    }

    pub fn reconcile_failure_inc(&self) {
        self.reconcile
            .failures
            .get_or_create(&ControllerLabels {
                controller: self.controller.clone(),
            })
            .inc();
    }

    pub fn reconcile_count_and_measure(&self, trace_id: &TraceId) -> ReconcileMeasurer {
        let controller_labels = ControllerLabels {
            controller: self.controller.clone(),
        };
        self.reconcile
            .operations
            .get_or_create(&controller_labels)
            .inc();
        ReconcileMeasurer {
            start: Instant::now(),
            labels: trace_id.try_into().ok(),
            metric: self
                .reconcile
                .duration
                .get_or_create(&controller_labels)
                .clone(),
        }
    }

    pub fn reconcile_deploy_delete_create_inc(&self) {
        let controller_labels = ControllerLabels {
            controller: self.controller.clone(),
        };
        self.reconcile
            .deploy_delete_create
            .get_or_create(&controller_labels)
            .inc();
    }

    pub fn spec_replicas_set(&self, namespace: &str, name: &str, replicas: i32) {
        let resource_labels = ResourceLabels {
            controller: self.controller.clone(),
            namespace: namespace.to_string(),
            name: name.to_string(),
        };
        self.spec_replicas
            .get_or_create(&resource_labels)
            .set(replicas as i64);
    }

    pub fn status_update_errors_inc(&self) {
        let controller_labels = ControllerLabels {
            controller: self.controller.clone(),
        };
        self.status_update_errors
            .get_or_create(&controller_labels)
            .inc();
    }

    pub fn triggered_inc(&self, action: Action, triggered_by: &str) {
        let triggered_labels = TriggeredLabels {
            controller: self.controller.clone(),
            action,
            triggered_by: triggered_by.to_string(),
        };
        self.triggered.get_or_create(&triggered_labels).inc();
    }

    pub fn watch_operations_failed_inc(&self) {
        let controller_labels = ControllerLabels {
            controller: self.controller.clone(),
        };
        self.watch_operations_failed
            .get_or_create(&controller_labels)
            .inc();
    }

    pub fn ready_set(&self, status: i64) {
        let controller_labels = ControllerLabels {
            controller: self.controller.clone(),
        };
        self.ready.get_or_create(&controller_labels).set(status);
    }
}

#[derive(Clone)]
pub struct ReconcileMetrics {
    pub operations: Family<ControllerLabels, Counter>,
    pub failures: Family<ControllerLabels, Counter>,
    pub duration: Family<ControllerLabels, HistogramWithExemplars<TraceLabel>>,
    pub deploy_delete_create: Family<ControllerLabels, Counter>,
}

impl Default for ReconcileMetrics {
    fn default() -> Self {
        Self {
            operations: Default::default(),
            failures: Default::default(),
            duration:
                Family::<ControllerLabels, HistogramWithExemplars<TraceLabel>>::new_with_constructor(
                    || HistogramWithExemplars::new([0.1, 0.5, 1., 5., 10.].into_iter()),
                ),
            deploy_delete_create: Default::default(),
        }
    }
}

/// Smart function duration measurer
///
/// Relies on Drop to calculate duration and register the observation in the histogram
pub struct ReconcileMeasurer {
    start: Instant,
    labels: Option<TraceLabel>,
    metric: HistogramWithExemplars<TraceLabel>,
}

impl Drop for ReconcileMeasurer {
    fn drop(&mut self) {
        let duration = self.start.elapsed().as_secs_f64();
        let labels = self.labels.take();
        self.metric
            .observe(duration, labels, Some(SystemTime::now()));
    }
}

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug, Default)]
pub struct TraceLabel {
    pub id: String,
}
impl TryFrom<&TraceId> for TraceLabel {
    type Error = Error;

    fn try_from(id: &TraceId) -> Result<TraceLabel, Self::Error> {
        if std::matches!(id, &TraceId::INVALID) {
            Err(Error::InvalidTraceId)
        } else {
            let trace_id = id.to_string();
            Ok(Self { id: trace_id })
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ControllerLabels {
    pub controller: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ResourceLabels {
    pub controller: String,
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TriggeredLabels {
    pub controller: String,
    pub action: Action,
    pub triggered_by: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum Action {
    Apply,
    Delete,
}
