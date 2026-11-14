use super::crd::{Kanidm, KanidmServerRole};
use super::reconcile::CLUSTER_LABEL;
use super::reconcile::statefulset::StatefulSetExt;
use super::restore::RESTORE_ANNOTATION;

use kaniop_k8s_util::error::{Error, Result};

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use kube::runtime::watcher;
use kube::{Api, Client, CustomResource, Resource, ResourceExt};
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info, warn};

pub const CONTROLLER_ID: &str = "kanidm-maintenance";
pub const MAINTENANCE_ANNOTATION: &str = "kanidm.kaniop.rs/maintenance-in-progress";
pub const FORCE_RESUME_ANNOTATION: &str = "maintenance.kaniop.rs/force-resume";
pub const PLAN_CONFIG_MAP_SUFFIX: &str = "maintenance";
pub const PLAN_KEY: &str = "plan.json";
pub const MAINTENANCE_INIT_CONTAINER: &str = "kaniop-maintenance";
pub const MAINTENANCE_INSTALL_CONTAINER: &str = "kaniop-maintenance-runner-install";
pub const MAINTENANCE_RUNNER_PATH: &str = "/opt/kaniop-maintenance/kaniop-maintenance-runner";
pub const MAINTENANCE_PLAN_PATH: &str = "/run/kaniop-maintenance/plan.json";
pub const MAINTENANCE_PLAN_VOLUME: &str = "kaniop-maintenance-plan";
pub const MAINTENANCE_TOOLS_VOLUME: &str = "kaniop-maintenance-tools";
pub const MAINTENANCE_PLAN_MOUNT_PATH: &str = "/run/kaniop-maintenance";
pub const MAINTENANCE_TOOLS_MOUNT_PATH: &str = "/opt/kaniop-maintenance";

const MAINTENANCE_FINALIZER: &str = "kanidmmaintenances.kaniop.rs/finalizer";
const REQUEUE: Duration = Duration::from_secs(2);
const DEFAULT_REPLICA_TIMEOUT_SECONDS: u64 = 30 * 60;
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [{"message": "KanidmMaintenance spec is immutable", "rule": "self == oldSelf"}]))
)]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmMaintenance",
    plural = "kanidmmaintenances",
    singular = "kanidmmaintenance",
    shortname = "idmmaintenance",
    namespaced,
    status = "KanidmMaintenanceStatus",
    printcolumn = r#"{"name":"Target","type":"string","jsonPath":".spec.targetRef.name"}"#,
    printcolumn = r#"{"name":"Operation","type":"string","jsonPath":".spec.operation"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmMaintenanceSpec {
    /// Exact Kanidm object to maintain. The UID prevents a delete/recreate with the same name from
    /// inheriting an old maintenance request.
    pub target_ref: KanidmMaintenanceTargetRef,
    /// Offline Kanidm database operation to execute.
    pub operation: KanidmMaintenanceOperation,
    /// Replica selection. Omitted means every replica, one at a time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<KanidmMaintenanceTarget>,
    /// Permit an operation that intentionally leaves no write-capable replica serving.
    #[serde(default)]
    pub allow_downtime: bool,
    /// Maximum time for one recreated replica to complete maintenance and become Ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replica_timeout_seconds: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmMaintenanceTargetRef {
    pub name: String,
    pub uid: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum KanidmMaintenanceOperation {
    Reindex,
    Verify,
    Vacuum,
}

impl KanidmMaintenanceOperation {
    fn runner_value(self) -> &'static str {
        match self {
            Self::Reindex => "reindex",
            Self::Verify => "verify",
            Self::Vacuum => "vacuum",
        }
    }

    /// Only operations whose arbitrary-interruption retry semantics have been explicitly qualified
    /// may be automatically retried by the init runner. Verify is read-only. Reindex and vacuum
    /// fail closed when a previous runner disappeared while the command was executing.
    fn retry_interrupted(self) -> bool {
        matches!(self, Self::Verify)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum KanidmMaintenanceTarget {
    AllReplicas,
    Instance { replica_group: String, ordinal: i32 },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum KanidmMaintenancePhase {
    #[default]
    Pending,
    Validating,
    AcquiringLock,
    Planning,
    RestartingReplica,
    RecoveringReplica,
    Completed,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceInstanceRef {
    pub replica_group: String,
    pub ordinal: i32,
    pub pod_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmMaintenanceStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_target_uid: Option<String>,
    #[serde(default)]
    pub phase: KanidmMaintenancePhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_target: Option<MaintenanceInstanceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_targets: Vec<MaintenanceInstanceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_pod_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_started_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone)]
struct MaintenanceContext {
    client: Client,
    recorder: Recorder,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerPlan<'a> {
    version: u32,
    active: bool,
    operation_id: &'a str,
    pod_name: &'a str,
    operation: &'a str,
    retry_interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_path: Option<&'a str>,
}

pub fn plan_config_map_name(kanidm_name: &str) -> String {
    format!("{kanidm_name}-{PLAN_CONFIG_MAP_SUFFIX}")
}

pub fn maintenance_runner_image() -> String {
    std::env::var("MAINTENANCE_RUNNER_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/pando85/kaniop:latest".to_string())
}

pub async fn run(client: Client) {
    let api = Api::<KanidmMaintenance>::all(client.clone());
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: CONTROLLER_ID.into(),
            instance: None,
        },
    );
    let ctx = Arc::new(MaintenanceContext { client, recorder });

    info!("starting {CONTROLLER_ID} controller");
    Controller::new(api, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(reconcile_maintenance, error_policy, ctx)
        .for_each(|result| async move {
            if let Err(error) = result {
                error!(%error, "KanidmMaintenance reconciliation failed");
            }
        })
        .await;
}

fn error_policy(
    maintenance: Arc<KanidmMaintenance>,
    error: &Error,
    _ctx: Arc<MaintenanceContext>,
) -> Action {
    warn!(maintenance = %maintenance.name_any(), %error, "maintenance reconciliation error");
    Action::requeue(Duration::from_secs(5))
}

async fn reconcile_maintenance(
    maintenance: Arc<KanidmMaintenance>,
    ctx: Arc<MaintenanceContext>,
) -> Result<Action> {
    let namespace = maintenance
        .namespace()
        .ok_or_else(|| Error::MissingData("KanidmMaintenance has no namespace".to_string()))?;
    let api = Api::<KanidmMaintenance>::namespaced(ctx.client.clone(), &namespace);
    finalizer(&api, MAINTENANCE_FINALIZER, maintenance, |event| {
        let ctx = ctx.clone();
        async move {
            match event {
                Finalizer::Apply(maintenance) => reconcile_apply(maintenance, ctx).await,
                Finalizer::Cleanup(maintenance) => cleanup(maintenance, ctx).await,
            }
        }
    })
    .await
    .map_err(|error| {
        Error::FinalizerError(
            "failed on KanidmMaintenance finalizer".to_string(),
            Box::new(error),
        )
    })
}

async fn reconcile_apply(
    maintenance: Arc<KanidmMaintenance>,
    ctx: Arc<MaintenanceContext>,
) -> Result<Action> {
    let phase = maintenance
        .status
        .as_ref()
        .map(|status| status.phase)
        .unwrap_or_default();

    match phase {
        KanidmMaintenancePhase::Pending => {
            set_phase(&maintenance, &ctx, KanidmMaintenancePhase::Validating, None).await?;
            Ok(Action::requeue(REQUEUE))
        }
        KanidmMaintenancePhase::Validating => match validate(&maintenance, &ctx).await {
            Ok(target) => {
                let mut status = maintenance.status.clone().unwrap_or_default();
                status.observed_target_uid = target.uid();
                status.phase = KanidmMaintenancePhase::AcquiringLock;
                status.message = None;
                patch_status(&maintenance, &ctx, status).await?;
                Ok(Action::requeue(REQUEUE))
            }
            Err(error) => {
                set_phase(
                    &maintenance,
                    &ctx,
                    KanidmMaintenancePhase::Failed,
                    Some(error.to_string()),
                )
                .await?;
                Ok(Action::requeue(Duration::from_secs(300)))
            }
        },
        KanidmMaintenancePhase::AcquiringLock => {
            let target = get_target(&maintenance, &ctx).await?;
            acquire_lock(&maintenance, &target, &ctx).await?;
            mark_maintenance(&maintenance, &target, &ctx).await?;
            set_phase(&maintenance, &ctx, KanidmMaintenancePhase::Planning, None).await?;
            Ok(Action::requeue(REQUEUE))
        }
        KanidmMaintenancePhase::Planning => {
            let target = get_target(&maintenance, &ctx).await?;
            ensure_lock_owned(&maintenance, &target, &ctx).await?;
            let planned = planned_targets(&maintenance, &target)?;
            let status = maintenance.status.clone().unwrap_or_default();
            let next = planned
                .into_iter()
                .find(|candidate| !status.completed_targets.contains(candidate));

            let Some(next) = next else {
                set_plan_inactive(&maintenance, &target, &ctx).await?;
                clear_maintenance(&maintenance, &target, &ctx).await?;
                release_lock(&maintenance, &target, &ctx).await?;
                set_phase(&maintenance, &ctx, KanidmMaintenancePhase::Completed, None).await?;
                return Ok(Action::requeue(Duration::from_secs(3600)));
            };

            if !safe_to_restart(&maintenance, &target, &next, &ctx).await? {
                return Ok(Action::requeue(REQUEUE));
            }

            let pod = get_pod(&target, &next, &ctx).await?.ok_or_else(|| {
                Error::MissingData(format!("pod {} does not exist", next.pod_name))
            })?;
            if !pod_ready(&pod) {
                return Ok(Action::requeue(REQUEUE));
            }

            let mut status = maintenance.status.clone().unwrap_or_default();
            status.current_target = Some(next);
            status.previous_pod_uid = pod.uid();
            status.current_started_at = Some(Time(Timestamp::now()));
            status.phase = KanidmMaintenancePhase::RestartingReplica;
            status.message = None;
            patch_status(&maintenance, &ctx, status).await?;
            Ok(Action::requeue(REQUEUE))
        }
        KanidmMaintenancePhase::RestartingReplica => {
            let target = get_target(&maintenance, &ctx).await?;
            ensure_lock_owned(&maintenance, &target, &ctx).await?;
            let status = maintenance.status.clone().unwrap_or_default();
            let current = status.current_target.clone().ok_or_else(|| {
                Error::MissingData("maintenance has no current target".to_string())
            })?;
            ensure_active_plan(&maintenance, &target, &current, &ctx).await?;

            let pod = get_pod(&target, &current, &ctx).await?;
            match pod {
                None => Ok(Action::requeue(REQUEUE)),
                Some(pod) if pod.uid() == status.previous_pod_uid => {
                    if pod.metadata.deletion_timestamp.is_none() {
                        if !safe_to_restart(&maintenance, &target, &current, &ctx).await? {
                            return Ok(Action::requeue(REQUEUE));
                        }
                        delete_pod(&target, &current, &ctx).await?;
                    }
                    Ok(Action::requeue(REQUEUE))
                }
                Some(pod) => {
                    if let Some(message) = maintenance_init_failure(&pod) {
                        set_phase(
                            &maintenance,
                            &ctx,
                            KanidmMaintenancePhase::Failed,
                            Some(message),
                        )
                        .await?;
                        return Ok(Action::requeue(Duration::from_secs(300)));
                    }
                    if maintenance_init_completed(&pod) {
                        set_phase(
                            &maintenance,
                            &ctx,
                            KanidmMaintenancePhase::RecoveringReplica,
                            None,
                        )
                        .await?;
                        return Ok(Action::requeue(REQUEUE));
                    }
                    if replica_timed_out(&maintenance) {
                        set_phase(
                            &maintenance,
                            &ctx,
                            KanidmMaintenancePhase::Failed,
                            Some(format!(
                                "maintenance timed out waiting for {} init container",
                                current.pod_name
                            )),
                        )
                        .await?;
                        return Ok(Action::requeue(Duration::from_secs(300)));
                    }
                    Ok(Action::requeue(REQUEUE))
                }
            }
        }
        KanidmMaintenancePhase::RecoveringReplica => {
            let target = get_target(&maintenance, &ctx).await?;
            ensure_lock_owned(&maintenance, &target, &ctx).await?;
            let mut status = maintenance.status.clone().unwrap_or_default();
            let current = status.current_target.clone().ok_or_else(|| {
                Error::MissingData("maintenance has no current target".to_string())
            })?;
            let Some(pod) = get_pod(&target, &current, &ctx).await? else {
                return Ok(Action::requeue(REQUEUE));
            };

            if let Some(message) = maintenance_init_failure(&pod) {
                set_phase(
                    &maintenance,
                    &ctx,
                    KanidmMaintenancePhase::Failed,
                    Some(message),
                )
                .await?;
                return Ok(Action::requeue(Duration::from_secs(300)));
            }

            if !pod_ready(&pod) {
                if replica_timed_out(&maintenance) {
                    set_phase(
                        &maintenance,
                        &ctx,
                        KanidmMaintenancePhase::Failed,
                        Some(format!(
                            "maintenance completed but {} did not become Ready before timeout",
                            current.pod_name
                        )),
                    )
                    .await?;
                    return Ok(Action::requeue(Duration::from_secs(300)));
                }
                return Ok(Action::requeue(REQUEUE));
            }

            set_plan_inactive(&maintenance, &target, &ctx).await?;
            if !status.completed_targets.contains(&current) {
                status.completed_targets.push(current);
            }
            status.current_target = None;
            status.previous_pod_uid = None;
            status.current_started_at = None;
            status.phase = KanidmMaintenancePhase::Planning;
            status.message = None;
            patch_status(&maintenance, &ctx, status).await?;
            Ok(Action::requeue(REQUEUE))
        }
        KanidmMaintenancePhase::Completed | KanidmMaintenancePhase::Failed => {
            Ok(Action::requeue(Duration::from_secs(3600)))
        }
    }
}

async fn cleanup(
    maintenance: Arc<KanidmMaintenance>,
    ctx: Arc<MaintenanceContext>,
) -> Result<Action> {
    let status = maintenance.status.clone().unwrap_or_default();
    let target = get_target(&maintenance, &ctx).await.ok();

    let safe_without_override = matches!(
        status.phase,
        KanidmMaintenancePhase::Pending
            | KanidmMaintenancePhase::Validating
            | KanidmMaintenancePhase::AcquiringLock
            | KanidmMaintenancePhase::Completed
    ) || (status.phase == KanidmMaintenancePhase::Planning
        && status.current_target.is_none());

    if !safe_without_override && !force_resume_requested(&maintenance) {
        if let Err(error) = ctx
            .recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "MaintenanceDeletionBlocked".to_string(),
                    note: Some(format!(
                        "Maintenance is in phase {:?}; add {}=true only after deciding that the replica may be started without the maintenance guard.",
                        status.phase, FORCE_RESUME_ANNOTATION
                    )),
                    action: "Maintenance".to_string(),
                    secondary: None,
                },
                &maintenance.object_ref(&()),
            )
            .await
        {
            warn!(%error, "failed to publish maintenance deletion warning");
        }
        return Ok(Action::requeue(Duration::from_secs(30)));
    }

    if let Some(target) = target {
        if force_resume_requested(&maintenance) {
            set_plan_inactive(&maintenance, &target, &ctx).await?;
            if let Some(current) = status.current_target.as_ref() {
                // Recreate the Pod after disabling the plan so a CrashLooping maintenance init
                // cannot keep stale projected ConfigMap contents indefinitely.
                let _ = delete_pod(&target, current, &ctx).await;
            }
        }
        clear_maintenance(&maintenance, &target, &ctx).await?;
        release_lock(&maintenance, &target, &ctx).await?;
    }
    Ok(Action::await_change())
}

async fn validate(maintenance: &KanidmMaintenance, ctx: &MaintenanceContext) -> Result<Kanidm> {
    let target = get_target(maintenance, ctx).await?;
    let actual_uid = target
        .uid()
        .ok_or_else(|| Error::MissingData("target Kanidm has no UID".to_string()))?;
    if actual_uid != maintenance.spec.target_ref.uid {
        return Err(Error::MissingData(format!(
            "target UID mismatch: expected {}, got {actual_uid}",
            maintenance.spec.target_ref.uid
        )));
    }

    let storage =
        target.spec.storage.as_ref().ok_or_else(|| {
            Error::MissingData("maintenance requires persistent storage".to_string())
        })?;
    if storage.empty_dir.is_some()
        || storage.ephemeral.is_some()
        || storage.volume_claim_template.is_none()
    {
        return Err(Error::MissingData(
            "rolling maintenance requires PVC-backed Kanidm storage".to_string(),
        ));
    }

    if mutable_image(&target.spec.image) {
        return Err(Error::MissingData(format!(
            "rolling maintenance requires a pinned Kanidm image; '{}' is mutable",
            target.spec.image
        )));
    }

    if let Some(owner) = target.annotations().get(RESTORE_ANNOTATION) {
        return Err(Error::MissingData(format!(
            "Kanidm is currently owned by restore operation {owner}"
        )));
    }
    if let Some(owner) = target.annotations().get(MAINTENANCE_ANNOTATION)
        && Some(owner) != maintenance.uid().as_ref()
    {
        return Err(Error::MissingData(format!(
            "Kanidm is currently owned by maintenance operation {owner}"
        )));
    }

    let targets = planned_targets(maintenance, &target)?;
    if targets.is_empty() {
        return Err(Error::MissingData(
            "maintenance selection contains no replicas".to_string(),
        ));
    }

    if !maintenance.spec.allow_downtime && selection_requires_write_downtime(&target, &targets)? {
        return Err(Error::MissingData(
            "maintenance would leave no write-capable replica serving; set allowDowntime=true to acknowledge service interruption"
                .to_string(),
        ));
    }

    Ok(target)
}

async fn get_target(maintenance: &KanidmMaintenance, ctx: &MaintenanceContext) -> Result<Kanidm> {
    let namespace = maintenance
        .namespace()
        .ok_or_else(|| Error::MissingData("maintenance has no namespace".to_string()))?;
    Api::<Kanidm>::namespaced(ctx.client.clone(), &namespace)
        .get(&maintenance.spec.target_ref.name)
        .await
        .map_err(|error| {
            Error::kube_error(
                "get",
                "Kanidm",
                &namespace,
                &maintenance.spec.target_ref.name,
                error,
            )
        })
}

fn planned_targets(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
) -> Result<Vec<MaintenanceInstanceRef>> {
    match maintenance.spec.target.as_ref() {
        None | Some(KanidmMaintenanceTarget::AllReplicas) => {
            let mut result = Vec::new();
            for replica_group in target
                .spec
                .replica_groups
                .iter()
                .filter(|group| !group.primary_node)
            {
                for ordinal in (0..replica_group.replicas).rev() {
                    result.push(instance_ref(target, &replica_group.name, ordinal));
                }
            }
            for replica_group in target
                .spec
                .replica_groups
                .iter()
                .filter(|group| group.primary_node)
            {
                for ordinal in (0..replica_group.replicas).rev() {
                    result.push(instance_ref(target, &replica_group.name, ordinal));
                }
            }
            Ok(result)
        }
        Some(KanidmMaintenanceTarget::Instance {
            replica_group,
            ordinal,
        }) => {
            let group = target
                .spec
                .replica_groups
                .iter()
                .find(|group| group.name == *replica_group)
                .ok_or_else(|| {
                    Error::MissingData(format!("replica group '{replica_group}' does not exist"))
                })?;
            if *ordinal < 0 || *ordinal >= group.replicas {
                return Err(Error::MissingData(format!(
                    "ordinal {ordinal} is outside replica group '{}' range 0..{}",
                    replica_group, group.replicas
                )));
            }
            Ok(vec![instance_ref(target, replica_group, *ordinal)])
        }
    }
}

fn instance_ref(target: &Kanidm, replica_group: &str, ordinal: i32) -> MaintenanceInstanceRef {
    MaintenanceInstanceRef {
        replica_group: replica_group.to_string(),
        ordinal,
        pod_name: target.pod_name(replica_group, ordinal),
    }
}

fn selection_requires_write_downtime(
    target: &Kanidm,
    selected: &[MaintenanceInstanceRef],
) -> Result<bool> {
    let writer_count: i32 = target
        .spec
        .replica_groups
        .iter()
        .filter(|group| write_capable(&group.role))
        .map(|group| group.replicas)
        .sum();
    if writer_count > 1 {
        return Ok(false);
    }

    for instance in selected {
        let group = target
            .spec
            .replica_groups
            .iter()
            .find(|group| group.name == instance.replica_group)
            .ok_or_else(|| {
                Error::MissingData(format!(
                    "replica group '{}' disappeared from maintenance plan",
                    instance.replica_group
                ))
            })?;
        if write_capable(&group.role) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn write_capable(role: &KanidmServerRole) -> bool {
    matches!(
        role,
        KanidmServerRole::WriteReplica | KanidmServerRole::WriteReplicaNoUi
    )
}

async fn safe_to_restart(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    current: &MaintenanceInstanceRef,
    ctx: &MaintenanceContext,
) -> Result<bool> {
    if maintenance.spec.allow_downtime {
        return Ok(true);
    }

    let namespace = target
        .namespace()
        .ok_or_else(|| Error::MissingData("Kanidm has no namespace".to_string()))?;
    let pods = Api::<Pod>::namespaced(ctx.client.clone(), &namespace)
        .list(&ListParams::default().labels(&format!("{CLUSTER_LABEL}={}", target.name_any())))
        .await
        .map_err(|error| Error::kube_error("list", "Pod", &namespace, target.name_any(), error))?;

    let ready_other_writers = pods
        .items
        .iter()
        .filter(|pod| pod.name_any() != current.pod_name && pod_ready(pod))
        .filter(|pod| {
            let pod_name = pod.name_any();
            target.spec.replica_groups.iter().any(|group| {
                write_capable(&group.role)
                    && (0..group.replicas)
                        .any(|ordinal| target.pod_name(&group.name, ordinal) == pod_name)
            })
        })
        .count();

    Ok(ready_other_writers >= 1)
}

async fn get_pod(
    target: &Kanidm,
    instance: &MaintenanceInstanceRef,
    ctx: &MaintenanceContext,
) -> Result<Option<Pod>> {
    let namespace = target
        .namespace()
        .ok_or_else(|| Error::MissingData("Kanidm has no namespace".to_string()))?;
    Api::<Pod>::namespaced(ctx.client.clone(), &namespace)
        .get_opt(&instance.pod_name)
        .await
        .map_err(|error| Error::kube_error("get", "Pod", &namespace, &instance.pod_name, error))
}

async fn delete_pod(
    target: &Kanidm,
    instance: &MaintenanceInstanceRef,
    ctx: &MaintenanceContext,
) -> Result<()> {
    let namespace = target.namespace().unwrap();
    Api::<Pod>::namespaced(ctx.client.clone(), &namespace)
        .delete(&instance.pod_name, &DeleteParams::default())
        .await
        .map(|_| ())
        .map_err(|error| Error::kube_error("delete", "Pod", &namespace, &instance.pod_name, error))
}

fn pod_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
}

fn maintenance_init_completed(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.init_container_statuses.as_ref())
        .and_then(|statuses| {
            statuses
                .iter()
                .find(|status| status.name == MAINTENANCE_INIT_CONTAINER)
        })
        .and_then(|status| status.state.as_ref())
        .and_then(|state| state.terminated.as_ref())
        .is_some_and(|terminated| terminated.exit_code == 0)
}

fn maintenance_init_failure(pod: &Pod) -> Option<String> {
    let status = pod
        .status
        .as_ref()?
        .init_container_statuses
        .as_ref()?
        .iter()
        .find(|status| status.name == MAINTENANCE_INIT_CONTAINER)?;

    let terminated = status
        .state
        .as_ref()
        .and_then(|state| state.terminated.as_ref())
        .filter(|terminated| terminated.exit_code != 0)
        .or_else(|| {
            status
                .last_state
                .as_ref()
                .and_then(|state| state.terminated.as_ref())
                .filter(|terminated| terminated.exit_code != 0)
        })?;

    Some(format!(
        "maintenance init failed on {} with exit code {}{}",
        pod.name_any(),
        terminated.exit_code,
        terminated
            .reason
            .as_deref()
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default()
    ))
}

fn replica_timed_out(maintenance: &KanidmMaintenance) -> bool {
    let Some(started) = maintenance
        .status
        .as_ref()
        .and_then(|status| status.current_started_at.as_ref())
    else {
        return false;
    };
    let timeout = maintenance
        .spec
        .replica_timeout_seconds
        .unwrap_or(DEFAULT_REPLICA_TIMEOUT_SECONDS) as i64;
    Timestamp::now().as_second() - started.0.as_second() >= timeout
}

async fn acquire_lock(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
) -> Result<()> {
    let namespace = target.namespace().unwrap();
    let name = plan_config_map_name(&target.name_any());
    let uid = maintenance
        .uid()
        .ok_or_else(|| Error::MissingData("maintenance has no UID".to_string()))?;
    let mut data = BTreeMap::new();
    data.insert("ownerUid".to_string(), uid.clone());
    data.insert(PLAN_KEY.to_string(), inactive_plan_json(&uid)?);
    let config_map = ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace.clone()),
            owner_references: target.controller_owner_ref(&()).map(|owner| vec![owner]),
            labels: Some(BTreeMap::from([(
                CLUSTER_LABEL.to_string(),
                target.name_any(),
            )])),
            ..ObjectMeta::default()
        },
        data: Some(data),
        ..ConfigMap::default()
    };

    let api = Api::<ConfigMap>::namespaced(ctx.client.clone(), &namespace);
    match api.create(&PostParams::default(), &config_map).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(status)) if status.code == 409 => {
            let existing = api.get(&name).await.map_err(|error| {
                Error::kube_error(
                    "get maintenance lock",
                    "ConfigMap",
                    &namespace,
                    &name,
                    error,
                )
            })?;
            if existing.data.as_ref().and_then(|data| data.get("ownerUid")) == Some(&uid) {
                Ok(())
            } else {
                Err(Error::MissingData(format!(
                    "another maintenance operation owns ConfigMap {namespace}/{name}"
                )))
            }
        }
        Err(error) => Err(Error::kube_error(
            "create maintenance lock",
            "ConfigMap",
            &namespace,
            &name,
            error,
        )),
    }
}

async fn ensure_lock_owned(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
) -> Result<()> {
    let namespace = target.namespace().unwrap();
    let name = plan_config_map_name(&target.name_any());
    let uid = maintenance
        .uid()
        .ok_or_else(|| Error::MissingData("maintenance has no UID".to_string()))?;
    let config_map = Api::<ConfigMap>::namespaced(ctx.client.clone(), &namespace)
        .get(&name)
        .await
        .map_err(|error| {
            Error::kube_error(
                "get maintenance lock",
                "ConfigMap",
                &namespace,
                &name,
                error,
            )
        })?;
    if config_map
        .data
        .as_ref()
        .and_then(|data| data.get("ownerUid"))
        != Some(&uid)
    {
        return Err(Error::MissingData(format!(
            "maintenance no longer owns ConfigMap {namespace}/{name}"
        )));
    }
    Ok(())
}

async fn ensure_active_plan(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    instance: &MaintenanceInstanceRef,
    ctx: &MaintenanceContext,
) -> Result<()> {
    let uid = maintenance
        .uid()
        .ok_or_else(|| Error::MissingData("maintenance has no UID".to_string()))?;
    let config_path = target
        .is_replication_enabled()
        .then_some("/run/kanidm/server.toml");
    let plan = RunnerPlan {
        version: 1,
        active: true,
        operation_id: &uid,
        pod_name: &instance.pod_name,
        operation: maintenance.spec.operation.runner_value(),
        retry_interrupted: maintenance.spec.operation.retry_interrupted(),
        config_path,
    };
    update_plan(maintenance, target, ctx, &plan).await
}

async fn set_plan_inactive(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
) -> Result<()> {
    let uid = maintenance
        .uid()
        .ok_or_else(|| Error::MissingData("maintenance has no UID".to_string()))?;
    let json = inactive_plan_json(&uid)?;
    update_plan_json(maintenance, target, ctx, json).await
}

fn inactive_plan_json(operation_id: &str) -> Result<String> {
    serde_json::to_string(&RunnerPlan {
        version: 1,
        active: false,
        operation_id,
        pod_name: "",
        operation: "verify",
        retry_interrupted: false,
        config_path: None,
    })
    .map_err(|error| Error::SerializationError("serialize maintenance plan".to_string(), error))
}

async fn update_plan(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
    plan: &RunnerPlan<'_>,
) -> Result<()> {
    let plan_json = serde_json::to_string(plan).map_err(|error| {
        Error::SerializationError("serialize maintenance plan".to_string(), error)
    })?;
    update_plan_json(maintenance, target, ctx, plan_json).await
}

async fn update_plan_json(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
    plan_json: String,
) -> Result<()> {
    ensure_lock_owned(maintenance, target, ctx).await?;
    let namespace = target.namespace().unwrap();
    let name = plan_config_map_name(&target.name_any());
    Api::<ConfigMap>::namespaced(ctx.client.clone(), &namespace)
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(json!({"data": {PLAN_KEY: plan_json}})),
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            Error::kube_error(
                "update maintenance plan",
                "ConfigMap",
                &namespace,
                &name,
                error,
            )
        })
}

async fn release_lock(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
) -> Result<()> {
    let namespace = target.namespace().unwrap();
    let name = plan_config_map_name(&target.name_any());
    let api = Api::<ConfigMap>::namespaced(ctx.client.clone(), &namespace);
    let Some(config_map) = api.get_opt(&name).await.map_err(|error| {
        Error::kube_error(
            "get maintenance lock",
            "ConfigMap",
            &namespace,
            &name,
            error,
        )
    })?
    else {
        return Ok(());
    };
    if config_map
        .data
        .as_ref()
        .and_then(|data| data.get("ownerUid"))
        != maintenance.uid().as_ref()
    {
        return Ok(());
    }
    api.delete(&name, &DeleteParams::default())
        .await
        .map(|_| ())
        .map_err(|error| {
            Error::kube_error(
                "delete maintenance lock",
                "ConfigMap",
                &namespace,
                &name,
                error,
            )
        })
}

async fn mark_maintenance(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
) -> Result<()> {
    let namespace = target.namespace().unwrap();
    let uid = maintenance
        .uid()
        .ok_or_else(|| Error::MissingData("maintenance has no UID".to_string()))?;
    Api::<Kanidm>::namespaced(ctx.client.clone(), &namespace)
        .patch(
            &target.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"annotations":{MAINTENANCE_ANNOTATION:uid}}})),
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            Error::kube_error(
                "mark maintenance",
                "Kanidm",
                &namespace,
                target.name_any(),
                error,
            )
        })
}

async fn clear_maintenance(
    maintenance: &KanidmMaintenance,
    target: &Kanidm,
    ctx: &MaintenanceContext,
) -> Result<()> {
    if target.annotations().get(MAINTENANCE_ANNOTATION) != maintenance.uid().as_ref() {
        return Ok(());
    }
    let namespace = target.namespace().unwrap();
    Api::<Kanidm>::namespaced(ctx.client.clone(), &namespace)
        .patch(
            &target.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"annotations":{MAINTENANCE_ANNOTATION:null}}})),
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            Error::kube_error(
                "clear maintenance",
                "Kanidm",
                &namespace,
                target.name_any(),
                error,
            )
        })
}

fn force_resume_requested(maintenance: &KanidmMaintenance) -> bool {
    maintenance
        .annotations()
        .get(FORCE_RESUME_ANNOTATION)
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

async fn patch_status(
    maintenance: &KanidmMaintenance,
    ctx: &MaintenanceContext,
    mut status: KanidmMaintenanceStatus,
) -> Result<()> {
    let namespace = maintenance.namespace().unwrap();
    let previous_phase = maintenance.status.as_ref().map(|current| current.phase);
    status.observed_generation = maintenance.metadata.generation;
    update_conditions(&mut status, maintenance.metadata.generation);

    Api::<KanidmMaintenance>::namespaced(ctx.client.clone(), &namespace)
        .patch_status(
            &maintenance.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"status": &status})),
        )
        .await
        .map_err(|error| {
            Error::kube_error(
                "patch status",
                "KanidmMaintenance",
                &namespace,
                maintenance.name_any(),
                error,
            )
        })?;

    if previous_phase != Some(status.phase) {
        record_transition(
            maintenance,
            ctx,
            previous_phase,
            status.phase,
            status.message.as_deref(),
        )
        .await;
    }
    Ok(())
}

async fn set_phase(
    maintenance: &KanidmMaintenance,
    ctx: &MaintenanceContext,
    phase: KanidmMaintenancePhase,
    message: Option<String>,
) -> Result<()> {
    let mut status = maintenance.status.clone().unwrap_or_default();
    status.phase = phase;
    status.message = message;
    patch_status(maintenance, ctx, status).await
}

fn update_conditions(status: &mut KanidmMaintenanceStatus, generation: Option<i64>) {
    let previous = status.conditions.clone();
    let phase = status.phase;
    let terminal_success = phase == KanidmMaintenancePhase::Completed;
    let terminal_failure = phase == KanidmMaintenancePhase::Failed;
    let progressing = !terminal_success && !terminal_failure;
    let message = status
        .message
        .clone()
        .unwrap_or_else(|| format!("Kanidm maintenance is in phase {phase:?}."));

    status.conditions = vec![
        maintenance_condition(
            &previous,
            "Progressing",
            if progressing {
                CONDITION_TRUE
            } else {
                CONDITION_FALSE
            },
            &format!("{phase:?}"),
            &message,
            generation,
        ),
        maintenance_condition(
            &previous,
            "Ready",
            if terminal_success {
                CONDITION_TRUE
            } else {
                CONDITION_FALSE
            },
            if terminal_success {
                "MaintenanceCompleted"
            } else {
                "MaintenanceNotCompleted"
            },
            if terminal_success {
                "Kanidm maintenance completed successfully."
            } else {
                "Kanidm maintenance has not completed successfully."
            },
            generation,
        ),
        maintenance_condition(
            &previous,
            "Failed",
            if terminal_failure {
                CONDITION_TRUE
            } else {
                CONDITION_FALSE
            },
            if terminal_failure {
                "MaintenanceFailed"
            } else {
                "NoMaintenanceFailure"
            },
            if terminal_failure {
                status
                    .message
                    .as_deref()
                    .unwrap_or("Kanidm maintenance failed.")
            } else {
                "No terminal maintenance failure has been recorded."
            },
            generation,
        ),
    ];
}

fn maintenance_condition(
    previous: &[Condition],
    condition_type: &str,
    condition_status: &str,
    reason: &str,
    message: &str,
    generation: Option<i64>,
) -> Condition {
    let last_transition_time = previous
        .iter()
        .find(|condition| {
            condition.type_ == condition_type
                && condition.status == condition_status
                && condition.reason == reason
        })
        .map(|condition| condition.last_transition_time.clone())
        .unwrap_or_else(|| Time(Timestamp::now()));
    Condition {
        type_: condition_type.to_string(),
        status: condition_status.to_string(),
        reason: reason.to_string(),
        message: message.to_string(),
        last_transition_time,
        observed_generation: generation,
    }
}

async fn record_transition(
    maintenance: &KanidmMaintenance,
    ctx: &MaintenanceContext,
    previous_phase: Option<KanidmMaintenancePhase>,
    phase: KanidmMaintenancePhase,
    message: Option<&str>,
) {
    let event_type = if phase == KanidmMaintenancePhase::Failed {
        EventType::Warning
    } else {
        EventType::Normal
    };
    let reason = if phase == KanidmMaintenancePhase::Failed {
        "MaintenanceFailed"
    } else {
        "MaintenancePhaseChanged"
    };
    let note = message.map(str::to_string).or_else(|| {
        Some(format!(
            "Kanidm maintenance phase changed from {} to {phase:?}.",
            previous_phase
                .map(|previous| format!("{previous:?}"))
                .unwrap_or_else(|| "None".to_string())
        ))
    });
    if let Err(error) = ctx
        .recorder
        .publish(
            &Event {
                type_: event_type,
                reason: reason.to_string(),
                note,
                action: "Maintenance".to_string(),
                secondary: None,
            },
            &maintenance.object_ref(&()),
        )
        .await
    {
        warn!(maintenance = %maintenance.name_any(), %error, "failed to publish maintenance event");
    }
}

fn mutable_image(image: &str) -> bool {
    image == "kanidm/server:latest"
        || image.ends_with(":latest")
        || (!image.contains('@')
            && !image
                .rsplit('/')
                .next()
                .is_some_and(|part| part.contains(':')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_retry_policy_is_conservative() {
        assert!(KanidmMaintenanceOperation::Verify.retry_interrupted());
        assert!(!KanidmMaintenanceOperation::Reindex.retry_interrupted());
        assert!(!KanidmMaintenanceOperation::Vacuum.retry_interrupted());
    }

    #[test]
    fn plan_config_map_name_is_stable() {
        assert_eq!(plan_config_map_name("example"), "example-maintenance");
    }
}
