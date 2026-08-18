use super::crd::Kanidm;
use super::reconcile::{CLUSTER_LABEL, statefulset::StatefulSetExt};

use kaniop_k8s_util::client::get_output;
use kaniop_k8s_util::error::{Error, Result};

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, EnvVar, PersistentVolumeClaim,
    PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use k8s_openapi::api::storage::v1::VolumeAttachment;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kube::api::{
    AttachParams, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams,
};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::finalizer::{Event as Finalizer, finalizer};
use kube::runtime::watcher;
use kube::{Api, Client, CustomResource, Resource, ResourceExt};
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
#[cfg(feature = "schemars")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, error, info, warn};

pub const CONTROLLER_ID: &str = "kanidm-restore";
pub const RESTORE_ANNOTATION: &str = "kanidm.kaniop.rs/restore-in-progress";
const RESTORE_FINALIZER: &str = "kanidmrestores.kaniop.rs/finalizer";
const DATA_VOLUME: &str = "kanidm-data";
const CONFIG_VOLUME: &str = "kanidm-config";
const DATA_PATH: &str = "/data";
const CONFIG_PATH: &str = "/run/kanidm";
const BACKUP_PATH: &str = "/data/backups";
const REQUEUE: Duration = Duration::from_secs(2);
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";
const CONDITION_PROGRESSING: &str = "Progressing";
const CONDITION_READY: &str = "Ready";
const CONDITION_FAILED: &str = "Failed";

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [{"message": "KanidmRestore spec is immutable", "rule": "self == oldSelf"}]))
)]
#[kube(
    category = "kaniop",
    group = "kaniop.rs",
    version = "v1beta1",
    kind = "KanidmRestore",
    plural = "kanidmrestores",
    singular = "kanidmrestore",
    shortname = "idmrestore",
    namespaced,
    status = "KanidmRestoreStatus",
    printcolumn = r#"{"name":"Target","type":"string","jsonPath":".spec.targetRef.name"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreSpec {
    pub target_ref: KanidmRestoreTargetRef,
    pub source: KanidmRestoreSource,
    /// Exact Kanidm image used for restore. It must equal the target image and may not use latest.
    pub restore_image: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreTargetRef {
    pub name: String,
    pub uid: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreSource {
    pub local: KanidmRestoreLocalSource,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreLocalSource {
    /// Basename of a backup below /data/backups. Paths and traversal are rejected.
    pub file_name: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum KanidmRestorePhase {
    #[default]
    Pending,
    Validating,
    Quiescing,
    RestoringPrimary,
    Verifying,
    RebuildingReplicas,
    Resuming,
    Completed,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_target_uid: Option<String>,
    #[serde(default)]
    pub phase: KanidmRestorePhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_job_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_job_name: Option<String>,
    #[serde(default)]
    pub replicas_cleared: bool,
    /// Persisted before the restore Job may be created. Once true, finalizer cleanup
    /// fails closed until the restore has completed.
    #[serde(default)]
    pub database_mutation_started: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone)]
struct RestoreMetrics {
    attempts: Counter<u64>,
    outcomes: Counter<u64>,
    duration_seconds: Histogram<f64>,
}

impl RestoreMetrics {
    fn new() -> Self {
        let meter = global::meter("kaniop");
        Self {
            attempts: meter
                .u64_counter("kanidm_restore_attempts")
                .with_description("Number of Kanidm restore attempts started")
                .build(),
            outcomes: meter
                .u64_counter("kanidm_restore_outcomes")
                .with_description("Number of terminal Kanidm restore outcomes")
                .build(),
            duration_seconds: meter
                .f64_histogram("kanidm_restore_duration_seconds")
                .with_description("Kanidm restore duration from object creation to terminal phase")
                .with_unit("s")
                .build(),
        }
    }
}

#[derive(Clone)]
struct RestoreContext {
    client: Client,
    recorder: Recorder,
    metrics: RestoreMetrics,
}

pub async fn run(client: Client) {
    let api = Api::<KanidmRestore>::all(client.clone());
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: CONTROLLER_ID.into(),
            instance: None,
        },
    );
    let ctx = Arc::new(RestoreContext {
        client,
        recorder,
        metrics: RestoreMetrics::new(),
    });
    info!("starting {CONTROLLER_ID} controller");
    Controller::new(api, watcher::Config::default().any_semantic())
        .shutdown_on_signal()
        .run(reconcile_restore, error_policy, ctx)
        .for_each(|result| async move {
            if let Err(error) = result {
                error!(%error, "KanidmRestore reconciliation failed");
            }
        })
        .await;
}

fn error_policy(restore: Arc<KanidmRestore>, error: &Error, _ctx: Arc<RestoreContext>) -> Action {
    warn!(restore = %restore.name_any(), %error, "restore reconciliation error");
    Action::requeue(Duration::from_secs(5))
}

async fn reconcile_restore(
    restore: Arc<KanidmRestore>,
    ctx: Arc<RestoreContext>,
) -> Result<Action> {
    let namespace = restore
        .namespace()
        .ok_or_else(|| Error::MissingData("KanidmRestore has no namespace".to_string()))?;
    let api = Api::<KanidmRestore>::namespaced(ctx.client.clone(), &namespace);
    finalizer(&api, RESTORE_FINALIZER, restore, |event| {
        let ctx = ctx.clone();
        async move {
            match event {
                Finalizer::Apply(restore) => reconcile_apply(restore, ctx).await,
                Finalizer::Cleanup(restore) => cleanup(restore, ctx).await,
            }
        }
    })
    .await
    .map_err(|error| {
        Error::FinalizerError(
            "failed on KanidmRestore finalizer".to_string(),
            Box::new(error),
        )
    })
}

async fn reconcile_apply(restore: Arc<KanidmRestore>, ctx: Arc<RestoreContext>) -> Result<Action> {
    let phase = restore.status.as_ref().map(|s| s.phase).unwrap_or_default();
    match phase {
        KanidmRestorePhase::Pending => {
            set_phase(&restore, &ctx, KanidmRestorePhase::Validating, None).await?;
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::Validating => match validate(&restore, &ctx).await {
            Ok(target) => {
                mark_restoring(&restore, &target, &ctx).await?;
                let mut status = restore.status.clone().unwrap_or_default();
                status.observed_target_uid = target.uid();
                status.phase = KanidmRestorePhase::Quiescing;
                status.message = None;
                patch_status(&restore, &ctx, status).await?;
                Ok(Action::requeue(REQUEUE))
            }
            Err(error) => {
                set_phase(
                    &restore,
                    &ctx,
                    KanidmRestorePhase::Failed,
                    Some(error.to_string()),
                )
                .await?;
                Ok(Action::requeue(Duration::from_secs(300)))
            }
        },
        KanidmRestorePhase::Quiescing => {
            let target = get_target(&restore, &ctx).await?;
            scale_all(&target, &ctx, 0).await?;
            if target_pods_stopped(&target, &ctx).await?
                && target_volumes_detached(&target, &ctx).await?
            {
                set_phase(&restore, &ctx, KanidmRestorePhase::RestoringPrimary, None).await?;
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::RestoringPrimary => {
            let target = get_target(&restore, &ctx).await?;
            ensure_restore_config(&restore, &target, &ctx).await?;
            if !restore
                .status
                .as_ref()
                .is_some_and(|status| status.database_mutation_started)
            {
                let mut status = restore.status.clone().unwrap_or_default();
                status.database_mutation_started = true;
                status.message = Some(
                    "database mutation boundary persisted; starting primary restore".to_string(),
                );
                patch_status(&restore, &ctx, status).await?;
                return Ok(Action::requeue(REQUEUE));
            }
            let name = restore_job_name(&restore);
            ensure_database_job(&restore, &target, &ctx, &name, false).await?;
            match job_state(&restore, &ctx, &name).await? {
                JobState::Complete => {
                    let mut status = restore.status.clone().unwrap_or_default();
                    status.restore_job_name = Some(name);
                    status.phase = KanidmRestorePhase::Verifying;
                    status.message = None;
                    patch_status(&restore, &ctx, status).await?;
                }
                JobState::Failed => {
                    fail_after_mutation(&restore, &ctx, "database restore job failed").await?
                }
                JobState::Running => {}
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::Verifying => {
            let target = get_target(&restore, &ctx).await?;
            let name = verify_job_name(&restore);
            ensure_database_job(&restore, &target, &ctx, &name, true).await?;
            match job_state(&restore, &ctx, &name).await? {
                JobState::Complete => {
                    let mut status = restore.status.clone().unwrap_or_default();
                    status.verify_job_name = Some(name);
                    status.phase = KanidmRestorePhase::RebuildingReplicas;
                    status.message = None;
                    patch_status(&restore, &ctx, status).await?;
                }
                JobState::Failed => {
                    fail_after_mutation(&restore, &ctx, "database verification failed").await?
                }
                JobState::Running => {}
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::RebuildingReplicas => {
            let target = get_target(&restore, &ctx).await?;
            let mut status = restore.status.clone().unwrap_or_default();
            if !status.replicas_cleared {
                if delete_secondary_pvcs(&target, &ctx).await? {
                    status.replicas_cleared = true;
                    status.message = Some("secondary database state cleared".to_string());
                    patch_status(&restore, &ctx, status).await?;
                }
                return Ok(Action::requeue(REQUEUE));
            }

            scale_primary(&target, &ctx, 1).await?;
            if !primary_ready(&target, &ctx).await? {
                return Ok(Action::requeue(REQUEUE));
            }
            scale_desired(&target, &ctx).await?;
            if all_desired_ready(&target, &ctx).await? {
                set_phase(&restore, &ctx, KanidmRestorePhase::Resuming, None).await?;
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::Resuming => {
            let target = get_target(&restore, &ctx).await?;
            clear_restoring(&restore, &target, &ctx).await?;
            set_phase(&restore, &ctx, KanidmRestorePhase::Completed, None).await?;
            Ok(Action::requeue(Duration::from_secs(3600)))
        }
        KanidmRestorePhase::Completed | KanidmRestorePhase::Failed => {
            Ok(Action::requeue(Duration::from_secs(3600)))
        }
    }
}

async fn cleanup(restore: Arc<KanidmRestore>, ctx: Arc<RestoreContext>) -> Result<Action> {
    let status = restore.status.clone().unwrap_or_default();
    if status.database_mutation_started && status.phase != KanidmRestorePhase::Completed {
        return Err(Error::MissingData(format!(
            "refusing to remove KanidmRestore after database mutation started in phase {:?}; recover the target or remove the finalizer explicitly",
            status.phase
        )));
    }
    if let Ok(target) = get_target(&restore, &ctx).await {
        let owns_maintenance =
            target.annotations().get(RESTORE_ANNOTATION) == restore.uid().as_ref();
        if owns_maintenance && !status.database_mutation_started {
            scale_desired(&target, &ctx).await?;
        }
        clear_restoring(&restore, &target, &ctx).await?;
    }
    Ok(Action::await_change())
}

async fn validate(restore: &KanidmRestore, ctx: &RestoreContext) -> Result<Kanidm> {
    let target = get_target(restore, ctx).await?;
    let actual_uid = target
        .uid()
        .ok_or_else(|| Error::MissingData("target Kanidm has no UID".to_string()))?;
    if actual_uid != restore.spec.target_ref.uid {
        return Err(Error::MissingData(format!(
            "target UID mismatch: expected {}, got {}",
            restore.spec.target_ref.uid, actual_uid
        )));
    }
    if !safe_basename(&restore.spec.source.local.file_name) {
        return Err(Error::MissingData(
            "restore source fileName must be a safe basename".to_string(),
        ));
    }
    if target.spec.image != restore.spec.restore_image || mutable_image(&restore.spec.restore_image)
    {
        return Err(Error::MissingData(format!(
            "restoreImage must be the target's pinned Kanidm image (target image is {})",
            target.spec.image
        )));
    }
    let storage = target
        .spec
        .storage
        .as_ref()
        .ok_or_else(|| Error::MissingData("restore requires persistent storage".to_string()))?;
    if storage.empty_dir.is_some()
        || storage.ephemeral.is_some()
        || storage.volume_claim_template.is_none()
    {
        return Err(Error::MissingData(
            "restore requires PVC-backed Kanidm storage".to_string(),
        ));
    }
    let primaries = target
        .spec
        .replica_groups
        .iter()
        .filter(|rg| rg.primary_node)
        .count();
    if primaries != 1 {
        return Err(Error::MissingData(
            "backup/restore requires exactly one primary replica group".to_string(),
        ));
    }
    validate_backup_source(restore, &target, ctx).await?;

    let ns = restore.namespace().unwrap();
    let restores = Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .list(&ListParams::default())
        .await
        .map_err(|e| Error::kube_error("list", "KanidmRestore", &ns, "*", e))?;
    if restores.items.iter().any(|other| {
        other.name_any() != restore.name_any()
            && other.spec.target_ref.name == restore.spec.target_ref.name
            && !matches!(
                other.status.as_ref().map(|s| s.phase),
                Some(KanidmRestorePhase::Completed | KanidmRestorePhase::Failed)
            )
    }) {
        return Err(Error::MissingData(
            "another active restore targets this Kanidm".to_string(),
        ));
    }
    Ok(target)
}

fn safe_basename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
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

async fn validate_backup_source(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<()> {
    let ns = target.namespace().unwrap();
    let primary = primary_group(target)?;
    let pod_name = format!("{}-0", target.statefulset_name(&primary.name));
    let backup_file = format!("{BACKUP_PATH}/{}", restore.spec.source.local.file_name);
    let pods = Api::<Pod>::namespaced(ctx.client.clone(), &ns);
    let attached = pods
        .exec(
            &pod_name,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                r#"test -f "$1""#.to_string(),
                "kaniop-backup-preflight".to_string(),
                backup_file.clone(),
            ],
            &AttachParams::default().container("kanidm"),
        )
        .await
        .map_err(|e| Error::kube_error("exec backup preflight in", "Pod", &ns, &pod_name, e))?;
    get_output(attached).await.map(|_| ()).map_err(|error| {
        Error::MissingData(format!(
            "restore source {backup_file} is not accessible on primary pod {pod_name}: {error}"
        ))
    })
}

async fn get_target(restore: &KanidmRestore, ctx: &RestoreContext) -> Result<Kanidm> {
    let ns = restore
        .namespace()
        .ok_or_else(|| Error::MissingData("restore has no namespace".to_string()))?;
    Api::<Kanidm>::namespaced(ctx.client.clone(), &ns)
        .get(&restore.spec.target_ref.name)
        .await
        .map_err(|e| Error::kube_error("get", "Kanidm", &ns, &restore.spec.target_ref.name, e))
}

async fn mark_restoring(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<()> {
    let ns = target.namespace().unwrap();
    let uid = restore
        .uid()
        .ok_or_else(|| Error::MissingData("restore has no UID".to_string()))?;
    Api::<Kanidm>::namespaced(ctx.client.clone(), &ns)
        .patch(
            &target.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"annotations":{RESTORE_ANNOTATION:uid}}})),
        )
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("mark restoring", "Kanidm", &ns, target.name_any(), e))
}

async fn clear_restoring(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<()> {
    let ns = target.namespace().unwrap();
    if target.annotations().get(RESTORE_ANNOTATION) != restore.uid().as_ref() {
        return Ok(());
    }
    Api::<Kanidm>::namespaced(ctx.client.clone(), &ns)
        .patch(
            &target.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"annotations":{RESTORE_ANNOTATION:null}}})),
        )
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("clear restoring", "Kanidm", &ns, target.name_any(), e))
}

async fn patch_status(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    mut status: KanidmRestoreStatus,
) -> Result<()> {
    let ns = restore.namespace().unwrap();
    let previous_phase = restore.status.as_ref().map(|current| current.phase);
    let next_phase = status.phase;
    status.observed_generation = restore.metadata.generation;
    update_restore_conditions(&mut status, restore.metadata.generation);

    Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .patch_status(
            &restore.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"status": &status})),
        )
        .await
        .map_err(|e| {
            Error::kube_error("patch status", "KanidmRestore", &ns, restore.name_any(), e)
        })?;

    if previous_phase != Some(next_phase) {
        record_restore_transition(
            restore,
            ctx,
            previous_phase,
            next_phase,
            status.message.as_deref(),
        )
        .await;
    }
    Ok(())
}

fn update_restore_conditions(status: &mut KanidmRestoreStatus, generation: Option<i64>) {
    let previous = status.conditions.clone();
    let phase = status.phase;
    let phase_reason = format!("{phase:?}");
    let message = status
        .message
        .clone()
        .unwrap_or_else(|| format!("Kanidm restore is in phase {phase_reason}."));
    let terminal_success = phase == KanidmRestorePhase::Completed;
    let terminal_failure = phase == KanidmRestorePhase::Failed;
    let progressing = !terminal_success && !terminal_failure;

    status.conditions = vec![
        restore_condition(
            &previous,
            CONDITION_PROGRESSING,
            if progressing {
                CONDITION_TRUE
            } else {
                CONDITION_FALSE
            },
            &phase_reason,
            &message,
            generation,
        ),
        restore_condition(
            &previous,
            CONDITION_READY,
            if terminal_success {
                CONDITION_TRUE
            } else {
                CONDITION_FALSE
            },
            if terminal_success {
                "RestoreCompleted"
            } else {
                "RestoreNotCompleted"
            },
            if terminal_success {
                "Kanidm restore completed successfully."
            } else {
                "Kanidm restore has not completed successfully."
            },
            generation,
        ),
        restore_condition(
            &previous,
            CONDITION_FAILED,
            if terminal_failure {
                CONDITION_TRUE
            } else {
                CONDITION_FALSE
            },
            if terminal_failure {
                "RestoreFailed"
            } else {
                "NoRestoreFailure"
            },
            if terminal_failure {
                status
                    .message
                    .as_deref()
                    .unwrap_or("Kanidm restore failed.")
            } else {
                "No terminal restore failure has been recorded."
            },
            generation,
        ),
    ];
}

fn restore_condition(
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

async fn record_restore_transition(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    previous_phase: Option<KanidmRestorePhase>,
    phase: KanidmRestorePhase,
    message: Option<&str>,
) {
    if phase == KanidmRestorePhase::Validating {
        ctx.metrics.attempts.add(1, &[]);
    }

    let result = match phase {
        KanidmRestorePhase::Completed => Some("success"),
        KanidmRestorePhase::Failed => Some("failure"),
        _ => None,
    };
    if let Some(result) = result {
        let attributes = [KeyValue::new("result", result)];
        ctx.metrics.outcomes.add(1, &attributes);
        if let Some(created) = restore.metadata.creation_timestamp.as_ref() {
            let elapsed = (Timestamp::now().as_second() - created.0.as_second()).max(0) as f64;
            ctx.metrics.duration_seconds.record(elapsed, &attributes);
        }
    }

    let reason = if phase == KanidmRestorePhase::Failed {
        "RestoreFailed"
    } else {
        "RestorePhaseChanged"
    };
    let note = message.map(str::to_string).or_else(|| {
        Some(format!(
            "Kanidm restore phase changed from {} to {phase:?}.",
            previous_phase
                .map(|previous| format!("{previous:?}"))
                .unwrap_or_else(|| "None".to_string())
        ))
    });
    if let Err(error) = ctx
        .recorder
        .publish(
            &Event {
                type_: if phase == KanidmRestorePhase::Failed {
                    EventType::Warning
                } else {
                    EventType::Normal
                },
                reason: reason.to_string(),
                note,
                action: "Restore".to_string(),
                secondary: None,
            },
            &restore.object_ref(&()),
        )
        .await
    {
        warn!(restore = %restore.name_any(), %error, "failed to publish restore event");
    }
}

async fn set_phase(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    phase: KanidmRestorePhase,
    message: Option<String>,
) -> Result<()> {
    let mut status = restore.status.clone().unwrap_or_default();
    status.phase = phase;
    status.message = message;
    patch_status(restore, ctx, status).await
}

async fn fail_after_mutation(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    message: &str,
) -> Result<()> {
    set_phase(
        restore,
        ctx,
        KanidmRestorePhase::Failed,
        Some(message.to_string()),
    )
    .await
}

fn primary_group(target: &Kanidm) -> Result<&super::crd::ReplicaGroup> {
    target
        .spec
        .replica_groups
        .iter()
        .find(|rg| rg.primary_node)
        .ok_or_else(|| Error::MissingData("primary replica group not found".to_string()))
}

async fn scale_all(target: &Kanidm, ctx: &RestoreContext, replicas: i32) -> Result<()> {
    let ns = target.namespace().unwrap();
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    for rg in &target.spec.replica_groups {
        let name = target.statefulset_name(&rg.name);
        api.patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"replicas":replicas}})),
        )
        .await
        .map_err(|e| Error::kube_error("scale", "StatefulSet", &ns, &name, e))?;
    }
    Ok(())
}

async fn scale_primary(target: &Kanidm, ctx: &RestoreContext, replicas: i32) -> Result<()> {
    let ns = target.namespace().unwrap();
    let rg = primary_group(target)?;
    let name = target.statefulset_name(&rg.name);
    Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns)
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"replicas":replicas}})),
        )
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("scale", "StatefulSet", &ns, &name, e))
}

async fn scale_desired(target: &Kanidm, ctx: &RestoreContext) -> Result<()> {
    let ns = target.namespace().unwrap();
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    for rg in &target.spec.replica_groups {
        let name = target.statefulset_name(&rg.name);
        api.patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"replicas":rg.replicas}})),
        )
        .await
        .map_err(|e| Error::kube_error("scale", "StatefulSet", &ns, &name, e))?;
    }
    Ok(())
}

async fn target_pods_stopped(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let pods = Api::<Pod>::namespaced(ctx.client.clone(), &ns)
        .list(&ListParams::default().labels(&format!("{CLUSTER_LABEL}={}", target.name_any())))
        .await
        .map_err(|e| Error::kube_error("list", "Pod", &ns, target.name_any(), e))?;
    Ok(pods.items.is_empty())
}

async fn target_volumes_detached(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(ctx.client.clone(), &ns);
    let mut pv_names = BTreeSet::new();
    let primary_name = primary_pvc_name(target)?;
    let primary_pvc = pvc_api
        .get(&primary_name)
        .await
        .map_err(|e| Error::kube_error("get", "PersistentVolumeClaim", &ns, &primary_name, e))?;
    let primary_pv = primary_pvc
        .spec
        .as_ref()
        .and_then(|spec| spec.volume_name.clone())
        .ok_or_else(|| {
            Error::MissingData(format!("primary PVC {ns}/{primary_name} has no bound PV"))
        })?;
    pv_names.insert(primary_pv);

    for rg in &target.spec.replica_groups {
        let sts = target.statefulset_name(&rg.name);
        for ordinal in 0..rg.replicas {
            let name = format!("{DATA_VOLUME}-{sts}-{ordinal}");
            if name == primary_name {
                continue;
            }
            if let Some(pvc) = pvc_api
                .get_opt(&name)
                .await
                .map_err(|e| Error::kube_error("get", "PersistentVolumeClaim", &ns, &name, e))?
                && let Some(pv_name) = pvc.spec.as_ref().and_then(|spec| spec.volume_name.clone())
            {
                pv_names.insert(pv_name);
            }
        }
    }

    let attachments = Api::<VolumeAttachment>::all(ctx.client.clone())
        .list(&ListParams::default())
        .await
        .map_err(|e| Error::kube_error("list", "VolumeAttachment", "", "*", e))?;
    Ok(!attachments.items.iter().any(|attachment| {
        attachment
            .spec
            .source
            .persistent_volume_name
            .as_ref()
            .is_some_and(|pv_name| pv_names.contains(pv_name))
    }))
}

async fn primary_ready(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let name = target.statefulset_name(&primary_group(target)?.name);
    let sts = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns)
        .get(&name)
        .await
        .map_err(|e| Error::kube_error("get", "StatefulSet", &ns, &name, e))?;
    Ok(sts
        .status
        .as_ref()
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0)
        >= 1)
}

async fn all_desired_ready(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    for rg in &target.spec.replica_groups {
        let name = target.statefulset_name(&rg.name);
        let sts = api
            .get(&name)
            .await
            .map_err(|e| Error::kube_error("get", "StatefulSet", &ns, &name, e))?;
        if sts
            .status
            .as_ref()
            .and_then(|s| s.ready_replicas)
            .unwrap_or(0)
            != rg.replicas
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn primary_pvc_name(target: &Kanidm) -> Result<String> {
    let rg = primary_group(target)?;
    Ok(format!(
        "{DATA_VOLUME}-{}-0",
        target.statefulset_name(&rg.name)
    ))
}

async fn delete_secondary_pvcs(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<PersistentVolumeClaim>::namespaced(ctx.client.clone(), &ns);
    let mut all_absent = true;
    for rg in &target.spec.replica_groups {
        let sts = target.statefulset_name(&rg.name);
        for ordinal in 0..rg.replicas {
            if rg.primary_node && ordinal == 0 {
                continue;
            }
            let name = format!("{DATA_VOLUME}-{sts}-{ordinal}");
            match api
                .get_opt(&name)
                .await
                .map_err(|e| Error::kube_error("get", "PersistentVolumeClaim", &ns, &name, e))?
            {
                None => {}
                Some(pvc) => {
                    all_absent = false;
                    if pvc.metadata.deletion_timestamp.is_none() {
                        match api.delete(&name, &DeleteParams::default()).await {
                            Ok(_) => debug!(pvc = %name, "deleting stale secondary PVC"),
                            Err(kube::Error::Api(status)) if status.code == 404 => {}
                            Err(error) => {
                                return Err(Error::kube_error(
                                    "delete",
                                    "PersistentVolumeClaim",
                                    &ns,
                                    &name,
                                    error,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(all_absent)
}

fn config_map_name(restore: &KanidmRestore) -> String {
    format!("{}-config", restore.name_any())
}
fn restore_job_name(restore: &KanidmRestore) -> String {
    format!("{}-restore", restore.name_any())
}
fn verify_job_name(restore: &KanidmRestore) -> String {
    format!("{}-verify", restore.name_any())
}

async fn ensure_restore_config(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<()> {
    let ns = restore.namespace().unwrap();
    let api = Api::<ConfigMap>::namespaced(ctx.client.clone(), &ns);
    let name = config_map_name(restore);
    if api
        .get_opt(&name)
        .await
        .map_err(|e| Error::kube_error("get", "ConfigMap", &ns, &name, e))?
        .is_some()
    {
        return Ok(());
    }
    let config = ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(ns.clone()),
            owner_references: restore.controller_owner_ref(&()).map(|r| vec![r]),
            ..Default::default()
        },
        data: Some(std::collections::BTreeMap::from([(
            "server.toml".to_string(),
            "version = \"2\"\n".to_string(),
        )])),
        ..Default::default()
    };
    api.create(&PostParams::default(), &config)
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("create", "ConfigMap", &ns, target.name_any(), e))
}

async fn ensure_database_job(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
    name: &str,
    verify: bool,
) -> Result<()> {
    let ns = restore.namespace().unwrap();
    let api = Api::<Job>::namespaced(ctx.client.clone(), &ns);
    if api
        .get_opt(name)
        .await
        .map_err(|e| Error::kube_error("get", "Job", &ns, name, e))?
        .is_some()
    {
        return Ok(());
    }
    let mut command = vec![
        "kanidmd".to_string(),
        "database".to_string(),
        if verify { "verify" } else { "restore" }.to_string(),
        "-c".to_string(),
        format!("{CONFIG_PATH}/server.toml"),
    ];
    if !verify {
        command.push(format!(
            "{BACKUP_PATH}/{}",
            restore.spec.source.local.file_name
        ));
    }
    let job = Job {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.clone()),
            owner_references: restore.controller_owner_ref(&()).map(|r| vec![r]),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta::default()),
                spec: Some(PodSpec {
                    automount_service_account_token: Some(false),
                    restart_policy: Some("Never".to_string()),
                    containers: vec![Container {
                        name: if verify { "verify" } else { "restore" }.to_string(),
                        image: Some(restore.spec.restore_image.clone()),
                        command: Some(command),
                        env: Some(vec![
                            EnvVar {
                                name: "KANIDM_DB_PATH".to_string(),
                                value: Some("/data/kanidm.db".to_string()),
                                ..Default::default()
                            },
                            EnvVar {
                                name: "KANIDM_DOMAIN".to_string(),
                                value: Some(target.spec.domain.clone()),
                                ..Default::default()
                            },
                        ]),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: DATA_VOLUME.to_string(),
                                mount_path: DATA_PATH.to_string(),
                                ..Default::default()
                            },
                            VolumeMount {
                                name: CONFIG_VOLUME.to_string(),
                                mount_path: CONFIG_PATH.to_string(),
                                read_only: Some(true),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    }],
                    volumes: Some(vec![
                        Volume {
                            name: DATA_VOLUME.to_string(),
                            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                                claim_name: primary_pvc_name(target)?,
                                read_only: Some(false),
                            }),
                            ..Default::default()
                        },
                        Volume {
                            name: CONFIG_VOLUME.to_string(),
                            config_map: Some(ConfigMapVolumeSource {
                                name: config_map_name(restore),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    };
    api.create(&PostParams::default(), &job)
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("create", "Job", &ns, name, e))
}

enum JobState {
    Running,
    Complete,
    Failed,
}

async fn job_state(restore: &KanidmRestore, ctx: &RestoreContext, name: &str) -> Result<JobState> {
    let ns = restore.namespace().unwrap();
    let job = Api::<Job>::namespaced(ctx.client.clone(), &ns)
        .get(name)
        .await
        .map_err(|e| Error::kube_error("get", "Job", &ns, name, e))?;
    if job.status.as_ref().and_then(|s| s.failed).unwrap_or(0) > 0 {
        return Ok(JobState::Failed);
    }
    if job.status.as_ref().and_then(|s| s.succeeded).unwrap_or(0) > 0 {
        return Ok(JobState::Complete);
    }
    Ok(JobState::Running)
}

#[cfg(test)]
mod tests {
    use super::{
        CONDITION_FAILED, CONDITION_READY, CONDITION_TRUE, KanidmRestorePhase, KanidmRestoreStatus,
        mutable_image, safe_basename, update_restore_conditions,
    };

    #[test]
    fn mutation_boundary_defaults_to_fail_open_before_restore_starts() {
        assert!(!KanidmRestoreStatus::default().database_mutation_started);
    }

    #[test]
    fn completed_restore_sets_ready_condition() {
        let mut status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::Completed,
            ..Default::default()
        };
        update_restore_conditions(&mut status, Some(7));
        assert!(status.conditions.iter().any(|condition| {
            condition.type_ == CONDITION_READY && condition.status == CONDITION_TRUE
        }));
        assert!(!status.conditions.iter().any(|condition| {
            condition.type_ == CONDITION_FAILED && condition.status == CONDITION_TRUE
        }));
    }

    #[test]
    fn failed_restore_sets_failed_condition() {
        let mut status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::Failed,
            message: Some("verification failed".to_string()),
            ..Default::default()
        };
        update_restore_conditions(&mut status, Some(8));
        assert!(status.conditions.iter().any(|condition| {
            condition.type_ == CONDITION_FAILED && condition.status == CONDITION_TRUE
        }));
    }

    #[test]
    fn restore_filename_is_confined_to_backup_directory() {
        assert!(safe_basename("backup.json.gz"));
        assert!(!safe_basename("../backup.json.gz"));
        assert!(!safe_basename("a/b"));
        assert!(!safe_basename(""));
    }

    #[test]
    fn restore_image_rejects_latest_and_unpinned_names() {
        assert!(mutable_image("kanidm/server:latest"));
        assert!(mutable_image("kanidm/server"));
        assert!(!mutable_image("kanidm/server:1.10.0"));
        assert!(!mutable_image("kanidm/server@sha256:abc"));
    }
}
