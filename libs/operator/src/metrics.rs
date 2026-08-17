use crate::controller::ControllerId;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use opentelemetry::{KeyValue, trace::TraceId};
use tokio::time::Instant;
use tracing::debug;

pub const KANIDM_RESOURCE_PERSON: &str = "Person";
pub const KANIDM_RESOURCE_GROUP: &str = "Group";
pub const KANIDM_RESOURCE_OAUTH2: &str = "OAuth2Client";
pub const KANIDM_RESOURCE_SERVICE_ACCOUNT: &str = "ServiceAccount";
pub const KANIDM_RESOURCE_DOMAIN: &str = "Domain";
pub const KANIDM_RESOURCE_MAIL_SENDER: &str = "MailSender";

pub const KANIDM_OP_GET: &str = "get";
pub const KANIDM_OP_GET_CREDENTIAL_STATUS: &str = "get_credential_status";
pub const KANIDM_OP_CREATE: &str = "create";
pub const KANIDM_OP_UPDATE: &str = "update";
pub const KANIDM_OP_DELETE: &str = "delete";
pub const KANIDM_OP_UNIX_EXTEND: &str = "unix_extend";
pub const KANIDM_OP_CREDENTIAL_UPDATE_INTENT: &str = "credential_update_intent";
pub const KANIDM_OP_SET_MEMBERS: &str = "set_members";
pub const KANIDM_OP_SET_MAIL: &str = "set_mail";
pub const KANIDM_OP_PURGE_MAIL: &str = "purge_mail";
pub const KANIDM_OP_ADD_MAIL: &str = "add_mail";
pub const KANIDM_OP_REMOVE_MAIL: &str = "remove_mail";
pub const KANIDM_OP_ACCOUNT_POLICY: &str = "account_policy";
pub const KANIDM_OP_PURGE_ATTR: &str = "purge_attr";
pub const KANIDM_OP_ROTATE_SECRET: &str = "rotate_secret";
pub const KANIDM_OP_GET_SECRET: &str = "get_secret";
pub const KANIDM_OP_ADD_ORIGIN: &str = "add_origin";
pub const KANIDM_OP_REMOVE_ORIGIN: &str = "remove_origin";
pub const KANIDM_OP_SCOPE_MAP: &str = "scope_map";
pub const KANIDM_OP_DELETE_SCOPE_MAP: &str = "delete_scope_map";
pub const KANIDM_OP_UPDATE_SCOPE_MAP: &str = "update_scope_map";
pub const KANIDM_OP_SUP_SCOPE_MAP: &str = "sup_scope_map";
pub const KANIDM_OP_DELETE_SUP_SCOPE_MAP: &str = "delete_sup_scope_map";
pub const KANIDM_OP_UPDATE_SUP_SCOPE_MAP: &str = "update_sup_scope_map";
pub const KANIDM_OP_CLAIM_MAP: &str = "claim_map";
pub const KANIDM_OP_DELETE_CLAIM_MAP: &str = "delete_claim_map";
pub const KANIDM_OP_UPDATE_CLAIM_MAP: &str = "update_claim_map";
pub const KANIDM_OP_CLAIM_MAP_JOIN: &str = "claim_map_join";
pub const KANIDM_OP_UPDATE_CLAIM_MAP_JOIN: &str = "update_claim_map_join";
pub const KANIDM_OP_IMAGE: &str = "image";
pub const KANIDM_OP_DELETE_IMAGE: &str = "delete_image";
pub const KANIDM_OP_UPDATE_IMAGE: &str = "update_image";
pub const KANIDM_OP_RESET_SECRET: &str = "reset_secret";
pub const KANIDM_OP_GENERATE_API_TOKEN: &str = "generate_api_token";
pub const KANIDM_OP_DESTROY_API_TOKEN: &str = "destroy_api_token";
pub const KANIDM_OP_LIST_API_TOKENS: &str = "list_api_tokens";
pub const KANIDM_OP_GENERATE_PASSWORD: &str = "generate_password";
pub const KANIDM_OP_SET_DISPLAY_NAME: &str = "set_display_name";
pub const KANIDM_OP_ADD_MEMBERS: &str = "add_members";
pub const KANIDM_OP_REMOVE_MEMBERS: &str = "remove_members";

pub const KANIDM_OUTCOME_CHANGED: &str = "changed";
pub const KANIDM_OUTCOME_UNCHANGED: &str = "unchanged";
pub const KANIDM_OUTCOME_ERROR: &str = "error";

pub async fn record_kanidm_sdk_call<F, T, E>(
    metrics: &ControllerMetrics,
    resource: &'static str,
    operation: &'static str,
    success_outcome: &'static str,
    future: F,
) -> std::result::Result<T, E>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
{
    let start = Instant::now();
    let result = future.await;
    let outcome = match &result {
        Ok(_) => success_outcome,
        Err(_) => KANIDM_OUTCOME_ERROR,
    };
    metrics.kanidm_sdk_calls.add(
        1,
        &[
            KeyValue::new("resource", resource),
            KeyValue::new("operation", operation),
            KeyValue::new("outcome", outcome),
        ],
    );
    metrics.kanidm_sdk_call_duration.record(
        start.elapsed().as_secs_f64(),
        &[
            KeyValue::new("resource", resource),
            KeyValue::new("operation", operation),
        ],
    );
    result
}

#[derive(Clone)]
pub struct Metrics {
    pub controllers: HashMap<ControllerId, Arc<ControllerMetrics>>,
}

impl Metrics {
    pub fn new(meter: &Meter, controller_names: &[&'static str]) -> Self {
        debug!(
            "Initializing operator metrics for controllers: {:?}",
            controller_names
        );
        let controllers = controller_names
            .iter()
            .map(|&id| (id, Arc::new(ControllerMetrics::new(id, meter))))
            .collect::<HashMap<ControllerId, Arc<ControllerMetrics>>>();

        debug!("Operator metrics initialized");
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
    active_reconciles: Gauge<i64>,
    active_reconciles_count: Arc<AtomicI64>,
    objects_in_backoff: Gauge<i64>,
    objects_in_backoff_count: Arc<AtomicI64>,
    kanidm_sdk_calls: Counter<u64>,
    kanidm_sdk_call_duration: Histogram<f64>,
    reconcile_outcome: Counter<u64>,
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

        let active_reconciles = meter
            .i64_gauge("active_reconciles")
            .with_description("Number of reconcile operations currently in flight")
            .build();

        let objects_in_backoff = meter
            .i64_gauge("objects_in_backoff")
            .with_description("Number of objects currently in error backoff state")
            .build();

        let kanidm_sdk_calls = meter
            .u64_counter("kanidm_sdk_calls")
            .with_description("Total number of Kanidm SDK calls")
            .build();

        let kanidm_sdk_call_duration = meter
            .f64_histogram("kanidm_sdk_call_duration_seconds")
            .with_description("Duration of Kanidm SDK calls in seconds")
            .with_boundaries(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0])
            .build();

        let reconcile_outcome = meter
            .u64_counter("reconcile_outcome")
            .with_description("Outcome of reconcile operations: changed, unchanged, or error")
            .build();

        Self {
            controller: controller.to_string(),
            reconcile,
            spec_replicas,
            status_update_errors,
            triggered,
            watch_operations_failed,
            ready,
            active_reconciles,
            active_reconciles_count: Arc::new(AtomicI64::new(0)),
            objects_in_backoff,
            objects_in_backoff_count: Arc::new(AtomicI64::new(0)),
            kanidm_sdk_calls,
            kanidm_sdk_call_duration,
            reconcile_outcome,
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

    pub fn reconcile_deploy_delete_create_inc(&self, resource_kind: &str, reason: &str) {
        self.reconcile.deploy_delete_create.add(
            1,
            &[
                KeyValue::new("controller", self.controller.clone()),
                KeyValue::new("resource_kind", resource_kind.to_string()),
                KeyValue::new("reason", reason.to_string()),
            ],
        );
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

    pub fn active_reconcile_inc(&self) -> ActiveReconcileGuard {
        let new = self.active_reconciles_count.fetch_add(1, Ordering::Relaxed) + 1;
        self.active_reconciles
            .record(new, &[KeyValue::new("controller", self.controller.clone())]);
        ActiveReconcileGuard {
            count: self.active_reconciles_count.clone(),
            gauge: self.active_reconciles.clone(),
            controller: self.controller.clone(),
        }
    }

    pub fn objects_in_backoff_inc(&self) {
        let new = self
            .objects_in_backoff_count
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        self.objects_in_backoff
            .record(new, &[KeyValue::new("controller", self.controller.clone())]);
    }

    pub fn objects_in_backoff_dec(&self) {
        let prev = self
            .objects_in_backoff_count
            .fetch_sub(1, Ordering::Relaxed);
        let new = (prev - 1).max(0);
        self.objects_in_backoff
            .record(new, &[KeyValue::new("controller", self.controller.clone())]);
    }

    pub fn active_reconciles_count(&self) -> i64 {
        self.active_reconciles_count.load(Ordering::Relaxed)
    }

    pub fn objects_in_backoff_count(&self) -> i64 {
        self.objects_in_backoff_count.load(Ordering::Relaxed)
    }

    pub fn kanidm_sdk_call_inc(
        &self,
        resource: &'static str,
        operation: &'static str,
        outcome: &'static str,
    ) {
        self.kanidm_sdk_calls.add(
            1,
            &[
                KeyValue::new("resource", resource),
                KeyValue::new("operation", operation),
                KeyValue::new("outcome", outcome),
            ],
        );
    }

    pub fn record_kanidm_sdk_outcome(
        &self,
        resource: &'static str,
        operation: &'static str,
        outcome: &'static str,
        duration: std::time::Duration,
    ) {
        self.kanidm_sdk_calls.add(
            1,
            &[
                KeyValue::new("resource", resource),
                KeyValue::new("operation", operation),
                KeyValue::new("outcome", outcome),
            ],
        );
        self.kanidm_sdk_call_duration.record(
            duration.as_secs_f64(),
            &[
                KeyValue::new("resource", resource),
                KeyValue::new("operation", operation),
            ],
        );
    }

    pub fn reconcile_outcome_record(&self, outcome: &'static str) {
        self.reconcile_outcome.add(
            1,
            &[
                KeyValue::new("controller", self.controller.clone()),
                KeyValue::new("outcome", outcome),
            ],
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
        debug!("Initializing reconcile metrics");
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
            .with_boundaries(vec![0.1, 0.5, 1.0, 5.0, 10.0])
            .build();

        let deploy_delete_create = meter
            .u64_counter("reconcile_deploy_delete_create")
            .with_description("Number of explicit resource delete/recreate operations (not limited to deployments)")
            .build();

        debug!("Reconcile metrics initialized");
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

pub struct ActiveReconcileGuard {
    count: Arc<AtomicI64>,
    gauge: Gauge<i64>,
    controller: String,
}

impl Drop for ActiveReconcileGuard {
    fn drop(&mut self) {
        let prev = self.count.fetch_sub(1, Ordering::Relaxed);
        self.gauge.record(
            (prev - 1).max(0),
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

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::metrics::MeterProvider;

    fn test_metrics() -> ControllerMetrics {
        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder().build();
        let meter = provider.meter("test");
        ControllerMetrics::new("test-controller", &meter)
    }

    #[test]
    fn active_reconcile_guard_increments_on_creation() {
        let m = test_metrics();
        assert_eq!(m.active_reconciles_count(), 0);
        let _guard = m.active_reconcile_inc();
        assert_eq!(m.active_reconciles_count(), 1);
    }

    #[test]
    fn active_reconcile_guard_decrements_on_drop() {
        let m = test_metrics();
        let guard = m.active_reconcile_inc();
        assert_eq!(m.active_reconciles_count(), 1);
        drop(guard);
        assert_eq!(m.active_reconciles_count(), 0);
    }

    #[test]
    fn active_reconcile_multiple_guards() {
        let m = test_metrics();
        let g1 = m.active_reconcile_inc();
        let g2 = m.active_reconcile_inc();
        let g3 = m.active_reconcile_inc();
        assert_eq!(m.active_reconciles_count(), 3);
        drop(g2);
        assert_eq!(m.active_reconciles_count(), 2);
        drop(g1);
        assert_eq!(m.active_reconciles_count(), 1);
        drop(g3);
        assert_eq!(m.active_reconciles_count(), 0);
    }

    #[test]
    fn active_reconcile_guard_does_not_underflow() {
        let m = test_metrics();
        let guard = m.active_reconcile_inc();
        drop(guard);
        assert_eq!(m.active_reconciles_count(), 0);
    }

    #[test]
    fn objects_in_backoff_inc_dec() {
        let m = test_metrics();
        assert_eq!(m.objects_in_backoff_count(), 0);
        m.objects_in_backoff_inc();
        assert_eq!(m.objects_in_backoff_count(), 1);
        m.objects_in_backoff_inc();
        assert_eq!(m.objects_in_backoff_count(), 2);
        m.objects_in_backoff_dec();
        assert_eq!(m.objects_in_backoff_count(), 1);
        m.objects_in_backoff_dec();
        assert_eq!(m.objects_in_backoff_count(), 0);
    }

    #[test]
    fn cloned_metrics_share_active_reconcile_counter() {
        let m = test_metrics();
        let m2 = m.clone();
        let _g = m.active_reconcile_inc();
        assert_eq!(m2.active_reconciles_count(), 1);
    }

    #[test]
    fn cloned_metrics_share_backoff_counter() {
        let m = test_metrics();
        let m2 = m.clone();
        m.objects_in_backoff_inc();
        assert_eq!(m2.objects_in_backoff_count(), 1);
        m2.objects_in_backoff_dec();
        assert_eq!(m.objects_in_backoff_count(), 0);
    }

    #[test]
    fn kanidm_sdk_call_inc_records_counter() {
        let m = test_metrics();
        m.kanidm_sdk_call_inc(
            KANIDM_RESOURCE_PERSON,
            KANIDM_OP_CREATE,
            KANIDM_OUTCOME_CHANGED,
        );
        m.kanidm_sdk_call_inc(
            KANIDM_RESOURCE_PERSON,
            KANIDM_OP_GET,
            KANIDM_OUTCOME_UNCHANGED,
        );
        m.kanidm_sdk_call_inc(
            KANIDM_RESOURCE_GROUP,
            KANIDM_OP_DELETE,
            KANIDM_OUTCOME_ERROR,
        );
    }

    #[test]
    fn record_kanidm_sdk_outcome_records_count_and_duration() {
        let m = test_metrics();
        m.record_kanidm_sdk_outcome(
            KANIDM_RESOURCE_PERSON,
            KANIDM_OP_CREATE,
            KANIDM_OUTCOME_CHANGED,
            std::time::Duration::from_millis(50),
        );
        m.record_kanidm_sdk_outcome(
            KANIDM_RESOURCE_PERSON,
            KANIDM_OP_GET,
            KANIDM_OUTCOME_UNCHANGED,
            std::time::Duration::from_millis(10),
        );
    }

    #[tokio::test]
    async fn record_kanidm_sdk_call_helper_records_changed_on_success() {
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(std::time::Duration::from_millis(50))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("test");
        let m = ControllerMetrics::new("test", &meter);

        let result: std::result::Result<(), &str> = record_kanidm_sdk_call(
            &m,
            KANIDM_RESOURCE_PERSON,
            KANIDM_OP_CREATE,
            KANIDM_OUTCOME_CHANGED,
            async { Ok(()) },
        )
        .await;
        assert!(result.is_ok());

        provider.force_flush().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains("kanidm_sdk_calls"));
        assert!(text.contains("kanidm_sdk_call_duration_seconds"));
        assert!(text.contains("changed"));
    }

    #[tokio::test]
    async fn record_kanidm_sdk_call_helper_records_error_on_failure() {
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(std::time::Duration::from_millis(50))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("test");
        let m = ControllerMetrics::new("test", &meter);

        let result: std::result::Result<(), &str> = record_kanidm_sdk_call(
            &m,
            KANIDM_RESOURCE_GROUP,
            KANIDM_OP_DELETE,
            KANIDM_OUTCOME_CHANGED,
            async { Err("something failed") },
        )
        .await;
        assert!(result.is_err());

        provider.force_flush().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains("error"));
    }

    #[tokio::test]
    async fn record_kanidm_sdk_call_with_explicit_outcome() {
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(std::time::Duration::from_millis(50))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("test");
        let m = ControllerMetrics::new("test", &meter);

        let result: std::result::Result<(), &str> = record_kanidm_sdk_call(
            &m,
            KANIDM_RESOURCE_OAUTH2,
            KANIDM_OP_GET_SECRET,
            KANIDM_OUTCOME_UNCHANGED,
            async { Ok(()) },
        )
        .await;
        assert!(result.is_ok());

        provider.force_flush().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains(r#""unchanged""#));
        assert!(!text.contains(r#""changed""#));
    }

    #[tokio::test]
    async fn concurrent_active_reconcile_guards_track_correctly() {
        let m = Arc::new(test_metrics());
        let mut handles = Vec::new();
        for _ in 0..100 {
            let m = m.clone();
            handles.push(tokio::spawn(async move {
                let _guard = m.active_reconcile_inc();
                tokio::task::yield_now().await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(m.active_reconciles_count(), 0);
    }

    #[tokio::test]
    async fn concurrent_backoff_inc_dec_stays_balanced() {
        let m = Arc::new(test_metrics());
        let mut handles = Vec::new();
        for _ in 0..50 {
            let m = m.clone();
            handles.push(tokio::spawn(async move {
                m.objects_in_backoff_inc();
                tokio::task::yield_now().await;
                m.objects_in_backoff_dec();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(m.objects_in_backoff_count(), 0);
    }

    #[tokio::test]
    async fn entry_api_insert_only_increments_once() {
        use std::collections::HashMap;
        use std::collections::hash_map::Entry;
        use tokio::sync::RwLock;

        let m = Arc::new(test_metrics());
        let cache: Arc<RwLock<HashMap<String, RwLock<u32>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let mut handles = Vec::new();

        for _ in 0..100 {
            let m = m.clone();
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let mut guard = cache.write().await;
                match guard.entry("same-key".to_string()) {
                    Entry::Vacant(v) => {
                        v.insert(RwLock::new(0));
                        m.objects_in_backoff_inc();
                    }
                    Entry::Occupied(_) => {}
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(m.objects_in_backoff_count(), 1);
        assert_eq!(cache.read().await.len(), 1);
    }

    #[tokio::test]
    async fn remove_only_decrements_on_actual_removal() {
        use std::collections::HashMap;
        use tokio::sync::RwLock;

        let m = Arc::new(test_metrics());
        let cache: Arc<RwLock<HashMap<String, u32>>> = Arc::new(RwLock::new(HashMap::new()));
        cache.write().await.insert("key".to_string(), 42);
        m.objects_in_backoff_inc();

        let mut handles = Vec::new();
        for _ in 0..100 {
            let m = m.clone();
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                let removed = cache.write().await.remove("key");
                if removed.is_some() {
                    m.objects_in_backoff_dec();
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(m.objects_in_backoff_count(), 0);
        assert!(cache.read().await.is_empty());
    }

    #[tokio::test]
    async fn reconcile_outcome_record_records_changed() {
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(std::time::Duration::from_millis(50))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("test");
        let m = ControllerMetrics::new("test-ctrl", &meter);

        m.reconcile_outcome_record(KANIDM_OUTCOME_CHANGED);

        provider.force_flush().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains("reconcile_outcome"));
        assert!(text.contains("changed"));
        assert!(text.contains("test-ctrl"));
    }

    #[tokio::test]
    async fn reconcile_outcome_record_records_unchanged() {
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(std::time::Duration::from_millis(50))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("test");
        let m = ControllerMetrics::new("test-ctrl", &meter);

        m.reconcile_outcome_record(KANIDM_OUTCOME_UNCHANGED);

        provider.force_flush().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains("reconcile_outcome"));
        assert!(text.contains("unchanged"));
    }

    #[tokio::test]
    async fn reconcile_outcome_record_records_error() {
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        let exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(exporter.clone())
            .with_interval(std::time::Duration::from_millis(50))
            .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        let meter = provider.meter("test");
        let m = ControllerMetrics::new("test-ctrl", &meter);

        m.reconcile_outcome_record(KANIDM_OUTCOME_ERROR);

        provider.force_flush().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let metrics = exporter.get_finished_metrics().unwrap();
        let text = format!("{:?}", metrics);
        assert!(text.contains("reconcile_outcome"));
        assert!(text.contains("error"));
    }
}
