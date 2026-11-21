use super::*;
use super::hardening::{
    CONDITION_REPLICA_CLEANUP_BLOCKED, SECONDARY_PVC_DELETION_TIMEOUT,
    SecondaryPvcCleanup, cleanup_blocker_message, cleanup_secondary_pvcs,
    phase_timed_out, publish_cleanup_blocked,
};

use crate::kanidm::reconcile::secret::{REPLICA_SECRET_KEY, SecretExt};
use crate::kanidm::reconcile::statefulset::KANIDM_CONFIG_PATH;
use kaniop_k8s_util::client::get_output;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use kube::api::AttachParams;
use regex::Regex;

const RESTORE_ROLLOUT_ANNOTATION: &str = "kanidm.kaniop.rs/restore-recovery";
pub(super) const FORCE_RELEASE_ANNOTATION: &str = "restore.kaniop.rs/force-release";
const ADMIN_USER: &str = "admin";
const IDM_ADMIN_USER: &str = "idm_admin";
const ADMIN_PASSWORD_KEY: &str = "ADMIN_PASSWORD";
const IDM_ADMIN_PASSWORD_KEY: &str = "IDM_ADMIN_PASSWORD";

static PASSWORD_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"new_password:\s*"([^"]+)""#).expect("password regex must be valid")
});
static CERT_REGEX_V1_9: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"certificate:\s*"([^"]+)""#).expect("certificate regex must be valid")
});
static CERT_REGEX_V1_10: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"certificate=([A-Za-z0-9_+/=-]+)"#)
        .expect("certificate regex must be valid")
});

pub(crate) async fn run(client: Client) {
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

fn error_policy(
    restore: Arc<KanidmRestore>,
    error: &Error,
    _ctx: Arc<RestoreContext>,
) -> Action {
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
    record_restore_start_timestamp(&restore, &ctx.metrics);
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
    .or_else(|error| match error {
        kube::runtime::finalizer::Error::RemoveFinalizer(kube::Error::Api(ae)) if ae.code == 404 => {
            debug!("KanidmRestore already removed during finalizer cleanup");
            Ok(Action::requeue(Duration::from_secs(1)))
        }
        _ => Err(Error::FinalizerError(
            "failed on KanidmRestore finalizer".to_string(),
            Box::new(error),
        )),
    })
}

async fn ensure_current_phase_timestamp(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
) -> Result<bool> {
    let status = restore.status.clone().unwrap_or_default();
    let key = format!("{:?}", status.phase);
    if status.phase_timestamps.contains_key(&key) {
        return Ok(false);
    }
    let ns = restore.namespace().unwrap();
    let phase_timestamps = BTreeMap::from([(key, Timestamp::now().to_string())]);
    Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .patch_status(
            &restore.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"status": {"phaseTimestamps": phase_timestamps}})),
        )
        .await
        .map_err(|error| {
            Error::kube_error(
                "patch phase timestamp",
                "KanidmRestore",
                &ns,
                restore.name_any(),
                error,
            )
        })?;
    Ok(true)
}

async fn reconcile_apply(
    restore: Arc<KanidmRestore>,
    ctx: Arc<RestoreContext>,
) -> Result<Action> {
    if ensure_current_phase_timestamp(&restore, &ctx).await? {
        return Ok(Action::requeue(REQUEUE));
    }

    let phase = restore.status.as_ref().map(|s| s.phase).unwrap_or_default();
    let result = match phase {
        KanidmRestorePhase::PreparingSource if !is_remote_source(&restore) => {
            reconcile_local_source(&restore, &ctx).await
        }
        KanidmRestorePhase::RebuildingReplicas => {
            reconcile_rebuilding_replicas(&restore, &ctx).await
        }
        KanidmRestorePhase::Resuming => reconcile_resuming(&restore, &ctx).await,
        _ => super::reconcile_apply(restore.clone(), ctx.clone()).await,
    };
    match result {
        Err(error)
            if dependency_gone_before_mutation(&restore, &error)
                && !phase_job_running(&restore, &ctx.client).await? =>
        {
            // The target is quiesced and a repository, backup or Job this phase needs was
            // deleted: retrying cannot succeed, so resume the target instead of holding it
            // at zero replicas until someone deletes the restore. A Job of this phase that is
            // still running may hold the PVC, so wait for it to end (its deadline bounds this).
            resume_before_mutation(&restore, &ctx).await?;
            set_phase(
                &restore,
                &ctx,
                KanidmRestorePhase::Failed,
                Some(format!(
                    "{error}: a restore dependency no longer exists; target restored to original state"
                )),
            )
            .await?;
            Ok(Action::requeue(REQUEUE))
        }
        result => result,
    }
}

async fn phase_job_running(restore: &KanidmRestore, client: &Client) -> Result<bool> {
    let phase = restore.status.as_ref().map(|s| s.phase).unwrap_or_default();
    let name = match phase {
        KanidmRestorePhase::SafetyBackup => safety_backup_job_name(restore),
        KanidmRestorePhase::PreparingSource if is_remote_source(restore) => {
            source_prep_job_name(restore)
        }
        KanidmRestorePhase::PreparingSource => source_check_job_name(restore),
        _ => return Ok(false),
    };
    let ns = restore.namespace().unwrap();
    let job = Api::<Job>::namespaced(client.clone(), &ns)
        .get_opt(&name)
        .await
        .map_err(|e| Error::kube_error("get", "Job", &ns, &name, e))?;
    if job.is_some_and(|job| job_state_from(&job) == JobState::Running) {
        return Ok(true);
    }
    // A deleted or finished Job can still have a terminating pod that mounts the PVC.
    let pods = Api::<Pod>::namespaced(client.clone(), &ns)
        .list(&ListParams::default().labels(&format!("job-name={name}")))
        .await
        .map_err(|e| Error::kube_error("list", "Pod", &ns, &name, e))?;
    Ok(pods.items.iter().any(|pod| {
        !matches!(
            pod.status.as_ref().and_then(|status| status.phase.as_deref()),
            Some("Succeeded" | "Failed")
        )
    }))
}

fn dependency_gone_before_mutation(restore: &KanidmRestore, error: &Error) -> bool {
    let status = restore.status.clone().unwrap_or_default();
    let quiesced_before_mutation = matches!(
        status.phase,
        KanidmRestorePhase::SafetyBackup | KanidmRestorePhase::PreparingSource
    ) && !status.database_mutation_started;
    let not_found = matches!(
        error,
        Error::KubeError(_, source) if matches!(&**source, kube::Error::Api(response) if response.code == 404)
    );
    quiesced_before_mutation && not_found
}

#[derive(Debug, PartialEq)]
enum CleanupDecision {
    Release,
    ContinueReconcile,
    RetainLock(String),
}

fn decide_cleanup(restore: &KanidmRestore) -> CleanupDecision {
    let status = restore.status.clone().unwrap_or_default();
    if status.database_mutation_started
        && status.phase != KanidmRestorePhase::Completed
        && status.phase != KanidmRestorePhase::Failed
    {
        return CleanupDecision::ContinueReconcile;
    }
    if status.database_mutation_started && status.phase == KanidmRestorePhase::Failed {
        let has_force_release = restore
            .metadata
            .annotations
            .as_ref()
            .is_some_and(|a| a.contains_key(FORCE_RELEASE_ANNOTATION));
        if !has_force_release {
            return CleanupDecision::RetainLock(
                "post-mutation restore deleted without force-release annotation; \
                target lock retained for manual intervention"
                    .to_string(),
            );
        }
    }
    CleanupDecision::Release
}

async fn cleanup(
    restore: Arc<KanidmRestore>,
    ctx: Arc<RestoreContext>,
) -> Result<Action> {
    match decide_cleanup(&restore) {
        CleanupDecision::ContinueReconcile => {
            reconcile_apply(restore.clone(), ctx.clone()).await?;
            Ok(Action::requeue(REQUEUE))
        }
        CleanupDecision::RetainLock(message) => {
            let already_reported = restore.status.as_ref().is_some_and(|status| {
                status.message.as_deref() == Some(&message)
            });
            if !already_reported {
                if let Err(error) = ctx
                    .recorder
                    .publish(
                        &Event {
                            type_: EventType::Warning,
                            reason: "LockRetained".to_string(),
                            note: Some(message.clone()),
                            action: "Cleanup".to_string(),
                            secondary: None,
                        },
                        &restore.object_ref(&()),
                    )
                    .await
                {
                    warn!(restore = %restore.name_any(), %error, "failed to publish lock-retained event");
                }
                set_phase(&restore, &ctx, KanidmRestorePhase::Failed, Some(message.clone())).await?;
            }
            Err(Error::MissingData(message))
        }
        CleanupDecision::Release => {
            let status = restore.status.clone().unwrap_or_default();
            if status.database_mutation_started && status.phase == KanidmRestorePhase::Failed {
                warn!(
                    restore = %restore.name_any(),
                    "force-release annotation present; clearing target lock after post-mutation failure"
                );
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
    }
}

async fn reconcile_local_source(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
) -> Result<Action> {
    let target = get_target(restore, ctx).await?;
    let name = source_check_job_name(restore);
    ensure_local_source_operation(restore, &target, ctx).await?;
    ensure_source_check_job(restore, &target, ctx, &name).await?;

    match job_state(restore, ctx, &name).await? {
        JobState::Complete => {
            let mut status = restore.status.clone().unwrap_or_default();
            status.source_prep_job_name = Some(name);
            status.phase = KanidmRestorePhase::RestoringPrimary;
            status.message = None;
            patch_status(restore, ctx, status).await?;
        }
        JobState::Failed => {
            resume_before_mutation(restore, ctx).await?;
            set_phase(
                restore,
                ctx,
                KanidmRestorePhase::Failed,
                Some(format!(
                    "backup file check failed: local backup preflight failed for target domain '{}'; target restored to original state",
                    target.spec.domain
                )),
            )
            .await?;
        }
        JobState::Running => {}
    }
    Ok(Action::requeue(REQUEUE))
}

async fn ensure_local_source_operation(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<()> {
    let ns = restore.namespace().unwrap();
    let file_name = restore
        .spec
        .source
        .local
        .as_ref()
        .map(|source| source.file_name.clone())
        .unwrap_or_default();
    let operation_doc = json!({
        "apiVersion": "backup.kaniop.rs/v1alpha1",
        "kind": "OperationDocument",
        "operation": "check",
        "path": format!("{BACKUP_PATH}/{file_name}"),
        "resultPath": "/run/kaniop-result/result.json",
        "format": "kanidmJsonGzip",
        "expectedDomain": target.spec.domain,
    })
    .to_string();
    let operation_cm_name = format!("{}-source-check-op", restore.name_any());
    ensure_operation_configmap(
        restore,
        &operation_cm_name,
        &operation_doc,
        &ns,
        ctx,
    )
    .await
}

async fn patch_cleanup_blocker(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    message: &str,
) -> Result<()> {
    let mut status = restore.status.clone().unwrap_or_default();
    status.message = Some(message.to_string());
    status.observed_generation = restore.metadata.generation;
    update_restore_conditions(&mut status, restore.metadata.generation);
    status
        .conditions
        .retain(|condition| condition.type_ != CONDITION_REPLICA_CLEANUP_BLOCKED);
    let previous = restore
        .status
        .as_ref()
        .map(|current| current.conditions.as_slice())
        .unwrap_or(&[]);
    status.conditions.push(restore_condition(
        previous,
        CONDITION_REPLICA_CLEANUP_BLOCKED,
        CONDITION_TRUE,
        "PVCDeletionBlocked",
        message,
        restore.metadata.generation,
    ));

    let ns = restore.namespace().unwrap();
    Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .patch_status(
            &restore.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"status": status})),
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            Error::kube_error(
                "patch replica cleanup blocker",
                "KanidmRestore",
                &ns,
                restore.name_any(),
                error,
            )
        })
}

async fn reconcile_rebuilding_replicas(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
) -> Result<Action> {
    let target = get_target(restore, ctx).await?;
    let mut status = restore.status.clone().unwrap_or_default();

    if !status.replicas_cleared {
        match cleanup_secondary_pvcs(&target, ctx).await? {
            SecondaryPvcCleanup::Complete => {
                status.replicas_cleared = true;
                status.message = Some("secondary database state cleared".to_string());
                patch_status(restore, ctx, status).await?;
            }
            SecondaryPvcCleanup::Progressing => {}
            SecondaryPvcCleanup::Blocked(blockers) => {
                let blocker_message = cleanup_blocker_message(&blockers);
                if phase_timed_out(
                    &status,
                    KanidmRestorePhase::RebuildingReplicas,
                    SECONDARY_PVC_DELETION_TIMEOUT,
                ) {
                    fail_after_mutation(
                        restore,
                        ctx,
                        &format!(
                            "timed out waiting for secondary PVC deletion: {blocker_message}"
                        ),
                    )
                    .await?;
                    return Ok(Action::requeue(Duration::from_secs(3600)));
                }

                let already_reported = status.conditions.iter().any(|condition| {
                    condition.type_ == CONDITION_REPLICA_CLEANUP_BLOCKED
                        && condition.status == CONDITION_TRUE
                        && condition.message == blocker_message
                });
                if !already_reported {
                    patch_cleanup_blocker(restore, ctx, &blocker_message).await?;
                    publish_cleanup_blocked(restore, ctx, &blocker_message).await;
                }
            }
        }
        return Ok(Action::requeue(REQUEUE));
    }

    scale_primary(&target, ctx, 1).await?;
    if !primary_ready(&target, ctx).await? {
        return Ok(Action::requeue(REQUEUE));
    }

    if !status.certificates_cleared {
        let replica_certs_absent = delete_replica_cert_secrets(&target, ctx).await?;
        let admin_secret_absent = delete_admin_secret(&target, ctx).await?;
        if !replica_certs_absent || !admin_secret_absent {
            return Ok(Action::requeue(REQUEUE));
        }
        status.certificates_cleared = true;
        status.message =
            Some("replica certificates and admin secret cleared for regeneration".to_string());
        patch_status(restore, ctx, status).await?;
        return Ok(Action::requeue(REQUEUE));
    }

    status.phase = KanidmRestorePhase::Resuming;
    status.message = Some("rebuilding original replica topology".to_string());
    patch_status(restore, ctx, status).await?;
    Ok(Action::requeue(REQUEUE))
}

fn replication_enabled(target: &Kanidm) -> bool {
    target.spec.replica_groups.len() > 1
        || target
            .spec
            .replica_groups
            .iter()
            .any(|group| group.replicas > 1)
        || !target.spec.external_replication_nodes.is_empty()
}

fn primary_pod_name(target: &Kanidm) -> Result<String> {
    Ok(format!(
        "{}-0",
        target.statefulset_name(&primary_group(target)?.name)
    ))
}

async fn exec_kanidm(
    target: &Kanidm,
    ctx: &RestoreContext,
    pod_name: &str,
    command: Vec<String>,
) -> Result<String> {
    let ns = target.namespace().unwrap();
    let attached = Api::<Pod>::namespaced(ctx.client.clone(), &ns)
        .exec(
            pod_name,
            command,
            &AttachParams::default().container("kanidm"),
        )
        .await
        .map_err(|error| Error::kube_error("exec", "Pod", &ns, pod_name, error))?;
    get_output(attached)
        .await
        .map_err(|error| Error::KubeExecError(format!("exec failed in pod {pod_name}: {error}")))
}

fn extract_password(output: &str) -> Result<String> {
    PASSWORD_REGEX
        .captures(output)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
        .ok_or_else(|| Error::ReceiveOutput("recovered password was not found".to_string()))
}

fn extract_cert(output: &str) -> Result<String> {
    CERT_REGEX_V1_9
        .captures(output)
        .and_then(|captures| captures.get(1))
        .or_else(|| {
            CERT_REGEX_V1_10
                .captures(output)
                .and_then(|captures| captures.get(1))
        })
        .map(|value| value.as_str().to_string())
        .ok_or_else(|| Error::ReceiveOutput("replica certificate was not found".to_string()))
}

async fn all_statefulsets_ready(
    target: &Kanidm,
    ctx: &RestoreContext,
    final_rollout: bool,
) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    for group in &target.spec.replica_groups {
        let name = target.statefulset_name(&group.name);
        let statefulset = api
            .get(&name)
            .await
            .map_err(|error| Error::kube_error("get", "StatefulSet", &ns, &name, error))?;
        if statefulset
            .spec
            .as_ref()
            .and_then(|spec| spec.replicas)
            .unwrap_or(0)
            != group.replicas
        {
            return Ok(false);
        }
        let Some(status) = statefulset.status.as_ref() else {
            return Ok(false);
        };
        if status.ready_replicas.unwrap_or(0) != group.replicas {
            return Ok(false);
        }
        if final_rollout
            && (status.updated_replicas.unwrap_or(0) != group.replicas
                || status.current_revision != status.update_revision)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn ensure_admin_secret(
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<Secret>::namespaced(ctx.client.clone(), &ns);
    let name = target.admins_secret_name();
    if api
        .get_opt(&name)
        .await
        .map_err(|error| Error::kube_error("get", "Secret", &ns, &name, error))?
        .is_some()
    {
        return Ok(false);
    }

    let pod_name = primary_pod_name(target)?;
    let admin_password = extract_password(
        &exec_kanidm(
            target,
            ctx,
            &pod_name,
            vec![
                "kanidmd".to_string(),
                "recover-account".to_string(),
                ADMIN_USER.to_string(),
            ],
        )
        .await?,
    )?;
    let idm_admin_password = extract_password(
        &exec_kanidm(
            target,
            ctx,
            &pod_name,
            vec![
                "kanidmd".to_string(),
                "recover-account".to_string(),
                IDM_ADMIN_USER.to_string(),
            ],
        )
        .await?,
    )?;

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(ns.clone()),
            owner_references: target.controller_owner_ref(&()).map(|owner| vec![owner]),
            annotations: target
                .spec
                .service
                .as_ref()
                .and_then(|service| service.annotations.clone()),
            labels: Some(BTreeMap::from([
                (CLUSTER_LABEL.to_string(), target.name_any()),
                (
                    SECRET_TYPE_LABEL.to_string(),
                    serde_plain::to_string(&SecretType::AdminPasswords).unwrap(),
                ),
            ])),
            ..Default::default()
        },
        string_data: Some(BTreeMap::from([
            ("ADMIN_USERNAME".to_string(), ADMIN_USER.to_string()),
            (ADMIN_PASSWORD_KEY.to_string(), admin_password),
            ("IDM_ADMIN_USERNAME".to_string(), IDM_ADMIN_USER.to_string()),
            (IDM_ADMIN_PASSWORD_KEY.to_string(), idm_admin_password),
        ])),
        ..Default::default()
    };
    match api.create(&PostParams::default(), &secret).await {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(status)) if status.code == 409 => Ok(false),
        Err(error) => Err(Error::kube_error("create", "Secret", &ns, &name, error)),
    }
}

async fn ensure_replica_secrets(
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<bool> {
    if !replication_enabled(target) {
        return Ok(false);
    }
    let ns = target.namespace().unwrap();
    let api = Api::<Secret>::namespaced(ctx.client.clone(), &ns);
    let mut changed = false;
    for group in &target.spec.replica_groups {
        let statefulset_name = target.statefulset_name(&group.name);
        for ordinal in 0..group.replicas {
            let pod_name = format!("{statefulset_name}-{ordinal}");
            let secret_name = target.replica_secret_name(&pod_name);
            if api
                .get_opt(&secret_name)
                .await
                .map_err(|error| Error::kube_error("get", "Secret", &ns, &secret_name, error))?
                .is_some()
            {
                continue;
            }
            let output = exec_kanidm(
                target,
                ctx,
                &pod_name,
                vec![
                    "kanidmd".to_string(),
                    "show-replication-certificate".to_string(),
                    "-c".to_string(),
                    KANIDM_CONFIG_PATH.to_string(),
                ],
            )
            .await?;
            let secret = target.build_replica_secret(extract_cert(&output)?, &pod_name);
            match api.create(&PostParams::default(), &secret).await {
                Ok(_) => changed = true,
                Err(kube::Error::Api(status)) if status.code == 409 => {}
                Err(error) => {
                    return Err(Error::kube_error(
                        "create",
                        "Secret",
                        &ns,
                        &secret_name,
                        error,
                    ));
                }
            }
        }
    }
    Ok(changed)
}

async fn ensure_restore_rollout(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let uid = restore
        .uid()
        .ok_or_else(|| Error::MissingData("restore has no UID".to_string()))?;
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    let mut changed = false;
    for group in &target.spec.replica_groups {
        let name = target.statefulset_name(&group.name);
        let statefulset = api
            .get(&name)
            .await
            .map_err(|error| Error::kube_error("get", "StatefulSet", &ns, &name, error))?;
        let current = statefulset
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|metadata| metadata.annotations.as_ref())
            .and_then(|annotations| annotations.get(RESTORE_ROLLOUT_ANNOTATION));
        if current == Some(&uid) {
            continue;
        }
        api.patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "spec": {
                    "template": {
                        "metadata": {
                            "annotations": {
                                RESTORE_ROLLOUT_ANNOTATION: uid
                            }
                        }
                    }
                }
            })),
        )
        .await
        .map_err(|error| Error::kube_error("patch", "StatefulSet", &ns, &name, error))?;
        changed = true;
    }
    Ok(changed)
}

async fn restore_secrets_ready(
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<Secret>::namespaced(ctx.client.clone(), &ns);
    let Some(admin) = api
        .get_opt(&target.admins_secret_name())
        .await
        .map_err(|error| {
            Error::kube_error(
                "get",
                "Secret",
                &ns,
                target.admins_secret_name(),
                error,
            )
        })?
    else {
        return Ok(false);
    };
    let has_admin_passwords = admin.data.as_ref().is_some_and(|data| {
        data.contains_key(ADMIN_PASSWORD_KEY) && data.contains_key(IDM_ADMIN_PASSWORD_KEY)
    });
    if !has_admin_passwords {
        return Ok(false);
    }

    if replication_enabled(target) {
        for group in &target.spec.replica_groups {
            let statefulset_name = target.statefulset_name(&group.name);
            for ordinal in 0..group.replicas {
                let pod_name = format!("{statefulset_name}-{ordinal}");
                let secret_name = target.replica_secret_name(&pod_name);
                let Some(secret) = api
                    .get_opt(&secret_name)
                    .await
                    .map_err(|error| {
                        Error::kube_error("get", "Secret", &ns, &secret_name, error)
                    })?
                else {
                    return Ok(false);
                };
                if !secret
                    .data
                    .as_ref()
                    .is_some_and(|data| data.contains_key(REPLICA_SECRET_KEY))
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

async fn reconcile_resuming(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
) -> Result<Action> {
    let target = get_target(restore, ctx).await?;

    scale_desired(&target, ctx).await?;
    if !all_statefulsets_ready(&target, ctx, false).await? {
        return Ok(Action::requeue(REQUEUE));
    }

    if ensure_admin_secret(&target, ctx).await? {
        return Ok(Action::requeue(REQUEUE));
    }
    if ensure_replica_secrets(&target, ctx).await? {
        return Ok(Action::requeue(REQUEUE));
    }
    if ensure_restore_rollout(restore, &target, ctx).await? {
        return Ok(Action::requeue(REQUEUE));
    }
    if !all_statefulsets_ready(&target, ctx, true).await?
        || !restore_secrets_ready(&target, ctx).await?
    {
        return Ok(Action::requeue(REQUEUE));
    }

    let refreshed_target = get_target(restore, ctx).await?;
    clear_restoring(restore, &refreshed_target, ctx).await?;
    set_phase(
        restore,
        ctx,
        KanidmRestorePhase::Completed,
        Some("original replica topology restored and ready".to_string()),
    )
    .await?;
    Ok(Action::requeue(Duration::from_secs(3600)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::api::ObjectMeta as ApiObjectMeta;
    use std::collections::BTreeMap;

    fn make_restore_for_cleanup(
        phase: KanidmRestorePhase,
        database_mutation_started: bool,
        annotations: Option<BTreeMap<String, String>>,
    ) -> KanidmRestore {
        KanidmRestore {
            metadata: ApiObjectMeta {
                name: Some("test-restore".to_string()),
                namespace: Some("default".to_string()),
                uid: Some("test-uid-abc".to_string()),
                annotations,
                ..Default::default()
            },
            spec: KanidmRestoreSpec {
                target_ref: KanidmRestoreTargetRef {
                    name: "corp-idm".to_string(),
                    uid: "test-uid-123".to_string(),
                },
                source: KanidmRestoreSource {
                    local: Some(super::super::KanidmRestoreLocalSource {
                        file_name: "backup.json".to_string(),
                    }),
                    backup_ref: None,
                },
                restore_image: "kanidm/server@sha256:abc".to_string(),
                safety_backup: None,
            },
            status: Some(KanidmRestoreStatus {
                phase,
                database_mutation_started,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn password_parser_supports_kanidmd_output() {
        assert_eq!(
            extract_password(r#"new_password: "secret-password""#).unwrap(),
            "secret-password"
        );
    }

    #[test]
    fn certificate_parser_supports_current_output() {
        assert_eq!(
            extract_cert("certificate=MIIB_test-certificate=").unwrap(),
            "MIIB_test-certificate="
        );
    }

    fn not_found_error() -> Error {
        Error::kube_error(
            "get",
            "KanidmBackupRepository",
            "default",
            "safety",
            kube::Error::Api(
                kube::core::Status::failure("not found", "NotFound")
                    .with_code(404)
                    .boxed(),
            ),
        )
    }

    #[test]
    fn deleted_dependency_fails_quiesced_restore_before_mutation() {
        for phase in [
            KanidmRestorePhase::SafetyBackup,
            KanidmRestorePhase::PreparingSource,
        ] {
            let restore = make_restore_for_cleanup(phase, false, None);
            assert!(dependency_gone_before_mutation(&restore, &not_found_error()));
        }
    }

    #[test]
    fn deleted_dependency_keeps_retrying_outside_pre_mutation_phases() {
        // After the mutation boundary the restore stays fail-closed.
        let mutated = make_restore_for_cleanup(KanidmRestorePhase::PreparingSource, true, None);
        assert!(!dependency_gone_before_mutation(&mutated, &not_found_error()));
        let restoring = make_restore_for_cleanup(KanidmRestorePhase::RestoringPrimary, true, None);
        assert!(!dependency_gone_before_mutation(&restoring, &not_found_error()));
        // Other pre-mutation phases are handled where they run: Validating turns missing
        // objects into Failed, and Quiescing has nothing that could be deleted except the target.
        let quiescing = make_restore_for_cleanup(KanidmRestorePhase::Quiescing, false, None);
        assert!(!dependency_gone_before_mutation(&quiescing, &not_found_error()));
        // Transient errors are retried.
        let safety = make_restore_for_cleanup(KanidmRestorePhase::SafetyBackup, false, None);
        let unavailable = Error::kube_error(
            "get",
            "KanidmBackupRepository",
            "default",
            "safety",
            kube::Error::Api(
                kube::core::Status::failure("unavailable", "ServiceUnavailable")
                    .with_code(503)
                    .boxed(),
            ),
        );
        assert!(!dependency_gone_before_mutation(&safety, &unavailable));
    }

    type MockHandle = tower_test::mock::Handle<
        http::Request<kube::client::Body>,
        http::Response<kube::client::Body>,
    >;

    const SAFETY_JOB_PATH: &str = "/apis/batch/v1/namespaces/default/jobs/test-restore-safety-backup";
    const PODS_PATH: &str = "/api/v1/namespaces/default/pods";

    async fn respond(handle: &mut MockHandle, path: &str, status: u16, body: serde_json::Value) {
        let (request, send) = handle.next_request().await.expect("service not called");
        assert_eq!(request.uri().path(), path);
        send.send_response(
            http::Response::builder()
                .status(status)
                .body(kube::client::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        );
    }

    fn job_not_found() -> serde_json::Value {
        serde_json::json!({
            "kind": "Status",
            "apiVersion": "v1",
            "metadata": {},
            "status": "Failure",
            "reason": "NotFound",
            "code": 404
        })
    }

    fn pod_list(phases: &[&str]) -> serde_json::Value {
        let items: Vec<_> = phases
            .iter()
            .enumerate()
            .map(|(index, phase)| {
                serde_json::json!({
                    "metadata": {"name": format!("pod-{index}"), "namespace": "default"},
                    "status": {"phase": phase}
                })
            })
            .collect();
        serde_json::json!({
            "apiVersion": "v1",
            "kind": "PodList",
            "metadata": {},
            "items": items
        })
    }

    async fn safety_phase_job_running(job: serde_json::Value, pods: serde_json::Value) -> bool {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::SafetyBackup, false, None);
        assert_eq!(safety_backup_job_name(&restore), "test-restore-safety-backup");
        let (service, mut handle) = tower_test::mock::pair();
        let client = Client::new(service, "default");
        let server = tokio::spawn(async move {
            let job_status = if job["kind"] == "Status" { 404 } else { 200 };
            respond(&mut handle, SAFETY_JOB_PATH, job_status, job).await;
            respond(&mut handle, PODS_PATH, 200, pods).await;
        });
        let running = phase_job_running(&restore, &client).await.unwrap();
        server.abort();
        running
    }

    #[tokio::test]
    async fn active_phase_job_blocks_resume() {
        let job = serde_json::json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {"name": "test-restore-safety", "namespace": "default"},
            "status": {"active": 1}
        });
        assert!(safety_phase_job_running(job, pod_list(&[])).await);
    }

    #[tokio::test]
    async fn terminating_pod_of_deleted_job_blocks_resume() {
        assert!(safety_phase_job_running(job_not_found(), pod_list(&["Running"])).await);
    }

    #[tokio::test]
    async fn deleted_job_with_finished_pods_allows_resume() {
        assert!(!safety_phase_job_running(job_not_found(), pod_list(&["Failed", "Succeeded"])).await);
    }

    #[test]
    fn force_release_annotation_matches_contract() {
        assert_eq!(FORCE_RELEASE_ANNOTATION, "restore.kaniop.rs/force-release");
    }

    #[test]
    fn cleanup_post_mutation_nonterminal_returns_continue_reconcile() {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::RestoringPrimary, true, None);
        assert_eq!(decide_cleanup(&restore), CleanupDecision::ContinueReconcile);
    }

    #[test]
    fn cleanup_post_mutation_verifying_returns_continue_reconcile() {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::Verifying, true, None);
        assert_eq!(decide_cleanup(&restore), CleanupDecision::ContinueReconcile);
    }

    #[test]
    fn cleanup_post_mutation_failed_without_force_release_returns_retain_lock() {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::Failed, true, None);
        let decision = decide_cleanup(&restore);
        assert!(matches!(decision, CleanupDecision::RetainLock(_)));
    }

    #[test]
    fn cleanup_post_mutation_failed_with_force_release_returns_release() {
        let annotations = BTreeMap::from([(FORCE_RELEASE_ANNOTATION.to_string(), "true".to_string())]);
        let restore = make_restore_for_cleanup(KanidmRestorePhase::Failed, true, Some(annotations));
        assert_eq!(decide_cleanup(&restore), CleanupDecision::Release);
    }

    #[test]
    fn cleanup_pre_mutation_returns_release() {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::Quiescing, false, None);
        assert_eq!(decide_cleanup(&restore), CleanupDecision::Release);
    }

    #[test]
    fn cleanup_completed_returns_release() {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::Completed, true, None);
        assert_eq!(decide_cleanup(&restore), CleanupDecision::Release);
    }

    #[test]
    fn cleanup_pending_pre_mutation_returns_release() {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::Pending, false, None);
        assert_eq!(decide_cleanup(&restore), CleanupDecision::Release);
    }

    #[test]
    fn cleanup_rebuilding_replicas_post_mutation_returns_continue_reconcile() {
        let restore = make_restore_for_cleanup(KanidmRestorePhase::RebuildingReplicas, true, None);
        assert_eq!(decide_cleanup(&restore), CleanupDecision::ContinueReconcile);
    }
}
