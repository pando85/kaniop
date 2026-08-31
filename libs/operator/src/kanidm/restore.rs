use super::crd::Kanidm;
use super::reconcile::CLUSTER_LABEL;
use super::reconcile::secret::{SECRET_TYPE_LABEL, SecretType};
use super::reconcile::statefulset::StatefulSetExt;

use kaniop_backup_core::auth::{
    AuthRole, build_auth_env_vars, build_auth_volume_mounts, build_auth_volumes,
    build_ca_bundle_volume, build_ca_bundle_volume_mount, build_encryption_env_vars,
    ca_bundle_env_var, ca_bundle_path,
};
use kaniop_backup_core::crd::{KanidmBackup, KanidmBackupPhase, KanidmBackupRepository};
use kaniop_backup_core::image::data_mover_image;
use kaniop_backup_core::operation::{
    OPERATION_DOC_VERSION, OperationDocument, OperationSpec, UploadOperation,
};
use kaniop_backup_core::result::{ExitCode, parse_result_document};
use kaniop_k8s_util::error::{Error, Result};

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMap, ConfigMapVolumeSource, Container, EnvVar, PersistentVolumeClaim,
    PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec, ResourceRequirements,
    SeccompProfile, Secret, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::api::storage::v1::VolumeAttachment;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams};
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
const SHARED_VOLUME: &str = "safety-backup-shared";
const DATA_PATH: &str = "/data";
const TLS_VOLUME: &str = "kanidm-certs";
const TLS_PATH: &str = "/etc/kanidm/tls";
const BACKUP_PATH: &str = "/data";
const SHARED_VOL_PATH: &str = "/shared";
const REQUEUE: Duration = Duration::from_secs(2);
const CONDITION_TRUE: &str = "True";
const CONDITION_FALSE: &str = "False";
const CONDITION_PROGRESSING: &str = "Progressing";
const CONDITION_READY: &str = "Ready";
const CONDITION_FAILED: &str = "Failed";

pub const BREAK_GLASS_REASON_ANNOTATION: &str = "backup.kaniop.rs/break-glass-reason";
pub const BREAK_GLASS_APPROVED_BY_ANNOTATION: &str = "backup.kaniop.rs/break-glass-approved-by";
const CONDITION_BREAK_GLASS: &str = "BreakGlassOverride";
const TERMINATION_MESSAGE_PATH: &str = "/run/kaniop-result/termination-message";
const SAFETY_BACKUP_RESULT_OPERATION: &str = "upload";
const SOURCE_PREP_RESULT_OPERATION: &str = "download";

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
    pub restore_image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup: Option<SafetyBackupConfig>,
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
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "exactly one of local or backupRef must be set",
            "rule": "(has(self.local) ? 1 : 0) + (has(self.backupRef) ? 1 : 0) == 1"
        }
    ]))
)]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreSource {
    /// Local file source. Mutually exclusive with `backupRef`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<KanidmRestoreLocalSource>,
    /// Remote cataloged backup reference. Mutually exclusive with `local`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_ref: Option<KanidmRestoreBackupRefSource>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreLocalSource {
    /// Basename of a backup below /data/backups. Paths and traversal are rejected.
    pub file_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreBackupRefSource {
    /// Name of a KanidmBackup resource in the same namespace representing a committed remote backup.
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SafetyBackupConfig {
    /// Reference to the KanidmBackupRepository used for the pre-restore safety backup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_ref: Option<SafetyBackupRepositoryRef>,
    /// Skip the mandatory safety backup. Requires break-glass annotations.
    #[serde(default)]
    pub skip: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SafetyBackupRepositoryRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
pub enum KanidmRestorePhase {
    #[default]
    Pending,
    Validating,
    Quiescing,
    SafetyBackup,
    PreparingSource,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup_job_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_prep_job_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup_expected_backup_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup_manifest_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup_payload_sha256: Option<String>,
    #[serde(default)]
    pub replicas_cleared: bool,
    #[serde(default)]
    pub certificates_cleared: bool,
    #[serde(default)]
    pub database_mutation_started: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub original_replicas: Vec<ReplicaCountEntry>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub phase_timestamps: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "schemars",
        schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))
    )]
    pub conditions: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReplicaCountEntry {
    pub group: String,
    pub replicas: i32,
}

#[derive(Clone)]
struct RestoreMetrics {
    attempts: Counter<u64>,
    outcomes: Counter<u64>,
    duration_seconds: Histogram<f64>,
    break_glass_total: Counter<u64>,
    safety_backup_duration_seconds: Histogram<f64>,
}

impl RestoreMetrics {
    fn new() -> Self {
        let meter = global::meter("kaniop");
        Self {
            attempts: meter
                .u64_counter("kaniop_restore_attempts_total")
                .with_description("Number of Kanidm restore attempts started")
                .build(),
            outcomes: meter
                .u64_counter("kaniop_restore_outcomes_total")
                .with_description("Number of terminal Kanidm restore outcomes")
                .build(),
            duration_seconds: meter
                .f64_histogram("kaniop_restore_duration_seconds")
                .with_description("Kanidm restore duration from object creation to terminal phase")
                .with_unit("s")
                .build(),
            break_glass_total: meter
                .u64_counter("kaniop_restore_break_glass_total")
                .with_description("Number of break-glass safety backup overrides")
                .build(),
            safety_backup_duration_seconds: meter
                .f64_histogram("kaniop_restore_safety_backup_duration_seconds")
                .with_description("Duration of pre-restore safety backup creation")
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
                status.original_replicas = capture_original_replicas(&target);
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
                let next_phase = if requires_safety_backup(&restore) {
                    KanidmRestorePhase::SafetyBackup
                } else {
                    record_break_glass(&restore, &ctx).await?;
                    KanidmRestorePhase::PreparingSource
                };
                if next_phase == KanidmRestorePhase::PreparingSource {
                    let refreshed = Api::<KanidmRestore>::namespaced(
                        ctx.client.clone(),
                        restore.namespace().as_deref().unwrap_or_default(),
                    )
                    .get(&restore.name_any())
                    .await
                    .map_err(|error| {
                        Error::kube_error(
                            "get",
                            "KanidmRestore",
                            restore.namespace().as_deref().unwrap_or_default(),
                            restore.name_any(),
                            error,
                        )
                    })?;
                    set_phase(&refreshed, &ctx, next_phase, None).await?;
                } else {
                    set_phase(&restore, &ctx, next_phase, None).await?;
                }
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::SafetyBackup => {
            let target = get_target(&restore, &ctx).await?;
            let name = safety_backup_job_name(&restore);
            let expected_backup_id = compute_safety_backup_id(&restore);
            let expected_manifest_key = compute_safety_manifest_key(&restore);
            ensure_safety_backup_job(
                &restore,
                &target,
                &ctx,
                &name,
                &expected_backup_id,
                &expected_manifest_key,
            )
            .await?;
            let mut status = restore.status.clone().unwrap_or_default();
            status.safety_backup_expected_backup_id = Some(expected_backup_id.clone());
            match job_state(&restore, &ctx, &name).await? {
                JobState::Complete => {
                    let result =
                        read_safety_backup_result(&restore, &ctx, &name, &expected_backup_id).await;
                    match result {
                        Ok(verified) => {
                            status.safety_backup_job_name = Some(name);
                            status.safety_backup_ref =
                                Some(format!("safety-{}", restore.name_any()));
                            status.safety_backup_manifest_key = Some(verified.manifest_key);
                            status.safety_backup_payload_sha256 = Some(verified.payload_sha256);
                            record_safety_backup_duration(&restore, &ctx).await;
                            status.phase = KanidmRestorePhase::PreparingSource;
                            status.message = None;
                            patch_status(&restore, &ctx, status).await?;
                        }
                        Err(error) => {
                            resume_before_mutation(&restore, &ctx).await?;
                            set_phase(
                                &restore,
                                &ctx,
                                KanidmRestorePhase::Failed,
                                Some(format!("safety backup result verification failed: {error}")),
                            )
                            .await?;
                        }
                    }
                }
                JobState::Failed => {
                    resume_before_mutation(&restore, &ctx).await?;
                    set_phase(
                        &restore,
                        &ctx,
                        KanidmRestorePhase::Failed,
                        Some(
                            "safety backup job failed; target restored to original state"
                                .to_string(),
                        ),
                    )
                    .await?;
                }
                JobState::Running => {
                    patch_status(&restore, &ctx, status).await?;
                }
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::PreparingSource => {
            if is_remote_source(&restore) {
                let target = get_target(&restore, &ctx).await?;
                let name = source_prep_job_name(&restore);
                ensure_source_prep_job(&restore, &target, &ctx, &name).await?;
                match job_state(&restore, &ctx, &name).await? {
                    JobState::Complete => {
                        let backup_id = restore
                            .spec
                            .source
                            .backup_ref
                            .as_ref()
                            .map(|b| b.name.clone())
                            .unwrap_or_default();
                        let backup = {
                            let ns = restore.namespace().unwrap();
                            Api::<KanidmBackup>::namespaced(ctx.client.clone(), &ns)
                                .get(&backup_id)
                                .await
                                .map_err(|e| {
                                    Error::kube_error("get", "KanidmBackup", &ns, &backup_id, e)
                                })?
                        };
                        let expected_backup_id = &backup.spec.backup_id;
                        let result =
                            read_source_prep_result(&restore, &ctx, &name, expected_backup_id)
                                .await;
                        match result {
                            Ok(verified) => {
                                if let Some(expected_sha256) = backup
                                    .status
                                    .as_ref()
                                    .and_then(|s| s.payload_sha256.as_deref())
                                {
                                    if verified.payload_sha256 != expected_sha256 {
                                        resume_before_mutation(&restore, &ctx).await?;
                                        set_phase(
                                            &restore,
                                            &ctx,
                                            KanidmRestorePhase::Failed,
                                            Some(format!(
                                                "payload SHA256 mismatch: backup CR has '{expected_sha256}', downloaded payload has '{}'",
                                                verified.payload_sha256
                                            )),
                                        )
                                        .await?;
                                        return Ok(Action::requeue(REQUEUE));
                                    }
                                }
                                let mut status = restore.status.clone().unwrap_or_default();
                                status.source_prep_job_name = Some(name);
                                status.phase = KanidmRestorePhase::RestoringPrimary;
                                status.message = None;
                                patch_status(&restore, &ctx, status).await?;
                            }
                            Err(error) => {
                                resume_before_mutation(&restore, &ctx).await?;
                                set_phase(
                                    &restore,
                                    &ctx,
                                    KanidmRestorePhase::Failed,
                                    Some(format!(
                                        "source preparation result verification failed: {error}"
                                    )),
                                )
                                .await?;
                            }
                        }
                    }
                    JobState::Failed => {
                        resume_before_mutation(&restore, &ctx).await?;
                        set_phase(
                            &restore,
                            &ctx,
                            KanidmRestorePhase::Failed,
                            Some(
                                "source preparation job failed; target restored to original state"
                                    .to_string(),
                            ),
                        )
                        .await?;
                    }
                    JobState::Running => {}
                }
            } else {
                let target = get_target(&restore, &ctx).await?;
                let name = source_check_job_name(&restore);
                ensure_source_check_job(&restore, &target, &ctx, &name).await?;
                match job_state(&restore, &ctx, &name).await? {
                    JobState::Complete => {
                        let mut status = restore.status.clone().unwrap_or_default();
                        status.source_prep_job_name = Some(name);
                        status.phase = KanidmRestorePhase::RestoringPrimary;
                        status.message = None;
                        patch_status(&restore, &ctx, status).await?;
                    }
                    JobState::Failed => {
                        resume_before_mutation(&restore, &ctx).await?;
                        set_phase(
                            &restore,
                            &ctx,
                            KanidmRestorePhase::Failed,
                            Some(
                                "local backup file check failed; target restored to original state"
                                    .to_string(),
                            ),
                        )
                        .await?;
                    }
                    JobState::Running => {}
                }
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::RestoringPrimary => {
            let target = get_target(&restore, &ctx).await?;
            let status = restore.status.as_ref().cloned().unwrap_or_default();
            if requires_safety_backup(&restore) && status.safety_backup_ref.is_none() {
                return Err(Error::MissingData(
                    "invariant violation: database mutation boundary attempted before safetyBackupRef was persisted".to_string(),
                ));
            }
            if !status.database_mutation_started {
                let mut status = status;
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
            if !status.certificates_cleared {
                delete_replica_cert_secrets(&target, &ctx).await?;
                delete_admin_secret(&target, &ctx).await?;
                status.certificates_cleared = true;
                status.message = Some(
                    "replica certificates and admin secret cleared for regeneration".to_string(),
                );
                patch_status(&restore, &ctx, status).await?;
                return Ok(Action::requeue(REQUEUE));
            }
            status.phase = KanidmRestorePhase::Resuming;
            status.message = None;
            patch_status(&restore, &ctx, status).await?;
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::Resuming => {
            let target = get_target(&restore, &ctx).await?;
            set_phase(&restore, &ctx, KanidmRestorePhase::Completed, None).await?;
            clear_restoring(&restore, &target, &ctx).await?;
            Ok(Action::requeue(Duration::from_secs(3600)))
        }
        KanidmRestorePhase::Completed | KanidmRestorePhase::Failed => {
            Ok(Action::requeue(Duration::from_secs(3600)))
        }
    }
}

async fn cleanup(restore: Arc<KanidmRestore>, ctx: Arc<RestoreContext>) -> Result<Action> {
    let status = restore.status.clone().unwrap_or_default();
    if status.database_mutation_started
        && status.phase != KanidmRestorePhase::Completed
        && status.phase != KanidmRestorePhase::Failed
    {
        reconcile_apply(restore.clone(), ctx.clone()).await?;
        return Ok(Action::requeue(REQUEUE));
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
    validate_source(restore)?;
    if is_remote_source(restore) {
        validate_backup_ref(restore, &target, ctx).await?;
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
    validate_safety_backup_config(restore)?;
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

    let break_glass = previous
        .iter()
        .find(|condition| condition.type_ == CONDITION_BREAK_GLASS)
        .cloned();
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
    if let Some(condition) = break_glass {
        status.conditions.push(condition);
    }
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

async fn delete_replica_cert_secrets(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<Secret>::namespaced(ctx.client.clone(), &ns);
    let label_selector = format!(
        "{}={},{}={}",
        CLUSTER_LABEL,
        target.name_any(),
        SECRET_TYPE_LABEL,
        serde_plain::to_string(&SecretType::ReplicaCert).unwrap()
    );
    let secrets = api
        .list(&ListParams::default().labels(&label_selector))
        .await
        .map_err(|e| Error::kube_error("list", "Secret", &ns, &label_selector, e))?;
    let mut all_absent = secrets.items.is_empty();
    for secret in secrets.items {
        let name = secret.name_any();
        if secret.metadata.deletion_timestamp.is_none() {
            match api.delete(&name, &DeleteParams::default()).await {
                Ok(_) => debug!(secret = %name, "deleting stale replica certificate secret"),
                Err(kube::Error::Api(status)) if status.code == 404 => {}
                Err(error) => {
                    return Err(Error::kube_error("delete", "Secret", &ns, &name, error));
                }
            }
            all_absent = false;
        }
    }
    Ok(all_absent)
}

async fn delete_admin_secret(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<Secret>::namespaced(ctx.client.clone(), &ns);
    let name = format!("{}-admin-passwords", target.name_any());
    match api
        .get_opt(&name)
        .await
        .map_err(|e| Error::kube_error("get", "Secret", &ns, &name, e))?
    {
        None => Ok(true),
        Some(secret) => {
            if secret.metadata.deletion_timestamp.is_some() {
                return Ok(false);
            }
            match api.delete(&name, &DeleteParams::default()).await {
                Ok(_) => {
                    debug!(secret = %name, "deleting stale admin secret");
                    Ok(false)
                }
                Err(kube::Error::Api(status)) if status.code == 404 => Ok(true),
                Err(error) => Err(Error::kube_error("delete", "Secret", &ns, &name, error)),
            }
        }
    }
}

fn restore_job_name(restore: &KanidmRestore) -> String {
    format!("{}-restore", restore.name_any())
}
fn verify_job_name(restore: &KanidmRestore) -> String {
    format!("{}-verify", restore.name_any())
}

fn build_kanidm_db_env_vars(target: &Kanidm) -> Vec<EnvVar> {
    let origin = target
        .spec
        .origin
        .clone()
        .unwrap_or_else(|| format!("https://{}", target.spec.domain));
    vec![
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
        EnvVar {
            name: "KANIDM_ORIGIN".to_string(),
            value: Some(origin),
            ..Default::default()
        },
        EnvVar {
            name: "KANIDM_TLS_CHAIN".to_string(),
            value: Some(format!("{TLS_PATH}/tls.crt")),
            ..Default::default()
        },
        EnvVar {
            name: "KANIDM_TLS_KEY".to_string(),
            value: Some(format!("{TLS_PATH}/tls.key")),
            ..Default::default()
        },
    ]
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
        if verify { "backup" } else { "restore" }.to_string(),
    ];
    if verify {
        command.push("/verify/verification.json.gz".to_string());
    } else {
        if is_remote_source(restore) {
            command.push(format!("{DATA_PATH}/source-payload.json.gz"));
        } else {
            let file_name = restore
                .spec
                .source
                .local
                .as_ref()
                .map(|l| l.file_name.clone())
                .unwrap_or_default();
            command.push(format!("{BACKUP_PATH}/{file_name}"));
        }
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
                    security_context: Some(k8s_openapi::api::core::v1::PodSecurityContext {
                        seccomp_profile: Some(SeccompProfile {
                            type_: "RuntimeDefault".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: if verify { "verify" } else { "restore" }.to_string(),
                        image: Some(restore.spec.restore_image.clone()),
                        command: Some(command),
                        env: Some(build_kanidm_db_env_vars(target)),
                        security_context: Some(kanidm_job_security_context()),
                        resources: Some(job_resource_requirements()),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: DATA_VOLUME.to_string(),
                                mount_path: DATA_PATH.to_string(),
                                ..Default::default()
                            },
                            VolumeMount {
                                name: TLS_VOLUME.to_string(),
                                mount_path: TLS_PATH.to_string(),
                                read_only: Some(true),
                                ..Default::default()
                            },
                            VolumeMount {
                                name: "verification-output".to_string(),
                                mount_path: "/verify".to_string(),
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
                            name: TLS_VOLUME.to_string(),
                            secret: Some(k8s_openapi::api::core::v1::SecretVolumeSource {
                                secret_name: Some(target.effective_tls_secret_name()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        Volume {
                            name: "verification-output".to_string(),
                            empty_dir: Some(Default::default()),
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

fn is_remote_source(restore: &KanidmRestore) -> bool {
    restore.spec.source.backup_ref.is_some()
}

fn requires_safety_backup(restore: &KanidmRestore) -> bool {
    let safety = restore.spec.safety_backup.as_ref();
    let skip = safety.is_some_and(|s| s.skip);
    !skip
}

fn validate_source(restore: &KanidmRestore) -> Result<()> {
    let has_local = restore.spec.source.local.is_some();
    let has_backup_ref = restore.spec.source.backup_ref.is_some();
    if has_local && has_backup_ref {
        return Err(Error::MissingData(
            "source.local and source.backupRef are mutually exclusive".to_string(),
        ));
    }
    if !has_local && !has_backup_ref {
        return Err(Error::MissingData(
            "source must specify either local or backupRef".to_string(),
        ));
    }
    if let Some(local) = &restore.spec.source.local {
        if !safe_basename(&local.file_name) {
            return Err(Error::MissingData(
                "restore source fileName must be a safe basename".to_string(),
            ));
        }
    }
    if let Some(backup_ref) = &restore.spec.source.backup_ref {
        if backup_ref.name.is_empty() {
            return Err(Error::MissingData(
                "source.backupRef.name must not be empty".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_safety_backup_config(restore: &KanidmRestore) -> Result<()> {
    let safety = restore.spec.safety_backup.as_ref();
    let skip = safety.is_some_and(|s| s.skip);
    if skip {
        let annotations = restore.metadata.annotations.as_ref();
        let reason = annotations
            .and_then(|a| a.get(BREAK_GLASS_REASON_ANNOTATION))
            .map(|s| s.trim())
            .unwrap_or("");
        let approver = annotations
            .and_then(|a| a.get(BREAK_GLASS_APPROVED_BY_ANNOTATION))
            .map(|s| s.trim())
            .unwrap_or("");
        if reason.is_empty() {
            return Err(Error::MissingData(format!(
                "break-glass requires non-empty annotation '{BREAK_GLASS_REASON_ANNOTATION}'"
            )));
        }
        if approver.is_empty() {
            return Err(Error::MissingData(format!(
                "break-glass requires non-empty annotation '{BREAK_GLASS_APPROVED_BY_ANNOTATION}'"
            )));
        }
    }
    if is_remote_source(restore) && !skip {
        let has_repo = safety
            .and_then(|s| s.repository_ref.as_ref())
            .is_some_and(|r| !r.name.is_empty());
        if !has_repo {
            return Err(Error::MissingData(
                "remote restore requires safetyBackup.repositoryRef".to_string(),
            ));
        }
    }
    Ok(())
}

fn capture_original_replicas(target: &Kanidm) -> Vec<ReplicaCountEntry> {
    target
        .spec
        .replica_groups
        .iter()
        .map(|rg| ReplicaCountEntry {
            group: rg.name.clone(),
            replicas: rg.replicas,
        })
        .collect()
}

async fn resume_before_mutation(restore: &KanidmRestore, ctx: &RestoreContext) -> Result<()> {
    if let Ok(target) = get_target(restore, ctx).await {
        let owns_maintenance =
            target.annotations().get(RESTORE_ANNOTATION) == restore.uid().as_ref();
        if owns_maintenance {
            scale_desired(&target, ctx).await?;
            clear_restoring(restore, &target, ctx).await?;
        }
    }
    Ok(())
}

async fn record_break_glass(restore: &KanidmRestore, ctx: &RestoreContext) -> Result<()> {
    ctx.metrics.break_glass_total.add(1, &[]);
    let reason = restore
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(BREAK_GLASS_REASON_ANNOTATION))
        .cloned()
        .unwrap_or_default();
    let approver = restore
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(BREAK_GLASS_APPROVED_BY_ANNOTATION))
        .cloned()
        .unwrap_or_default();
    let mut status = restore.status.clone().unwrap_or_default();
    let break_glass_condition = restore_condition(
        &status.conditions,
        CONDITION_BREAK_GLASS,
        CONDITION_TRUE,
        "BreakGlassOverride",
        &format!("Safety backup skipped via break-glass. reason={reason}, approvedBy={approver}"),
        restore.metadata.generation,
    );
    status
        .conditions
        .retain(|c| c.type_ != CONDITION_BREAK_GLASS);
    status.conditions.push(break_glass_condition);
    patch_status(restore, ctx, status).await?;
    if let Err(error) = ctx
        .recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: "SafetyBackupSkipped".to_string(),
                note: Some(format!(
                    "Break-glass: safety backup skipped. reason={reason}, approvedBy={approver}"
                )),
                action: "Restore".to_string(),
                secondary: None,
            },
            &restore.object_ref(&()),
        )
        .await
    {
        warn!(restore = %restore.name_any(), %error, "failed to publish break-glass event");
    }
    warn!(
        restore = %restore.name_any(),
        reason = %reason,
        approver = %approver,
        "break-glass safety backup override activated"
    );
    Ok(())
}

async fn record_safety_backup_duration(restore: &KanidmRestore, ctx: &RestoreContext) {
    let status = restore.status.as_ref().cloned().unwrap_or_default();
    if let Some(start) = status.phase_timestamps.get("SafetyBackup") {
        if let Ok(start_ts) = start.parse::<Timestamp>() {
            let elapsed = (Timestamp::now().as_second() - start_ts.as_second()).max(0) as f64;
            ctx.metrics
                .safety_backup_duration_seconds
                .record(elapsed, &[]);
        }
    }
}

fn safety_backup_job_name(restore: &KanidmRestore) -> String {
    format!("{}-safety-backup", restore.name_any())
}

fn source_prep_job_name(restore: &KanidmRestore) -> String {
    format!("{}-source-prep", restore.name_any())
}

fn source_check_job_name(restore: &KanidmRestore) -> String {
    format!("{}-source-check", restore.name_any())
}

async fn ensure_source_check_job(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
    name: &str,
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
    let file_name = restore
        .spec
        .source
        .local
        .as_ref()
        .map(|l| l.file_name.clone())
        .unwrap_or_default();
    let operation_doc = serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "check",
        "path": format!("{BACKUP_PATH}/{file_name}"),
        "resultPath": "/run/kaniop-result/result.json",
        "format": "kanidmJsonGzip",
    })
    .to_string();
    let operation_cm_name = format!("{}-source-check-op", restore.name_any());
    ensure_operation_configmap(restore, &operation_cm_name, &operation_doc, &ns, ctx).await?;
    let data_mover_image = data_mover_image();
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
                    security_context: Some(k8s_openapi::api::core::v1::PodSecurityContext {
                        run_as_non_root: Some(true),
                        seccomp_profile: Some(k8s_openapi::api::core::v1::SeccompProfile {
                            type_: "RuntimeDefault".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: "source-check".to_string(),
                        image: Some(data_mover_image),
                        command: Some(vec![
                            "/bin/kaniop-data-mover".to_string(),
                            "check".to_string(),
                            "--operation-doc".to_string(),
                            "/run/kaniop/operation.json".to_string(),
                        ]),
                        security_context: Some(hardened_security_context()),
                        resources: Some(job_resource_requirements()),
                        env: Some(vec![EnvVar {
                            name: "RUST_LOG".to_string(),
                            value: Some("info".to_string()),
                            ..Default::default()
                        }]),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: DATA_VOLUME.to_string(),
                                mount_path: DATA_PATH.to_string(),
                                read_only: Some(true),
                                ..Default::default()
                            },
                            VolumeMount {
                                name: "operation".to_string(),
                                mount_path: "/run/kaniop".to_string(),
                                read_only: Some(true),
                                ..Default::default()
                            },
                            VolumeMount {
                                name: "result".to_string(),
                                mount_path: "/run/kaniop-result".to_string(),
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
                                read_only: Some(true),
                            }),
                            ..Default::default()
                        },
                        Volume {
                            name: "operation".to_string(),
                            config_map: Some(k8s_openapi::api::core::v1::ConfigMapVolumeSource {
                                name: operation_cm_name,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        Volume {
                            name: "result".to_string(),
                            empty_dir: Some(Default::default()),
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

const SAFETY_BACKUP_NAMESPACE: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";

fn compute_safety_backup_id(restore: &KanidmRestore) -> String {
    let ns = restore.namespace().unwrap_or_default();
    let name = restore.name_any();
    let namespace_uuid =
        uuid::Uuid::parse_str(SAFETY_BACKUP_NAMESPACE).expect("valid UUID for namespace");
    let name_uuid = uuid::Uuid::new_v5(&namespace_uuid, ns.as_bytes());
    uuid::Uuid::new_v5(&name_uuid, name.as_bytes()).to_string()
}

fn compute_safety_manifest_key(restore: &KanidmRestore) -> String {
    let ns = restore.namespace().unwrap_or_default();
    let kanidm_uid = &restore.spec.target_ref.uid;
    let backup_id = compute_safety_backup_id(restore);
    format!("v1/tenants/{ns}/clusters/{kanidm_uid}/backups/{backup_id}/manifest.json")
}

struct VerifiedSafetyBackup {
    manifest_key: String,
    payload_sha256: String,
}

async fn read_safety_backup_result(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    job_name: &str,
    expected_backup_id: &str,
) -> Result<VerifiedSafetyBackup> {
    let ns = restore.namespace().unwrap();
    let pods = Api::<Pod>::namespaced(ctx.client.clone(), &ns);
    let pod_list = pods
        .list(&ListParams::default().labels(&format!("job-name={job_name}")))
        .await
        .map_err(|e| Error::kube_error("list", "Pod", &ns, job_name, e))?;
    let pod = pod_list.items.first().ok_or_else(|| {
        Error::MissingData(format!("no pod found for safety backup job {job_name}"))
    })?;
    let pod_name = pod.name_any();
    let container_status = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())
        .and_then(|statuses| statuses.iter().find(|cs| cs.name == "safety-upload"))
        .ok_or_else(|| {
            Error::MissingData(format!(
                "safety-upload container status not found in pod {pod_name}"
            ))
        })?;
    let termination_message = container_status
        .state
        .as_ref()
        .and_then(|state| state.terminated.as_ref())
        .and_then(|t| t.message.as_ref())
        .ok_or_else(|| {
            Error::MissingData(format!(
                "termination message not found in pod {pod_name}; result transport failed"
            ))
        })?;
    if termination_message.is_empty() {
        return Err(Error::MissingData(format!(
            "termination message is empty in pod {pod_name}; result document was not written"
        )));
    }
    let result_doc = parse_result_document(termination_message).map_err(|e| {
        Error::ParseError(format!(
            "failed to parse result document from pod {pod_name}: {e}"
        ))
    })?;
    if result_doc.api_version != kaniop_backup_core::result::RESULT_DOC_VERSION {
        return Err(Error::ParseError(format!(
            "unsupported result document version: {}",
            result_doc.api_version
        )));
    }
    if result_doc.kind != "ResultDocument" {
        return Err(Error::ParseError(format!(
            "invalid result document kind: {}",
            result_doc.kind
        )));
    }
    if result_doc.operation != SAFETY_BACKUP_RESULT_OPERATION {
        return Err(Error::ParseError(format!(
            "result document operation mismatch: expected '{}', got '{}'",
            SAFETY_BACKUP_RESULT_OPERATION, result_doc.operation
        )));
    }
    if !result_doc.success {
        return Err(Error::ParseError(format!(
            "safety backup reported failure with exit code {:?}",
            result_doc.exit_code
        )));
    }
    if result_doc.exit_code != ExitCode::Success {
        return Err(Error::ParseError(format!(
            "safety backup exit code is not Success: {:?}",
            result_doc.exit_code
        )));
    }
    let doc_backup_id = result_doc
        .backup_id
        .as_deref()
        .ok_or_else(|| Error::ParseError("result document missing backupId".to_string()))?;
    if doc_backup_id != expected_backup_id {
        return Err(Error::ParseError(format!(
            "backup ID mismatch: expected '{expected_backup_id}', got '{doc_backup_id}'"
        )));
    }
    let manifest_key = result_doc
        .manifest_key
        .clone()
        .ok_or_else(|| Error::ParseError("result document missing manifestKey".to_string()))?;
    if manifest_key.is_empty() {
        return Err(Error::ParseError(
            "result document manifestKey is empty".to_string(),
        ));
    }
    let payload_sha256 = result_doc
        .payload_sha256
        .clone()
        .ok_or_else(|| Error::ParseError("result document missing payloadSha256".to_string()))?;
    if payload_sha256.is_empty() {
        return Err(Error::ParseError(
            "result document payloadSha256 is empty".to_string(),
        ));
    }
    info!(
        restore = %restore.name_any(),
        backup_id = %expected_backup_id,
        manifest_key = %manifest_key,
        "safety backup result verified"
    );
    Ok(VerifiedSafetyBackup {
        manifest_key,
        payload_sha256,
    })
}

struct VerifiedSourcePrepResult {
    #[allow(dead_code)]
    manifest_key: String,
    payload_sha256: String,
}

async fn read_source_prep_result(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    job_name: &str,
    expected_backup_id: &str,
) -> Result<VerifiedSourcePrepResult> {
    let ns = restore.namespace().unwrap();
    let pods = Api::<Pod>::namespaced(ctx.client.clone(), &ns);
    let pod_list = pods
        .list(&ListParams::default().labels(&format!("job-name={job_name}")))
        .await
        .map_err(|e| Error::kube_error("list", "Pod", &ns, job_name, e))?;
    let pod = pod_list.items.first().ok_or_else(|| {
        Error::MissingData(format!("no pod found for source prep job {job_name}"))
    })?;
    let pod_name = pod.name_any();
    let container_status = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())
        .and_then(|statuses| statuses.iter().find(|cs| cs.name == "source-prep"))
        .ok_or_else(|| {
            Error::MissingData(format!(
                "source-prep container status not found in pod {pod_name}"
            ))
        })?;
    let termination_message = container_status
        .state
        .as_ref()
        .and_then(|state| state.terminated.as_ref())
        .and_then(|t| t.message.as_ref())
        .ok_or_else(|| {
            Error::MissingData(format!(
                "termination message not found in pod {pod_name}; result transport failed"
            ))
        })?;
    if termination_message.is_empty() {
        return Err(Error::MissingData(format!(
            "termination message is empty in pod {pod_name}; result document was not written"
        )));
    }
    let result_doc = parse_result_document(termination_message).map_err(|e| {
        Error::ParseError(format!(
            "failed to parse result document from pod {pod_name}: {e}"
        ))
    })?;
    if result_doc.api_version != kaniop_backup_core::result::RESULT_DOC_VERSION {
        return Err(Error::ParseError(format!(
            "unsupported result document version: {}",
            result_doc.api_version
        )));
    }
    if result_doc.kind != "ResultDocument" {
        return Err(Error::ParseError(format!(
            "invalid result document kind: {}",
            result_doc.kind
        )));
    }
    if result_doc.operation != SOURCE_PREP_RESULT_OPERATION {
        return Err(Error::ParseError(format!(
            "result document operation mismatch: expected '{}', got '{}'",
            SOURCE_PREP_RESULT_OPERATION, result_doc.operation
        )));
    }
    if !result_doc.success {
        return Err(Error::ParseError(format!(
            "source prep reported failure with exit code {:?}",
            result_doc.exit_code
        )));
    }
    if result_doc.exit_code != ExitCode::Success {
        return Err(Error::ParseError(format!(
            "source prep exit code is not Success: {:?}",
            result_doc.exit_code
        )));
    }
    let doc_backup_id = result_doc
        .backup_id
        .as_deref()
        .ok_or_else(|| Error::ParseError("result document missing backupId".to_string()))?;
    if doc_backup_id != expected_backup_id {
        return Err(Error::ParseError(format!(
            "backup ID mismatch: expected '{expected_backup_id}', got '{doc_backup_id}'"
        )));
    }
    let manifest_key = result_doc
        .manifest_key
        .clone()
        .ok_or_else(|| Error::ParseError("result document missing manifestKey".to_string()))?;
    if manifest_key.is_empty() {
        return Err(Error::ParseError(
            "result document manifestKey is empty".to_string(),
        ));
    }
    let payload_sha256 = result_doc
        .payload_sha256
        .clone()
        .ok_or_else(|| Error::ParseError("result document missing payloadSha256".to_string()))?;
    if payload_sha256.is_empty() {
        return Err(Error::ParseError(
            "result document payloadSha256 is empty".to_string(),
        ));
    }
    info!(
        restore = %restore.name_any(),
        backup_id = %expected_backup_id,
        manifest_key = %manifest_key,
        payload_sha256 = %payload_sha256,
        "source prep result verified"
    );
    Ok(VerifiedSourcePrepResult {
        manifest_key,
        payload_sha256,
    })
}

fn hardened_security_context() -> SecurityContext {
    SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(k8s_openapi::api::core::v1::Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        }),
        read_only_root_filesystem: Some(true),
        run_as_non_root: Some(true),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn kanidm_job_security_context() -> SecurityContext {
    SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(k8s_openapi::api::core::v1::Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        }),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn job_resource_requirements() -> ResourceRequirements {
    ResourceRequirements {
        requests: Some(std::collections::BTreeMap::from([
            ("cpu".to_string(), Quantity("100m".to_string())),
            ("memory".to_string(), Quantity("128Mi".to_string())),
        ])),
        limits: Some(std::collections::BTreeMap::from([
            ("cpu".to_string(), Quantity("2".to_string())),
            ("memory".to_string(), Quantity("2Gi".to_string())),
        ])),
        ..Default::default()
    }
}

async fn ensure_safety_backup_job(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
    name: &str,
    backup_id: &str,
    manifest_key: &str,
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
    let safety = restore
        .spec
        .safety_backup
        .as_ref()
        .ok_or_else(|| Error::MissingData("safetyBackup config required".to_string()))?;
    let repo_ref = safety
        .repository_ref
        .as_ref()
        .ok_or_else(|| Error::MissingData("safetyBackup.repositoryRef required".to_string()))?;
    let repo = Api::<KanidmBackupRepository>::namespaced(ctx.client.clone(), &ns)
        .get(&repo_ref.name)
        .await
        .map_err(|e| Error::kube_error("get", "KanidmBackupRepository", &ns, &repo_ref.name, e))?;
    let _namespace_uid = &ns;
    let _kanidm_uid = &restore.spec.target_ref.uid;
    let endpoint = &repo.spec.s3.endpoint;
    let region = &repo.spec.s3.region;
    let operation_doc = build_safety_upload_operation_doc(
        restore,
        target,
        backup_id,
        manifest_key,
        &repo.spec.s3.bucket,
        &repo.spec.s3.prefix,
        endpoint,
        region,
        repo.spec.s3.force_path_style,
        repo.spec.s3.insecure,
        repo.spec
            .s3
            .ca_bundle_ref
            .as_deref()
            .map(|_| ca_bundle_path())
            .as_deref(),
        repo.spec.encryption.as_ref(),
    )?;
    let operation_cm_name = format!("{name}-op");
    ensure_operation_configmap(restore, &operation_cm_name, &operation_doc, &ns, ctx).await?;
    let auth_method = &repo.spec.authentication.writer;
    let data_mover_image = data_mover_image();
    let init_command = vec![
        "kanidmd".to_string(),
        "database".to_string(),
        "backup".to_string(),
        format!("{SHARED_VOL_PATH}/safety-backup.json.gz"),
    ];
    let upload_script = format!(
        r#"set -eu
/bin/kaniop-data-mover upload --operation-doc /run/kaniop/operation.json
RESULT_FILE="/run/kaniop-result/result.json"
TERM_MSG="{TERMINATION_MESSAGE_PATH}"
if [ ! -f "$RESULT_FILE" ]; then
  echo "ERROR: result document not found at $RESULT_FILE"
  printf '%.4096s' "result document not found at $RESULT_FILE" > "$TERM_MSG"
  exit 1
fi
if ! grep -q '"success": true' "$RESULT_FILE" || ! grep -q '"exitCode": "success"' "$RESULT_FILE"; then
  echo "ERROR: safety backup upload reported failure"
  dd if="$RESULT_FILE" bs=4000 count=1 2>/dev/null
  dd if="$RESULT_FILE" of="$TERM_MSG" bs=4096 count=1 2>/dev/null
  exit 1
fi
dd if="$RESULT_FILE" of="$TERM_MSG" bs=4096 count=1 2>/dev/null
echo "safety backup upload completed successfully"
"#
    );
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
                    security_context: Some(k8s_openapi::api::core::v1::PodSecurityContext {
                        seccomp_profile: Some(k8s_openapi::api::core::v1::SeccompProfile {
                            type_: "RuntimeDefault".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    init_containers: Some(vec![Container {
                        name: "safety-backup".to_string(),
                        image: Some(restore.spec.restore_image.clone()),
                        command: Some(init_command),
                        security_context: Some(SecurityContext {
                            allow_privilege_escalation: Some(false),
                            capabilities: Some(Capabilities {
                                drop: Some(vec!["ALL".to_string()]),
                                ..Default::default()
                            }),
                            read_only_root_filesystem: Some(true),
                            seccomp_profile: Some(SeccompProfile {
                                type_: "RuntimeDefault".to_string(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        resources: Some(job_resource_requirements()),
                        env: Some(build_kanidm_db_env_vars(target)),
                        volume_mounts: Some(vec![
                            VolumeMount {
                                name: DATA_VOLUME.to_string(),
                                mount_path: DATA_PATH.to_string(),
                                read_only: Some(false),
                                ..Default::default()
                            },
                            VolumeMount {
                                name: TLS_VOLUME.to_string(),
                                mount_path: TLS_PATH.to_string(),
                                read_only: Some(true),
                                ..Default::default()
                            },
                            VolumeMount {
                                name: SHARED_VOLUME.to_string(),
                                mount_path: SHARED_VOL_PATH.to_string(),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    }]),
                    containers: vec![Container {
                        name: "safety-upload".to_string(),
                        image: Some(data_mover_image),
                        command: Some(vec!["/bin/sh".to_string()]),
                        args: Some(vec!["-c".to_string(), upload_script]),
                        security_context: Some(hardened_security_context()),
                        resources: Some(job_resource_requirements()),
                        env: {
                            let mut env_vars =
                                build_auth_env_vars(auth_method, &repo_ref.name, AuthRole::Writer);
                            env_vars.push(EnvVar {
                                name: "RUST_LOG".to_string(),
                                value: Some("info".to_string()),
                                ..Default::default()
                            });
                            if repo.spec.s3.ca_bundle_ref.is_some() {
                                env_vars.push(ca_bundle_env_var());
                            }
                            env_vars.extend(build_encryption_env_vars(
                                repo.spec
                                    .encryption
                                    .as_ref()
                                    .and_then(|e| e.key_ref.as_ref()),
                            ));
                            Some(env_vars)
                        },
                        termination_message_path: Some(TERMINATION_MESSAGE_PATH.to_string()),
                        termination_message_policy: Some("FallbackToLogsOnError".to_string()),
                        volume_mounts: {
                            let mut mounts = vec![
                                VolumeMount {
                                    name: SHARED_VOLUME.to_string(),
                                    mount_path: SHARED_VOL_PATH.to_string(),
                                    read_only: Some(true),
                                    ..Default::default()
                                },
                                VolumeMount {
                                    name: "operation".to_string(),
                                    mount_path: "/run/kaniop".to_string(),
                                    read_only: Some(true),
                                    ..Default::default()
                                },
                                VolumeMount {
                                    name: "result".to_string(),
                                    mount_path: "/run/kaniop-result".to_string(),
                                    ..Default::default()
                                },
                            ];
                            mounts.extend(build_auth_volume_mounts(auth_method));
                            if repo.spec.s3.ca_bundle_ref.is_some() {
                                mounts.push(build_ca_bundle_volume_mount());
                            }
                            Some(mounts)
                        },
                        ..Default::default()
                    }],
                    volumes: {
                        let mut vols = vec![
                            Volume {
                                name: DATA_VOLUME.to_string(),
                                persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                                    claim_name: primary_pvc_name(target)?,
                                    read_only: Some(false),
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: TLS_VOLUME.to_string(),
                                secret: Some(k8s_openapi::api::core::v1::SecretVolumeSource {
                                    secret_name: Some(target.effective_tls_secret_name()),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: SHARED_VOLUME.to_string(),
                                empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
                                    size_limit: Some(crate::controller::backup_job_volume_size()),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: "operation".to_string(),
                                config_map: Some(ConfigMapVolumeSource {
                                    name: operation_cm_name,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: "result".to_string(),
                                empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
                                    size_limit: Some(Quantity("16Mi".to_string())),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        ];
                        vols.extend(build_auth_volumes(auth_method));
                        if let Some(ca_bundle_ref) = &repo.spec.s3.ca_bundle_ref {
                            vols.push(build_ca_bundle_volume(ca_bundle_ref));
                        }
                        Some(vols)
                    },
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

async fn ensure_source_prep_job(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
    name: &str,
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
    let backup_name = restore
        .spec
        .source
        .backup_ref
        .as_ref()
        .map(|b| b.name.clone())
        .unwrap_or_default();
    let backup = Api::<KanidmBackup>::namespaced(ctx.client.clone(), &ns)
        .get(&backup_name)
        .await
        .map_err(|e| Error::kube_error("get", "KanidmBackup", &ns, &backup_name, e))?;
    let repo = Api::<KanidmBackupRepository>::namespaced(ctx.client.clone(), &ns)
        .get(&backup.spec.repository_ref.name)
        .await
        .map_err(|e| {
            Error::kube_error(
                "get",
                "KanidmBackupRepository",
                &ns,
                &backup.spec.repository_ref.name,
                e,
            )
        })?;
    let endpoint = &repo.spec.s3.endpoint;
    let region = &repo.spec.s3.region;
    let operation_doc = build_download_operation_doc(
        restore,
        target,
        &backup.spec.manifest_key,
        &backup.spec.backup_id,
        &repo.spec.s3.bucket,
        &repo.spec.s3.prefix,
        endpoint,
        region,
        repo.spec.s3.force_path_style,
        repo.spec.s3.insecure,
        repo.spec
            .s3
            .ca_bundle_ref
            .as_deref()
            .map(|_| ca_bundle_path())
            .as_deref(),
        repo.spec.encryption.as_ref(),
    );
    let operation_cm_name = format!("{}-source-prep-op", restore.name_any());
    ensure_operation_configmap(restore, &operation_cm_name, &operation_doc, &ns, ctx).await?;
    let auth_method = &repo.spec.authentication.reader;
    let data_mover_image = data_mover_image();
    let download_script = format!(
        r#"set -eu
/bin/kaniop-data-mover download --operation-doc /run/kaniop/operation.json
RESULT_FILE="/run/kaniop-result/result.json"
TERM_MSG="{TERMINATION_MESSAGE_PATH}"
if [ ! -f "$RESULT_FILE" ]; then
  echo "ERROR: result document not found at $RESULT_FILE"
  printf '%.4096s' "result document not found at $RESULT_FILE" > "$TERM_MSG"
  exit 1
fi
if ! grep -q '"success": true' "$RESULT_FILE" || ! grep -q '"exitCode": "success"' "$RESULT_FILE"; then
  echo "ERROR: source preparation download reported failure"
  dd if="$RESULT_FILE" bs=4000 count=1 2>/dev/null
  dd if="$RESULT_FILE" of="$TERM_MSG" bs=4096 count=1 2>/dev/null
  exit 1
fi
dd if="$RESULT_FILE" of="$TERM_MSG" bs=4096 count=1 2>/dev/null
echo "source preparation download completed successfully"
"#
    );
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
                    security_context: Some(k8s_openapi::api::core::v1::PodSecurityContext {
                        run_as_non_root: Some(true),
                        seccomp_profile: Some(k8s_openapi::api::core::v1::SeccompProfile {
                            type_: "RuntimeDefault".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: "source-prep".to_string(),
                        image: Some(data_mover_image),
                        command: Some(vec!["/bin/sh".to_string()]),
                        args: Some(vec!["-c".to_string(), download_script]),
                        security_context: Some(hardened_security_context()),
                        resources: Some(job_resource_requirements()),
                        termination_message_path: Some(TERMINATION_MESSAGE_PATH.to_string()),
                        termination_message_policy: Some("FallbackToLogsOnError".to_string()),
                        env: {
                            let mut env_vars = build_auth_env_vars(
                                auth_method,
                                &backup.spec.repository_ref.name,
                                AuthRole::Reader,
                            );
                            env_vars.push(EnvVar {
                                name: "RUST_LOG".to_string(),
                                value: Some("info".to_string()),
                                ..Default::default()
                            });
                            if repo.spec.s3.ca_bundle_ref.is_some() {
                                env_vars.push(ca_bundle_env_var());
                            }
                            env_vars.extend(build_encryption_env_vars(
                                repo.spec
                                    .encryption
                                    .as_ref()
                                    .and_then(|e| e.key_ref.as_ref()),
                            ));
                            Some(env_vars)
                        },
                        volume_mounts: {
                            let mut mounts = vec![
                                VolumeMount {
                                    name: DATA_VOLUME.to_string(),
                                    mount_path: DATA_PATH.to_string(),
                                    ..Default::default()
                                },
                                VolumeMount {
                                    name: "operation".to_string(),
                                    mount_path: "/run/kaniop".to_string(),
                                    read_only: Some(true),
                                    ..Default::default()
                                },
                                VolumeMount {
                                    name: "result".to_string(),
                                    mount_path: "/run/kaniop-result".to_string(),
                                    ..Default::default()
                                },
                            ];
                            mounts.extend(build_auth_volume_mounts(auth_method));
                            if repo.spec.s3.ca_bundle_ref.is_some() {
                                mounts.push(build_ca_bundle_volume_mount());
                            }
                            Some(mounts)
                        },
                        ..Default::default()
                    }],
                    volumes: {
                        let mut vols = vec![
                            Volume {
                                name: DATA_VOLUME.to_string(),
                                persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                                    claim_name: primary_pvc_name(target)?,
                                    read_only: Some(false),
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: "operation".to_string(),
                                config_map: Some(ConfigMapVolumeSource {
                                    name: operation_cm_name,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            Volume {
                                name: "result".to_string(),
                                empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
                                    size_limit: Some(Quantity("16Mi".to_string())),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        ];
                        vols.extend(build_auth_volumes(auth_method));
                        if let Some(ca_bundle_ref) = &repo.spec.s3.ca_bundle_ref {
                            vols.push(build_ca_bundle_volume(ca_bundle_ref));
                        }
                        Some(vols)
                    },
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

#[allow(clippy::too_many_arguments)]
fn build_safety_upload_operation_doc(
    restore: &KanidmRestore,
    target: &Kanidm,
    backup_id: &str,
    manifest_key: &str,
    bucket: &str,
    prefix: &str,
    endpoint: &str,
    region: &str,
    force_path_style: bool,
    insecure: bool,
    ca_bundle_path: Option<&str>,
    encryption: Option<&kaniop_backup_core::crd::RepositoryEncryption>,
) -> Result<String> {
    let enc_mode =
        encryption.map(|e| serde_json::to_value(&e.mode).unwrap_or(serde_json::Value::Null));
    let enc_key_id = encryption
        .and_then(|e| e.key_id.as_ref())
        .map(|s| serde_json::Value::String(s.clone()));
    let op = UploadOperation {
        payload_path: format!("{SHARED_VOL_PATH}/safety-backup.json.gz"),
        bucket: bucket.to_string(),
        prefix: prefix.to_string(),
        endpoint: endpoint.to_string(),
        region: region.to_string(),
        force_path_style,
        insecure,
        ca_bundle_path: ca_bundle_path.map(str::to_string),
        backup_id: backup_id.to_string(),
        namespace_uid: restore.namespace().unwrap_or_default(),
        kanidm_uid: restore.spec.target_ref.uid.clone(),
        kanidm_name: restore.spec.target_ref.name.clone(),
        domain: target.spec.domain.clone(),
        kanidm_version: target
            .status
            .as_ref()
            .and_then(|s| s.version.as_ref())
            .map(|v| v.image_tag.clone())
            .unwrap_or_default(),
        image_digest: None,
        consistency: "kanidm-offline".to_string(),
        reason: "restore-safety".to_string(),
        encryption_mode: enc_mode.and_then(|v| v.as_str().map(String::from)),
        encryption_key_id: enc_key_id.and_then(|v| v.as_str().map(String::from)),
        result_path: "/run/kaniop-result/result.json".to_string(),
        max_concurrent_parts: 4,
        max_retries: 3,
    };
    let _ = manifest_key;
    let doc = OperationDocument {
        api_version: OPERATION_DOC_VERSION.to_string(),
        kind: "OperationDocument".to_string(),
        spec: OperationSpec::Upload(op),
    };
    serde_json::to_string(&doc)
        .map_err(|e| Error::MissingData(format!("failed to serialize operation document: {e}")))
}

async fn validate_backup_ref(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<()> {
    let backup_ref = restore
        .spec
        .source
        .backup_ref
        .as_ref()
        .ok_or_else(|| Error::MissingData("backupRef source not set".to_string()))?;
    let ns = restore.namespace().unwrap();
    let backup = Api::<KanidmBackup>::namespaced(ctx.client.clone(), &ns)
        .get(&backup_ref.name)
        .await
        .map_err(|e| Error::kube_error("get", "KanidmBackup", &ns, &backup_ref.name, e))?;
    if backup.status.as_ref().map(|s| &s.phase) != Some(&KanidmBackupPhase::Ready) {
        return Err(Error::MissingData(format!(
            "KanidmBackup '{}' is not Ready (phase: {:?})",
            backup_ref.name,
            backup.status.as_ref().map(|s| &s.phase)
        )));
    }
    if backup.spec.kanidm_ref.uid != restore.spec.target_ref.uid {
        return Err(Error::MissingData(format!(
            "KanidmBackup kanidmRef.uid '{}' does not match target UID '{}'",
            backup.spec.kanidm_ref.uid, restore.spec.target_ref.uid
        )));
    }
    if backup.spec.kanidm_ref.name != restore.spec.target_ref.name {
        return Err(Error::MissingData(format!(
            "KanidmBackup kanidmRef.name '{}' does not match target name '{}'",
            backup.spec.kanidm_ref.name, restore.spec.target_ref.name
        )));
    }
    let repo = Api::<KanidmBackupRepository>::namespaced(ctx.client.clone(), &ns)
        .get(&backup.spec.repository_ref.name)
        .await
        .map_err(|e| {
            Error::kube_error(
                "get",
                "KanidmBackupRepository",
                &ns,
                &backup.spec.repository_ref.name,
                e,
            )
        })?;
    if !has_accepted_condition(
        &repo
            .status
            .as_ref()
            .map(|s| &s.conditions)
            .cloned()
            .unwrap_or_default(),
    ) {
        return Err(Error::MissingData(format!(
            "KanidmBackupRepository '{}' configuration has not been accepted",
            backup.spec.repository_ref.name
        )));
    }
    validate_backup_compatibility(restore, target, &backup)?;
    Ok(())
}

fn has_accepted_condition(conditions: &[Condition]) -> bool {
    conditions
        .iter()
        .any(|c| c.type_ == "Ready" && c.status == "True" && c.reason == "Accepted")
}

fn validate_backup_compatibility(
    restore: &KanidmRestore,
    target: &Kanidm,
    backup: &KanidmBackup,
) -> Result<()> {
    let status = backup.status.as_ref();
    if let Some(backup_version) = status.and_then(|s| s.kanidm_version.as_ref()) {
        let target_version = target
            .status
            .as_ref()
            .and_then(|s| s.version.as_ref())
            .map(|v| v.image_tag.as_str());
        if let Some(tv) = target_version {
            if !tv.is_empty() && !backup_version.is_empty() && tv != backup_version {
                return Err(Error::MissingData(format!(
                    "backup Kanidm version '{backup_version}' does not match target version '{tv}'"
                )));
            }
        }
    }
    if let Some(backup_digest) = status.and_then(|s| s.image_digest.as_ref()) {
        if !backup_digest.is_empty()
            && restore.spec.restore_image.contains('@')
            && !restore.spec.restore_image.contains(backup_digest)
        {
            return Err(Error::MissingData(format!(
                "restore image digest does not match backup image digest '{backup_digest}'"
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_download_operation_doc(
    restore: &KanidmRestore,
    target: &Kanidm,
    manifest_key: &str,
    expected_backup_id: &str,
    bucket: &str,
    prefix: &str,
    endpoint: &str,
    region: &str,
    force_path_style: bool,
    insecure: bool,
    ca_bundle_path: Option<&str>,
    encryption: Option<&kaniop_backup_core::crd::RepositoryEncryption>,
) -> String {
    let enc_mode =
        encryption.map(|e| serde_json::to_value(&e.mode).unwrap_or(serde_json::Value::Null));
    let enc_key_id = encryption
        .and_then(|e| e.key_id.as_ref())
        .map(|s| serde_json::Value::String(s.clone()));
    serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "download",
        "manifestKey": manifest_key,
        "bucket": bucket,
        "prefix": prefix,
        "endpoint": endpoint,
        "region": region,
        "forcePathStyle": force_path_style,
        "insecure": insecure,
        "caBundlePath": ca_bundle_path,
        "expectedBackupId": expected_backup_id,
        "expectedKanidmUid": restore.spec.target_ref.uid,
        "expectedDomain": target.spec.domain,
        "outputPath": format!("{DATA_PATH}/source-payload.json.gz"),
        "resultPath": "/run/kaniop-result/result.json",
        "maxRetries": 3,
        "encryptionMode": enc_mode,
        "encryptionKeyId": enc_key_id,
    })
    .to_string()
}

async fn ensure_operation_configmap(
    restore: &KanidmRestore,
    name: &str,
    data: &str,
    ns: &str,
    ctx: &RestoreContext,
) -> Result<()> {
    let api = Api::<ConfigMap>::namespaced(ctx.client.clone(), ns);
    if api
        .get_opt(name)
        .await
        .map_err(|e| Error::kube_error("get", "ConfigMap", ns, name, e))?
        .is_some()
    {
        return Ok(());
    }
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.to_string()),
            owner_references: restore.controller_owner_ref(&()).map(|r| vec![r]),
            ..Default::default()
        },
        data: Some(std::collections::BTreeMap::from([(
            "operation.json".to_string(),
            data.to_string(),
        )])),
        ..Default::default()
    };
    api.create(&PostParams::default(), &cm)
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("create", "ConfigMap", ns, name, e))
}

#[cfg(test)]
mod tests {
    use super::{
        BREAK_GLASS_APPROVED_BY_ANNOTATION, BREAK_GLASS_REASON_ANNOTATION, CONDITION_BREAK_GLASS,
        CONDITION_FAILED, CONDITION_READY, CONDITION_TRUE, KanidmRestore, KanidmRestorePhase,
        KanidmRestoreSource, KanidmRestoreSpec, KanidmRestoreStatus, KanidmRestoreTargetRef,
        ReplicaCountEntry, SafetyBackupConfig, SafetyBackupRepositoryRef,
        hardened_security_context, has_accepted_condition, is_remote_source,
        kanidm_job_security_context, mutable_image, requires_safety_backup, safe_basename,
        validate_safety_backup_config, validate_source,
    };
    use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
    use kube::api::ObjectMeta as ApiObjectMeta;
    use std::collections::BTreeMap;

    fn make_restore(
        source: KanidmRestoreSource,
        safety: Option<SafetyBackupConfig>,
    ) -> KanidmRestore {
        KanidmRestore {
            metadata: ApiObjectMeta {
                name: Some("test-restore".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: "corp-idm".to_string(),
                    uid: "test-uid-123".to_string(),
                },
                source,
                restore_image: "kanidm/server@sha256:abc".to_string(),
                safety_backup: safety,
            },
            status: None,
        }
    }

    fn kanidm_with_version(image_tag: &str) -> super::super::crd::Kanidm {
        super::super::crd::Kanidm {
            status: Some(super::super::crd::KanidmStatus {
                available_replicas: 1,
                replicas: 1,
                unavailable_replicas: 0,
                updated_replicas: 1,
                replica_statuses: vec![],
                replica_column: "1/1".to_string(),
                secret_name: None,
                version: Some(super::super::crd::KanidmVersionStatus {
                    image_tag: image_tag.to_string(),
                    upgrade_check_result: super::super::crd::KanidmUpgradeCheckResult::Passed,
                    compatibility_result: super::super::crd::VersionCompatibilityResult::Compatible,
                }),
                domain_appearance_image: None,
                mail_sender: None,
                conditions: None,
            }),
            ..Default::default()
        }
    }

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
        super::update_restore_conditions(&mut status, Some(7));
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
        super::update_restore_conditions(&mut status, Some(8));
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

    #[test]
    fn source_validation_rejects_both_local_and_backup_ref() {
        let source = KanidmRestoreSource {
            local: Some(super::KanidmRestoreLocalSource {
                file_name: "backup.json".to_string(),
            }),
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "some-backup".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        assert!(validate_source(&restore).is_err());
    }

    #[test]
    fn source_validation_rejects_empty_source() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: None,
        };
        let restore = make_restore(source, None);
        assert!(validate_source(&restore).is_err());
    }

    #[test]
    fn source_validation_accepts_local_source() {
        let source = KanidmRestoreSource {
            local: Some(super::KanidmRestoreLocalSource {
                file_name: "backup.json".to_string(),
            }),
            backup_ref: None,
        };
        let restore = make_restore(source, None);
        assert!(validate_source(&restore).is_ok());
    }

    #[test]
    fn source_validation_accepts_backup_ref_source() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "corp-idm-019c7c76".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        assert!(validate_source(&restore).is_ok());
    }

    #[test]
    fn source_validation_rejects_empty_backup_ref_name() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        assert!(validate_source(&restore).is_err());
    }

    #[test]
    fn is_remote_source_true_for_backup_ref() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        assert!(is_remote_source(&restore));
    }

    #[test]
    fn is_remote_source_false_for_local() {
        let source = KanidmRestoreSource {
            local: Some(super::KanidmRestoreLocalSource {
                file_name: "backup.json".to_string(),
            }),
            backup_ref: None,
        };
        let restore = make_restore(source, None);
        assert!(!is_remote_source(&restore));
    }

    #[test]
    fn requires_safety_backup_default_true() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        assert!(requires_safety_backup(&restore));
    }

    #[test]
    fn requires_safety_backup_false_when_skip() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let safety = SafetyBackupConfig {
            repository_ref: Some(SafetyBackupRepositoryRef {
                name: "offsite".to_string(),
            }),
            skip: true,
        };
        let restore = make_restore(source, Some(safety));
        assert!(!requires_safety_backup(&restore));
    }

    #[test]
    fn break_glass_validation_rejects_skip_without_reason() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let safety = SafetyBackupConfig {
            repository_ref: Some(SafetyBackupRepositoryRef {
                name: "offsite".to_string(),
            }),
            skip: true,
        };
        let mut restore = make_restore(source, Some(safety));
        restore.metadata.annotations = Some(BTreeMap::from([(
            BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
            "commander@example.com".to_string(),
        )]));
        assert!(validate_safety_backup_config(&restore).is_err());
    }

    #[test]
    fn break_glass_validation_rejects_skip_without_approver() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let safety = SafetyBackupConfig {
            repository_ref: Some(SafetyBackupRepositoryRef {
                name: "offsite".to_string(),
            }),
            skip: true,
        };
        let mut restore = make_restore(source, Some(safety));
        restore.metadata.annotations = Some(BTreeMap::from([(
            BREAK_GLASS_REASON_ANNOTATION.to_string(),
            "PVC is unreadable".to_string(),
        )]));
        assert!(validate_safety_backup_config(&restore).is_err());
    }

    #[test]
    fn break_glass_validation_accepts_valid_annotations() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let safety = SafetyBackupConfig {
            repository_ref: Some(SafetyBackupRepositoryRef {
                name: "offsite".to_string(),
            }),
            skip: true,
        };
        let mut restore = make_restore(source, Some(safety));
        restore.metadata.annotations = Some(BTreeMap::from([
            (
                BREAK_GLASS_REASON_ANNOTATION.to_string(),
                "PVC is unreadable".to_string(),
            ),
            (
                BREAK_GLASS_APPROVED_BY_ANNOTATION.to_string(),
                "commander@example.com".to_string(),
            ),
        ]));
        assert!(validate_safety_backup_config(&restore).is_ok());
    }

    #[test]
    fn remote_source_requires_repository_ref_when_not_skipping() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let safety = SafetyBackupConfig {
            repository_ref: None,
            skip: false,
        };
        let restore = make_restore(source, Some(safety));
        assert!(validate_safety_backup_config(&restore).is_err());
    }

    #[test]
    fn remote_source_accepts_repository_ref() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let safety = SafetyBackupConfig {
            repository_ref: Some(SafetyBackupRepositoryRef {
                name: "offsite".to_string(),
            }),
            skip: false,
        };
        let restore = make_restore(source, Some(safety));
        assert!(validate_safety_backup_config(&restore).is_ok());
    }

    #[test]
    fn phase_ordering_includes_safety_backup_and_preparing_source() {
        let phases = vec![
            KanidmRestorePhase::Pending,
            KanidmRestorePhase::Validating,
            KanidmRestorePhase::Quiescing,
            KanidmRestorePhase::SafetyBackup,
            KanidmRestorePhase::PreparingSource,
            KanidmRestorePhase::RestoringPrimary,
            KanidmRestorePhase::Verifying,
            KanidmRestorePhase::RebuildingReplicas,
            KanidmRestorePhase::Resuming,
            KanidmRestorePhase::Completed,
        ];
        assert_eq!(phases.len(), 10);
        assert_eq!(phases[3], KanidmRestorePhase::SafetyBackup);
        assert_eq!(phases[4], KanidmRestorePhase::PreparingSource);
    }

    #[test]
    fn status_default_has_empty_phase_timestamps() {
        let status = KanidmRestoreStatus::default();
        assert!(status.phase_timestamps.is_empty());
    }

    #[test]
    fn status_default_has_empty_original_replicas() {
        let status = KanidmRestoreStatus::default();
        assert!(status.original_replicas.is_empty());
    }

    #[test]
    fn hardened_security_context_drops_all_capabilities() {
        let ctx = hardened_security_context();
        assert_eq!(ctx.allow_privilege_escalation, Some(false));
        assert_eq!(ctx.read_only_root_filesystem, Some(true));
        assert_eq!(ctx.run_as_non_root, Some(true));
        let caps = ctx.capabilities.unwrap();
        assert_eq!(caps.drop, Some(vec!["ALL".to_string()]));
        assert_eq!(
            ctx.seccomp_profile.as_ref().map(|s| s.type_.as_str()),
            Some("RuntimeDefault")
        );
    }

    #[test]
    fn kanidm_job_security_context_drops_caps_without_run_as_non_root() {
        let ctx = kanidm_job_security_context();
        assert_eq!(ctx.allow_privilege_escalation, Some(false));
        assert!(ctx.run_as_non_root.is_none());
        assert!(ctx.read_only_root_filesystem.is_none());
        let caps = ctx.capabilities.unwrap();
        assert_eq!(caps.drop, Some(vec!["ALL".to_string()]));
        assert_eq!(
            ctx.seccomp_profile.as_ref().map(|s| s.type_.as_str()),
            Some("RuntimeDefault")
        );
    }

    #[test]
    fn safety_backup_job_name_is_deterministic() {
        let source = KanidmRestoreSource {
            local: Some(super::KanidmRestoreLocalSource {
                file_name: "backup.json".to_string(),
            }),
            backup_ref: None,
        };
        let restore = make_restore(source, None);
        let name1 = super::safety_backup_job_name(&restore);
        let name2 = super::safety_backup_job_name(&restore);
        assert_eq!(name1, name2);
        assert_eq!(name1, "test-restore-safety-backup");
    }

    #[test]
    fn source_prep_job_name_is_deterministic() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let name = super::source_prep_job_name(&restore);
        assert_eq!(name, "test-restore-source-prep");
    }

    #[test]
    fn break_glass_condition_is_recorded_in_status() {
        let mut status = KanidmRestoreStatus::default();
        let condition = super::restore_condition(
            &status.conditions,
            CONDITION_BREAK_GLASS,
            CONDITION_TRUE,
            "BreakGlassOverride",
            "Safety backup skipped",
            Some(1),
        );
        status.conditions.push(condition);
        assert!(status.conditions.iter().any(|c| {
            c.type_ == CONDITION_BREAK_GLASS
                && c.status == CONDITION_TRUE
                && c.reason == "BreakGlassOverride"
        }));
    }

    #[test]
    fn replica_count_entry_serialization() {
        let entry = ReplicaCountEntry {
            group: "primary".to_string(),
            replicas: 3,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"group\":\"primary\""));
        assert!(json.contains("\"replicas\":3"));
    }

    #[test]
    fn local_source_does_not_require_repository_ref() {
        let source = KanidmRestoreSource {
            local: Some(super::KanidmRestoreLocalSource {
                file_name: "backup.json".to_string(),
            }),
            backup_ref: None,
        };
        let safety = SafetyBackupConfig {
            repository_ref: None,
            skip: false,
        };
        let restore = make_restore(source, Some(safety));
        assert!(validate_safety_backup_config(&restore).is_ok());
    }

    #[test]
    fn skip_without_break_glass_annotations_is_rejected() {
        let source = KanidmRestoreSource {
            local: Some(super::KanidmRestoreLocalSource {
                file_name: "backup.json".to_string(),
            }),
            backup_ref: None,
        };
        let safety = SafetyBackupConfig {
            repository_ref: None,
            skip: true,
        };
        let restore = make_restore(source, Some(safety));
        assert!(validate_safety_backup_config(&restore).is_err());
    }

    #[test]
    fn has_accepted_condition_true() {
        let conditions = vec![Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            reason: "Accepted".to_string(),
            message: String::new(),
            last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::now(),
            ),
            observed_generation: None,
        }];
        assert!(has_accepted_condition(&conditions));
    }

    #[test]
    fn has_accepted_condition_false_when_not_accepted_reason() {
        let conditions = vec![Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            reason: "SomeOtherReason".to_string(),
            message: String::new(),
            last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::now(),
            ),
            observed_generation: None,
        }];
        assert!(!has_accepted_condition(&conditions));
    }

    #[test]
    fn has_accepted_condition_false_when_status_false() {
        let conditions = vec![Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            reason: "Accepted".to_string(),
            message: String::new(),
            last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::now(),
            ),
            observed_generation: None,
        }];
        assert!(!has_accepted_condition(&conditions));
    }

    #[test]
    fn has_accepted_condition_empty() {
        assert!(!has_accepted_condition(&[]));
    }

    #[test]
    fn safety_backup_upload_operation_doc_is_valid() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let mut restore = make_restore(source, None);
        restore.metadata.namespace = Some("test-ns".to_string());
        let target = super::super::crd::Kanidm::default();
        let doc_str = super::build_safety_upload_operation_doc(
            &restore,
            &target,
            "test-backup-id",
            "v1/tenants/test-ns/clusters/test-uid/backups/test-backup-id/manifest.json",
            "my-bucket",
            "prod",
            "https://s3.example.com",
            "us-east-1",
            false,
            false,
            None,
            None,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert_eq!(parsed["apiVersion"], "backup.kaniop.rs/v1alpha1");
        assert_eq!(parsed["kind"], "OperationDocument");
        assert_eq!(parsed["operation"], "upload");
        assert_eq!(parsed["bucket"], "my-bucket");
        assert_eq!(parsed["prefix"], "prod");
        assert_eq!(parsed["endpoint"], "https://s3.example.com");
        assert_eq!(parsed["region"], "us-east-1");
        assert_eq!(parsed["backupId"], "test-backup-id");
        assert_eq!(parsed["consistency"], "kanidm-offline");
        assert_eq!(parsed["reason"], "restore-safety");
        assert_eq!(
            parsed["payloadPath"],
            format!("{}/safety-backup.json.gz", super::SHARED_VOL_PATH)
        );
        assert_eq!(parsed["resultPath"], "/run/kaniop-result/result.json");
    }

    #[test]
    fn download_operation_doc_uses_real_repository_data() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let target = super::super::crd::Kanidm::default();
        let doc_str = super::build_download_operation_doc(
            &restore,
            &target,
            "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "real-bucket",
            "real-prefix",
            "https://real-endpoint.com",
            "eu-west-1",
            true,
            false,
            None,
            None,
        );
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert_eq!(parsed["bucket"], "real-bucket");
        assert_eq!(parsed["prefix"], "real-prefix");
        assert_eq!(parsed["endpoint"], "https://real-endpoint.com");
        assert_eq!(parsed["region"], "eu-west-1");
        assert_eq!(parsed["forcePathStyle"], true);
        assert_eq!(
            parsed["manifestKey"],
            "v1/tenants/ns/clusters/k/backups/b/manifest.json"
        );
        assert_eq!(
            parsed["expectedBackupId"],
            "019c7c76-f423-7a12-8f41-2bea7588a303"
        );
    }

    #[test]
    fn safety_backup_ref_is_set_after_verification() {
        let status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::SafetyBackup,
            safety_backup_job_name: Some("test-safety-backup".to_string()),
            safety_backup_ref: Some("safety-test-restore".to_string()),
            safety_backup_expected_backup_id: Some(
                "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
            ),
            safety_backup_manifest_key: Some(
                "v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json".to_string(),
            ),
            safety_backup_payload_sha256: Some("e3b0c44298fc1c149afbf4c8996fb924".to_string()),
            ..Default::default()
        };
        assert!(status.safety_backup_ref.is_some());
        assert!(
            status
                .safety_backup_ref
                .as_ref()
                .unwrap()
                .starts_with("safety-")
        );
        assert!(status.safety_backup_expected_backup_id.is_some());
        assert!(status.safety_backup_manifest_key.is_some());
        assert!(status.safety_backup_payload_sha256.is_some());
    }

    #[test]
    fn database_mutation_not_started_without_safety_backup_ref() {
        let status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::PreparingSource,
            safety_backup_ref: None,
            database_mutation_started: false,
            ..Default::default()
        };
        assert!(!status.database_mutation_started);
        assert!(status.safety_backup_ref.is_none());
    }

    #[test]
    fn pre_boundary_failure_does_not_set_mutation_started() {
        let status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::Failed,
            message: Some(
                "safety backup job failed; target restored to original state".to_string(),
            ),
            database_mutation_started: false,
            ..Default::default()
        };
        assert!(!status.database_mutation_started);
    }

    #[test]
    fn shared_volume_constants_are_distinct() {
        assert_ne!(super::SHARED_VOLUME, super::DATA_VOLUME);
        assert_ne!(super::SHARED_VOL_PATH, super::DATA_PATH);
    }

    #[test]
    fn validate_backup_compatibility_matching_versions_passes() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let target = kanidm_with_version("1.10.4");
        let backup = kaniop_backup_core::crd::KanidmBackup {
            metadata: kube::api::ObjectMeta::default(),
            spec: kaniop_backup_core::crd::KanidmBackupSpec {
                backup_id: "id".to_string(),
                kanidm_ref: kaniop_backup_core::crd::BackupKanidmRef {
                    name: "k".to_string(),
                    uid: "uid".to_string(),
                },
                repository_ref: kaniop_backup_core::crd::BackupRepositoryRef {
                    name: "repo".to_string(),
                },
                manifest_key: "key".to_string(),
            },
            status: Some(kaniop_backup_core::crd::KanidmBackupStatus {
                kanidm_version: Some("1.10.4".to_string()),
                ..Default::default()
            }),
        };
        assert!(super::validate_backup_compatibility(&restore, &target, &backup).is_ok());
    }

    #[test]
    fn validate_backup_compatibility_version_mismatch_fails() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let target = kanidm_with_version("1.11.0");
        let backup = kaniop_backup_core::crd::KanidmBackup {
            metadata: kube::api::ObjectMeta::default(),
            spec: kaniop_backup_core::crd::KanidmBackupSpec {
                backup_id: "id".to_string(),
                kanidm_ref: kaniop_backup_core::crd::BackupKanidmRef {
                    name: "k".to_string(),
                    uid: "uid".to_string(),
                },
                repository_ref: kaniop_backup_core::crd::BackupRepositoryRef {
                    name: "repo".to_string(),
                },
                manifest_key: "key".to_string(),
            },
            status: Some(kaniop_backup_core::crd::KanidmBackupStatus {
                kanidm_version: Some("1.10.4".to_string()),
                ..Default::default()
            }),
        };
        assert!(super::validate_backup_compatibility(&restore, &target, &backup).is_err());
    }

    #[test]
    fn validate_backup_compatibility_no_backup_version_passes() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let target = super::super::crd::Kanidm::default();
        let backup = kaniop_backup_core::crd::KanidmBackup {
            metadata: kube::api::ObjectMeta::default(),
            spec: kaniop_backup_core::crd::KanidmBackupSpec {
                backup_id: "id".to_string(),
                kanidm_ref: kaniop_backup_core::crd::BackupKanidmRef {
                    name: "k".to_string(),
                    uid: "uid".to_string(),
                },
                repository_ref: kaniop_backup_core::crd::BackupRepositoryRef {
                    name: "repo".to_string(),
                },
                manifest_key: "key".to_string(),
            },
            status: None,
        };
        assert!(super::validate_backup_compatibility(&restore, &target, &backup).is_ok());
    }

    #[test]
    fn validate_backup_compatibility_image_digest_mismatch_fails() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let mut restore = make_restore(source, None);
        restore.spec.restore_image = "kanidm/server@sha256:differentdigest".to_string();
        let target = super::super::crd::Kanidm::default();
        let backup = kaniop_backup_core::crd::KanidmBackup {
            metadata: kube::api::ObjectMeta::default(),
            spec: kaniop_backup_core::crd::KanidmBackupSpec {
                backup_id: "id".to_string(),
                kanidm_ref: kaniop_backup_core::crd::BackupKanidmRef {
                    name: "k".to_string(),
                    uid: "uid".to_string(),
                },
                repository_ref: kaniop_backup_core::crd::BackupRepositoryRef {
                    name: "repo".to_string(),
                },
                manifest_key: "key".to_string(),
            },
            status: Some(kaniop_backup_core::crd::KanidmBackupStatus {
                image_digest: Some("sha256:originaldigest".to_string()),
                ..Default::default()
            }),
        };
        assert!(super::validate_backup_compatibility(&restore, &target, &backup).is_err());
    }

    #[test]
    fn validate_backup_compatibility_image_digest_match_passes() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let mut restore = make_restore(source, None);
        restore.spec.restore_image = "kanidm/server@sha256:abc123".to_string();
        let target = super::super::crd::Kanidm::default();
        let backup = kaniop_backup_core::crd::KanidmBackup {
            metadata: kube::api::ObjectMeta::default(),
            spec: kaniop_backup_core::crd::KanidmBackupSpec {
                backup_id: "id".to_string(),
                kanidm_ref: kaniop_backup_core::crd::BackupKanidmRef {
                    name: "k".to_string(),
                    uid: "uid".to_string(),
                },
                repository_ref: kaniop_backup_core::crd::BackupRepositoryRef {
                    name: "repo".to_string(),
                },
                manifest_key: "key".to_string(),
            },
            status: Some(kaniop_backup_core::crd::KanidmBackupStatus {
                image_digest: Some("sha256:abc123".to_string()),
                ..Default::default()
            }),
        };
        assert!(super::validate_backup_compatibility(&restore, &target, &backup).is_ok());
    }

    #[test]
    fn safety_backup_id_is_deterministic() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let id1 = super::compute_safety_backup_id(&restore);
        let id2 = super::compute_safety_backup_id(&restore);
        assert_eq!(id1, id2);
        assert!(uuid::Uuid::parse_str(&id1).is_ok());
    }

    #[test]
    fn safety_backup_id_differs_for_different_restores() {
        let source1 = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore1 = make_restore(source1, None);
        let mut restore2 = make_restore(
            KanidmRestoreSource {
                local: None,
                backup_ref: Some(super::KanidmRestoreBackupRefSource {
                    name: "backup-2".to_string(),
                }),
            },
            None,
        );
        restore2.metadata.name = Some("different-restore".to_string());
        let id1 = super::compute_safety_backup_id(&restore1);
        let id2 = super::compute_safety_backup_id(&restore2);
        assert_ne!(id1, id2);
    }

    #[test]
    fn safety_manifest_key_is_deterministic() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let key1 = super::compute_safety_manifest_key(&restore);
        let key2 = super::compute_safety_manifest_key(&restore);
        assert_eq!(key1, key2);
        assert!(key1.contains("manifest.json"));
        assert!(key1.starts_with("v1/tenants/"));
    }

    #[test]
    fn safety_backup_id_survives_restart() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let mut restore = make_restore(source, None);
        let original_id = super::compute_safety_backup_id(&restore);
        restore.status = Some(KanidmRestoreStatus {
            phase: KanidmRestorePhase::SafetyBackup,
            safety_backup_expected_backup_id: Some(original_id.clone()),
            ..Default::default()
        });
        let recomputed_id = super::compute_safety_backup_id(&restore);
        assert_eq!(original_id, recomputed_id);
    }

    #[test]
    fn mutation_boundary_requires_safety_backup_ref_when_safety_required() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        assert!(super::requires_safety_backup(&restore));
        let status_without_ref = KanidmRestoreStatus {
            phase: KanidmRestorePhase::RestoringPrimary,
            safety_backup_ref: None,
            database_mutation_started: false,
            ..Default::default()
        };
        assert!(status_without_ref.safety_backup_ref.is_none());
        assert!(!status_without_ref.database_mutation_started);
    }

    #[test]
    fn mutation_boundary_allows_progression_when_safety_ref_set() {
        let status_with_ref = KanidmRestoreStatus {
            phase: KanidmRestorePhase::RestoringPrimary,
            safety_backup_ref: Some("safety-test-restore".to_string()),
            safety_backup_manifest_key: Some(
                "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),
            ),
            safety_backup_payload_sha256: Some("abc123".to_string()),
            database_mutation_started: false,
            ..Default::default()
        };
        assert!(status_with_ref.safety_backup_ref.is_some());
        assert!(!status_with_ref.database_mutation_started);
    }

    #[test]
    fn result_document_verification_rejects_missing_backup_id() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "upload",
            "success": true,
            "exitCode": "success",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "payloadSha256": "abc123"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert!(result.backup_id.is_none());
    }

    #[test]
    fn result_document_verification_rejects_missing_manifest_key() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "upload",
            "success": true,
            "exitCode": "success",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "payloadSha256": "abc123"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert!(result.manifest_key.is_none());
    }

    #[test]
    fn result_document_verification_rejects_missing_payload_sha256() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "upload",
            "success": true,
            "exitCode": "success",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert!(result.payload_sha256.is_none());
    }

    #[test]
    fn result_document_verification_rejects_failure_exit_code() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "upload",
            "success": false,
            "exitCode": "integrity",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "payloadSha256": "abc123"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert!(!result.success);
        assert_eq!(
            result.exit_code,
            kaniop_backup_core::result::ExitCode::Integrity
        );
    }

    #[test]
    fn result_document_verification_rejects_wrong_operation() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "download",
            "success": true,
            "exitCode": "success",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "payloadSha256": "abc123"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert_ne!(result.operation, super::SAFETY_BACKUP_RESULT_OPERATION);
    }

    #[test]
    fn result_document_verification_accepts_valid_document() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "upload",
            "success": true,
            "exitCode": "success",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "payloadSha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert!(result.success);
        assert_eq!(
            result.exit_code,
            kaniop_backup_core::result::ExitCode::Success
        );
        assert_eq!(result.operation, "upload");
        assert!(result.backup_id.is_some());
        assert!(result.manifest_key.is_some());
        assert!(result.payload_sha256.is_some());
    }

    #[test]
    fn termination_message_path_constant_is_valid() {
        assert!(super::TERMINATION_MESSAGE_PATH.starts_with('/'));
        assert!(super::TERMINATION_MESSAGE_PATH.contains("kaniop-result"));
    }

    #[test]
    fn status_tracks_safety_backup_evidence() {
        let status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::PreparingSource,
            safety_backup_ref: Some("safety-test-restore".to_string()),
            safety_backup_expected_backup_id: Some(
                "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
            ),
            safety_backup_manifest_key: Some(
                "v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json".to_string(),
            ),
            safety_backup_payload_sha256: Some("e3b0c44298fc1c149afbf4c8996fb924".to_string()),
            ..Default::default()
        };
        assert!(status.safety_backup_ref.is_some());
        assert!(status.safety_backup_expected_backup_id.is_some());
        assert!(status.safety_backup_manifest_key.is_some());
        assert!(status.safety_backup_payload_sha256.is_some());
    }

    #[test]
    fn fail_closed_semantics_preserved_on_result_parse_failure() {
        let invalid_json = "not valid json";
        let result = kaniop_backup_core::result::parse_result_document(invalid_json);
        assert!(result.is_err());
    }

    #[test]
    fn fail_closed_semantics_preserved_on_oversized_result() {
        let oversized_json = format!(
            "{{\"apiVersion\":\"{}\",\"kind\":\"ResultDocument\",\"operation\":\"upload\",\"success\":true,\"exitCode\":\"success\",\"padding\":\"{}\"}}",
            kaniop_backup_core::result::RESULT_DOC_VERSION,
            "x".repeat(kaniop_backup_core::result::MAX_RESULT_DOC_SIZE + 1)
        );
        let result = kaniop_backup_core::result::parse_result_document(&oversized_json);
        assert!(result.is_err());
    }

    #[test]
    fn safety_upload_operation_doc_includes_ca_bundle_path_when_set() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let mut restore = make_restore(source, None);
        restore.metadata.namespace = Some("test-ns".to_string());
        let target = super::super::crd::Kanidm::default();
        let ca_path = kaniop_backup_core::auth::ca_bundle_path();
        let doc_str = super::build_safety_upload_operation_doc(
            &restore,
            &target,
            "test-backup-id",
            "v1/tenants/test-ns/clusters/test-uid/backups/test-backup-id/manifest.json",
            "my-bucket",
            "prod",
            "https://s3.example.com",
            "us-east-1",
            false,
            false,
            Some(&ca_path),
            None,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert_eq!(parsed["caBundlePath"], ca_path);
    }

    #[test]
    fn safety_upload_operation_doc_omits_ca_bundle_path_when_absent() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let mut restore = make_restore(source, None);
        restore.metadata.namespace = Some("test-ns".to_string());
        let target = super::super::crd::Kanidm::default();
        let doc_str = super::build_safety_upload_operation_doc(
            &restore,
            &target,
            "test-backup-id",
            "v1/tenants/test-ns/clusters/test-uid/backups/test-backup-id/manifest.json",
            "my-bucket",
            "prod",
            "https://s3.example.com",
            "us-east-1",
            false,
            false,
            None,
            None,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert!(parsed["caBundlePath"].is_null());
    }

    #[test]
    fn download_operation_doc_includes_ca_bundle_path_when_set() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let target = super::super::crd::Kanidm::default();
        let ca_path = kaniop_backup_core::auth::ca_bundle_path();
        let doc_str = super::build_download_operation_doc(
            &restore,
            &target,
            "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "real-bucket",
            "real-prefix",
            "https://real-endpoint.com",
            "eu-west-1",
            true,
            false,
            Some(&ca_path),
            None,
        );
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert_eq!(parsed["caBundlePath"], ca_path);
    }

    #[test]
    fn download_operation_doc_omits_ca_bundle_path_when_absent() {
        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let target = super::super::crd::Kanidm::default();
        let doc_str = super::build_download_operation_doc(
            &restore,
            &target,
            "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "real-bucket",
            "real-prefix",
            "https://real-endpoint.com",
            "eu-west-1",
            true,
            false,
            None,
            None,
        );
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert!(parsed["caBundlePath"].is_null());
    }

    #[test]
    fn source_prep_result_operation_is_download() {
        assert_eq!(super::SOURCE_PREP_RESULT_OPERATION, "download");
        assert_ne!(
            super::SOURCE_PREP_RESULT_OPERATION,
            super::SAFETY_BACKUP_RESULT_OPERATION
        );
    }

    #[test]
    fn download_result_document_verification_accepts_valid_document() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "download",
            "success": true,
            "exitCode": "success",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "payloadSha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert!(result.success);
        assert_eq!(
            result.exit_code,
            kaniop_backup_core::result::ExitCode::Success
        );
        assert_eq!(result.operation, "download");
        assert_eq!(
            result.backup_id.as_deref(),
            Some("019c7c76-f423-7a12-8f41-2bea7588a303")
        );
        assert!(result.manifest_key.is_some());
        assert!(result.payload_sha256.is_some());
    }

    #[test]
    fn download_result_document_rejects_wrong_operation() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "upload",
            "success": true,
            "exitCode": "success",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "payloadSha256": "abc123"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert_ne!(result.operation, super::SOURCE_PREP_RESULT_OPERATION);
    }

    #[test]
    fn download_result_document_rejects_failure_success_field() {
        let result_json = r#"{
            "apiVersion": "backup.kaniop.rs/v1alpha1",
            "kind": "ResultDocument",
            "operation": "download",
            "success": false,
            "exitCode": "retryable",
            "backupId": "019c7c76-f423-7a12-8f41-2bea7588a303",
            "manifestKey": "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "payloadSha256": "abc123"
        }"#;
        let result = kaniop_backup_core::result::parse_result_document(result_json).unwrap();
        assert!(!result.success);
        assert_ne!(
            result.exit_code,
            kaniop_backup_core::result::ExitCode::Success
        );
    }

    #[test]
    fn pre_mutation_boundary_blocks_transition_on_corrupt_download() {
        let status = KanidmRestoreStatus {
            phase: KanidmRestorePhase::PreparingSource,
            database_mutation_started: false,
            ..Default::default()
        };
        assert!(!status.database_mutation_started);
        assert_eq!(status.phase, KanidmRestorePhase::PreparingSource);
    }

    #[test]
    fn backup_job_volume_size_default_is_10gi() {
        let qty = crate::controller::backup_job_volume_size();
        assert_eq!(qty, Quantity("10Gi".to_string()));
    }

    #[test]
    fn encryption_env_vars_present_when_client_side_key_ref_set() {
        use kaniop_backup_core::auth::build_encryption_env_vars;
        use kaniop_backup_core::crd::SecretRef;

        let key_ref = SecretRef {
            name: "kek-secret".to_string(),
        };
        let env_vars = build_encryption_env_vars(Some(&key_ref));
        assert_eq!(env_vars.len(), 1);
        assert_eq!(env_vars[0].name, "KANIOP_ENCRYPTION_KEY");
        let secret_ref = env_vars[0]
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(secret_ref.name, "kek-secret");
        assert_eq!(secret_ref.key, "encryption-key");
    }

    #[test]
    fn encryption_env_vars_absent_when_no_key_ref() {
        use kaniop_backup_core::auth::build_encryption_env_vars;

        let env_vars = build_encryption_env_vars(None);
        assert!(
            env_vars.is_empty(),
            "encryption env vars must be empty when key_ref is None"
        );
    }

    #[test]
    fn safety_upload_operation_doc_includes_encryption_fields_when_client_side() {
        use kaniop_backup_core::crd::{EncryptionMode, RepositoryEncryption, SecretRef};

        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let mut restore = make_restore(source, None);
        restore.metadata.namespace = Some("test-ns".to_string());
        let target = super::super::crd::Kanidm::default();
        let encryption = RepositoryEncryption {
            mode: EncryptionMode::ClientSide,
            key_id: None,
            key_ref: Some(SecretRef {
                name: "kek-secret".to_string(),
            }),
        };
        let doc_str = super::build_safety_upload_operation_doc(
            &restore,
            &target,
            "test-backup-id",
            "v1/tenants/test-ns/clusters/test-uid/backups/test-backup-id/manifest.json",
            "my-bucket",
            "prod",
            "https://s3.example.com",
            "us-east-1",
            false,
            false,
            None,
            Some(&encryption),
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert_eq!(parsed["encryptionMode"], "clientSide");
        assert!(parsed["encryptionKeyId"].is_null());
    }

    #[test]
    fn download_operation_doc_includes_encryption_fields_when_provider_kms() {
        use kaniop_backup_core::crd::{EncryptionMode, RepositoryEncryption};

        let source = KanidmRestoreSource {
            local: None,
            backup_ref: Some(super::KanidmRestoreBackupRefSource {
                name: "backup-1".to_string(),
            }),
        };
        let restore = make_restore(source, None);
        let target = super::super::crd::Kanidm::default();
        let encryption = RepositoryEncryption {
            mode: EncryptionMode::ProviderKms,
            key_id: Some("alias/kaniop-backups".to_string()),
            key_ref: None,
        };
        let doc_str = super::build_download_operation_doc(
            &restore,
            &target,
            "v1/tenants/ns/clusters/k/backups/b/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "real-bucket",
            "real-prefix",
            "https://real-endpoint.com",
            "eu-west-1",
            true,
            false,
            None,
            Some(&encryption),
        );
        let parsed: serde_json::Value = serde_json::from_str(&doc_str).unwrap();
        assert_eq!(parsed["encryptionMode"], "providerKms");
        assert_eq!(parsed["encryptionKeyId"], "alias/kaniop-backups");
    }
}
