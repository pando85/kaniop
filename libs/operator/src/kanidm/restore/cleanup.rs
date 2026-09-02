use super::*;

pub(super) const CONDITION_REPLICA_CLEANUP_BLOCKED: &str = "ReplicaCleanupBlocked";
pub(super) const SECONDARY_PVC_DELETION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub(super) struct PvcCleanupBlocker {
    pvc_name: String,
    finalizers: Vec<String>,
    pod_references: Vec<String>,
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
    for group in &target.spec.replica_groups {
        let statefulset_name = target.statefulset_name(&group.name);
        for ordinal in 0..group.replicas {
            if group.primary_node && ordinal == 0 {
                continue;
            }
            let name = format!("{DATA_VOLUME}-{statefulset_name}-{ordinal}");
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
            Duration::from_secs(1),
        ));
    }
}
