from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        raise RuntimeError(f"missing hardening anchor: {label}")
    return text.replace(old, new, 1)


# Reject backup configurations that cannot persist a logical backup, and require
# the single authoritative primary that owns Kanidm's online backup scheduler.
crd_path = Path("libs/operator/src/kanidm/crd.rs")
crd = crd_path.read_text()
crd = replace_once(
    crd,
    '''#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
// workaround: '`' character is not allowed in the kube `doc` attribute during doctests
''',
    '''#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, Default)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(extend("x-kubernetes-validations" = [
        {
            "message": "backup requires PVC-backed storage",
            "rule": "!has(self.backup) || (has(self.storage) && has(self.storage.volumeClaimTemplate) && !has(self.storage.emptyDir) && !has(self.storage.ephemeral))"
        },
        {
            "message": "backup requires exactly one primary replica group",
            "rule": "!has(self.backup) || self.replicaGroups.filter(r, r.primaryNode == true).size() == 1"
        }
    ]))
)]
// workaround: '`' character is not allowed in the kube `doc` attribute during doctests
''',
    "Kanidm backup admission validations",
)
crd_path.write_text(crd)


path = Path("libs/operator/src/kanidm/restore.rs")
text = path.read_text()

# Reuse Kaniop's existing Kubernetes exec output handling and add the storage API
# type used to prove that all Kanidm PVs are detached before offline mutation.
text = replace_once(
    text,
    '''use kaniop_k8s_util::error::{Error, Result};

use std::sync::Arc;
''',
    '''use kaniop_k8s_util::client::get_output;
use kaniop_k8s_util::error::{Error, Result};

use std::collections::BTreeSet;
use std::sync::Arc;
''',
    "restore preflight imports",
)
text = replace_once(
    text,
    '''use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, EnvVar, PersistentVolumeClaim,
    PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use kube::api::{DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams};
''',
    '''use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, EnvVar, PersistentVolumeClaim,
    PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use k8s_openapi::api::storage::v1::VolumeAttachment;
use kube::api::{
    AttachParams, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams,
};
''',
    "VolumeAttachment and exec imports",
)

# Persist the exact fail-closed boundary before any restore Job can mutate the DB.
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

# Do not enter offline mutation until both the pods and CSI attachments are gone.
text = replace_once(
    text,
    '''            if target_pods_stopped(&target, &ctx).await? {
                set_phase(
                    &restore,
                    &ctx,
                    KanidmRestorePhase::RestoringPrimary,
                    None,
                )
                .await?;
            }
''',
    '''            if target_pods_stopped(&target, &ctx).await?
                && target_volumes_detached(&target, &ctx).await?
            {
                set_phase(
                    &restore,
                    &ctx,
                    KanidmRestorePhase::RestoringPrimary,
                    None,
                )
                .await?;
            }
''',
    "wait for volume detach before restore",
)

# Stale replica PVC deletion is asynchronous. Never mark it complete until every
# secondary claim is actually absent, otherwise a restarted StatefulSet can race
# a still-terminating old volume.
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

# Before the mutation boundary a restore can be cancelled safely. If it already
# quiesced Kanidm, put desired replica counts back before releasing the write gate.
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
            scale_desired(&target, &ctx).await?;
        }
        clear_restoring(&restore, &target, &ctx).await?;
    }
    Ok(Action::await_change())
}
''',
    "fail closed only after persisted mutation boundary",
)

# Validation must prove that the selected local backup exists while the live
# primary is still running. The shell receives the path as a positional argument,
# so the user-controlled basename is never interpolated into shell syntax.
text = replace_once(
    text,
    '''    if primaries != 1 {
        return Err(Error::MissingData("backup/restore requires exactly one primary replica group".to_string()));
    }

    let ns = restore.namespace().unwrap();
''',
    '''    if primaries != 1 {
        return Err(Error::MissingData("backup/restore requires exactly one primary replica group".to_string()));
    }
    validate_backup_source(restore, &target, ctx).await?;

    let ns = restore.namespace().unwrap();
''',
    "validate backup source before downtime",
)
text = replace_once(
    text,
    '''fn mutable_image(image: &str) -> bool {
    image == "kanidm/server:latest" || image.ends_with(":latest") || (!image.contains('@') && !image.rsplit('/').next().is_some_and(|part| part.contains(':')))
}

async fn get_target(restore: &KanidmRestore, ctx: &RestoreContext) -> Result<Kanidm> {
''',
    '''fn mutable_image(image: &str) -> bool {
    image == "kanidm/server:latest" || image.ends_with(":latest") || (!image.contains('@') && !image.rsplit('/').next().is_some_and(|part| part.contains(':')))
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
                "test -f \"$1\"".to_string(),
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
''',
    "backup source preflight helper",
)

# Enumerate bound PVs from the target claims and require zero VolumeAttachments.
# This is stronger than waiting for pods alone and avoids racing CSI detach/RWO state.
text = replace_once(
    text,
    '''async fn primary_ready(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
''',
    '''async fn target_volumes_detached(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
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
        .ok_or_else(|| Error::MissingData(format!("primary PVC {ns}/{primary_name} has no bound PV")))?;
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
''',
    "VolumeAttachment detach helper",
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


# Restore needs cluster-scoped read access to CSI attachment state.
rbac_path = Path("charts/kaniop/templates/clusterrole.yaml")
rbac = rbac_path.read_text()
rbac = replace_once(
    rbac,
    '''  - apiGroups:
      - batch
    resources:
      - jobs
    verbs:
      - '*'
''',
    '''  - apiGroups:
      - storage.k8s.io
    resources:
      - volumeattachments
    verbs:
      - get
      - list
      - watch
  - apiGroups:
      - batch
    resources:
      - jobs
    verbs:
      - '*'
''',
    "VolumeAttachment RBAC",
)
rbac_path.write_text(rbac)
