use crate::controller::{
    BACKUP_JOB_TTL_SECONDS, RESULT_PATH, background_delete_params, build_data_mover_wrapper,
    data_mover_image, default_resource_requirements, extract_termination_message,
    hardened_pod_security_context, hardened_security_context, select_succeeded_pod,
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
use kaniop_backup_core::result::{ExitCode, ResultDocument};
use kaniop_operator::backoff_reconciler;
use kaniop_operator::controller::{ControllerId, State, check_api_queryable, error_policy};
use kaniop_operator::kanidm::crd::Kanidm;
use kaniop_operator::kanidm::restore::RESTORE_ANNOTATION;

use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kaniop_k8s_util::error::{Error, Result};
use kube::api::{ListParams, ObjectMeta, Patch, PatchParams};
use kube::client::Client;
use kube::runtime::controller::{self, Controller};
use kube::runtime::watcher::Config;
use kube::{Api, ResourceExt};
use tokio::time::Duration;
use tracing::{debug, info, warn};

pub const CONTROLLER_ID: ControllerId = "backup";
const VALIDATION_JOB_PREFIX: &str = "kaniop-backup-validate";
const DELETION_JOB_PREFIX: &str = "kaniop-backup-delete";
const BACKUP_FINALIZER: &str = "kanidmbackups.kaniop.rs/finalizer";
const REQUEUE_NORMAL: Duration = Duration::from_secs(300);
const REQUEUE_JOB_PENDING: Duration = Duration::from_secs(10);
const REQUEUE_DELETION: Duration = Duration::from_secs(30);

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
            let failure_message =
                match read_job_result(&ctx.client, namespace, &job, "validate").await {
                    Ok(Some(result)) => result
                        .error
                        .as_ref()
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "Manifest validation Job failed".to_string()),
                    _ => "Manifest validation Job failed; manifest is unreachable or invalid"
                        .to_string(),
                };

            let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
            job_api
                .delete(&job.name_any(), &background_delete_params())
                .await
                .ok();

            status.phase = KanidmBackupPhase::Invalid;
            let condition = Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                observed_generation: obj.metadata.generation,
                last_transition_time: Time(Timestamp::now()),
                reason: "ValidationFailed".to_string(),
                message: format!("Manifest validation failed: {failure_message}"),
            };
            status.conditions.retain(|c| c.type_ != "Ready");
            status.conditions.push(condition);
            patch_backup_status(ctx, namespace, name, status).await?;
            return Ok((
                kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                false,
            ));
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
                    status.consistency = Some("kanidm-offline".to_string());
                    status.reason = Some("validated".to_string());
                    status.size_bytes = result.payload_size_bytes;
                    status.payload_sha256 = result.payload_sha256;
                    status.created_at = None;

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

                    status.phase = KanidmBackupPhase::Invalid;
                    let condition = Condition {
                        type_: "Ready".to_string(),
                        status: "False".to_string(),
                        observed_generation: obj.metadata.generation,
                        last_transition_time: Time(Timestamp::now()),
                        reason: "ManifestInvalid".to_string(),
                        message: format!("Manifest validation failed: {error_msg}"),
                    };
                    status.conditions.retain(|c| c.type_ != "Ready");
                    status.conditions.push(condition);

                    let job_api: Api<Job> = Api::namespaced(ctx.client.clone(), namespace);
                    job_api
                        .delete(&job.name_any(), &background_delete_params())
                        .await
                        .ok();

                    patch_backup_status(ctx, namespace, name, status).await?;
                    return Ok((
                        kube::runtime::controller::Action::requeue(REQUEUE_NORMAL),
                        false,
                    ));
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
            let failure_message =
                match read_job_result(&ctx.client, namespace, &job, "delete").await {
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
}
