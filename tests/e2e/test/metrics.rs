use super::{setup_kanidm_connection, stabilization_delay, wait_for};

use kaniop_person::crd::KanidmPersonAccount;

use std::process::{Child, Command, Stdio};

use kube::{
    Api,
    api::{DeleteParams, PostParams},
    runtime::wait::Condition,
};
use serde_json::json;

const KANIDM_NAME: &str = "test-metrics";
const OPERATOR_NAMESPACE: &str = "kaniop";
const OPERATOR_SERVICE: &str = "kaniop";
const METRICS_LOCAL_PORT: u16 = 19090;

struct PortForward(Child);

impl PortForward {
    fn start(local_port: u16, namespace: &str, service: &str, target_port: u16) -> Self {
        let child = Command::new("kubectl")
            .args([
                "port-forward",
                "-n",
                namespace,
                &format!("svc/{service}"),
                &format!("{local_port}:{target_port}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start kubectl port-forward");
        Self(child)
    }

    fn wait_ready(&self, local_port: u16, max_attempts: u32) {
        let url = format!("http://127.0.0.1:{local_port}/healthz");
        for _ in 0..max_attempts {
            if ureq::get(&url).call().is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        panic!("port-forward not ready after {max_attempts} attempts");
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scrape_metrics(local_port: u16) -> String {
    let url = format!("http://127.0.0.1:{local_port}/metrics");
    for _ in 0..40 {
        if let Ok(response) = ureq::get(&url).call()
            && let Ok(metrics) = response.into_body().read_to_string()
            && metrics.contains("kaniop_")
        {
            return metrics;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("metrics not available after 40 attempts");
}

fn is_person_ready() -> impl Condition<KanidmPersonAccount> + 'static {
    move |obj: Option<&KanidmPersonAccount>| {
        obj.and_then(|p| p.status.as_ref()).is_some_and(|s| s.ready)
    }
}

e2e_test!(
    #[serial_test::serial(metrics)]
    metrics_exposed_after_person_reconcile,
    {
        let name = "test-metrics-person-reconcile";
        let s = setup_kanidm_connection(KANIDM_NAME).await;

        let person_spec = json!({
            "kanidmRef": {
                "name": KANIDM_NAME,
            },
            "personAttributes": {
                "displayname": "Metrics Test",
                "mail": ["metrics@example.com"],
            },
        });
        let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        person_api
            .create(&PostParams::default(), &person)
            .await
            .unwrap();

        wait_for(person_api.clone(), name, is_person_ready()).await;

        tokio::time::sleep(stabilization_delay()).await;

        let pf = PortForward::start(
            METRICS_LOCAL_PORT,
            OPERATOR_NAMESPACE,
            OPERATOR_SERVICE,
            8080,
        );
        pf.wait_ready(METRICS_LOCAL_PORT, 20);

        let metrics = scrape_metrics(METRICS_LOCAL_PORT);

        assert!(
            metrics.contains("kaniop_kanidm_sdk_calls_total"),
            "kanidm_sdk_calls metric missing"
        );
        assert!(
            metrics.contains("kaniop_kanidm_sdk_call_duration_seconds"),
            "kanidm_sdk_call_duration metric missing"
        );
        assert!(
            metrics.contains("kaniop_reconcile_outcome_total"),
            "reconcile_outcome metric missing"
        );

        assert!(
            metrics.contains(r#"resource="Person""#),
            "Person resource label missing from kanidm_sdk_calls"
        );
        assert!(
            metrics.contains(r#"operation="create""#),
            "create operation label missing from kanidm_sdk_calls"
        );
        assert!(
            metrics.contains(r#"outcome="changed""#),
            "changed outcome missing from metrics"
        );

        assert!(
            metrics.contains(r#"resource="Person""#),
            "Person resource label missing from kanidm_sdk_calls"
        );

        let known_outcomes = ["changed", "unchanged"];
        for outcome in known_outcomes {
            let label = format!(r#"outcome="{outcome}""#);
            assert!(
                metrics.contains(&label),
                "expected bounded outcome {outcome} not found"
            );
        }

        assert!(
            metrics.contains("kaniop_active_reconciles"),
            "active_reconciles gauge missing"
        );

        person_api.delete(name, &DeleteParams::default()).await.ok();
    }
);

e2e_test!(
    #[serial_test::serial(metrics)]
    metrics_kanidm_sdk_operations_bounded,
    {
        let name = "test-metrics-sdk-ops-bounded";
        let s = setup_kanidm_connection(KANIDM_NAME).await;

        let person_spec = json!({
            "kanidmRef": {
                "name": KANIDM_NAME,
            },
            "personAttributes": {
                "displayname": "Metrics SDK Ops Test",
            },
        });
        let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        person_api
            .create(&PostParams::default(), &person)
            .await
            .unwrap();

        wait_for(person_api.clone(), name, is_person_ready()).await;

        tokio::time::sleep(stabilization_delay()).await;

        let pf = PortForward::start(
            METRICS_LOCAL_PORT,
            OPERATOR_NAMESPACE,
            OPERATOR_SERVICE,
            8080,
        );
        pf.wait_ready(METRICS_LOCAL_PORT, 20);

        let metrics = scrape_metrics(METRICS_LOCAL_PORT);

        let expected_operations = ["get", "create", "update"];
        for op in expected_operations {
            let label = format!(r#"operation="{op}""#);
            assert!(
                metrics.contains(&label),
                "expected bounded operation {op} not found in metrics"
            );
        }

        assert!(
            metrics.contains("kaniop_kanidm_sdk_call_duration_seconds_bucket"),
            "histogram buckets missing for kanidm_sdk_call_duration"
        );
        assert!(
            metrics.contains("kaniop_kanidm_sdk_call_duration_seconds_count"),
            "histogram count missing for kanidm_sdk_call_duration"
        );

        person_api.delete(name, &DeleteParams::default()).await.ok();
    }
);

e2e_test!(
    #[serial_test::serial(metrics)]
    metrics_reconcile_outcome_per_controller,
    {
        let name = "test-metrics-reconcile-outcome";
        let s = setup_kanidm_connection(KANIDM_NAME).await;

        let person_spec = json!({
            "kanidmRef": {
                "name": KANIDM_NAME,
            },
            "personAttributes": {
                "displayname": "Reconcile Outcome Test",
            },
        });
        let person = KanidmPersonAccount::new(name, serde_json::from_value(person_spec).unwrap());
        let person_api = Api::<KanidmPersonAccount>::namespaced(s.client.clone(), "default");
        person_api
            .create(&PostParams::default(), &person)
            .await
            .unwrap();

        wait_for(person_api.clone(), name, is_person_ready()).await;

        tokio::time::sleep(stabilization_delay()).await;

        let pf = PortForward::start(
            METRICS_LOCAL_PORT,
            OPERATOR_NAMESPACE,
            OPERATOR_SERVICE,
            8080,
        );
        pf.wait_ready(METRICS_LOCAL_PORT, 20);

        let metrics = scrape_metrics(METRICS_LOCAL_PORT);

        let known_controllers = ["kanidm", "person-account"];
        for controller in known_controllers {
            let label = format!(r#"controller="{controller}""#);
            assert!(
                metrics.contains(&label),
                "expected controller label {controller} not found in reconcile_outcome"
            );
        }

        assert!(
            metrics.contains(r#"outcome="changed""#),
            "changed outcome missing from reconcile_outcome"
        );

        person_api.delete(name, &DeleteParams::default()).await.ok();
    }
);
