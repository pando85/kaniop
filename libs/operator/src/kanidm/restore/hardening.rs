use super::*;

use crate::kanidm::crd::KanidmReplicaState;
use crate::kanidm::reconcile::secret::SecretExt;
use crate::kanidm::reconcile::statefulset::KANIDM_CONFIG_PATH;
use kaniop_k8s_util::client::get_output;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use kube::api::AttachParams;
use regex::Regex;

pub(super) const CONDITION_REPLICA_CLEANUP_BLOCKED: &str = "ReplicaCleanupBlocked";
pub(super) const SECONDARY_PVC_DELETION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const RESTORE_ROLLOUT_ANNOTATION: &str = "kanidm.kaniop.rs/restore-recovery";
const ADMIN_USER: &str = "admin";
const IDM_ADMIN_USER: &str = "idm_admin";
const ADMIN_PASSWORD_KEY: &str = "ADMIN_PASSWORD";
const IDM_ADMIN_PASSWORD_KEY: &str = "IDM_ADMIN_PASSWORD";

static PASSWORD_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"new_password:\s*\"([^\"]+)\""#).expect("password regex must be valid")
});
static CERT_REGEX_V1_9: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"certificate:\s*\"([^\"]+)\""#).expect("certificate regex must be valid")
});
static CERT_REGEX_V1_10: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"certificate=([A-Za-z0-9_+/=-]+)"#)
        .expect("certificate regex must be valid")
});

#[derive(Debug, Clone)]
pub(super) struct PvcCleanupBlocker {
    pub(super) pvc_name: String,
    pub(super) finalizers: Vec<String>,
    pub(super) pod_references: Vec<String>,
}

#[derive(Debug)]
pub(super) enum SecondaryPvcCleanup {
    Complete,
    Progressing,
    Blocked(Vec<PvcCleanupBlocker>),
}

impl PvcCleanupBlocker {
    fn message(&self) -> String {
        let finalizers = if self.finalizers.is_empty() {
            "none".to_string()
        } else {
            self.finalizers.join(",")
        };
        let pod_references = if self.pod_references.is_empty() {
            "none".to_string()
        } else {
            self.pod_references.join(",")
        };
        format!(
            "secondary PVC '{}' is terminating; finalizers=[{}]; referencedBy=[{}]",
            self.pvc_name, finalizers, pod_references
        )
    }
}

pub(super) fn cleanup_blocker_message(blockers: &[PvcCleanupBlocker]) -> String {
    blockers
        .iter()
        .map(PvcCleanupBlocker::message)
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn phase_timed_out(
    status: &KanidmRestoreStatus,
    phase: KanidmRestorePhase,
    timeout: Duration,
) -> bool {
    let Some(start) = status.phase_timestamps.get(&format!("{phase:?}")) else {
        return false;
    };
    let Ok(start) = start.parse::<Timestamp>() else {
        return false;
    };
    let elapsed = Timestamp::now().as_second() - start.as_second();
    elapsed >= timeout.as_secs() as i64
}

fn pods_referencing_pvc(pods: &[Pod], pvc_name: &str) -> Vec<String> {
    pods.iter()
        .filter(|pod| {
            pod.spec.as_ref().is_some_and(|spec| {
                spec.volumes.as_ref().is_some_and(|volumes| {
                    volumes.iter().any(|volume| {
                        volume
                            .persistent_volume_claim
                            .as_ref()
                            .is_some_and(|claim| claim.claim_name == pvc_name)
                    })
                })
            })
        })
        .map(|pod| {
            let phase = pod
                .status
                .as_ref()
                .and_then(|status| status.phase.as_deref())
                .unwrap_or("Unknown");
            let owner = pod
                .owner_references()
                .iter()
                .find(|owner| owner.controller == Some(true))
                .or_else(|| pod.owner_references().first())
                .map(|owner| format!("{}/{}", owner.kind, owner.name))
                .unwrap_or_else(|| "unowned".to_string());
            format!("{}(phase={phase},owner={owner})", pod.name_any())
        })
        .collect()
}

pub(super) async fn cleanup_secondary_pvcs(
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<SecondaryPvcCleanup> {
    let ns = target.namespace().unwrap();
    let pvc_api = Api::<PersistentVolumeClaim>::namespaced(ctx.client.clone(), &ns);
    let pods = Api::<Pod>::namespaced(ctx.client.clone(), &ns)
        .list(&ListParams::default())
        .await
        .map_err(|error| Error::kube_error("list", "Pod", &ns, "*", error))?;

    let mut any_present = false;
    let mut blockers = Vec::new();
    for rg in &target.spec.replica_groups {
        let sts = target.statefulset_name(&rg.name);
        for ordinal in 0..rg.replicas {
            if rg.primary_node && ordinal == 0 {
                continue;
            }
            let name = format!("{DATA_VOLUME}-{sts}-{ordinal}");
            let Some(pvc) = pvc_api
                .get_opt(&name)
                .await
                .map_err(|error| {
                    Error::kube_error("get", "PersistentVolumeClaim", &ns, &name, error)
                })?
            else {
                continue;
            };
            any_present = true;

            if pvc.metadata.deletion_timestamp.is_none() {
                match pvc_api.delete(&name, &DeleteParams::default()).await {
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
                continue;
            }

            let finalizers = pvc.metadata.finalizers.clone().unwrap_or_default();
            let pod_references = pods_referencing_pvc(&pods.items, &name);
            if !finalizers.is_empty() || !pod_references.is_empty() {
                blockers.push(PvcCleanupBlocker {
                    pvc_name: name,
                    finalizers,
                    pod_references,
                });
            }
        }
    }

    if !blockers.is_empty() {
        Ok(SecondaryPvcCleanup::Blocked(blockers))
    } else if any_present {
        Ok(SecondaryPvcCleanup::Progressing)
    } else {
        Ok(SecondaryPvcCleanup::Complete)
    }
}

pub(super) async fn publish_cleanup_blocked(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    message: &str,
) {
    if let Err(error) = ctx
        .recorder
        .publish(
            &Event {
                type_: EventType::Warning,
                reason: "ReplicaCleanupBlocked".to_string(),
                note: Some(message.to_string()),
                action: "RebuildReplicas".to_string(),
                secondary: None,
            },
            &restore.object_ref(&()),
        )
        .await
    {
        warn!(restore = %restore.name_any(), %error, "failed to publish replica cleanup blocker event");
    }
}

pub(super) async fn source_check_failure_message(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
    job_name: &str,
) -> Option<String> {
    let ns = restore.namespace()?;
    let pods = Api::<Pod>::namespaced(ctx.client.clone(), &ns)
        .list(&ListParams::default().labels(&format!("job-name={job_name}")))
        .await
        .ok()?;
    let pod = pods.items.first()?;
    let message = pod
        .status
        .as_ref()?
        .container_statuses
        .as_ref()?
        .iter()
        .find(|container| container.name == "source-check")?
        .state
        .as_ref()?
        .terminated
        .as_ref()?
        .message
        .as_deref()?;
    let result = parse_result_document(message).ok()?;
    if result.operation != "check" || result.success {
        return None;
    }
    result.error.map(|error| error.message)
}

#[derive(Debug)]
pub(super) enum ResumeTopology {
    Waiting(String),
    Ready,
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

fn primary_pod_name(target: &Kanidm) -> Result<String> {
    Ok(format!(
        "{}-0",
        target.statefulset_name(&primary_group(target)?.name)
    ))
}

async fn all_statefulsets_ready(target: &Kanidm, ctx: &RestoreContext, final_rollout: bool) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    for rg in &target.spec.replica_groups {
        let name = target.statefulset_name(&rg.name);
        let sts = api
            .get(&name)
            .await
            .map_err(|error| Error::kube_error("get", "StatefulSet", &ns, &name, error))?;
        let spec_replicas = sts.spec.as_ref().and_then(|spec| spec.replicas).unwrap_or(0);
        let Some(status) = sts.status.as_ref() else {
            return Ok(false);
        };
        if spec_replicas != rg.replicas || status.ready_replicas.unwrap_or(0) != rg.replicas {
            return Ok(false);
        }
        if final_rollout
            && (status.updated_replicas.unwrap_or(0) != rg.replicas
                || status.current_revision != status.update_revision)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn ensure_admin_secret(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
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
            annotations: target.spec.service.as_ref().and_then(|service| service.annotations.clone()),
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

async fn ensure_replica_secrets(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    if !target.is_replication_enabled() {
        return Ok(false);
    }
    let ns = target.namespace().unwrap();
    let api = Api::<Secret>::namespaced(ctx.client.clone(), &ns);
    let mut changed = false;
    for rg in &target.spec.replica_groups {
        let sts_name = target.statefulset_name(&rg.name);
        for ordinal in 0..rg.replicas {
            let pod_name = format!("{sts_name}-{ordinal}");
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
    for rg in &target.spec.replica_groups {
        let name = target.statefulset_name(&rg.name);
        let sts = api
            .get(&name)
            .await
            .map_err(|error| Error::kube_error("get", "StatefulSet", &ns, &name, error))?;
        let current = sts
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
                            "annotations": { RESTORE_ROLLOUT_ANNOTATION: uid }
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

fn target_status_ready(target: &Kanidm) -> bool {
    let desired: i32 = target.spec.replica_groups.iter().map(|group| group.replicas).sum();
    let Some(status) = target.status.as_ref() else {
        return false;
    };
    if status.available_replicas != desired
        || status.replicas != desired
        || status.unavailable_replicas != 0
    {
        return false;
    }
    if target.is_replication_enabled()
        && (status.replica_statuses.len() != desired as usize
            || status
                .replica_statuses
                .iter()
                .any(|replica| replica.state != KanidmReplicaState::Ready))
    {
        return false;
    }
    true
}

pub(super) async fn resume_topology(
    restore: &KanidmRestore,
    target: &Kanidm,
    ctx: &RestoreContext,
) -> Result<ResumeTopology> {
    scale_desired(target, ctx).await?;
    if !all_statefulsets_ready(target, ctx, false).await? {
        return Ok(ResumeTopology::Waiting(
            "waiting for desired replica topology to start".to_string(),
        ));
    }

    if ensure_admin_secret(target, ctx).await? {
        return Ok(ResumeTopology::Waiting(
            "admin credentials regenerated from restored primary".to_string(),
        ));
    }
    if ensure_replica_secrets(target, ctx).await? {
        return Ok(ResumeTopology::Waiting(
            "replica certificates regenerated from restored replicas".to_string(),
        ));
    }
    if ensure_restore_rollout(restore, target, ctx).await? {
        return Ok(ResumeTopology::Waiting(
            "restarting restored topology with regenerated credentials".to_string(),
        ));
    }
    if !all_statefulsets_ready(target, ctx, true).await? || !target_status_ready(target) {
        return Ok(ResumeTopology::Waiting(
            "waiting for restored topology and replica certificates to become ready".to_string(),
        ));
    }

    Ok(ResumeTopology::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pvc_blocker_message_is_actionable() {
        let blocker = PvcCleanupBlocker {
            pvc_name: "kanidm-data-idm-default-1".to_string(),
            finalizers: vec!["kubernetes.io/pvc-protection".to_string()],
            pod_references: vec!["holder-abc(phase=Succeeded,owner=Job/holder)".to_string()],
        };
        let message = cleanup_blocker_message(&[blocker]);
        assert!(message.contains("kanidm-data-idm-default-1"));
        assert!(message.contains("kubernetes.io/pvc-protection"));
        assert!(message.contains("Job/holder"));
    }

    #[test]
    fn phase_timeout_uses_persisted_timestamp() {
        let mut status = KanidmRestoreStatus::default();
        status.phase_timestamps.insert(
            "RebuildingReplicas".to_string(),
            "2000-01-01T00:00:00Z".to_string(),
        );
        assert!(phase_timed_out(
            &status,
            KanidmRestorePhase::RebuildingReplicas,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn password_parser_matches_kanidmd_output() {
        let output = r#"new_password: \"secret-password\""#;
        assert_eq!(extract_password(output).unwrap(), "secret-password");
    }

    #[test]
    fn cert_parser_supports_current_output() {
        let output = "certificate=MIIB_test-certificate=";
        assert_eq!(extract_cert(output).unwrap(), "MIIB_test-certificate=");
    }
}
