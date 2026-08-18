from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        raise RuntimeError(f"missing hardening anchor: {label}")
    return text.replace(old, new, 1)


path = Path("libs/operator/src/kanidm/restore.rs")
text = path.read_text()

text = replace_once(
    text,
    '''    #[serde(default)]
    pub replicas_cleared: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
''',
    '''    #[serde(default)]
    pub replicas_cleared: bool,
    /// Persisted before the restore Job may be created. Once true, finalizer cleanup
    /// fails closed until the restore has completed.
    #[serde(default)]
    pub database_mutation_started: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
''',
    "restore status mutation flag",
)

text = replace_once(
    text,
    '''        KanidmRestorePhase::RestoringPrimary => {
            let target = get_target(&restore, &ctx).await?;
            ensure_restore_config(&restore, &target, &ctx).await?;
            let name = restore_job_name(&restore);
            ensure_database_job(&restore, &target, &ctx, &name, false).await?;
''',
    '''        KanidmRestorePhase::RestoringPrimary => {
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
''',
    "persist mutation boundary before restore job",
)

text = replace_once(
    text,
    '''            if !status.replicas_cleared {
                delete_secondary_pvcs(&target, &ctx).await?;
                status.replicas_cleared = true;
                status.message = Some("secondary database state cleared".to_string());
                patch_status(&restore, &ctx, status).await?;
                return Ok(Action::requeue(REQUEUE));
            }
''',
    '''            if !status.replicas_cleared {
                if delete_secondary_pvcs(&target, &ctx).await? {
                    status.replicas_cleared = true;
                    status.message = Some("secondary database state cleared".to_string());
                    patch_status(&restore, &ctx, status).await?;
                }
                return Ok(Action::requeue(REQUEUE));
            }
''',
    "wait for secondary pvc deletion",
)

text = replace_once(
    text,
    '''async fn cleanup(restore: Arc<KanidmRestore>, ctx: Arc<RestoreContext>) -> Result<Action> {
    let phase = restore.status.as_ref().map(|s| s.phase).unwrap_or_default();
    if !matches!(phase, KanidmRestorePhase::Pending | KanidmRestorePhase::Validating | KanidmRestorePhase::Completed) {
        return Err(Error::MissingData(format!(
            "refusing to remove KanidmRestore while destructive restore is in phase {phase:?}; recover the target or remove the finalizer explicitly"
        )));
    }
    if let Ok(target) = get_target(&restore, &ctx).await {
        clear_restoring(&restore, &target, &ctx).await?;
    }
    Ok(Action::await_change())
}
''',
    '''async fn cleanup(restore: Arc<KanidmRestore>, ctx: Arc<RestoreContext>) -> Result<Action> {
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
            // A cancellation or validation/control-plane failure before the mutation
            // boundary is safe to roll back. Restore desired replica counts before
            // lifting the write gate so a quiesced target is not stranded at zero.
            scale_desired(&target, &ctx).await?;
        }
        clear_restoring(&restore, &target, &ctx).await?;
    }
    Ok(Action::await_change())
}
''',
    "fail closed only after persisted mutation boundary",
)

text = replace_once(
    text,
    '''async fn delete_secondary_pvcs(target: &Kanidm, ctx: &RestoreContext) -> Result<()> {
    let ns = target.namespace().unwrap();
    let api = Api::<PersistentVolumeClaim>::namespaced(ctx.client.clone(), &ns);
    for rg in &target.spec.replica_groups {
        let sts = target.statefulset_name(&rg.name);
        for ordinal in 0..rg.replicas {
            if rg.primary_node && ordinal == 0 {
                continue;
            }
            let name = format!("{DATA_VOLUME}-{sts}-{ordinal}");
            match api.delete(&name, &DeleteParams::default()).await {
                Ok(_) => debug!(pvc = %name, "deleted stale secondary PVC"),
                Err(kube::Error::Api(status)) if status.code == 404 => {}
                Err(error) => return Err(Error::kube_error("delete", "PersistentVolumeClaim", &ns, &name, error)),
            }
        }
    }
    Ok(())
}
''',
    '''async fn delete_secondary_pvcs(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
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
''',
    "idempotent secondary pvc deletion",
)

text = replace_once(
    text,
    '''mod tests {
    use super::{mutable_image, safe_basename};
''',
    '''mod tests {
    use super::{KanidmRestoreStatus, mutable_image, safe_basename};

    #[test]
    fn mutation_boundary_defaults_to_fail_open_before_restore_starts() {
        assert!(!KanidmRestoreStatus::default().database_mutation_started);
    }
''',
    "mutation boundary unit test",
)

path.write_text(text)
