//! Status of a `KanidmRadiusServer`: a pure builder plus the SSA write.
//!
//! Status is a report and cache only. Every pass rebuilds it from what the
//! pass established ([`StatusReport`]) and the live Deployment and Service.
//! All five conditions are always emitted, keyed by `type`; a condition's
//! `lastTransitionTime` moves only when its status changes.

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Service;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kaniop_k8s_util::error::{Error, Result};
use kube::api::{Api, Patch};
use kube::{Client, Resource, ResourceExt};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::crd::{KanidmRadiusServer, KanidmRadiusServerStatus};
use crate::reconcile::apply::apply_params;
use crate::reconcile::error::{Failure, RadiusBlock};

pub const TYPE_IDENTITY_READY: &str = "IdentityReady";
pub const TYPE_CONFIGURATION_READY: &str = "ConfigurationReady";
pub const TYPE_AVAILABLE: &str = "Available";
pub const TYPE_EXPOSED: &str = "Exposed";
pub const TYPE_ROTATING: &str = "Rotating";

/// Longest condition message written; longer transient error texts are cut.
const MAX_MESSAGE_LEN: usize = 512;

/// The five conditions, in the order they are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionKind {
    IdentityReady,
    ConfigurationReady,
    Available,
    Exposed,
    Rotating,
}

impl ConditionKind {
    pub const ALL: [ConditionKind; 5] = [
        Self::IdentityReady,
        Self::ConfigurationReady,
        Self::Available,
        Self::Exposed,
        Self::Rotating,
    ];

    pub fn type_name(self) -> &'static str {
        match self {
            Self::IdentityReady => TYPE_IDENTITY_READY,
            Self::ConfigurationReady => TYPE_CONFIGURATION_READY,
            Self::Available => TYPE_AVAILABLE,
            Self::Exposed => TYPE_EXPOSED,
            Self::Rotating => TYPE_ROTATING,
        }
    }
}

/// Condition status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Truth {
    True,
    False,
    Unknown,
}

impl Truth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::True => "True",
            Self::False => "False",
            Self::Unknown => "Unknown",
        }
    }
}

/// One condition's status, reason and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub truth: Truth,
    pub reason: String,
    pub message: String,
}

impl Verdict {
    pub fn new(truth: Truth, reason: &str, message: impl Into<String>) -> Self {
        Self {
            truth,
            reason: reason.to_string(),
            message: message.into(),
        }
    }

    pub fn yes(reason: &str, message: impl Into<String>) -> Self {
        Self::new(Truth::True, reason, message)
    }

    pub fn no(reason: &str, message: impl Into<String>) -> Self {
        Self::new(Truth::False, reason, message)
    }

    pub fn unknown(reason: &str, message: impl Into<String>) -> Self {
        Self::new(Truth::Unknown, reason, message)
    }

    /// A condition the pass stopped before reaching.
    pub fn not_evaluated() -> Self {
        Self::unknown(
            "NotEvaluated",
            "not evaluated: an earlier step did not complete",
        )
    }
}

/// Token facts a pass observed after its writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenFacts {
    /// The token the Deployment template runs with, and when Kanidm issued it.
    pub current: Option<(Uuid, OffsetDateTime)>,
    /// Kanidm expiry of the current token; `None` never expires.
    pub current_expiry: Option<OffsetDateTime>,
    pub config_secret_name: Option<String>,
    pub retiring: Vec<Uuid>,
}

/// What one reconcile pass established. `available`/`exposed` override the
/// verdict derived from the Deployment/Service (a foreign object, or an
/// apply that failed); `tokens` is `None` when the pass did not observe
/// tokens, and the previous status values are then kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusReport {
    pub identity: Verdict,
    pub configuration: Verdict,
    pub rotating: Verdict,
    pub available: Option<Verdict>,
    pub exposed: Option<Verdict>,
    pub tokens: Option<TokenFacts>,
}

impl Default for StatusReport {
    fn default() -> Self {
        Self {
            identity: Verdict::not_evaluated(),
            configuration: Verdict::not_evaluated(),
            rotating: Verdict::not_evaluated(),
            available: None,
            exposed: None,
            tokens: None,
        }
    }
}

impl StatusReport {
    /// Record why the pass stopped on the condition it belongs to. The
    /// conditions of steps already completed keep their verdict.
    pub fn record_failure(&mut self, failure: &Failure) {
        match failure {
            Failure::Block(block) => self.set(block.condition(), block_verdict(block)),
            Failure::Transient { condition, error } => self.set(
                *condition,
                Verdict::unknown("TransientError", bounded(error.to_string())),
            ),
        }
    }

    fn set(&mut self, kind: ConditionKind, verdict: Verdict) {
        match kind {
            ConditionKind::IdentityReady => self.identity = verdict,
            ConditionKind::ConfigurationReady => self.configuration = verdict,
            ConditionKind::Rotating => self.rotating = verdict,
            ConditionKind::Available => self.available = Some(verdict),
            ConditionKind::Exposed => self.exposed = Some(verdict),
        }
    }
}

/// The verdict for a block, on the condition it belongs to.
pub fn block_verdict(block: &RadiusBlock) -> Verdict {
    // A withheld or failed rotation is still a rotation in progress; every
    // other block makes its condition false.
    let truth = match block.condition() {
        ConditionKind::Rotating => Truth::True,
        _ => Truth::False,
    };
    Verdict::new(truth, block.reason(), bounded(block.to_string()))
}

fn bounded(mut message: String) -> String {
    if message.len() > MAX_MESSAGE_LEN {
        let mut end = MAX_MESSAGE_LEN - 3;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push_str("...");
    }
    message
}

/// `Available`: the rollout is complete and every desired replica is
/// available. A rollout in progress is `False` even while old pods serve.
pub fn available_verdict(deployment: Option<&Deployment>) -> Verdict {
    let Some(deployment) = deployment else {
        return Verdict::no("DeploymentMissing", "the RADIUS Deployment does not exist");
    };
    let desired = deployment
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(1);
    let status = deployment.status.clone().unwrap_or_default();
    let stuck = status.conditions.iter().flatten().any(|c| {
        c.type_ == "Progressing"
            && c.status == "False"
            && c.reason.as_deref() == Some("ProgressDeadlineExceeded")
    });
    if stuck {
        return Verdict::no(
            "ProgressDeadlineExceeded",
            "the Deployment rollout exceeded its progress deadline",
        );
    }
    let observed = match (deployment.metadata.generation, status.observed_generation) {
        (Some(generation), Some(observed)) => observed >= generation,
        (Some(_), None) => false,
        (None, _) => true,
    };
    let total = status.replicas.unwrap_or(0);
    let updated = status.updated_replicas.unwrap_or(0);
    let available = status.available_replicas.unwrap_or(0);
    if !observed || updated < desired || total > updated {
        return Verdict::no(
            "RolloutInProgress",
            format!("{updated}/{desired} replicas updated, {available} available, {total} total"),
        );
    }
    if available < desired {
        return Verdict::no(
            "ReplicasUnavailable",
            format!("{available}/{desired} replicas available"),
        );
    }
    Verdict::yes(
        "RolloutComplete",
        format!("{available}/{desired} replicas available"),
    )
}

/// `Exposed` and the endpoint clients should use.
pub fn exposed_verdict(service: Option<&Service>) -> (Verdict, Option<String>) {
    let Some(spec) = service.and_then(|s| s.spec.as_ref()) else {
        return (
            Verdict::no("ServiceMissing", "the RADIUS Service does not exist"),
            None,
        );
    };
    let port = spec.ports.iter().flatten().next();
    let service_port = port.map_or(0, |p| p.port);
    match spec.type_.as_deref().unwrap_or("ClusterIP") {
        "LoadBalancer" => {
            let address = service
                .and_then(|s| s.status.as_ref())
                .and_then(|s| s.load_balancer.as_ref())
                .and_then(|lb| lb.ingress.as_ref())
                .and_then(|ingress| {
                    ingress
                        .iter()
                        .find_map(|i| i.ip.clone().or_else(|| i.hostname.clone()))
                });
            match address {
                Some(address) => (
                    Verdict::yes(
                        "LoadBalancerReady",
                        format!("load balancer address {address}"),
                    ),
                    Some(host_port(&address, service_port)),
                ),
                None => (
                    Verdict::no(
                        "LoadBalancerPending",
                        "waiting for the load balancer to assign an address",
                    ),
                    None,
                ),
            }
        }
        "NodePort" => match port.and_then(|p| p.node_port) {
            Some(node_port) => (
                Verdict::yes(
                    "NodePortAssigned",
                    format!("node port {node_port} on every node"),
                ),
                Some(format!("*:{node_port}")),
            ),
            None => (
                Verdict::no("NodePortPending", "waiting for a node port to be assigned"),
                None,
            ),
        },
        _ => match spec
            .cluster_ip
            .as_deref()
            .filter(|ip| !ip.is_empty() && *ip != "None")
        {
            Some(ip) => (
                Verdict::yes("ClusterIPAssigned", format!("cluster IP {ip}")),
                Some(host_port(ip, service_port)),
            ),
            None => (
                Verdict::no("ClusterIPPending", "waiting for a cluster IP"),
                None,
            ),
        },
    }
}

/// `host:port`, with an IPv6 address in brackets.
fn host_port(host: &str, port: i32) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// `ready` is true only when identity, configuration, availability and
/// exposure are all `True`. `Rotating` is informational.
pub fn is_ready(conditions: &[Condition]) -> bool {
    [
        TYPE_IDENTITY_READY,
        TYPE_CONFIGURATION_READY,
        TYPE_AVAILABLE,
        TYPE_EXPOSED,
    ]
    .iter()
    .all(|type_| {
        conditions
            .iter()
            .any(|c| c.type_ == *type_ && c.status == Truth::True.as_str())
    })
}

/// Kanidm token time as a Kubernetes timestamp.
pub fn kube_time(at: OffsetDateTime) -> Option<Time> {
    let nanos = i32::try_from(at.nanosecond()).ok()?;
    Timestamp::new(at.unix_timestamp(), nanos).ok().map(Time)
}

/// Build the full status. Pure: `now` is the transition time of a condition
/// whose status changed.
pub fn build_status(
    generation: Option<i64>,
    previous: Option<&KanidmRadiusServerStatus>,
    report: &StatusReport,
    deployment: Option<&Deployment>,
    service: Option<&Service>,
    now: &Time,
) -> KanidmRadiusServerStatus {
    let (exposed, endpoint) = exposed_verdict(service);
    let available = report
        .available
        .clone()
        .unwrap_or_else(|| available_verdict(deployment));
    let exposed = report.exposed.clone().unwrap_or(exposed);
    let previous_conditions = previous
        .and_then(|p| p.conditions.as_deref())
        .unwrap_or(&[]);
    let conditions: Vec<Condition> = ConditionKind::ALL
        .iter()
        .map(|kind| {
            let verdict = match kind {
                ConditionKind::IdentityReady => &report.identity,
                ConditionKind::ConfigurationReady => &report.configuration,
                ConditionKind::Available => &available,
                ConditionKind::Exposed => &exposed,
                ConditionKind::Rotating => &report.rotating,
            };
            let type_ = kind.type_name();
            let status = verdict.truth.as_str();
            // The time moves only when the status changes (API conventions).
            let last_transition_time = previous_conditions
                .iter()
                .find(|c| c.type_ == type_ && c.status == status)
                .map_or_else(|| now.clone(), |c| c.last_transition_time.clone());
            Condition {
                type_: type_.to_string(),
                status: status.to_string(),
                reason: verdict.reason.clone(),
                message: verdict.message.clone(),
                last_transition_time,
                observed_generation: generation,
            }
        })
        .collect();

    let (token_id, token_issued_at, config_secret_name, retiring_token_ids) = match &report.tokens {
        Some(facts) => (
            facts.current.map(|(id, _)| id.to_string()),
            facts.current.and_then(|(_, at)| kube_time(at)),
            facts.config_secret_name.clone(),
            facts.retiring.iter().map(Uuid::to_string).collect(),
        ),
        None => (
            previous.and_then(|p| p.token_id.clone()),
            previous.and_then(|p| p.token_issued_at.clone()),
            previous.and_then(|p| p.config_secret_name.clone()),
            previous
                .map(|p| p.retiring_token_ids.clone())
                .unwrap_or_default(),
        ),
    };

    KanidmRadiusServerStatus {
        ready: is_ready(&conditions),
        conditions: Some(conditions),
        observed_generation: generation,
        endpoint,
        token_id,
        token_issued_at,
        config_secret_name,
        deployment_name: deployment.map(ResourceExt::name_any),
        service_name: service.map(ResourceExt::name_any),
        replicas: deployment
            .and_then(|d| d.spec.as_ref())
            .and_then(|s| s.replicas),
        ready_replicas: deployment
            .and_then(|d| d.status.as_ref())
            .and_then(|s| s.ready_replicas),
        retiring_token_ids,
    }
}

/// Server-side apply the status with `force` under our field manager.
/// Returns whether a write was needed (an unchanged status is not written).
pub async fn patch_status(
    client: Client,
    radius: &KanidmRadiusServer,
    status: KanidmRadiusServerStatus,
) -> Result<bool> {
    if radius.status.as_ref() == Some(&status) {
        return Ok(false);
    }
    let namespace = radius.namespace().unwrap_or_default();
    let name = radius.name_any();
    let patch = Patch::Apply(json!({
        "apiVersion": KanidmRadiusServer::api_version(&()),
        "kind": KanidmRadiusServer::kind(&()),
        "status": status,
    }));
    Api::<KanidmRadiusServer>::namespaced(client, &namespace)
        .patch_status(&name, &apply_params(), &patch)
        .await
        .map_err(|e| Error::kube_status_error("KanidmRadiusServer", &namespace, name, e))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::apps::v1::{DeploymentCondition, DeploymentSpec, DeploymentStatus};
    use k8s_openapi::api::core::v1::{
        LoadBalancerIngress, LoadBalancerStatus, ServicePort, ServiceSpec, ServiceStatus,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    use super::*;
    use crate::reconcile::error::ExecError;
    use crate::reconcile::inputs::RadiusPreflightError;

    const TOKEN: Uuid = Uuid::from_u128(0xa);
    const OLD_TOKEN: Uuid = Uuid::from_u128(0xb);

    fn at(seconds: i64) -> Time {
        Time(Timestamp::from_second(seconds).unwrap())
    }

    fn deployment(desired: i32, updated: i32, total: i32, available: i32) -> Deployment {
        Deployment {
            metadata: ObjectMeta {
                name: Some("campus".into()),
                generation: Some(3),
                ..Default::default()
            },
            spec: Some(DeploymentSpec {
                replicas: Some(desired),
                ..Default::default()
            }),
            status: Some(DeploymentStatus {
                observed_generation: Some(3),
                replicas: Some(total),
                updated_replicas: Some(updated),
                available_replicas: Some(available),
                ready_replicas: Some(available),
                ..Default::default()
            }),
        }
    }

    fn healthy_deployment() -> Deployment {
        deployment(2, 2, 2, 2)
    }

    fn service(type_: &str) -> Service {
        Service {
            metadata: ObjectMeta {
                name: Some("campus".into()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: Some(type_.into()),
                cluster_ip: Some("10.96.0.10".into()),
                ports: Some(vec![ServicePort {
                    port: 1812,
                    node_port: (type_ != "ClusterIP").then_some(31812),
                    protocol: Some("UDP".into()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            status: None,
        }
    }

    fn load_balancer(ingress: Vec<LoadBalancerIngress>) -> Service {
        Service {
            status: Some(ServiceStatus {
                load_balancer: Some(LoadBalancerStatus {
                    ingress: Some(ingress),
                }),
                ..Default::default()
            }),
            ..service("LoadBalancer")
        }
    }

    fn all_good() -> StatusReport {
        StatusReport {
            identity: Verdict::yes("AccountReady", "ok"),
            configuration: Verdict::yes("ConfigurationValid", "ok"),
            rotating: Verdict::no("Idle", "ok"),
            available: None,
            exposed: None,
            tokens: Some(TokenFacts {
                current: Some((TOKEN, OffsetDateTime::from_unix_timestamp(1_000).unwrap())),
                current_expiry: None,
                config_secret_name: Some("campus-0123456789".into()),
                retiring: vec![],
            }),
        }
    }

    fn condition<'a>(status: &'a KanidmRadiusServerStatus, type_: &str) -> &'a Condition {
        status
            .conditions
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.type_ == type_)
            .unwrap_or_else(|| panic!("{type_} missing"))
    }

    fn build(
        report: &StatusReport,
        deployment: Option<&Deployment>,
        service: Option<&Service>,
    ) -> KanidmRadiusServerStatus {
        build_status(Some(7), None, report, deployment, service, &at(100))
    }

    #[test]
    fn always_emits_all_five_conditions_in_order() {
        let cases = [
            (StatusReport::default(), None, None),
            (
                all_good(),
                Some(healthy_deployment()),
                Some(service("ClusterIP")),
            ),
        ];
        for (report, deployment, service) in cases {
            let status = build(&report, deployment.as_ref(), service.as_ref());
            let types: Vec<_> = status
                .conditions
                .as_ref()
                .unwrap()
                .iter()
                .map(|c| c.type_.as_str())
                .collect();
            assert_eq!(
                types,
                [
                    "IdentityReady",
                    "ConfigurationReady",
                    "Available",
                    "Exposed",
                    "Rotating"
                ]
            );
            for c in status.conditions.as_ref().unwrap() {
                assert_eq!(c.observed_generation, Some(7));
                assert!(!c.reason.is_empty());
            }
            assert_eq!(status.observed_generation, Some(7));
        }
    }

    #[test]
    fn healthy_world_is_ready_and_reports_fields() {
        let deployment = healthy_deployment();
        let service = service("ClusterIP");
        let status = build(&all_good(), Some(&deployment), Some(&service));
        assert!(status.ready);
        assert_eq!(status.token_id.as_deref(), Some(TOKEN.to_string().as_str()));
        assert_eq!(status.token_issued_at, Some(at(1_000)));
        assert_eq!(
            status.config_secret_name.as_deref(),
            Some("campus-0123456789")
        );
        assert_eq!(status.deployment_name.as_deref(), Some("campus"));
        assert_eq!(status.service_name.as_deref(), Some("campus"));
        assert_eq!(status.replicas, Some(2));
        assert_eq!(status.ready_replicas, Some(2));
        assert_eq!(status.endpoint.as_deref(), Some("10.96.0.10:1812"));
        assert!(status.retiring_token_ids.is_empty());
    }

    #[test]
    fn last_transition_time_is_kept_while_status_is_unchanged() {
        let deployment = healthy_deployment();
        let service = service("ClusterIP");
        let first = build_status(
            Some(7),
            None,
            &all_good(),
            Some(&deployment),
            Some(&service),
            &at(100),
        );
        let second = build_status(
            Some(7),
            Some(&first),
            &all_good(),
            Some(&deployment),
            Some(&service),
            &at(200),
        );
        assert_eq!(
            first, second,
            "an unchanged world must give an identical status"
        );

        // A reason or message change with the same status keeps the time.
        let mut report = all_good();
        report.rotating = Verdict::no("Idle", "different message");
        let third = build_status(
            Some(7),
            Some(&second),
            &report,
            Some(&deployment),
            Some(&service),
            &at(300),
        );
        assert_eq!(
            condition(&third, TYPE_ROTATING).last_transition_time,
            at(100)
        );

        // A status change moves only that condition's time.
        report.rotating = Verdict::yes("Rotating", "minted");
        let fourth = build_status(
            Some(7),
            Some(&third),
            &report,
            Some(&deployment),
            Some(&service),
            &at(400),
        );
        assert_eq!(
            condition(&fourth, TYPE_ROTATING).last_transition_time,
            at(400)
        );
        assert_eq!(
            condition(&fourth, TYPE_AVAILABLE).last_transition_time,
            at(100)
        );
        assert_eq!(
            condition(&fourth, TYPE_IDENTITY_READY).last_transition_time,
            at(100)
        );
    }

    #[test]
    fn ready_follows_the_condition_matrix() {
        let deployment = healthy_deployment();
        let service = service("ClusterIP");
        let base = build(&all_good(), Some(&deployment), Some(&service));
        assert!(base.ready);

        // Rotating does not affect readiness.
        let mut rotating = all_good();
        rotating.rotating = Verdict::yes("Rotating", "retiring");
        assert!(build(&rotating, Some(&deployment), Some(&service)).ready);

        for kind in [
            ConditionKind::IdentityReady,
            ConditionKind::ConfigurationReady,
            ConditionKind::Available,
            ConditionKind::Exposed,
        ] {
            for truth in [Truth::False, Truth::Unknown] {
                let mut report = all_good();
                report.set(kind, Verdict::new(truth, "Blocked", "x"));
                let status = build(&report, Some(&deployment), Some(&service));
                assert!(!status.ready, "{kind:?}={truth:?} must not be ready");
                assert!(!is_ready(status.conditions.as_ref().unwrap()));
            }
        }
        // Missing Deployment or Service is not ready.
        assert!(!build(&all_good(), None, Some(&service)).ready);
        assert!(!build(&all_good(), Some(&deployment), None).ready);
    }

    #[test]
    fn available_requires_a_complete_rollout() {
        let cases = [
            (Some(deployment(2, 2, 2, 2)), Truth::True, "RolloutComplete"),
            // New pods surging while old ones still serve.
            (
                Some(deployment(2, 1, 3, 2)),
                Truth::False,
                "RolloutInProgress",
            ),
            // All updated, old pod still terminating in the count.
            (
                Some(deployment(2, 2, 3, 2)),
                Truth::False,
                "RolloutInProgress",
            ),
            (
                Some(deployment(2, 2, 2, 1)),
                Truth::False,
                "ReplicasUnavailable",
            ),
            (None, Truth::False, "DeploymentMissing"),
        ];
        for (deployment, truth, reason) in cases {
            let verdict = available_verdict(deployment.as_ref());
            assert_eq!((verdict.truth, verdict.reason.as_str()), (truth, reason));
        }

        // Status from an older generation is not trusted.
        let mut stale = deployment(2, 2, 2, 2);
        stale.status.as_mut().unwrap().observed_generation = Some(2);
        assert_eq!(available_verdict(Some(&stale)).reason, "RolloutInProgress");

        let mut stuck = deployment(2, 1, 3, 2);
        stuck.status.as_mut().unwrap().conditions = Some(vec![DeploymentCondition {
            type_: "Progressing".into(),
            status: "False".into(),
            reason: Some("ProgressDeadlineExceeded".into()),
            ..Default::default()
        }]);
        assert_eq!(
            available_verdict(Some(&stuck)).reason,
            "ProgressDeadlineExceeded"
        );
    }

    #[test]
    fn endpoint_per_service_type() {
        let (verdict, endpoint) = exposed_verdict(Some(&service("ClusterIP")));
        assert_eq!(verdict.truth, Truth::True);
        assert_eq!(endpoint.as_deref(), Some("10.96.0.10:1812"));

        let (verdict, endpoint) = exposed_verdict(Some(&service("NodePort")));
        assert_eq!(verdict.truth, Truth::True);
        assert_eq!(endpoint.as_deref(), Some("*:31812"));

        // A LoadBalancer without an address is not exposed, whatever the
        // cluster IP says.
        let (verdict, endpoint) = exposed_verdict(Some(&service("LoadBalancer")));
        assert_eq!(
            (verdict.truth, verdict.reason.as_str()),
            (Truth::False, "LoadBalancerPending")
        );
        assert_eq!(endpoint, None);
        let (verdict, endpoint) = exposed_verdict(Some(&load_balancer(vec![])));
        assert_eq!(verdict.truth, Truth::False);
        assert_eq!(endpoint, None);

        let (verdict, endpoint) =
            exposed_verdict(Some(&load_balancer(vec![LoadBalancerIngress {
                ip: Some("203.0.113.7".into()),
                ..Default::default()
            }])));
        assert_eq!(verdict.truth, Truth::True);
        assert_eq!(endpoint.as_deref(), Some("203.0.113.7:1812"));

        let (verdict, endpoint) =
            exposed_verdict(Some(&load_balancer(vec![LoadBalancerIngress {
                hostname: Some("radius.example.net".into()),
                ..Default::default()
            }])));
        assert_eq!(verdict.truth, Truth::True);
        assert_eq!(endpoint.as_deref(), Some("radius.example.net:1812"));

        let (_, endpoint) = exposed_verdict(Some(&load_balancer(vec![LoadBalancerIngress {
            ip: Some("2001:db8::7".into()),
            ..Default::default()
        }])));
        assert_eq!(endpoint.as_deref(), Some("[2001:db8::7]:1812"));

        let (verdict, endpoint) = exposed_verdict(None);
        assert_eq!(
            (verdict.truth, verdict.reason.as_str()),
            (Truth::False, "ServiceMissing")
        );
        assert_eq!(endpoint, None);
    }

    #[test]
    fn invalid_input_keeps_available_reflecting_the_running_deployment() {
        // A NAS Secret key was deleted: the pass stops at input validation,
        // touches nothing, and the running Deployment still reports Available.
        let previous = build(
            &all_good(),
            Some(&healthy_deployment()),
            Some(&service("ClusterIP")),
        );
        let mut report = StatusReport::default();
        report.record_failure(&Failure::Block(RadiusBlock::Inputs(
            RadiusPreflightError::NasSecretKeyUnavailable,
        )));
        let status = build_status(
            Some(8),
            Some(&previous),
            &report,
            Some(&healthy_deployment()),
            Some(&service("ClusterIP")),
            &at(500),
        );
        let configuration = condition(&status, TYPE_CONFIGURATION_READY);
        assert_eq!(configuration.status, "False");
        assert_eq!(configuration.reason, "NasSecretKeyInvalid");
        assert_eq!(condition(&status, TYPE_AVAILABLE).status, "True");
        assert_eq!(condition(&status, TYPE_EXPOSED).status, "True");
        assert!(!status.ready);
        // Token facts were not observed: the last known ones are kept.
        assert_eq!(status.token_id, previous.token_id);
        assert_eq!(status.config_secret_name, previous.config_secret_name);
    }

    #[test]
    fn failures_land_on_their_condition() {
        let cases: Vec<(Failure, ConditionKind, Truth, &str)> = vec![
            (
                Failure::exec(ConditionKind::IdentityReady)(ExecError::AccountNotOwned {
                    account: "kaniop_radius_x".into(),
                }),
                ConditionKind::IdentityReady,
                Truth::False,
                "OwnershipConflict",
            ),
            (
                Failure::exec(ConditionKind::Available)(ExecError::ForeignObject {
                    kind: "Deployment".into(),
                    namespace: "ns".into(),
                    name: "campus".into(),
                }),
                ConditionKind::Available,
                Truth::False,
                "ForeignObject",
            ),
            (
                Failure::exec(ConditionKind::Exposed)(ExecError::ForeignObject {
                    kind: "Service".into(),
                    namespace: "ns".into(),
                    name: "campus".into(),
                }),
                ConditionKind::Exposed,
                Truth::False,
                "ForeignObject",
            ),
            (
                Failure::Block(RadiusBlock::TooManyTokens),
                ConditionKind::Rotating,
                Truth::True,
                "TooManyTokens",
            ),
            // The object decides the condition: a foreign Deployment found by
            // the observation of the rotation stage is still `Available`.
            (
                Failure::exec(ConditionKind::Rotating)(ExecError::ForeignObject {
                    kind: "Deployment".into(),
                    namespace: "ns".into(),
                    name: "campus".into(),
                }),
                ConditionKind::Available,
                Truth::False,
                "ForeignObject",
            ),
            // Otherwise the stage that raised the block decides.
            (
                Failure::exec(ConditionKind::IdentityReady)(ExecError::MissingMetadata {
                    kind: "Entry",
                    field: "uuid",
                }),
                ConditionKind::IdentityReady,
                Truth::False,
                "MissingMetadata",
            ),
            (
                Failure::exec(ConditionKind::Rotating)(ExecError::ForeignObject {
                    kind: "Secret".into(),
                    namespace: "ns".into(),
                    name: "campus-0a1b2c3d4e".into(),
                }),
                ConditionKind::Rotating,
                Truth::True,
                "ForeignObject",
            ),
            (
                Failure::exec(ConditionKind::ConfigurationReady)(ExecError::ConcurrentMutation {
                    kind: "Secret".into(),
                    namespace: "ns".into(),
                    name: "campus-0a1b2c3d4e".into(),
                }),
                ConditionKind::ConfigurationReady,
                Truth::False,
                "ConcurrentMutation",
            ),
            (
                Failure::Transient {
                    condition: ConditionKind::IdentityReady,
                    error: Error::MissingData("kanidm unreachable".into()),
                },
                ConditionKind::IdentityReady,
                Truth::Unknown,
                "TransientError",
            ),
        ];
        for (failure, kind, truth, reason) in cases {
            let mut report = all_good();
            report.record_failure(&failure);
            let status = build(
                &report,
                Some(&healthy_deployment()),
                Some(&service("ClusterIP")),
            );
            let c = condition(&status, kind.type_name());
            assert_eq!(
                (c.status.as_str(), c.reason.as_str()),
                (truth.as_str(), reason),
                "{failure:?}"
            );
            assert!(!c.message.is_empty());
        }
    }

    #[test]
    fn transient_error_keeps_its_source_and_writes_the_condition_first() {
        // The reconciler records the failure on the condition, then hands the
        // same error (with its source) to the backoff.
        let source = kube::Error::Api(
            kube::core::Status::failure("etcd timeout", "InternalError")
                .with_code(500)
                .boxed(),
        );
        let failure = Failure::Transient {
            condition: ConditionKind::ConfigurationReady,
            error: Error::kube_error("get", "Secret", "ns", "nas", source),
        };
        let mut report = all_good();
        report.record_failure(&failure);
        let status = build(
            &report,
            Some(&healthy_deployment()),
            Some(&service("ClusterIP")),
        );
        let c = condition(&status, TYPE_CONFIGURATION_READY);
        assert_eq!(
            (c.status.as_str(), c.reason.as_str()),
            ("Unknown", "TransientError")
        );
        assert!(c.message.contains("etcd timeout"), "{}", c.message);
        let Failure::Transient { error, .. } = failure else {
            unreachable!()
        };
        assert!(error.is_retryable());
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn retiring_tokens_are_reported_by_id() {
        let mut report = all_good();
        report.tokens.as_mut().unwrap().retiring = vec![OLD_TOKEN];
        let status = build(
            &report,
            Some(&healthy_deployment()),
            Some(&service("ClusterIP")),
        );
        assert_eq!(status.retiring_token_ids, [OLD_TOKEN.to_string()]);
    }

    #[test]
    fn long_transient_messages_are_bounded() {
        let mut report = all_good();
        report.record_failure(&Failure::Transient {
            condition: ConditionKind::Rotating,
            error: Error::MissingData("x".repeat(4096)),
        });
        let status = build(&report, None, None);
        assert!(condition(&status, TYPE_ROTATING).message.len() <= MAX_MESSAGE_LEN);
    }
}
