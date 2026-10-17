use crate::controller::discovery::read_discover_result;
use crate::controller::{
    BACKUP_JOB_TTL_SECONDS, DISCOVERED_LABEL, RESULT_PATH, background_delete_params,
    build_data_mover_wrapper, data_mover_image, default_resource_requirements,
    extract_termination_message, hardened_pod_security_context, hardened_security_context,
    select_failed_pod, select_succeeded_pod,
};
use crate::crd::{
    BackupKanidmRef, BackupRepositoryRef, KanidmBackup, KanidmBackupPhase, KanidmBackupRepository,
    KanidmBackupStatus,
};

use kaniop_backup_core::auth::{
    AuthRole, build_auth_env_vars, build_auth_volume_mounts, build_auth_volumes,
    build_ca_bundle_volume, build_ca_bundle_volume_mount, ca_bundle_env_var, ca_bundle_path,
};
use kaniop_backup_core::paths::RepositoryPath;
use kaniop_backup_core::result::{ExitCode, MANIFEST_NOT_FOUND, ResultDocument};
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{ControllerId, State, check_api_queryable, error_policy};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::RESTORE_ANNOTATION;

use std::sync::{Arc, OnceLock};

use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kaniop_k8s_util::error::{Error, Result};
use kube::api::{DeleteParams, ListParams, ObjectMeta, Patch, PatchParams};
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::events::{Event, EventType};
use kube::runtime::watcher::Config;
use kube::{Api, Resource, ResourceExt};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use tokio::time::Duration;
use tracing::{debug, info, warn};

pub const CONTROLLER_ID: ControllerId = "backup";
const VALIDATION_JOB_PREFIX: &str = "kaniop-backup-validate";
const DELETION_JOB_PREFIX: &str = "kaniop-backup-delete";
const BACKUP_FINALIZER: &str = "kanidmbackups.kaniop.rs/finalizer";
const REQUEUE_NORMAL: Duration = Duration::from_secs(300);
const REQUEUE_JOB_PENDING: Duration = Duration::from_secs(10);
const REQUEUE_DELETION: Duration = Duration::from_secs(30);
const REQUEUE_DELETION_DEFERRED_INITIAL: Duration = Duration::from_secs(60);
const REQUEUE_DELETION_DEFERRED_MAX: Duration = Duration::from_secs(600);
/// Upper bound on waiting for a discover Job to observe a deletion. Discovery consumes a
/// completed discover Job on the next scan tick, and `ttlSecondsAfterFinished`
/// (`BACKUP_JOB_TTL_SECONDS`) removes stragglers, so this grace only protects deletion
/// liveness when a discover Job is stuck.
const DISCOVERY_CONFIRMATION_GRACE: Duration = Duration::from_secs(1800);

#[derive(Clone)]
pub struct BackupMetrics {
    validation_failures: Counter<u64>,
    deletions_deferred: Counter<u64>,
    gc_deferred: Counter<u64>,
}

impl BackupMetrics {
    pub fn new(meter: &Meter) -> Self {
        let validation_failures = meter
            .u64_counter("backup_validation_failures")
            .with_description("Total number of backup manifest validation failures")
            .build();

        let deletions_deferred = meter
            .u64_counter("backup_deletions_deferred")
            .with_description(
                "Total number of backup deletions deferred due to active restore references",
            )
            .build();

        let gc_deferred = meter
            .u64_counter("backup_gc_deferred")
            .with_description("Total number of backup garbage-collection deferrals by reason")
            .build();

        Self {
            validation_failures,
            deletions_deferred,
            gc_deferred,
        }
    }

    pub fn inc_validation_failure(&self, namespace: &str) {
        self.validation_failures
            .add(1, &[KeyValue::new("namespace", namespace.to_string())]);
    }

    pub fn inc_deletion_deferred(&self, namespace: &str) {
        self.deletions_deferred
            .add(1, &[KeyValue::new("namespace", namespace.to_string())]);
        self.inc_gc_deferred(namespace, "active_restore");
    }

    pub fn inc_gc_deferred(&self, namespace: &str, reason: &str) {
        self.gc_deferred.add(
            1,
            &[
                KeyValue::new("namespace", namespace.to_string()),
                KeyValue::new("reason", reason.to_string()),
            ],
        );
    }
}

fn backup_metrics() -> &'static BackupMetrics {
    static INSTANCE: OnceLock<BackupMetrics> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let meter = opentelemetry::global::meter("kaniop");
        BackupMetrics::new(&meter)
    })
}

pub async fn run(state: State, client: Client) {
    let backup = check_api_queryable::<KanidmBackup>(client.clone()).await;

    let ctx = Arc::new(state.to_context(client, CONTROLLER_ID));

    info!("starting {CONTROLLER_ID} controller");
    let backup_controller = Controller::new(backup, Config::default().any_semantic())
        .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
        .shutdown_on_signal()
        .run(
            backoff_reconciler!(reconcile_backup),
            error_policy,
            ctx.clone(),
        )
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    ctx.metrics.ready_set(1);
    tokio::join!(backup_controller);
}

fn validation_job_name(backup_name: &str) -> String {
    let suffix = &backup_name[..backup_name.len().min(40)];
    format!("{VALIDATION_JOB_PREFIX}-{suffix}")
}

fn deletion_job_name(backup_name: &str) -> String {
    let suffix = &backup_name[..backup_name.len().min(40)];
    format!("{DELETION_JOB_PREFIX}-{suffix}")
}

pub fn build_validation_job(
    backup: &KanidmBackup,
    repository: &KanidmBackupRepository,
    namespace: &str,
) -> Job {
    let spec = &repository.spec;
    let endpoint = &spec.s3.endpoint;
    let region = &spec.s3.region;

    let ca_bundle_path = spec.s3.ca_bundle_ref.as_ref().map(|_| ca_bundle_path());

    let operation_json = serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "download",
        "manifestKey": backup.spec.manifest_key,
        "bucket": spec.s3.bucket,
        "prefix": spec.s3.prefix,
        "endpoint": endpoint,
        "region": region,
        "forcePathStyle": spec.s3.force_path_style,
        "insecure": spec.s3.insecure,
        "caBundlePath": ca_bundle_path,
        "expectedBackupId": backup.spec.backup_id,
        "expectedKanidmUid": backup.spec.kanidm_ref.uid,
        "expectedDomain": "*",
        "outputPath": "/kaniop-staging/payload",
        "manifestOnly": true,
        "resultPath": RESULT_PATH,
    });

    let auth_method = &spec.authentication.reader;
    let mut env_vars = build_auth_env_vars(auth_method, &repository.name_any(), AuthRole::Reader);
    env_vars.push(EnvVar {
        name: "RUST_LOG".to_string(),
        value: Some("info".to_string()),
        ..Default::default()
    });

    let mut volumes = vec![
        Volume {
            name: "result".to_string(),
            empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "staging".to_string(),
            empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
                ..Default::default()
            }),
            ..Default::default()
        },
    ];
    let mut volume_mounts = vec![
        VolumeMount {
            name: "result".to_string(),
            mount_path: "/kaniop-result".to_string(),
            ..Default::default()
        },
        VolumeMount {
            name: "staging".to_string(),
            mount_path: "/kaniop-staging".to_string(),
            ..Default::default()
        },
    ];

    volumes.extend(build_auth_volumes(auth_method));
    volume_mounts.extend(build_auth_volume_mounts(auth_method));

    if let Some(ca_bundle_ref) = &spec.s3.ca_bundle_ref {
        volumes.push(build_ca_bundle_volume(ca_bundle_ref));
        volume_mounts.push(build_ca_bundle_volume_mount());
        env_vars.push(ca_bundle_env_var());
    }

    let job_name = validation_job_name(&backup.name_any());

    Job {
        metadata: ObjectMeta {
            name: Some(job_name),
            namespace: Some(namespace.to_string()),
            labels: Some(
                [
                    (
                        "app.kubernetes.io/managed-by".to_string(),
                        "kaniop".to_string(),
                    ),
                    ("kaniop.rs/backup".to_string(), backup.name_any()),
                    ("kaniop.rs/operation".to_string(), "validation".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            ttl_seconds_after_finished: Some(BACKUP_JOB_TTL_SECONDS),
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    automount_service_account_token: Some(false),
                    restart_policy: Some("Never".to_string()),
                    security_context: Some(hardened_pod_security_context()),
                    containers: vec![Container {
                        name: "validate".to_string(),
                        image: Some(data_mover_image()),
                        command: Some(vec!["/bin/sh".to_string()]),
                        args: Some(vec![
                            "-c".to_string(),
                            build_data_mover_wrapper("download"),
                            "--".to_string(),
                            serde_json::to_string(&operation_json).unwrap_or_default(),
                        ]),
                        env: Some(env_vars),
                        security_context: Some(hardened_security_context()),
                        termination_message_policy: Some("FallbackToLogsOnError".to_string()),
                        volume_mounts: Some(volume_mounts),
                        resources: Some(default_resource_requirements()),
                        ..Default::default()
                    }],
                    volumes: Some(volumes),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub fn build_deletion_job(
    repository: &KanidmBackupRepository,
    namespace: &str,
    backup_name: &str,
    backup_prefix: &str,
) -> Job {
    let spec = &repository.spec;
    let endpoint = &spec.s3.endpoint;
    let region = &spec.s3.region;

    let ca_bundle_path = spec.s3.ca_bundle_ref.as_ref().map(|_| ca_bundle_path());

    let operation_json = serde_json::json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "delete-plan",
        "backupPrefix": backup_prefix,
        "bucket": spec.s3.bucket,
        "prefix": spec.s3.prefix,
        "endpoint": endpoint,
        "region": region,
        "forcePathStyle": spec.s3.force_path_style,
        "insecure": spec.s3.insecure,
        "caBundlePath": ca_bundle_path,
        "resultPath": RESULT_PATH,
    });

    let auth_method = &spec.authentication.deleter;
    let mut env_vars = build_auth_env_vars(auth_method, &repository.name_any(), AuthRole::Deleter);
    env_vars.push(EnvVar {
        name: "RUST_LOG".to_string(),
        value: Some("info".to_string()),
        ..Default::default()
    });

    let mut volumes = vec![Volume {
        name: "result".to_string(),
        empty_dir: Some(k8s_openapi::api::core::v1::EmptyDirVolumeSource {
            ..Default::default()
        }),
        ..Default::default()
    }];
    let mut volume_mounts = vec![VolumeMount {
        name: "result".to_string(),
        mount_path: "/kaniop-result".to_string(),
        ..Default::default()
    }];

    volumes.extend(build_auth_volumes(auth_method));
    volume_mounts.extend(build_auth_volume_mounts(auth_method));

    if let Some(ca_bundle_ref) = &spec.s3.ca_bundle_ref {
        volumes.push(build_ca_bundle_volume(ca_bundle_ref));
        volume_mounts.push(build_ca_bundle_volume_mount());
        env_vars.push(ca_bundle_env_var());
    }

    Job {
        metadata: ObjectMeta {
            name: Some(deletion_job_name(backup_name)),
            namespace: Some(namespace.to_string()),
            labels: Some(
                [
                    (
                        "app.kubernetes.io/managed-by".to_string(),
                        "kaniop".to_string(),
                    ),
                    ("kaniop.rs/repository".to_string(), repository.name_any()),
                    ("kaniop.rs/backup".to_string(), backup_name.to_string()),
                    ("kaniop.rs/operation".to_string(), "deletion".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            ttl_seconds_after_finished: Some(BACKUP_JOB_TTL_SECONDS),
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    automount_service_account_token: Some(false),
                    restart_policy: Some("Never".to_string()),
                    security_context: Some(hardened_pod_security_context()),
                    containers: vec![Container {
                        name: "delete".to_string(),
                        image: Some(data_mover_image()),
                        command: Some(vec!["/bin/sh".to_string()]),
                        args: Some(vec![
                            "-c".to_string(),
                            build_data_mover_wrapper("delete-plan"),
                            "--".to_string(),
                            serde_json::to_string(&operation_json).unwrap_or_default(),
                        ]),
                        env: Some(env_vars),
                        security_context: Some(hardened_security_context()),
                        termination_message_policy: Some("FallbackToLogsOnError".to_string()),
                        volume_mounts: Some(volume_mounts),
                        resources: Some(default_resource_requirements()),
                        ..Default::default()
                    }],
                    volumes: Some(volumes),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn find_job(
    client: &Client,
    namespace: &str,
    backup_name: &str,
    operation: &str,
) -> Result<Option<Job>> {
    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!(
        "kaniop.rs/backup={backup_name},kaniop.rs/operation={operation}"
    ));
    let jobs = job_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list {operation} jobs for backup {namespace}/{backup_name}"),
            Box::new(e),
        )
    })?;
    Ok(jobs.items.into_iter().next())
}

async fn read_job_result(
    client: &Client,
    namespace: &str,
    job: &Job,
    container: &str,
) -> Result<Option<ResultDocument>> {
    let job_name = job.name_any();
    let pod_api: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!("job-name={job_name}"));
    let pods = pod_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list pods for job {namespace}/{job_name}"),
            Box::new(e),
        )
    })?;

    let pod = match select_succeeded_pod(&pods.items) {
        Some(p) => p,
        None => return Ok(None),
    };

    let raw = match extract_termination_message(pod, container) {
        Some(msg) => msg,
        None => return Ok(None),
    };

    let doc = kaniop_backup_core::result::parse_result_document(&raw).map_err(|e| {
        Error::MissingData(format!(
            "result document for {namespace}/{job_name} is invalid: {e}"
        ))
    })?;
    Ok(Some(doc))
}

async fn read_failed_job_result(
    client: &Client,
    namespace: &str,
    job: &Job,
    container: &str,
) -> Result<Option<ResultDocument>> {
    let job_name = job.name_any();
    let pod_api: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!("job-name={job_name}"));
    let pods = pod_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list pods for job {namespace}/{job_name}"),
            Box::new(e),
        )
    })?;

    let pod = match select_failed_pod(&pods.items) {
        Some(p) => p,
        None => return Ok(None),
    };

    let raw = match extract_termination_message(pod, container) {
        Some(msg) => msg,
        None => return Ok(None),
    };

    let doc = kaniop_backup_core::result::parse_result_document(&raw).map_err(|e| {
        Error::MissingData(format!(
            "result document for failed pod {namespace}/{job_name} is invalid: {e}"
        ))
    })?;
    Ok(Some(doc))
}

pub fn manifest_to_backup_cr(
    manifest_key: &str,
    backup_id: &str,
    repository_name: &str,
    kanidm_name: &str,
    kanidm_uid: &str,
) -> KanidmBackup {
    let backup_name = format!("kb-{}", &backup_id[..backup_id.len().min(8)]);
    KanidmBackup {
        metadata: ObjectMeta {
            name: Some(backup_name),
            labels: Some(
                [
                    ("kaniop.rs/backup-id".to_string(), backup_id.to_string()),
                    (
                        "kaniop.rs/repository".to_string(),
                        repository_name.to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        spec: crate::crd::KanidmBackupSpec {
            backup_id: backup_id.to_string(),
            kanidm_ref: BackupKanidmRef {
                name: kanidm_name.to_string(),
                uid: kanidm_uid.to_string(),
            },
            repository_ref: BackupRepositoryRef {
                name: repository_name.to_string(),
            },
            manifest_key: manifest_key.to_string(),
        },
        status: None,
    }
}

fn job_is_complete(job: &Job) -> bool {
    job.status
        .as_ref()
        .is_some_and(|s| s.succeeded.is_some_and(|v| v > 0))
}

fn job_has_failed(job: &Job) -> bool {
    job.status
        .as_ref()
        .is_some_and(|s| s.failed.is_some_and(|v| v > 0))
}

fn is_kube_not_found(err: &kube::Error) -> bool {
    matches!(err, kube::Error::Api(ae) if ae.code == 404)
}

fn job_start_time(job: &Job) -> Option<&Time> {
    job.status
        .as_ref()
        .and_then(|s| s.start_time.as_ref())
        .or(job.metadata.creation_timestamp.as_ref())
}

fn discover_job_started_before(job: &Job, deleted_at: &Time) -> bool {
    job_start_time(job).is_some_and(|start| start.0 < deleted_at.0)
}

/// Whether a discover Job can still resurrect `manifest_key`. A Job that started after the
/// deletion finished sees the objects gone, and a completed Job whose snapshot demonstrably
/// omits the key has already been consumed without it. An unfinished or unreadable Job is
/// treated as referencing the key.
fn discover_job_may_reference(
    job: &Job,
    result: Option<&ResultDocument>,
    deleted_at: &Time,
    manifest_key: &str,
) -> bool {
    if !discover_job_started_before(job, deleted_at) {
        return false;
    }
    if !job_is_complete(job) {
        return true;
    }
    match result.and_then(|doc| doc.discovery.as_ref()) {
        Some(discovery) => discovery
            .manifest_keys
            .iter()
            .any(|key| key == manifest_key),
        None => true,
    }
}

/// The first discover Job that can still resurrect the deleted backup.
fn conflicting_discover_job<'a>(
    snapshots: &[(&'a Job, Option<ResultDocument>)],
    deleted_at: &Time,
    manifest_key: &str,
) -> Option<&'a Job> {
    snapshots
        .iter()
        .find(|(job, result)| {
            discover_job_may_reference(job, result.as_ref(), deleted_at, manifest_key)
        })
        .map(|(job, _)| *job)
}

/// Whether the finalizer must keep waiting for discovery to observe the deletion. The caller
/// resolves the conflicting Job once with [`conflicting_discover_job`] and passes it here, so the
/// decision and the Job named in the log message can never disagree, and the snapshots are only
/// scanned once per reconcile.
fn discovery_confirmation_pending(
    conflicting: Option<&Job>,
    deleted_at: Option<&Time>,
    now: Timestamp,
    grace: Duration,
) -> bool {
    let (Some(_), Some(deleted_at)) = (conflicting, deleted_at) else {
        return false;
    };
    let elapsed = now
        .since(deleted_at.0)
        .map(|d| d.get_seconds())
        .unwrap_or_default();
    elapsed <= grace.as_secs() as i64
}

#[derive(Debug, PartialEq, Eq)]
enum ValidationFailure {
    ManifestAbsent { message: String },
    Other { message: String },
}

fn classify_validation_failure(result: Option<&ResultDocument>) -> ValidationFailure {
    let error = result.and_then(|r| r.error.as_ref());
    let message = match error {
        Some(e) => format!("{}: {}", e.code, e.message),
        None => "Manifest validation Job failed; manifest is unreachable or invalid".to_string(),
    };
    if error.is_some_and(|e| e.code == MANIFEST_NOT_FOUND) {
        ValidationFailure::ManifestAbsent { message }
    } else {
        ValidationFailure::Other { message }
    }
}

pub(crate) fn is_discovery_owned(obj: &KanidmBackup) -> bool {
    obj.metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get(DISCOVERED_LABEL))
        .is_some_and(|value| value == "true")
}

async fn list_discover_jobs(
    client: &Client,
    namespace: &str,
    repository_name: &str,
) -> Result<Vec<Job>> {
    let job_api: Api<Job> = Api::namespaced(client.clone(), namespace);
    let lp = ListParams::default().labels(&format!(
        "kaniop.rs/repository={repository_name},kaniop.rs/operation=discover"
    ));
    let jobs = job_api.list(&lp).await.map_err(|e| {
        Error::KubeError(
            format!("failed to list discover jobs for repository {namespace}/{repository_name}"),
            Box::new(e),
        )
    })?;
    Ok(jobs.items)
}

fn compute_deferred_backoff(existing_condition: Option<&Condition>) -> Duration {
    let Some(cond) = existing_condition else {
        return REQUEUE_DELETION_DEFERRED_INITIAL;
    };
    let now_secs = Timestamp::now().as_second();
    let then_secs = cond.last_transition_time.0.as_second();
    let elapsed = (now_secs - then_secs).max(0) as u64;
    let mut delay = REQUEUE_DELETION_DEFERRED_INITIAL.as_secs();
    while delay <= elapsed && delay < REQUEUE_DELETION_DEFERRED_MAX.as_secs() {
        delay = (delay * 2).min(REQUEUE_DELETION_DEFERRED_MAX.as_secs());
    }
    Duration::from_secs(delay.min(REQUEUE_DELETION_DEFERRED_MAX.as_secs()))
}

fn build_deferred_condition(
    existing: Option<&Condition>,
    reason: &str,
    message: String,
    generation: Option<i64>,
) -> Condition {
    let transition_time = existing
        .filter(|c| c.reason == reason)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Time(Timestamp::now()));

    Condition {
        type_: "DeletionDeferred".to_string(),
        status: "True".to_string(),
        observed_generation: generation,
        last_transition_time: transition_time,
        reason: reason.to_string(),
        message,
    }
}

fn needs_metadata_backfill(status: &KanidmBackupStatus) -> bool {
    status.phase == KanidmBackupPhase::Ready
        && (status.created_at.is_none()
            || status.consistency.is_none()
            || status.reason.is_none()
            || status.kanidm_version.is_none())
}

async fn get_repository(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    namespace: &str,
    repository_name: &str,
) -> Result<Option<KanidmBackupRepository>> {
    let api: Api<KanidmBackupRepository> = Api::namespaced(ctx.client.clone(), namespace);
    match api.get(repository_name).await {
        Ok(repo) => Ok(Some(repo)),
        Err(e) if is_kube_not_found(&e) => Ok(None),
        Err(e) => Err(Error::KubeError(
            format!("failed to get repository {namespace}/{repository_name}"),
            Box::new(e),
        )),
    }
}

fn build_add_finalizer_patch(existing: Option<&Vec<String>>) -> serde_json::Value {
    let mut finalizers = existing.cloned().unwrap_or_default();
    if !finalizers.contains(&BACKUP_FINALIZER.to_string()) {
        finalizers.push(BACKUP_FINALIZER.to_string());
    }
    serde_json::json!({
        "metadata": {
            "finalizers": finalizers
        }
    })
}

async fn reconcile_backup(
    obj: Arc<KanidmBackup>,
    ctx: Arc<kaniop_operator::controller::context::Context<KanidmBackup>>,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    debug!(%namespace, %name, "reconciling KanidmBackup");

    let api: Api<KanidmBackup> = Api::namespaced(ctx.client.clone(), &namespace);
    let has_finalizer = obj
        .metadata
        .finalizers
        .as_ref()
        .is_some_and(|f| f.iter().any(|s| s == BACKUP_FINALIZER));
    let is_deleting = obj.metadata.deletion_timestamp.is_some();

    if is_deleting {
        if !has_finalizer {
            return Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                true,
            ));
        }
        return reconcile_cleanup(obj, ctx).await;
    }

    if !has_finalizer {
        let patch = build_add_finalizer_patch(obj.metadata.finalizers.as_ref());
        api.patch(&name, &PatchParams::default(), &Patch::Merge(patch))
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to add finalizer for {namespace}/{name}"),
                    Box::new(e),
                )
            })?;
        info!(%namespace, %name, "added finalizer");
        return Ok((
            kube::runtime::controller::Action::requeue(Duration::from_secs(1)),
            false,
        ));
    }

    reconcile_apply(obj, ctx).await
}

async fn reconcile_apply(
    obj: Arc<KanidmBackup>,
    ctx: Arc<kaniop_operator::controller::context::Context<KanidmBackup>>,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    debug!(%namespace, %name, "reconciling KanidmBackup");

    let spec = &obj.spec;

    if spec.backup_id.is_empty() {
        return Err(Error::MissingData("backupId is required".to_string()));
    }

    if spec.manifest_key.is_empty() {
        return Err(Error::MissingData("manifestKey is required".to_string()));
    }

    if spec.manifest_key.contains("..") {
        return Err(Error::MissingData(
            "manifestKey contains path traversal".to_string(),
        ));
    }

    if !spec.manifest_key.ends_with("/manifest.json") {
        return Err(Error::MissingData(
            "manifestKey must reference a manifest.json object".to_string(),
        ));
    }

    let mut status = obj.status.clone().unwrap_or_default();
    status.observed_generation = obj.metadata.generation;

    match status.phase {
        KanidmBackupPhase::Discovering => {
            handle_discovering(&obj, &ctx, &namespace, &name, &mut status).await
        }
        KanidmBackupPhase::Ready => {
            if needs_metadata_backfill(&status) {
                info!(
                    %namespace,
                    %name,
                    "Ready backup missing manifest metadata; triggering revalidation"
                );
                status.phase = KanidmBackupPhase::Discovering;
                status.consistency = None;
                status.reason = None;
                status.kanidm_version = None;
                status.image_digest = None;
                status.created_at = None;
                status.conditions.retain(|c| c.type_ != "Ready");
                patch_backup_status(&ctx, &namespace, &name, &status).await?;
                return Ok((
                    kube::runtime::controller::Action::requeue(Duration::from_secs(1)),
                    false,
                ));
            }
            patch_backup_status(&ctx, &namespace, &name, &status).await?;
            Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                true,
            ))
        }
        KanidmBackupPhase::Deleting => {
            handle_deletion(&obj, &ctx, &namespace, &name, &mut status).await
        }
        KanidmBackupPhase::Deleted => {
            patch_backup_status(&ctx, &namespace, &name, &status).await?;
            Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                true,
            ))
        }
        KanidmBackupPhase::Invalid => {
            patch_backup_status(&ctx, &namespace, &name, &status).await?;
            Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                false,
            ))
        }
    }
}

async fn reconcile_cleanup(
    obj: Arc<KanidmBackup>,
    ctx: Arc<kaniop_operator::controller::context::Context<KanidmBackup>>,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let name = obj.name_any();
    let namespace = obj.namespace().unwrap_or_default();
    debug!(%namespace, %name, "cleaning up KanidmBackup");

    let mut status = obj.status.clone().unwrap_or_default();
    status.observed_generation = obj.metadata.generation;

    if status.phase != KanidmBackupPhase::Deleting && status.phase != KanidmBackupPhase::Deleted {
        status.phase = KanidmBackupPhase::Deleting;
        patch_backup_status(&ctx, &namespace, &name, &status).await?;
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_DELETION),
            false,
        ));
    }

    if status.phase == KanidmBackupPhase::Deleted {
        let deleted_at = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready" && c.reason == "Deleted")
            .map(|c| c.last_transition_time.clone());
        if let Some(deleted_at) = deleted_at.as_ref() {
            let discover_jobs =
                list_discover_jobs(&ctx.client, &namespace, &obj.spec.repository_ref.name).await?;
            let mut snapshots = Vec::new();
            for job in &discover_jobs {
                if !discover_job_started_before(job, deleted_at) {
                    continue;
                }
                let result = if job_is_complete(job) {
                    match read_discover_result(&ctx.client, &namespace, job).await {
                        Ok(result) => result,
                        Err(e) => {
                            debug!(
                                %namespace,
                                %name,
                                job = %job.name_any(),
                                error = %e,
                                "discover Job result is not readable yet; treating it as conflicting"
                            );
                            None
                        }
                    }
                } else {
                    None
                };
                snapshots.push((job, result));
            }
            let manifest_key = obj.spec.manifest_key.as_str();
            let conflicting = conflicting_discover_job(&snapshots, deleted_at, manifest_key);
            let pending = discovery_confirmation_pending(
                conflicting,
                Some(deleted_at),
                Timestamp::now(),
                DISCOVERY_CONFIRMATION_GRACE,
            );
            if let Some(job) = conflicting {
                if pending {
                    info!(
                        %namespace,
                        %name,
                        job = %job.name_any(),
                        "stale discover Job may still report this backup; waiting for discovery to observe the deletion"
                    );
                    patch_backup_status(&ctx, &namespace, &name, &status).await?;
                    return Ok((
                        kube::runtime::controller::Action::requeue(REQUEUE_DELETION),
                        false,
                    ));
                }
                warn!(
                    %namespace,
                    %name,
                    job = %job.name_any(),
                    grace_secs = DISCOVERY_CONFIRMATION_GRACE.as_secs(),
                    "discovery confirmation grace exceeded; removing finalizer"
                );
            }
        }

        let api: Api<KanidmBackup> = Api::namespaced(ctx.client.clone(), &namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": null
            }
        });
        api.patch(&name, &PatchParams::default(), &Patch::Merge(patch))
            .await
            .map_err(|e| {
                Error::KubeError(
                    format!("failed to remove finalizer for {namespace}/{name}"),
                    Box::new(e),
                )
            })?;
        info!(%namespace, %name, "removed finalizer");
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
            true,
        ));
    }

    handle_deletion(&obj, &ctx, &namespace, &name, &mut status).await
}

async fn handle_discovering(
    obj: &KanidmBackup,
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    namespace: &str,
    name: &str,
    status: &mut KanidmBackupStatus,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let repository = match get_repository(ctx, namespace, &obj.spec.repository_ref.name).await? {
        Some(repo) => repo,
        None => {
            warn!(
                namespace,
                name,
                repository = %obj.spec.repository_ref.name,
                "repository not found; backup can never be validated, marking invalid"
            );
            status.phase = KanidmBackupPhase::Invalid;
            let condition = Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                observed_generation: obj.metadata.generation,
                last_transition_time: Time(Timestamp::now()),
                reason: "RepositoryGone".to_string(),
                message: format!(
                    "Repository {} no longer exists; backup cannot be validated",
                    obj.spec.repository_ref.name
                ),
            };
            status.conditions.retain(|c| c.type_ != "Ready");
            status.conditions.push(condition);
            patch_backup_status(ctx, namespace, name, status).await?;
            return Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                false,
            ));
        }
    };

    let existing_job = find_job(&ctx.client, namespace, name, "validation").await?;

    if let Some(job) = existing_job {
        if job_has_failed(&job) {
            let failed_result = read_failed_job_result(&ctx.client, namespace, &job, "validate")
                .await
                .ok()
                .flatten();

            let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
            job_api
                .delete(&job.name_any(), &background_delete_params())
                .await
                .ok();

            return match classify_validation_failure(failed_result.as_ref()) {
                ValidationFailure::ManifestAbsent { message } => {
                    handle_manifest_absent(obj, ctx, namespace, name, status, &message).await
                }
                ValidationFailure::Other { message } => {
                    backup_metrics().inc_validation_failure(namespace);
                    mark_backup_invalid(
                        ctx,
                        namespace,
                        name,
                        status,
                        obj,
                        "ValidationFailed",
                        format!("Manifest validation failed: {message}"),
                    )
                    .await
                }
            };
        }

        if job_is_complete(&job) {
            match read_job_result(&ctx.client, namespace, &job, "validate").await? {
                Some(result)
                    if result.success
                        && result.exit_code == ExitCode::Success
                        && result.backup_id.as_deref() == Some(&obj.spec.backup_id)
                        && result.manifest_key.is_some() =>
                {
                    status.phase = KanidmBackupPhase::Ready;
                    status.consistency = result.consistency;
                    status.reason = result.reason;
                    status.kanidm_version = result.kanidm_version;
                    status.image_digest = result.image_digest;
                    status.size_bytes = result.payload_size_bytes;
                    status.payload_sha256 = result.payload_sha256;
                    status.created_at = result.created_at;

                    let ready_condition = Condition {
                        type_: "Ready".to_string(),
                        status: "True".to_string(),
                        observed_generation: obj.metadata.generation,
                        last_transition_time: Time(Timestamp::now()),
                        reason: "ManifestValidated".to_string(),
                        message: format!(
                            "Backup validated via manifest {} with payload checksum evidence",
                            result.manifest_key.as_deref().unwrap_or("unknown")
                        ),
                    };
                    status.conditions.retain(|c| c.type_ != "Ready");
                    status.conditions.push(ready_condition);

                    let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
                    job_api
                        .delete(&job.name_any(), &background_delete_params())
                        .await
                        .ok();

                    patch_backup_status(ctx, namespace, name, status).await?;
                    return Ok((
                        kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                        true,
                    ));
                }
                Some(result) => {
                    let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
                    job_api
                        .delete(&job.name_any(), &background_delete_params())
                        .await
                        .ok();

                    if let ValidationFailure::ManifestAbsent { message } =
                        classify_validation_failure(Some(&result))
                    {
                        return handle_manifest_absent(obj, ctx, namespace, name, status, &message)
                            .await;
                    }

                    let error_msg = result
                        .error
                        .as_ref()
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| {
                            if result.backup_id.as_deref() != Some(&obj.spec.backup_id) {
                                "backup ID mismatch between manifest and spec".to_string()
                            } else {
                                "validation returned non-success result".to_string()
                            }
                        });

                    return mark_backup_invalid(
                        ctx,
                        namespace,
                        name,
                        status,
                        obj,
                        "ManifestInvalid",
                        format!("Manifest validation failed: {error_msg}"),
                    )
                    .await;
                }
                None => {
                    debug!(
                        namespace,
                        name, "validation Job completed but result not yet readable; requeueing"
                    );
                    patch_backup_status(ctx, namespace, name, status).await?;
                    return Ok((
                        kube::runtime::controller::Action::requeue(REQUEUE_JOB_PENDING),
                        false,
                    ));
                }
            }
        }

        debug!(namespace, name, "validation Job still running");
        patch_backup_status(ctx, namespace, name, status).await?;
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_JOB_PENDING),
            false,
        ));
    }

    let validation_job = build_validation_job(obj, &repository, namespace);
    let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
    match job_api.create(&Default::default(), &validation_job).await {
        Ok(_) => {
            info!(namespace, name, "created validation Job");
        }
        Err(kube::Error::Api(ae)) if ae.code == 409 => {
            debug!(namespace, name, "validation Job already exists");
        }
        Err(e) => {
            return Err(Error::KubeError(
                format!("failed to create validation Job for {namespace}/{name}"),
                Box::new(e),
            ));
        }
    }

    let condition = Condition {
        type_: "Ready".to_string(),
        status: "False".to_string(),
        observed_generation: obj.metadata.generation,
        last_transition_time: Time(Timestamp::now()),
        reason: "Validating".to_string(),
        message: "Manifest validation Job created".to_string(),
    };
    status.conditions.retain(|c| c.type_ != "Ready");
    status.conditions.push(condition);
    patch_backup_status(ctx, namespace, name, status).await?;

    Ok((
        kube::runtime::controller::Action::requeue(REQUEUE_JOB_PENDING),
        false,
    ))
}

async fn mark_backup_invalid(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    namespace: &str,
    name: &str,
    status: &mut KanidmBackupStatus,
    obj: &KanidmBackup,
    reason: &str,
    message: String,
) -> Result<(kube::runtime::controller::Action, bool)> {
    status.phase = KanidmBackupPhase::Invalid;
    let condition = Condition {
        type_: "Ready".to_string(),
        status: "False".to_string(),
        observed_generation: obj.metadata.generation,
        last_transition_time: Time(Timestamp::now()),
        reason: reason.to_string(),
        message,
    };
    status.conditions.retain(|c| c.type_ != "Ready");
    status.conditions.push(condition);
    patch_backup_status(ctx, namespace, name, status).await?;
    Ok((
        kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
        false,
    ))
}

/// Shared dispatch for a manifest that is demonstrably absent. A record created by
/// discovery is a phantom catalogue entry and is discarded without counting a failure; a
/// user-authored backup with a missing manifest is a real error and is parked Invalid.
async fn handle_manifest_absent(
    obj: &KanidmBackup,
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    namespace: &str,
    name: &str,
    status: &mut KanidmBackupStatus,
    message: &str,
) -> Result<(kube::runtime::controller::Action, bool)> {
    if is_discovery_owned(obj) {
        return discard_phantom_backup(obj, ctx, namespace, name, message).await;
    }
    backup_metrics().inc_validation_failure(namespace);
    mark_backup_invalid(
        ctx,
        namespace,
        name,
        status,
        obj,
        "ManifestAbsent",
        format!(
            "Manifest {} no longer exists in the repository: {message}",
            obj.spec.manifest_key
        ),
    )
    .await
}

/// Deletes a discovered backup record whose manifest is gone. The normal finalizer flow
/// cleans the whole prefix, which may still hold the payload.
async fn discard_phantom_backup(
    obj: &KanidmBackup,
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    namespace: &str,
    name: &str,
    message: &str,
) -> Result<(kube::runtime::controller::Action, bool)> {
    warn!(
        namespace,
        name,
        manifest_key = %obj.spec.manifest_key,
        error = message,
        "discovered manifest is absent; discarding phantom backup record"
    );

    let _ = ctx
        .recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: "ManifestAbsent".to_string(),
                note: Some(format!(
                    "Discovered manifest {} no longer exists in the repository; discarding phantom backup record: {message}",
                    obj.spec.manifest_key
                )),
                action: "PhantomBackupDiscarded".into(),
                secondary: None,
            },
            &obj.object_ref(&()),
        )
        .await;

    let api: Api<KanidmBackup> = Api::namespaced(ctx.client.clone(), namespace);
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => {
            info!(namespace, name, "deleted phantom backup record");
        }
        Err(e) if is_kube_not_found(&e) => {
            debug!(namespace, name, "phantom backup record already deleted");
        }
        Err(e) => {
            return Err(Error::KubeError(
                format!("failed to delete phantom backup {namespace}/{name}"),
                Box::new(e),
            ));
        }
    }

    Ok((
        kube::runtime::controller::Action::requeue(REQUEUE_DELETION),
        false,
    ))
}

async fn handle_deletion(
    obj: &KanidmBackup,
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    namespace: &str,
    name: &str,
    status: &mut KanidmBackupStatus,
) -> Result<(kube::runtime::controller::Action, bool)> {
    let is_referenced = check_referenced_by_active_restore(ctx, obj, namespace).await?;
    if is_referenced {
        warn!(
            namespace,
            name, "backup deletion deferred: referenced by active restore"
        );
        backup_metrics().inc_deletion_deferred(namespace);
        status.phase = KanidmBackupPhase::Ready;
        let deferred_condition = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: obj.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "ActiveRestoreReference".to_string(),
            message: "Backup is referenced by an active restore and cannot be deleted".to_string(),
        };
        status.conditions.retain(|c| c.type_ != "DeletionDeferred");
        status.conditions.push(deferred_condition);
        patch_backup_status(ctx, namespace, name, status).await?;
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
            false,
        ));
    }

    let repository = match get_repository(ctx, namespace, &obj.spec.repository_ref.name).await? {
        Some(repo) => repo,
        None => {
            warn!(
                namespace,
                name,
                repository = %obj.spec.repository_ref.name,
                "repository not found during deletion; proceeding without S3 cleanup, data may be orphaned"
            );
            status.phase = KanidmBackupPhase::Deleted;
            let condition = Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                observed_generation: obj.metadata.generation,
                last_transition_time: Time(Timestamp::now()),
                reason: "RepositoryGoneDataOrphaned".to_string(),
                message: format!(
                    "Repository {} no longer exists; deletion proceeds without S3 cleanup, objects may be orphaned",
                    obj.spec.repository_ref.name
                ),
            };
            status.conditions.retain(|c| c.type_ != "Ready");
            status.conditions.push(condition);
            patch_backup_status(ctx, namespace, name, status).await?;
            return Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                true,
            ));
        }
    };

    let existing_job = find_job(&ctx.client, namespace, name, "deletion").await?;

    if let Some(job) = existing_job {
        if job_has_failed(&job) {
            let failed_result =
                read_failed_job_result(&ctx.client, namespace, &job, "delete").await;

            let defer_reason = match &failed_result {
                Ok(Some(doc)) => doc.deletion.as_ref().and_then(|d| d.classify_deferral()),
                _ => None,
            };

            if let Some(reason) = defer_reason {
                let failed_count = failed_result
                    .as_ref()
                    .ok()
                    .and_then(|d| d.as_ref())
                    .and_then(|doc| doc.deletion.as_ref())
                    .map(|d| d.failed_keys.len())
                    .unwrap_or(0);

                warn!(
                    namespace,
                    name,
                    reason = reason.as_str(),
                    failed_keys = failed_count,
                    "deletion deferred: keys could not be removed"
                );

                backup_metrics().inc_gc_deferred(namespace, reason.as_str());

                let existing_deferred = status
                    .conditions
                    .iter()
                    .find(|c| c.type_ == "DeletionDeferred")
                    .cloned();
                let backoff = compute_deferred_backoff(existing_deferred.as_ref());

                let deferred_condition = build_deferred_condition(
                    existing_deferred.as_ref(),
                    reason.condition_reason(),
                    format!(
                        "{failed_count} key(s) could not be deleted: {}",
                        reason.as_str()
                    ),
                    obj.metadata.generation,
                );
                status.conditions.retain(|c| c.type_ != "DeletionDeferred");
                status.conditions.push(deferred_condition);

                let _ = ctx
                    .recorder
                    .publish(
                        &Event {
                            type_: EventType::Warning,
                            reason: reason.condition_reason().to_string(),
                            note: Some(format!(
                                "{failed_count} key(s) deferred during garbage collection"
                            )),
                            action: "DeletionDeferred".into(),
                            secondary: None,
                        },
                        &obj.object_ref(&()),
                    )
                    .await;

                let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
                job_api
                    .delete(&job.name_any(), &background_delete_params())
                    .await
                    .ok();
                patch_backup_status(ctx, namespace, name, status).await?;
                return Ok((kube::runtime::controller::Action::requeue(backoff), false));
            }

            let failure_message = match &failed_result {
                Ok(Some(result)) => result
                    .error
                    .as_ref()
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| "deletion Job failed".to_string()),
                _ => "deletion Job failed".to_string(),
            };

            warn!(
                namespace,
                name,
                error = failure_message,
                "deletion Job failed; will retry"
            );

            let existing_deferred = status
                .conditions
                .iter()
                .find(|c| c.type_ == "DeletionDeferred")
                .cloned();

            let retry_condition = build_deferred_condition(
                existing_deferred.as_ref(),
                "DeletionFailed",
                format!("Deletion Job failed: {failure_message}"),
                obj.metadata.generation,
            );
            status.conditions.retain(|c| c.type_ != "DeletionDeferred");
            status.conditions.push(retry_condition);

            let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
            job_api
                .delete(&job.name_any(), &background_delete_params())
                .await
                .ok();
            patch_backup_status(ctx, namespace, name, status).await?;
            return Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_DELETION),
                false,
            ));
        }

        if job_is_complete(&job) {
            let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
            job_api
                .delete(&job.name_any(), &background_delete_params())
                .await
                .ok();

            status.phase = KanidmBackupPhase::Deleted;
            let deleted_condition = Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                observed_generation: obj.metadata.generation,
                last_transition_time: Time(Timestamp::now()),
                reason: "Deleted".to_string(),
                message: "Backup data deleted from repository".to_string(),
            };
            status.conditions.retain(|c| c.type_ != "Ready");
            status.conditions.push(deleted_condition);
            patch_backup_status(ctx, namespace, name, status).await?;

            return Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                true,
            ));
        }

        debug!(namespace, name, "deletion Job still running");
        patch_backup_status(ctx, namespace, name, status).await?;
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_DELETION),
            false,
        ));
    }

    let repo_path = RepositoryPath::new(&repository.spec.s3.bucket, &repository.spec.s3.prefix)
        .map_err(|e| Error::MissingData(format!("invalid repository path: {e}")))?;

    let manifest_key = &obj.spec.manifest_key;
    if !repo_path.contains_key(manifest_key) {
        warn!(
            namespace,
            name, "manifest key escapes repository; marking deleted"
        );
        status.phase = KanidmBackupPhase::Deleted;
        patch_backup_status(ctx, namespace, name, status).await?;
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
            true,
        ));
    }

    let backup_prefix = manifest_key
        .strip_suffix("/manifest.json")
        .map(|p| format!("{p}/"))
        .unwrap_or_else(|| manifest_key.clone());

    if !repo_path.contains_prefix(&backup_prefix) {
        warn!(
            namespace,
            name, "backup prefix escapes repository; marking deleted"
        );
        status.phase = KanidmBackupPhase::Deleted;
        patch_backup_status(ctx, namespace, name, status).await?;
        return Ok((
            kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
            true,
        ));
    }

    let deletion_job = build_deletion_job(&repository, namespace, name, &backup_prefix);
    let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
    match job_api.create(&Default::default(), &deletion_job).await {
        Ok(_) => {
            info!(namespace, name, "created deletion Job");
        }
        Err(kube::Error::Api(ae)) if ae.code == 409 => {
            debug!(namespace, name, "deletion Job already exists");
        }
        Err(e) => {
            return Err(Error::KubeError(
                format!("failed to create deletion Job for {namespace}/{name}"),
                Box::new(e),
            ));
        }
    }

    patch_backup_status(ctx, namespace, name, status).await?;
    Ok((
        kube::runtime::controller::Action::requeue(REQUEUE_DELETION),
        false,
    ))
}

async fn check_referenced_by_active_restore(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    backup: &KanidmBackup,
    namespace: &str,
) -> Result<bool> {
    let kanidm_name = &backup.spec.kanidm_ref.name;
    let api: Api<Kanidm> = Api::namespaced(ctx.client.clone(), namespace);
    match api.get(kanidm_name).await {
        Ok(kanidm) => Ok(kanidm.annotations().contains_key(RESTORE_ANNOTATION)),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(false),
        Err(e) => Err(Error::KubeError(
            format!("failed to check restore status for Kanidm {namespace}/{kanidm_name}"),
            Box::new(e),
        )),
    }
}

async fn patch_backup_status(
    ctx: &kaniop_operator::controller::context::Context<KanidmBackup>,
    namespace: &str,
    name: &str,
    status: &KanidmBackupStatus,
) -> Result<()> {
    let api: Api<KanidmBackup> = Api::namespaced(ctx.client.clone(), namespace);
    let patch = serde_json::json!({
        "apiVersion": "kaniop.rs/v1alpha1",
        "kind": "KanidmBackup",
        "status": status
    });
    api.patch_status(
        name,
        &PatchParams::apply(CONTROLLER_ID),
        &Patch::Apply(patch),
    )
    .await
    .map_err(|e| {
        Error::KubeError(
            format!("failed to patch status for {namespace}/{name}"),
            Box::new(e),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{KanidmBackupPhase, KanidmBackupSpec, KanidmBackupStatus};
    use chrono::Datelike;

    fn make_backup(backup_id: &str, manifest_key: &str) -> KanidmBackup {
        KanidmBackup {
            metadata: ObjectMeta {
                name: Some(format!("kb-{}", &backup_id[..8])),
                namespace: Some("default".to_string()),
                generation: Some(1),
                ..Default::default()
            },
            spec: KanidmBackupSpec {
                backup_id: backup_id.to_string(),
                kanidm_ref: BackupKanidmRef {
                    name: "corp-idm".to_string(),
                    uid: "k-uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
                manifest_key: manifest_key.to_string(),
            },
            status: None,
        }
    }

    #[test]
    fn manifest_to_backup_cr_stores_manifest_key_not_payload_key() {
        let manifest_key = "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json";
        let cr = manifest_to_backup_cr(
            manifest_key,
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "corp-idm",
            "k-uid",
        );
        assert_eq!(cr.spec.manifest_key, manifest_key);
        assert!(cr.spec.manifest_key.ends_with("/manifest.json"));
        assert!(!cr.spec.manifest_key.contains("/payload/"));
    }

    #[test]
    fn manifest_to_backup_cr_name_is_deterministic() {
        let cr1 = manifest_to_backup_cr(
            "prefix/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        let cr2 = manifest_to_backup_cr(
            "different-prefix/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        assert_eq!(cr1.metadata.name, cr2.metadata.name);
    }

    #[test]
    fn manifest_to_backup_cr_different_backup_ids_produce_different_names() {
        let cr1 = manifest_to_backup_cr(
            "m1/manifest.json",
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "kanidm",
            "uid",
        );
        let cr2 = manifest_to_backup_cr(
            "m2/manifest.json",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "repo",
            "kanidm",
            "uid",
        );
        assert_ne!(cr1.metadata.name, cr2.metadata.name);
    }

    #[test]
    fn validation_job_has_hardened_security() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        let repository = KanidmBackupRepository {
            metadata: ObjectMeta {
                name: Some("offsite".to_string()),
                ..Default::default()
            },
            spec: crate::crd::KanidmBackupRepositorySpec {
                s3: crate::crd::S3Config {
                    bucket: "my-bucket".to_string(),
                    prefix: "prod".to_string(),
                    region: "us-east-1".to_string(),
                    endpoint: "https://s3.example.com".to_string(),
                    force_path_style: false,
                    insecure: false,
                    ca_bundle_ref: None,
                },
                authentication: crate::crd::RepositoryAuthentication {
                    writer: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(crate::crd::SecretRef {
                            name: "writer".to_string(),
                        }),
                    },
                    reader: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(crate::crd::SecretRef {
                            name: "reader".to_string(),
                        }),
                    },
                    deleter: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: Some(crate::crd::SecretRef {
                            name: "deleter".to_string(),
                        }),
                    },
                },
                encryption: None,
                limits: None,
            },
            status: None,
        };

        let job = build_validation_job(&backup, &repository, "default");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();

        assert_eq!(pod_spec.automount_service_account_token, Some(false));

        let pod_sec = pod_spec.security_context.unwrap();
        assert_eq!(pod_sec.run_as_non_root, Some(true));
        assert_eq!(
            pod_sec.seccomp_profile.as_ref().map(|s| s.type_.as_str()),
            Some("RuntimeDefault")
        );

        let container = &pod_spec.containers[0];
        let sec = container.security_context.as_ref().unwrap();
        assert_eq!(sec.run_as_non_root, Some(true));
        assert_eq!(sec.read_only_root_filesystem, Some(true));
        assert_eq!(sec.allow_privilege_escalation, Some(false));
        let caps = sec.capabilities.as_ref().unwrap();
        assert_eq!(caps.drop, Some(vec!["ALL".to_string()]));

        assert!(container.resources.is_some());
    }

    #[test]
    fn validation_job_sets_ttl_seconds_after_finished() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        let repository = make_repository_with_auth(Some("w"), Some("r"), Some("d"));
        let job = build_validation_job(&backup, &repository, "default");
        let job_spec = job.spec.unwrap();
        assert_eq!(
            job_spec.ttl_seconds_after_finished,
            Some(BACKUP_JOB_TTL_SECONDS)
        );
    }

    #[test]
    fn deletion_job_sets_ttl_seconds_after_finished() {
        let repository = make_repository_with_auth(Some("w"), Some("r"), Some("d"));
        let prefix = "p/v1/tenants/ns/clusters/k/backups/b1/";
        let job = build_deletion_job(&repository, "default", "kb-test", prefix);
        let job_spec = job.spec.unwrap();
        assert_eq!(
            job_spec.ttl_seconds_after_finished,
            Some(BACKUP_JOB_TTL_SECONDS)
        );
    }

    #[test]
    fn deletion_job_has_hardened_security() {
        let repository = KanidmBackupRepository {
            metadata: ObjectMeta {
                name: Some("offsite".to_string()),
                ..Default::default()
            },
            spec: crate::crd::KanidmBackupRepositorySpec {
                s3: crate::crd::S3Config {
                    bucket: "b".to_string(),
                    prefix: "p".to_string(),
                    region: "r".to_string(),
                    endpoint: "https://s3.example.com".to_string(),
                    force_path_style: false,
                    insecure: false,
                    ca_bundle_ref: None,
                },
                authentication: crate::crd::RepositoryAuthentication {
                    writer: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                    reader: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                    deleter: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                },
                encryption: None,
                limits: None,
            },
            status: None,
        };

        let prefix = "p/v1/tenants/ns/clusters/k/backups/b1/";
        let job = build_deletion_job(&repository, "default", "kb-test", prefix);
        let pod_spec = job.spec.unwrap().template.spec.unwrap();

        assert_eq!(pod_spec.automount_service_account_token, Some(false));
        let container = &pod_spec.containers[0];
        let sec = container.security_context.as_ref().unwrap();
        assert_eq!(sec.allow_privilege_escalation, Some(false));
        assert_eq!(sec.read_only_root_filesystem, Some(true));
    }

    #[test]
    fn default_phase_is_discovering() {
        let status = KanidmBackupStatus::default();
        assert_eq!(status.phase, KanidmBackupPhase::Discovering);
    }

    #[test]
    fn discovering_never_transitions_to_ready_without_evidence() {
        let status = KanidmBackupStatus::default();
        assert_eq!(status.phase, KanidmBackupPhase::Discovering);
        assert_ne!(status.phase, KanidmBackupPhase::Ready);
    }

    #[test]
    fn validation_job_operation_contains_manifest_key() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        let repository = KanidmBackupRepository {
            metadata: ObjectMeta {
                name: Some("offsite".to_string()),
                ..Default::default()
            },
            spec: crate::crd::KanidmBackupRepositorySpec {
                s3: crate::crd::S3Config {
                    bucket: "b".to_string(),
                    prefix: "prod".to_string(),
                    region: "r".to_string(),
                    endpoint: "https://s3.example.com".to_string(),
                    force_path_style: false,
                    insecure: false,
                    ca_bundle_ref: None,
                },
                authentication: crate::crd::RepositoryAuthentication {
                    writer: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                    reader: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                    deleter: crate::crd::AuthMethod {
                        workload_identity: None,
                        secret_ref: None,
                    },
                },
                encryption: None,
                limits: None,
            },
            status: None,
        };

        let job = build_validation_job(&backup, &repository, "default");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let args = pod_spec.containers[0].args.as_ref().unwrap();
        let op_json = &args[3];
        let op: serde_json::Value = serde_json::from_str(op_json).unwrap();
        assert_eq!(
            op["manifestKey"],
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json"
        );
        assert_eq!(op["manifestOnly"], true);
        assert_eq!(
            op["expectedBackupId"],
            "019c7c76-f423-7a12-8f41-2bea7588a303"
        );
    }

    #[test]
    fn job_is_complete_detects_succeeded() {
        let job = Job {
            status: Some(k8s_openapi::api::batch::v1::JobStatus {
                succeeded: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(job_is_complete(&job));
        assert!(!job_has_failed(&job));
    }

    #[test]
    fn job_has_failed_detects_failure() {
        let job = Job {
            status: Some(k8s_openapi::api::batch::v1::JobStatus {
                failed: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(job_has_failed(&job));
        assert!(!job_is_complete(&job));
    }

    #[test]
    fn manifest_key_must_end_with_manifest_json() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/payload/data.gz",
        );
        assert!(!backup.spec.manifest_key.ends_with("/manifest.json"));
    }

    #[test]
    fn table_test_phase_transitions_require_evidence() {
        let cases = [
            (KanidmBackupPhase::Discovering, false),
            (KanidmBackupPhase::Ready, true),
            (KanidmBackupPhase::Invalid, false),
            (KanidmBackupPhase::Deleted, false),
        ];
        for (phase, expected_ready) in cases {
            let status = KanidmBackupStatus {
                phase,
                ..Default::default()
            };
            let is_ready = status.phase == KanidmBackupPhase::Ready;
            assert_eq!(
                is_ready, expected_ready,
                "phase {phase:?} should have ready={expected_ready}"
            );
        }
    }

    #[test]
    fn table_test_manifest_key_validation() {
        let cases = [
            (
                "prod/v1/tenants/ns/clusters/k/backups/b/manifest.json",
                true,
            ),
            (
                "prod/v1/tenants/ns/clusters/k/backups/b/payload/data",
                false,
            ),
            ("prefix/manifest.json", true),
            ("", false),
            ("prod/../manifest.json", false),
        ];
        for (key, should_be_valid) in cases {
            let is_valid =
                !key.is_empty() && key.ends_with("/manifest.json") && !key.contains("..");
            assert_eq!(
                is_valid, should_be_valid,
                "key '{key}' should be valid={should_be_valid}"
            );
        }
    }

    fn make_repository_with_auth(
        writer_secret: Option<&str>,
        reader_secret: Option<&str>,
        deleter_secret: Option<&str>,
    ) -> KanidmBackupRepository {
        use crate::crd::{AuthMethod, SecretRef};

        let make_method = |secret: Option<&str>| AuthMethod {
            workload_identity: if secret.is_none() {
                Some(kaniop_backup_core::crd::WorkloadIdentity { audience: None })
            } else {
                None
            },
            secret_ref: secret.map(|s| SecretRef {
                name: s.to_string(),
            }),
        };

        KanidmBackupRepository {
            metadata: kube::api::ObjectMeta {
                name: Some("offsite".to_string()),
                ..Default::default()
            },
            spec: crate::crd::KanidmBackupRepositorySpec {
                s3: crate::crd::S3Config {
                    bucket: "b".to_string(),
                    prefix: "p".to_string(),
                    region: "r".to_string(),
                    endpoint: "https://s3.example.com".to_string(),
                    force_path_style: false,
                    insecure: false,
                    ca_bundle_ref: None,
                },
                authentication: crate::crd::RepositoryAuthentication {
                    writer: make_method(writer_secret),
                    reader: make_method(reader_secret),
                    deleter: make_method(deleter_secret),
                },
                encryption: None,
                limits: None,
            },
            status: None,
        }
    }

    #[test]
    fn validation_job_injects_reader_auth() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        let repository = make_repository_with_auth(Some("w"), Some("reader-secret"), Some("d"));

        let job = build_validation_job(&backup, &repository, "default");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let container = &pod_spec.containers[0];
        let env_vars = container.env.as_ref().unwrap();

        let aws_key = env_vars
            .iter()
            .find(|e| e.name == "AWS_ACCESS_KEY_ID")
            .expect("validation job should inject AWS_ACCESS_KEY_ID");
        let key_ref = aws_key
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.name, "reader-secret");
    }

    #[test]
    fn deletion_job_injects_deleter_auth() {
        let repository = make_repository_with_auth(Some("w"), Some("r"), Some("deleter-secret"));
        let prefix = "p/v1/tenants/ns/clusters/k/backups/b1/";

        let job = build_deletion_job(&repository, "default", "kb-test", prefix);
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let container = &pod_spec.containers[0];
        let env_vars = container.env.as_ref().unwrap();

        let aws_key = env_vars
            .iter()
            .find(|e| e.name == "AWS_ACCESS_KEY_ID")
            .expect("deletion job should inject AWS_ACCESS_KEY_ID");
        let key_ref = aws_key
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.name, "deleter-secret");
    }

    #[test]
    fn validation_job_includes_session_token_env() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        let repository = make_repository_with_auth(Some("w"), Some("r"), Some("d"));

        let job = build_validation_job(&backup, &repository, "default");
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let container = &pod_spec.containers[0];
        let env_vars = container.env.as_ref().unwrap();

        let session_token = env_vars.iter().find(|e| e.name == "AWS_SESSION_TOKEN");
        assert!(
            session_token.is_some(),
            "validation job should include AWS_SESSION_TOKEN (optional)"
        );
        let key_ref = session_token
            .unwrap()
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(key_ref.optional, Some(true));
    }

    #[test]
    fn deletion_job_operation_contains_backup_prefix() {
        let repository = make_repository_with_auth(Some("w"), Some("r"), Some("d"));
        let prefix = "p/v1/tenants/ns/clusters/k/backups/b1/";
        let job = build_deletion_job(&repository, "default", "kb-test", prefix);
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let args = pod_spec.containers[0].args.as_ref().unwrap();
        let op_json = &args[3];
        let op: serde_json::Value = serde_json::from_str(op_json).unwrap();
        assert_eq!(op["backupPrefix"], prefix);
        assert!(op["keys"].is_null());
    }

    #[test]
    fn finalizer_name_is_correct() {
        assert_eq!(BACKUP_FINALIZER, "kanidmbackups.kaniop.rs/finalizer");
    }

    #[test]
    fn add_finalizer_patch_has_correct_shape() {
        let patch = build_add_finalizer_patch(None);
        let finalizers = patch["metadata"]["finalizers"]
            .as_array()
            .expect("finalizers must be an array");
        assert!(
            finalizers
                .iter()
                .any(|v| v.as_str() == Some(BACKUP_FINALIZER)),
            "patch must include the backup finalizer"
        );
    }

    #[test]
    fn add_finalizer_patch_preserves_existing_finalizers() {
        let existing = vec!["other.example/finalizer".to_string()];
        let patch = build_add_finalizer_patch(Some(&existing));
        let finalizers = patch["metadata"]["finalizers"]
            .as_array()
            .expect("finalizers must be an array");
        assert_eq!(finalizers.len(), 2);
        assert!(
            finalizers
                .iter()
                .any(|v| v.as_str() == Some("other.example/finalizer"))
        );
        assert!(
            finalizers
                .iter()
                .any(|v| v.as_str() == Some(BACKUP_FINALIZER))
        );
    }

    #[test]
    fn add_finalizer_patch_does_not_duplicate_when_already_present() {
        let existing = vec![BACKUP_FINALIZER.to_string()];
        let patch = build_add_finalizer_patch(Some(&existing));
        let finalizers = patch["metadata"]["finalizers"]
            .as_array()
            .expect("finalizers must be an array");
        assert_eq!(finalizers.len(), 1);
    }

    #[test]
    fn is_kube_not_found_detects_404() {
        let not_found_err = kube::Error::Api(Box::new(kube::core::Status {
            code: 404,
            message: "not found".to_string(),
            reason: "NotFound".to_string(),
            ..Default::default()
        }));
        assert!(is_kube_not_found(&not_found_err));
    }

    #[test]
    fn is_kube_not_found_rejects_other_codes() {
        let server_err = kube::Error::Api(Box::new(kube::core::Status {
            code: 500,
            message: "internal error".to_string(),
            reason: "InternalError".to_string(),
            ..Default::default()
        }));
        assert!(!is_kube_not_found(&server_err));

        let conflict_err = kube::Error::Api(Box::new(kube::core::Status {
            code: 409,
            message: "conflict".to_string(),
            reason: "Conflict".to_string(),
            ..Default::default()
        }));
        assert!(!is_kube_not_found(&conflict_err));
    }

    #[test]
    fn is_kube_not_found_rejects_non_api_errors() {
        let other_err = kube::Error::Service("connection refused".to_string().into());
        assert!(!is_kube_not_found(&other_err));
    }

    #[test]
    fn deletion_repository_gone_condition_reason() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        let mut status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Deleted,
            ..Default::default()
        };
        let condition = Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: backup.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "RepositoryGoneDataOrphaned".to_string(),
            message: "Repository offsite no longer exists; deletion proceeds without S3 cleanup, objects may be orphaned".to_string(),
        };
        status.conditions.retain(|c| c.type_ != "Ready");
        status.conditions.push(condition);

        assert_eq!(status.phase, KanidmBackupPhase::Deleted);
        let ready_cond = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .unwrap();
        assert_eq!(ready_cond.status, "False");
        assert_eq!(ready_cond.reason, "RepositoryGoneDataOrphaned");
        assert!(ready_cond.message.contains("orphaned"));
    }

    #[test]
    fn discovering_repository_gone_condition_reason() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        let mut status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Invalid,
            ..Default::default()
        };
        let condition = Condition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            observed_generation: backup.metadata.generation,
            last_transition_time: Time(Timestamp::now()),
            reason: "RepositoryGone".to_string(),
            message: "Repository offsite no longer exists; backup cannot be validated".to_string(),
        };
        status.conditions.retain(|c| c.type_ != "Ready");
        status.conditions.push(condition);

        assert_eq!(status.phase, KanidmBackupPhase::Invalid);
        let ready_cond = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .unwrap();
        assert_eq!(ready_cond.status, "False");
        assert_eq!(ready_cond.reason, "RepositoryGone");
    }

    #[test]
    fn backup_metrics_new_creates_counters() {
        use opentelemetry::metrics::MeterProvider;
        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder().build();
        let meter = provider.meter("test");
        let metrics = BackupMetrics::new(&meter);
        metrics.inc_validation_failure("default");
        metrics.inc_deletion_deferred("default");
        metrics.inc_gc_deferred("default", "object_lock");
        metrics.inc_gc_deferred("default", "access_denied");
        metrics.inc_gc_deferred("default", "active_restore");
    }

    #[test]
    fn compute_deferred_backoff_no_condition_returns_initial() {
        let backoff = compute_deferred_backoff(None);
        assert_eq!(backoff, REQUEUE_DELETION_DEFERRED_INITIAL);
    }

    #[test]
    fn compute_deferred_backoff_recent_condition_returns_initial() {
        let cond = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::now()),
            reason: "ObjectLockRetention".to_string(),
            message: "test".to_string(),
        };
        let backoff = compute_deferred_backoff(Some(&cond));
        assert_eq!(backoff, REQUEUE_DELETION_DEFERRED_INITIAL);
    }

    #[test]
    fn compute_deferred_backoff_caps_at_max() {
        let old_secs = Timestamp::now().as_second() - 86400;
        let old_time = Timestamp::from_second(old_secs).unwrap();
        let cond = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(old_time),
            reason: "ObjectLockRetention".to_string(),
            message: "test".to_string(),
        };
        let backoff = compute_deferred_backoff(Some(&cond));
        assert_eq!(backoff, REQUEUE_DELETION_DEFERRED_MAX);
    }

    #[test]
    fn needs_metadata_backfill_ready_without_created_at() {
        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            consistency: Some("kanidm-offline".to_string()),
            reason: Some("scheduled".to_string()),
            kanidm_version: Some("1.10.4".to_string()),
            created_at: None,
            ..Default::default()
        };
        assert!(needs_metadata_backfill(&status));
    }

    #[test]
    fn needs_metadata_backfill_ready_without_consistency() {
        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            created_at: Some("2026-08-18T02:03:41Z".to_string()),
            consistency: None,
            reason: Some("scheduled".to_string()),
            kanidm_version: Some("1.10.4".to_string()),
            ..Default::default()
        };
        assert!(needs_metadata_backfill(&status));
    }

    #[test]
    fn needs_metadata_backfill_ready_without_reason() {
        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            created_at: Some("2026-08-18T02:03:41Z".to_string()),
            consistency: Some("kanidm-offline".to_string()),
            kanidm_version: Some("1.10.4".to_string()),
            ..Default::default()
        };
        assert!(needs_metadata_backfill(&status));
    }

    #[test]
    fn needs_metadata_backfill_ready_without_version() {
        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            created_at: Some("2026-08-18T02:03:41Z".to_string()),
            consistency: Some("kanidm-offline".to_string()),
            reason: Some("scheduled".to_string()),
            ..Default::default()
        };
        assert!(needs_metadata_backfill(&status));
    }

    #[test]
    fn needs_metadata_backfill_ready_with_manifest_metadata() {
        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            created_at: Some("2026-08-18T02:03:41Z".to_string()),
            consistency: Some("kanidm-offline".to_string()),
            reason: Some("scheduled".to_string()),
            kanidm_version: Some("1.10.4".to_string()),
            ..Default::default()
        };
        assert!(!needs_metadata_backfill(&status));
    }

    #[test]
    fn needs_metadata_backfill_not_ready_phase() {
        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Discovering,
            created_at: None,
            consistency: None,
            ..Default::default()
        };
        assert!(!needs_metadata_backfill(&status));
    }

    #[test]
    fn result_document_propagates_manifest_metadata_to_status() {
        let mut result = kaniop_backup_core::result::ResultDocument::success("download");
        result.backup_id = Some("019c7c76-f423-7a12-8f41-2bea7588a303".to_string());
        result.manifest_key = Some("key/manifest.json".to_string());
        result.created_at = Some("2026-08-18T02:03:41Z".to_string());
        result.consistency = Some("kanidm-offline".to_string());
        result.reason = Some("scheduled".to_string());
        result.kanidm_version = Some("1.10.4".to_string());
        result.image_digest = Some("sha256:abc".to_string());
        result.payload_size_bytes = Some(1024);
        result.payload_sha256 = Some("abc".to_string());

        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            consistency: result.consistency.clone(),
            reason: result.reason.clone(),
            kanidm_version: result.kanidm_version.clone(),
            image_digest: result.image_digest.clone(),
            size_bytes: result.payload_size_bytes,
            payload_sha256: result.payload_sha256,
            created_at: result.created_at.clone(),
            ..Default::default()
        };

        assert_eq!(status.created_at.as_deref(), Some("2026-08-18T02:03:41Z"));
        assert_eq!(status.consistency.as_deref(), Some("kanidm-offline"));
        assert_eq!(status.reason.as_deref(), Some("scheduled"));
        assert_eq!(status.kanidm_version.as_deref(), Some("1.10.4"));
        assert_eq!(status.image_digest.as_deref(), Some("sha256:abc"));
        assert_eq!(status.phase, KanidmBackupPhase::Ready);
    }

    #[test]
    fn retention_sees_created_at_from_status() {
        use kaniop_backup_core::retention::{BackupEntry, parse_timestamp};

        let status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            created_at: Some("2026-08-18T02:03:41Z".to_string()),
            consistency: Some("kanidm-offline".to_string()),
            reason: Some("scheduled".to_string()),
            ..Default::default()
        };

        let created_str = status.created_at.as_deref().unwrap();
        let created_at = parse_timestamp(created_str).unwrap();
        let entry = BackupEntry {
            id: "kb-test".to_string(),
            created_at,
            consistency: status.consistency.clone().unwrap_or_default(),
            reason: status.reason.clone().unwrap_or_default(),
            referenced_by_active_restore: false,
            safety_backup_min_retention_hours: None,
        };

        assert_eq!(entry.id, "kb-test");
        assert_eq!(entry.created_at.year(), 2026);
        assert_eq!(entry.created_at.month(), 8);
        assert_eq!(entry.consistency, "kanidm-offline");
    }

    #[test]
    fn backfill_resets_to_discovering() {
        let mut status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            consistency: Some("kanidm-offline".to_string()),
            reason: Some("scheduled".to_string()),
            kanidm_version: Some("1.10.4".to_string()),
            image_digest: Some("sha256:abc".to_string()),
            created_at: None,
            ..Default::default()
        };

        assert!(needs_metadata_backfill(&status));

        status.phase = KanidmBackupPhase::Discovering;
        status.consistency = None;
        status.reason = None;
        status.kanidm_version = None;
        status.image_digest = None;
        status.created_at = None;
        status.conditions.retain(|c| c.type_ != "Ready");

        assert_eq!(status.phase, KanidmBackupPhase::Discovering);
        assert!(status.consistency.is_none());
        assert!(status.reason.is_none());
        assert!(status.kanidm_version.is_none());
        assert!(status.image_digest.is_none());
        assert!(status.created_at.is_none());
        assert!(!needs_metadata_backfill(&status));
    }

    #[test]
    fn deletion_deferred_object_lock_condition_shape() {
        let mut status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Deleting,
            ..Default::default()
        };
        let condition = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::now()),
            reason: "ObjectLockRetention".to_string(),
            message: "2 key(s) could not be deleted: object_lock".to_string(),
        };
        status.conditions.retain(|c| c.type_ != "DeletionDeferred");
        status.conditions.push(condition);

        let deferred = status
            .conditions
            .iter()
            .find(|c| c.type_ == "DeletionDeferred")
            .unwrap();
        assert_eq!(deferred.status, "True");
        assert_eq!(deferred.reason, "ObjectLockRetention");
        assert!(deferred.message.contains("object_lock"));
        assert_eq!(status.phase, KanidmBackupPhase::Deleting);
    }

    #[test]
    fn deletion_deferred_access_denied_condition_shape() {
        let mut status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Deleting,
            ..Default::default()
        };
        let condition = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::now()),
            reason: "AccessDenied".to_string(),
            message: "1 key(s) could not be deleted: access_denied".to_string(),
        };
        status.conditions.retain(|c| c.type_ != "DeletionDeferred");
        status.conditions.push(condition);

        let deferred = status
            .conditions
            .iter()
            .find(|c| c.type_ == "DeletionDeferred")
            .unwrap();
        assert_eq!(deferred.status, "True");
        assert_eq!(deferred.reason, "AccessDenied");
        assert_eq!(status.phase, KanidmBackupPhase::Deleting);
    }

    #[test]
    fn deletion_deferred_active_restore_sets_correct_reason() {
        let mut status = KanidmBackupStatus {
            phase: KanidmBackupPhase::Ready,
            ..Default::default()
        };
        let condition = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::now()),
            reason: "ActiveRestoreReference".to_string(),
            message: "Backup is referenced by an active restore and cannot be deleted".to_string(),
        };
        status.conditions.retain(|c| c.type_ != "DeletionDeferred");
        status.conditions.push(condition);

        let deferred = status
            .conditions
            .iter()
            .find(|c| c.type_ == "DeletionDeferred")
            .unwrap();
        assert_eq!(deferred.reason, "ActiveRestoreReference");
    }

    #[test]
    fn build_deferred_condition_preserves_transition_time_same_reason() {
        let original_time =
            Time(Timestamp::from_second(Timestamp::now().as_second() - 300).unwrap());
        let existing = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: original_time.clone(),
            reason: "ObjectLockRetention".to_string(),
            message: "old message".to_string(),
        };

        let new_cond = build_deferred_condition(
            Some(&existing),
            "ObjectLockRetention",
            "new message".to_string(),
            Some(1),
        );

        assert_eq!(new_cond.last_transition_time.0, original_time.0);
        assert_eq!(new_cond.reason, "ObjectLockRetention");
        assert_eq!(new_cond.message, "new message");
    }

    #[test]
    fn build_deferred_condition_resets_transition_time_on_reason_change() {
        let old_time = Time(Timestamp::from_second(Timestamp::now().as_second() - 300).unwrap());
        let existing = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: old_time.clone(),
            reason: "AccessDenied".to_string(),
            message: "old".to_string(),
        };

        let before = Timestamp::now().as_second();
        let new_cond = build_deferred_condition(
            Some(&existing),
            "ObjectLockRetention",
            "new".to_string(),
            Some(1),
        );
        let after = Timestamp::now().as_second();

        let new_secs = new_cond.last_transition_time.0.as_second();
        assert!(new_secs >= before && new_secs <= after);
        assert_ne!(new_secs, old_time.0.as_second());
        assert_eq!(new_cond.reason, "ObjectLockRetention");
    }

    #[test]
    fn build_deferred_condition_no_existing_uses_now() {
        let before = Timestamp::now().as_second();
        let cond =
            build_deferred_condition(None, "ObjectLockRetention", "msg".to_string(), Some(1));
        let after = Timestamp::now().as_second();

        let secs = cond.last_transition_time.0.as_second();
        assert!(secs >= before && secs <= after);
    }

    #[test]
    fn consecutive_failures_same_reason_preserve_transition_time() {
        let t0_secs = Timestamp::now().as_second() - 600;
        let t0 = Time(Timestamp::from_second(t0_secs).unwrap());

        let initial = Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: t0.clone(),
            reason: "ObjectLockRetention".to_string(),
            message: "1 key(s)".to_string(),
        };
        let mut conditions = vec![initial];

        let backoff1 =
            compute_deferred_backoff(conditions.iter().find(|c| c.type_ == "DeletionDeferred"));
        assert!(backoff1 >= Duration::from_secs(300));

        let cond2 = build_deferred_condition(
            conditions.iter().find(|c| c.type_ == "DeletionDeferred"),
            "ObjectLockRetention",
            "1 key(s) still deferred".to_string(),
            Some(1),
        );
        conditions.retain(|c| c.type_ != "DeletionDeferred");
        conditions.push(cond2);

        assert_eq!(
            conditions[0].last_transition_time.0.as_second(),
            t0_secs,
            "transition_time must be preserved across same-reason updates"
        );

        let backoff2 =
            compute_deferred_backoff(conditions.iter().find(|c| c.type_ == "DeletionDeferred"));
        assert_eq!(
            backoff2, backoff1,
            "backoff must stay same when transition_time preserved"
        );
    }

    #[test]
    fn reason_transition_resets_backoff() {
        let t0_secs = Timestamp::now().as_second() - 600;
        let t0 = Time(Timestamp::from_second(t0_secs).unwrap());

        let mut conditions: Vec<Condition> = vec![Condition {
            type_: "DeletionDeferred".to_string(),
            status: "True".to_string(),
            observed_generation: Some(1),
            last_transition_time: t0.clone(),
            reason: "ObjectLockRetention".to_string(),
            message: "old".to_string(),
        }];

        let backoff_before =
            compute_deferred_backoff(conditions.iter().find(|c| c.type_ == "DeletionDeferred"));
        assert!(backoff_before >= Duration::from_secs(300));

        let new_cond = build_deferred_condition(
            conditions.iter().find(|c| c.type_ == "DeletionDeferred"),
            "AccessDenied",
            "new reason".to_string(),
            Some(1),
        );
        conditions.retain(|c| c.type_ != "DeletionDeferred");
        conditions.push(new_cond);

        let backoff_after =
            compute_deferred_backoff(conditions.iter().find(|c| c.type_ == "DeletionDeferred"));
        assert_eq!(
            backoff_after, REQUEUE_DELETION_DEFERRED_INITIAL,
            "backoff must reset to initial on reason transition"
        );
        assert_eq!(conditions[0].reason, "AccessDenied");
        assert_ne!(
            conditions[0].last_transition_time.0.as_second(),
            t0_secs,
            "transition_time must be refreshed on reason change"
        );
    }

    #[test]
    fn backoff_sequence_is_bounded_exponential() {
        let t0_secs = Timestamp::now().as_second();

        let expected = [
            REQUEUE_DELETION_DEFERRED_INITIAL,
            Duration::from_secs(120),
            Duration::from_secs(240),
            Duration::from_secs(480),
            REQUEUE_DELETION_DEFERRED_MAX,
            REQUEUE_DELETION_DEFERRED_MAX,
        ];

        let cumulative_offsets = [0u64, 60, 180, 420, 900, 1500];

        for (i, (&offset, &expected_delay)) in
            cumulative_offsets.iter().zip(expected.iter()).enumerate()
        {
            let fake_time = Time(Timestamp::from_second(t0_secs - offset as i64).unwrap());
            let cond = Condition {
                type_: "DeletionDeferred".to_string(),
                status: "True".to_string(),
                observed_generation: Some(1),
                last_transition_time: fake_time,
                reason: "ObjectLockRetention".to_string(),
                message: format!("step {i}"),
            };
            let delay = compute_deferred_backoff(Some(&cond));
            assert_eq!(
                delay, expected_delay,
                "step {i}: expected {expected_delay:?}, got {delay:?}"
            );
        }
    }

    const BASE_SECS: i64 = 1_700_000_000;

    fn ts(offset: i64) -> Timestamp {
        Timestamp::from_second(BASE_SECS + offset).unwrap()
    }

    const DELETED_MANIFEST_KEY: &str =
        "prod/v1/tenants/ns/clusters/k/backups/deadbeef/manifest.json";

    fn discover_job(start: Option<i64>, created: Option<i64>) -> Job {
        Job {
            metadata: ObjectMeta {
                name: Some("kaniop-backup-discover-offsite-daily".to_string()),
                creation_timestamp: created.map(|offset| Time(ts(offset))),
                ..Default::default()
            },
            status: Some(k8s_openapi::api::batch::v1::JobStatus {
                start_time: start.map(|offset| Time(ts(offset))),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn completed_discover_job(start: i64) -> Job {
        let mut job = discover_job(Some(start), None);
        if let Some(status) = job.status.as_mut() {
            status.succeeded = Some(1);
        }
        job
    }

    fn discover_snapshot(keys: &[&str]) -> ResultDocument {
        let mut doc = ResultDocument::success("discover");
        doc.discovery = Some(kaniop_backup_core::result::DiscoverResult {
            manifest_keys: keys.iter().map(|key| key.to_string()).collect(),
            total_found: keys.len() as u32,
            truncated: false,
        });
        doc
    }

    /// Composes the gate exactly like `reconcile_cleanup` does: resolve the conflicting discover
    /// Job once, then decide whether the finalizer must keep waiting for it.
    fn gate_pending(
        jobs: &[(&Job, Option<ResultDocument>)],
        deleted_at: Option<&Time>,
        now_offset: i64,
    ) -> bool {
        let conflicting = deleted_at.and_then(|deleted_at| {
            conflicting_discover_job(jobs, deleted_at, DELETED_MANIFEST_KEY)
        });
        discovery_confirmation_pending(
            conflicting,
            deleted_at,
            ts(now_offset),
            DISCOVERY_CONFIRMATION_GRACE,
        )
    }

    #[test]
    fn discovery_confirmation_pending_without_jobs_is_false() {
        let deleted_at = Time(ts(0));
        assert!(!gate_pending(&[], Some(&deleted_at), 60));
    }

    #[test]
    fn discovery_confirmation_pending_with_running_job_started_before_deletion() {
        let deleted_at = Time(ts(0));
        let job = discover_job(Some(-120), Some(-130));
        assert!(gate_pending(&[(&job, None)], Some(&deleted_at), 60));
    }

    #[test]
    fn discovery_confirmation_pending_with_job_started_after_deletion() {
        let deleted_at = Time(ts(0));
        let job = discover_job(Some(30), Some(20));
        assert!(!gate_pending(&[(&job, None)], Some(&deleted_at), 60));
    }

    #[test]
    fn discovery_confirmation_pending_without_deleted_at_is_false() {
        let job = discover_job(Some(-120), Some(-130));
        assert!(!gate_pending(&[(&job, None)], None, 60));
    }

    #[test]
    fn discovery_confirmation_pending_waits_for_completed_job_whose_snapshot_has_the_key() {
        let deleted_at = Time(ts(0));
        let job = completed_discover_job(-120);
        let snapshot = discover_snapshot(&["prod/other/manifest.json", DELETED_MANIFEST_KEY]);
        assert!(gate_pending(
            &[(&job, Some(snapshot))],
            Some(&deleted_at),
            60
        ));
    }

    #[test]
    fn discovery_confirmation_pending_ignores_completed_job_whose_snapshot_lacks_the_key() {
        let deleted_at = Time(ts(0));
        let job = completed_discover_job(-120);
        let snapshot = discover_snapshot(&["prod/other/manifest.json"]);
        assert!(!gate_pending(
            &[(&job, Some(snapshot))],
            Some(&deleted_at),
            60
        ));
    }

    #[test]
    fn discovery_confirmation_pending_waits_when_completed_job_result_is_unreadable() {
        let deleted_at = Time(ts(0));
        let job = completed_discover_job(-120);
        assert!(gate_pending(&[(&job, None)], Some(&deleted_at), 60));
    }

    #[test]
    fn table_test_discover_job_may_reference() {
        let deleted_at = Time(ts(0));
        let running_before = discover_job(Some(-120), None);
        let running_after = discover_job(Some(120), None);
        let completed_before = completed_discover_job(-120);
        let created_only_before = discover_job(None, Some(-120));
        let without_timestamps = discover_job(None, None);
        let key_snapshot = discover_snapshot(&[DELETED_MANIFEST_KEY]);
        let other_snapshot = discover_snapshot(&["prod/other/manifest.json"]);

        let cases = [
            (
                &running_before,
                None,
                true,
                "running Job has no snapshot yet",
            ),
            (
                &running_after,
                None,
                false,
                "Job started after the deletion sees the objects gone",
            ),
            (
                &completed_before,
                Some(&key_snapshot),
                true,
                "completed snapshot still lists the key",
            ),
            (
                &completed_before,
                Some(&other_snapshot),
                false,
                "completed snapshot demonstrably omits the key",
            ),
            (
                &completed_before,
                None,
                true,
                "unreadable result of a completed Job is treated as conflicting",
            ),
            (
                &created_only_before,
                None,
                true,
                "creationTimestamp is used when status.start_time is missing",
            ),
            (
                &without_timestamps,
                None,
                false,
                "a Job with no timestamp cannot be placed before the deletion",
            ),
        ];
        for (job, result, expected, reason) in cases {
            assert_eq!(
                discover_job_may_reference(job, result, &deleted_at, DELETED_MANIFEST_KEY),
                expected,
                "{reason}: job {:?}",
                job.status
                    .as_ref()
                    .map(|s| (s.start_time.as_ref(), s.succeeded))
            );
        }
    }

    #[test]
    fn discovery_confirmation_pending_bounds_wait_with_grace() {
        let deleted_at = Time(ts(0));
        let running = discover_job(Some(-4000), Some(-4000));
        let completed_with_key = completed_discover_job(-4000);
        let snapshot = discover_snapshot(&[DELETED_MANIFEST_KEY]);
        let jobs = [(&running, None), (&completed_with_key, Some(snapshot))];
        assert!(gate_pending(&jobs, Some(&deleted_at), 100));
        assert!(!gate_pending(&jobs, Some(&deleted_at), 4000));
    }

    #[test]
    fn discovery_confirmation_pending_falls_back_to_creation_timestamp() {
        let deleted_at = Time(ts(0));
        let created_only = discover_job(None, Some(-120));
        assert!(gate_pending(
            &[(&created_only, None)],
            Some(&deleted_at),
            60
        ));
        let without_timestamps = discover_job(None, None);
        assert!(!gate_pending(
            &[(&without_timestamps, None)],
            Some(&deleted_at),
            60
        ));
    }

    #[test]
    fn conflicting_discover_job_selects_the_stale_job() {
        let deleted_at = Time(ts(0));
        let mut stale = discover_job(Some(-120), None);
        stale.metadata.name = Some("stale-discover".to_string());
        let mut consumed = completed_discover_job(-120);
        consumed.metadata.name = Some("consumed-discover".to_string());
        let snapshot = discover_snapshot(&["prod/other/manifest.json"]);
        let jobs = [(&consumed, Some(snapshot)), (&stale, None)];
        let found = conflicting_discover_job(&jobs, &deleted_at, DELETED_MANIFEST_KEY)
            .map(|job| job.name_any());
        assert_eq!(found, Some("stale-discover".to_string()));
    }

    #[test]
    fn discovery_confirmation_grace_outlives_job_ttl() {
        assert!(
            DISCOVERY_CONFIRMATION_GRACE > Duration::from_secs(BACKUP_JOB_TTL_SECONDS as u64),
            "the grace must cover at least one discover Job lifetime before giving up"
        );
    }

    fn failure_document(code: &str) -> ResultDocument {
        ResultDocument::failure(
            "download",
            ExitCode::InvalidInput,
            code,
            "manifest object is gone",
        )
    }

    #[test]
    fn classify_validation_failure_manifest_not_found_is_absent() {
        let doc = failure_document(MANIFEST_NOT_FOUND);
        match classify_validation_failure(Some(&doc)) {
            ValidationFailure::ManifestAbsent { message } => {
                assert!(message.starts_with("MANIFEST_NOT_FOUND: "));
            }
            ValidationFailure::Other { message } => {
                panic!("expected ManifestAbsent, got Other with message {message}")
            }
        }
    }

    #[test]
    fn classify_validation_failure_non_success_document_with_manifest_not_found_is_absent() {
        let mut doc = ResultDocument::success("download");
        doc.exit_code = ExitCode::InvalidInput;
        doc.error = Some(kaniop_backup_core::result::ResultError {
            code: MANIFEST_NOT_FOUND.to_string(),
            message: "manifest object not found".to_string(),
        });
        assert!(matches!(
            classify_validation_failure(Some(&doc)),
            ValidationFailure::ManifestAbsent { .. }
        ));
    }

    #[test]
    fn classify_validation_failure_other_code_is_other() {
        let doc = failure_document("MANIFEST_FETCH_ERROR");
        match classify_validation_failure(Some(&doc)) {
            ValidationFailure::Other { message } => {
                assert!(message.starts_with("MANIFEST_FETCH_ERROR: "));
            }
            ValidationFailure::ManifestAbsent { message } => {
                panic!("expected Other, got ManifestAbsent with message {message}")
            }
        }
    }

    #[test]
    fn classify_validation_failure_document_without_error_is_other() {
        let doc = ResultDocument::success("download");
        match classify_validation_failure(Some(&doc)) {
            ValidationFailure::Other { message } => {
                assert_eq!(
                    message,
                    "Manifest validation Job failed; manifest is unreachable or invalid"
                );
            }
            ValidationFailure::ManifestAbsent { message } => {
                panic!("expected Other, got ManifestAbsent with message {message}")
            }
        }
    }

    #[test]
    fn classify_validation_failure_without_document_is_other() {
        assert!(matches!(
            classify_validation_failure(None),
            ValidationFailure::Other { .. }
        ));
    }

    fn backup_with_discovered_label(value: Option<&str>) -> KanidmBackup {
        let mut backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "prod/v1/tenants/ns/clusters/k/backups/019c7c76/manifest.json",
        );
        if let Some(value) = value {
            backup.metadata.labels = Some(
                [(DISCOVERED_LABEL.to_string(), value.to_string())]
                    .into_iter()
                    .collect(),
            );
        }
        backup
    }

    #[test]
    fn is_discovery_owned_true_label() {
        assert!(is_discovery_owned(&backup_with_discovered_label(Some(
            "true"
        ))));
    }

    #[test]
    fn is_discovery_owned_false_label() {
        assert!(!is_discovery_owned(&backup_with_discovered_label(Some(
            "false"
        ))));
    }

    #[test]
    fn is_discovery_owned_unrelated_label_value() {
        assert!(!is_discovery_owned(&backup_with_discovered_label(Some(
            "yes"
        ))));
    }

    #[test]
    fn is_discovery_owned_without_labels() {
        assert!(!is_discovery_owned(&backup_with_discovered_label(None)));
    }
}
