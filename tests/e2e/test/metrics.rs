use super::{poll_until, setup_kanidm_connection, stabilization_delay, wait_for};
use crate::test::mail_sender::{cleanup_mail_sender, create_smtp_secret};

use kaniop_person::crd::KanidmPersonAccount;

use std::process::{Child, Command, Stdio};

use kube::{
    Api,
    api::{DeleteParams, Patch, PatchParams, PostParams},
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

e2e_test!(
    #[serial_test::serial(metrics)]
    metrics_mail_sender_group_membership_unchanged_in_steady_state,
    {
        use k8s_openapi::jiff::Timestamp;
        use kaniop_operator::kanidm::crd::MailSenderSpec;

        let name = "test-metrics-mail-sender-steady";
        let s = setup_kanidm_connection(name).await;

        let smtp_secret_name = format!("{name}-smtp-credentials");
        create_smtp_secret(&s.client, &smtp_secret_name, "smtp-user", "smtp-password").await;

        let kanidm_api =
            Api::<kaniop_operator::kanidm::crd::Kanidm>::namespaced(s.client.clone(), "default");
        let mut kanidm = kanidm_api.get(name).await.unwrap();
        kanidm.spec.mail_sender = Some(MailSenderSpec {
            relay: "smtps://smtp.example.com".to_string(),
            credentials_secret: kaniop_operator::kanidm::crd::MailSenderCredentialsSecret {
                name: smtp_secret_name.clone(),
                ..Default::default()
            },
            from_address: "kanidm@example.com".to_string(),
            ..Default::default()
        });
        kanidm.metadata.managed_fields = None;
        kanidm_api
            .patch(
                name,
                &PatchParams::apply("e2e-test").force(),
                &Patch::Apply(&kanidm),
            )
            .await
            .unwrap();

        wait_for(
            kanidm_api.clone(),
            name,
            crate::test::kanidm::is_kanidm("Available"),
        )
        .await;
        wait_for(
            kanidm_api.clone(),
            name,
            crate::test::mail_sender::is_mail_sender_ready(),
        )
        .await;

        tokio::time::sleep(stabilization_delay()).await;

        let pf = PortForward::start(
            METRICS_LOCAL_PORT,
            OPERATOR_NAMESPACE,
            OPERATOR_SERVICE,
            8080,
        );
        pf.wait_ready(METRICS_LOCAL_PORT, 20);

        let baseline_metrics = scrape_metrics(METRICS_LOCAL_PORT);
        let baseline_get_unchanged = extract_metric_value(
            &baseline_metrics,
            "kaniop_kanidm_sdk_calls_total",
            &[
                ("resource", "MailSender"),
                ("operation", "get"),
                ("outcome", "unchanged"),
            ],
        )
        .unwrap_or(0);

        kanidm_api
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Merge(&json!({"metadata": {"annotations": {"kanidm/force-update": Timestamp::now().to_string()}}})),
            )
            .await
            .unwrap();

        wait_for(
            kanidm_api.clone(),
            name,
            crate::test::kanidm::is_kanidm("Available"),
        )
        .await;

        tokio::time::sleep(stabilization_delay()).await;

        let final_get_unchanged = poll_until("get/unchanged metric to increase", || async {
            let metrics = scrape_metrics(METRICS_LOCAL_PORT);
            let current = extract_metric_value(
                &metrics,
                "kaniop_kanidm_sdk_calls_total",
                &[
                    ("resource", "MailSender"),
                    ("operation", "get"),
                    ("outcome", "unchanged"),
                ],
            )
            .unwrap_or(0);
            (current > baseline_get_unchanged).then_some(current)
        })
        .await;

        assert!(
            final_get_unchanged > baseline_get_unchanged,
            "Expected get/unchanged counter to increase from {} after forced reconcile, \
             but got {}. This indicates ensure_mail_sender_in_group is not recording GET calls.",
            baseline_get_unchanged,
            final_get_unchanged
        );

        cleanup_mail_sender(&s.client, &kanidm_api, name, &smtp_secret_name).await;
    }
);

fn extract_metric_value(metrics: &str, metric_name: &str, labels: &[(&str, &str)]) -> Option<u64> {
    let pattern_start = format!(r#"{}{{"#, metric_name);

    metrics
        .lines()
        .filter(|line| line.contains(&pattern_start) && !line.starts_with('#'))
        .filter_map(|line| {
            let after_metric = line.strip_prefix(&pattern_start)?;
            let (labels_part, value_part) = after_metric.rsplit_once('}')?;
            let value = value_part.trim().parse().ok()?;
            let labels_str = labels_part.trim();

            let line_labels: Vec<(&str, &str)> = labels_str
                .split(',')
                .filter_map(|label| {
                    let label = label.trim();
                    let (k, v) = label.split_once('=')?;
                    let v = v.trim_matches('"');
                    Some((k.trim(), v))
                })
                .collect();

            let all_labels_match = labels.iter().all(|(k, v)| {
                line_labels
                    .iter()
                    .any(|(line_k, line_v)| *line_k == *k && *line_v == *v)
            });

            all_labels_match.then_some(value)
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_metric_value_basic() {
        let metrics = "kaniop_test_metric{foo=\"bar\"} 42\n";
        let result = extract_metric_value(metrics, "kaniop_test_metric", &[("foo", "bar")]);
        assert_eq!(result, Some(42));
    }

    #[test]
    fn test_extract_metric_value_missing_metric() {
        let metrics = "kaniop_other_metric{foo=\"bar\"} 42\n";
        let result = extract_metric_value(metrics, "kaniop_test_metric", &[("foo", "bar")]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_metric_value_partial_labels_match() {
        let metrics = "kaniop_test_metric{foo=\"bar\",baz=\"qux\"} 50\n";
        let result = extract_metric_value(metrics, "kaniop_test_metric", &[("foo", "wrong")]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_metric_value_multiple_lines() {
        let metrics = r#"kaniop_test_metric{foo="bar"} 10
kaniop_test_metric{foo="baz"} 20
"#;
        let result = extract_metric_value(metrics, "kaniop_test_metric", &[("foo", "baz")]);
        assert_eq!(result, Some(20));
    }

    #[test]
    fn test_extract_metric_value_comments_ignored() {
        let metrics = r#"# HELP kaniop_test_metric help text
# TYPE kaniop_test_metric counter
kaniop_test_metric{foo="bar"} 30
"#;
        let result = extract_metric_value(metrics, "kaniop_test_metric", &[("foo", "bar")]);
        assert_eq!(result, Some(30));
    }

    #[test]
    fn test_extract_metric_value_histogram_bucket() {
        let metrics = r#"kaniop_histogram_bucket{le="0.5",foo="bar"} 15
kaniop_histogram_bucket{le="+Inf",foo="bar"} 25
"#;
        let result = extract_metric_value(
            metrics,
            "kaniop_histogram_bucket",
            &[("le", "0.5"), ("foo", "bar")],
        );
        assert_eq!(result, Some(15));
    }

    #[test]
    fn test_extract_metric_value_multiple_labels_different_order() {
        let metrics = r#"kaniop_test_metric{c="3",a="1",b="2"} 99
"#;
        let result = extract_metric_value(
            metrics,
            "kaniop_test_metric",
            &[("b", "2"), ("c", "3"), ("a", "1")],
        );
        assert_eq!(result, Some(99));
    }
}
