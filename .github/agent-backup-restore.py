from pathlib import Path


def read(path):
    return Path(path).read_text()


def write(path, content):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text(content)


def replace_once(path, old, new):
    text = read(path)
    if text.count(old) != 1:
        raise RuntimeError(f"expected exactly one match in {path}: {old[:100]!r}, got {text.count(old)}")
    write(path, text.replace(old, new, 1))


# Kanidm backup API.
path = "libs/operator/src/kanidm/crd.rs"
replace_once(
    path,
    '    /// StorageSpec defines the configured storage for a group Kanidm servers.\n',
    '''    /// Configures Kanidm-native online logical backups.\n    ///\n    /// Backups are written to `/data/backups` on the single replica group marked as the\n    /// primary node. Local backup configuration requires persistent PVC-backed storage.\n    #[serde(skip_serializing_if = "Option::is_none")]\n    pub backup: Option<KanidmBackupSpec>,\n\n    /// StorageSpec defines the configured storage for a group Kanidm servers.\n''',
)
replace_once(
    path,
    '#[derive(Serialize, Deserialize, Clone, Debug, Default)]\n#[cfg_attr(feature = "schemars", derive(JsonSchema))]\n#[serde(rename_all = "camelCase")]\npub struct KanidmStorage {',
    '''#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]\n#[cfg_attr(feature = "schemars", derive(JsonSchema))]\n#[serde(rename_all = "camelCase")]\npub struct KanidmBackupSpec {\n    /// Cron expression passed to Kanidm's native online backup scheduler.\n    #[schemars(extend("x-kubernetes-validations" = [{"message": "backup.schedule cannot be empty", "rule": "self.size() > 0"}]))]\n    pub schedule: String,\n\n    /// Number of completed local backups retained by Kanidm.\n    #[serde(default = "default_backup_versions")]\n    #[schemars(extend("x-kubernetes-validations" = [{"message": "backup.versions must be greater than zero", "rule": "self > 0"}]))]\n    pub versions: u32,\n}\n\nimpl Default for KanidmBackupSpec {\n    fn default() -> Self {\n        Self {\n            schedule: "0 2 * * *".to_string(),\n            versions: default_backup_versions(),\n        }\n    }\n}\n\nfn default_backup_versions() -> u32 {\n    7\n}\n\n#[derive(Serialize, Deserialize, Clone, Debug, Default)]\n#[cfg_attr(feature = "schemars", derive(JsonSchema))]\n#[serde(rename_all = "camelCase")]\npub struct KanidmStorage {''',
)

# Generated server.toml is required for replication or backup. Backups are enabled only on the
# pod selected by KANIDM_PRIMARY_NODE.
path = "libs/operator/src/kanidm/reconcile/statefulset.rs"
replace_once(
    path,
    'pub(super) const KANIDM_CONFIG_PATH: &str = "/run/kanidm/server.toml";\n',
    'pub(super) const KANIDM_CONFIG_PATH: &str = "/run/kanidm/server.toml";\npub const KANIDM_BACKUP_PATH: &str = "/data/backups";\n',
)
replace_once(
    path,
    '      version = "2"\n\n      {% set pod_env = env.POD_NAME | upper | replace(\'-\', \'_\') -%}\n      [replication]\n',
    '''      version = "2"\n\n      {% if env.KANIOP_BACKUP_ENABLED == "true" and env.POD_NAME == env.KANIDM_PRIMARY_NODE -%}\n      [online_backup]\n      path = "/data/backups"\n      schedule = "{{ env.KANIOP_BACKUP_SCHEDULE }}"\n      versions = {{ env.KANIOP_BACKUP_VERSIONS }}\n      {% endif -%}\n\n      {% if env.KANIOP_REPLICATION_ENABLED == "true" -%}\n      {% set pod_env = env.POD_NAME | upper | replace('-', '_') -%}\n      [replication]\n''',
)
replace_once(
    path,
    '      {%- endfor -%}\n    dest: "{{ env.KANIDM_CONFIG_PATH }}"\n',
    '      {%- endfor -%}\n      {% endif -%}\n    dest: "{{ env.KANIDM_CONFIG_PATH }}"\n',
)
replace_once(
    path,
    'impl Kanidm {\n    fn generate_pod_labels',
    '''impl Kanidm {\n    fn uses_generated_config(&self) -> bool {\n        self.is_replication_enabled() || self.spec.backup.is_some()\n    }\n\n    fn generate_pod_labels''',
)
replace_once(
    path,
    '                self.is_replication_enabled()\n                    .then(|| self.generate_config_volume_mount()),\n',
    '                self.uses_generated_config()\n                    .then(|| self.generate_config_volume_mount()),\n',
)
replace_once(
    path,
    '    fn generate_init_containers(&self, replica_group: &ReplicaGroup) -> Result<Vec<Container>> {\n        if self.is_replication_enabled() {\n',
    '    fn generate_init_containers(&self, replica_group: &ReplicaGroup) -> Result<Vec<Container>> {\n        if self.uses_generated_config() {\n',
)
replace_once(
    path,
    '                    EnvVar {\n                        name: "KANIDM_NAME".to_string(),\n                        value: Some(self.name_any()),\n                        ..EnvVar::default()\n                    },\n                ])\n',
    '''                    EnvVar {\n                        name: "KANIDM_NAME".to_string(),\n                        value: Some(self.name_any()),\n                        ..EnvVar::default()\n                    },\n                    EnvVar {\n                        name: "KANIOP_REPLICATION_ENABLED".to_string(),\n                        value: Some(self.is_replication_enabled().to_string()),\n                        ..EnvVar::default()\n                    },\n                    EnvVar {\n                        name: "KANIOP_BACKUP_ENABLED".to_string(),\n                        value: Some(self.spec.backup.is_some().to_string()),\n                        ..EnvVar::default()\n                    },\n                ])\n                .chain(self.spec.backup.as_ref().into_iter().flat_map(|backup| {\n                    [\n                        EnvVar {\n                            name: "KANIOP_BACKUP_SCHEDULE".to_string(),\n                            value: Some(backup.schedule.clone()),\n                            ..EnvVar::default()\n                        },\n                        EnvVar {\n                            name: "KANIOP_BACKUP_VERSIONS".to_string(),\n                            value: Some(backup.versions.to_string()),\n                            ..EnvVar::default()\n                        },\n                    ]\n                }))\n''',
)
replace_once(
    path,
    '                self.is_replication_enabled()\n                    .then(|| vec!["-c".to_string(), KANIDM_CONFIG_PATH.to_string()])\n',
    '                self.uses_generated_config()\n                    .then(|| vec!["-c".to_string(), KANIDM_CONFIG_PATH.to_string()])\n',
)
replace_once(
    path,
    '                .chain(self.is_replication_enabled().then(|| Volume {\n',
    '                .chain(self.uses_generated_config().then(|| Volume {\n',
)

# Shared write gate used by every identity reconciler.
path = "libs/operator/src/controller/context.rs"
replace_once(
    path,
    '    /// Return [`Kanidm`] of the given object\n',
    '''    /// Return true when normal identity reconciliation may mutate the target Kanidm.\n    pub fn kanidm_write_allowed(&self, obj: &K) -> bool {\n        self.get_kanidm(obj).is_none_or(|kanidm| {\n            !kanidm\n                .annotations()\n                .contains_key(crate::kanidm::restore::RESTORE_ANNOTATION)\n        })\n    }\n\n    /// Return [`Kanidm`] of the given object\n''',
)

for path in [
    "libs/person/src/reconcile.rs",
    "libs/group/src/reconcile.rs",
    "libs/oauth2/src/reconcile/mod.rs",
    "libs/service-account/src/reconcile/mod.rs",
]:
    replace_once(
        path,
        '    let kanidm_client = ctx.get_idm_client(&',
        '''    if !ctx.kaniop_ctx.kanidm_write_allowed(&''',
    )
    # The previous replacement intentionally leaves the object name and suffix. Repair the first
    # occurrence into a gate followed by the original client acquisition using a regex-like split.
    text = read(path)
    marker = '    if !ctx.kaniop_ctx.kanidm_write_allowed(&'
    start = text.index(marker)
    end = text.index(').await?;\n', start)
    obj_expr = text[start + len(marker):end]
    replacement = (
        marker + obj_expr + ') {\n'
        '        debug!(msg = "Kanidm restore in progress, pausing identity writes");\n'
        '        return Ok((Action::requeue(Duration::from_secs(5)), false));\n'
        '    }\n'
        f'    let kanidm_client = ctx.get_idm_client(&{obj_expr}).await?;\n'
    )
    text = text[:start] + replacement + text[end + len(').await?;\n'):]
    write(path, text)

# Pause normal Kanidm reconciliation while the restore controller owns StatefulSet replica counts.
path = "libs/operator/src/kanidm/reconcile/mod.rs"
replace_once(
    path,
    'async fn reconcile(\n    kanidm: Arc<Kanidm>,\n    ctx: Arc<Context>,\n    status: KanidmStatus,\n) -> Result<(Action, bool)> {\n    let mut changed = false;\n',
    '''async fn reconcile(\n    kanidm: Arc<Kanidm>,\n    ctx: Arc<Context>,\n    status: KanidmStatus,\n) -> Result<(Action, bool)> {\n    if kanidm\n        .annotations()\n        .contains_key(crate::kanidm::restore::RESTORE_ANNOTATION)\n    {\n        ctx.kaniop_ctx.release_kanidm_clients(&kanidm).await;\n        return Ok((Action::requeue(Duration::from_secs(5)), false));\n    }\n\n    let mut changed = false;\n''',
)

# Register module and restore controller.
replace_once(
    "libs/operator/src/kanidm/mod.rs",
    'pub mod reconcile;\n',
    'pub mod reconcile;\npub mod restore;\n',
)

# CRD generator.
path = "cmd/crdgen/src/main.rs"
replace_once(path, 'use kaniop_operator::kanidm::crd::Kanidm;\n', 'use kaniop_operator::kanidm::crd::Kanidm;\nuse kaniop_operator::kanidm::restore::KanidmRestore;\n')
replace_once(path, '        Kanidm::crd(),\n        KanidmGroup::crd(),\n', '        Kanidm::crd(),\n        KanidmRestore::crd(),\n        KanidmGroup::crd(),\n')
# There are two generated CRD arrays (main + test).
text = read(path)
needle = '            Kanidm::crd(),\n            KanidmGroup::crd(),\n'
if needle in text:
    text = text.replace(needle, '            Kanidm::crd(),\n            KanidmRestore::crd(),\n            KanidmGroup::crd(),\n', 1)
write(path, text)

# Example Kanidm uses durable storage when backups are configured.
path = "cmd/examples/src/kanidm.rs"
replace_once(path, '        KanidmBackendTLSPolicy, KanidmBackendTLSPolicyValidation, KanidmGateway,\n', '        KanidmBackendTLSPolicy, KanidmBackendTLSPolicyValidation, KanidmBackupSpec, KanidmGateway,\n')
replace_once(
    path,
    '            storage: Some(KanidmStorage {\n                empty_dir: Some(Default::default()),\n                ephemeral: Some(Default::default()),\n',
    '            backup: Some(KanidmBackupSpec {\n                schedule: "0 2 * * *".to_string(),\n                versions: 7,\n            }),\n            storage: Some(KanidmStorage {\n                empty_dir: None,\n                ephemeral: None,\n',
)

# Restore example.
write(
    "cmd/examples/src/kanidm_restore.rs",
    '''use kaniop_operator::kanidm::restore::{\n    KanidmRestore, KanidmRestoreLocalSource, KanidmRestoreSource, KanidmRestoreSpec,\n    KanidmRestoreTargetRef,\n};\nuse kube::api::ObjectMeta;\n\npub fn example() -> KanidmRestore {\n    KanidmRestore {\n        metadata: ObjectMeta {\n            name: Some("my-idm-restore".to_string()),\n            namespace: Some("default".to_string()),\n            ..Default::default()\n        },\n        spec: KanidmRestoreSpec {\n            target_ref: KanidmRestoreTargetRef {\n                name: "my-idm".to_string(),\n                uid: "replace-with-kanidm-uid".to_string(),\n            },\n            source: KanidmRestoreSource {\n                local: KanidmRestoreLocalSource {\n                    file_name: "backup.json.gz".to_string(),\n                },\n            },\n            restore_image: "kanidm/server:1.10.0".to_string(),\n        },\n        status: None,\n    }\n}\n''',
)
path = "cmd/examples/src/main.rs"
replace_once(path, 'mod kanidm;\n', 'mod kanidm;\nmod kanidm_restore;\n')
replace_once(path, '    let person = person::example(&kanidm);\n', '    let restore = kanidm_restore::example();\n    let person = person::example(&kanidm);\n')
replace_once(
    path,
    '    let person_schema = schema_for!(kaniop_person::crd::KanidmPersonAccount);\n',
    '''    let restore_schema = schema_for!(kaniop_operator::kanidm::restore::KanidmRestore);\n    let restore_schema_json = serde_json::to_value(&restore_schema).unwrap();\n    write_to_file(&restore, &restore_schema_json, "examples/kanidm-restore.yaml").unwrap();\n\n    let person_schema = schema_for!(kaniop_person::crd::KanidmPersonAccount);\n''',
)

# Operator entrypoint: restore is an independent controller sharing the Kubernetes client.
path = "cmd/operator/src/main.rs"
replace_once(
    path,
    '        kaniop_operator::kanidm::controller::CONTROLLER_ID,\n',
    '        kaniop_operator::kanidm::controller::CONTROLLER_ID,\n        kaniop_operator::kanidm::restore::CONTROLLER_ID,\n',
)
# Add restore future in both leader and non-leader branches.
text = read(path)
text = text.replace(
    '        let group_c = kaniop_group::controller::run(state.clone(), client.clone());\n',
    '        let restore_c = kaniop_operator::kanidm::restore::run(client.clone());\n        let group_c = kaniop_group::controller::run(state.clone(), client.clone());\n',
)
text = text.replace(
    '            tokio::join!(kanidm_c, group_c, oauth2_c, person_c, service_account_c);\n',
    '            tokio::join!(kanidm_c, restore_c, group_c, oauth2_c, person_c, service_account_c);\n',
)
text = text.replace(
    '            kanidm_c,\n            group_c,\n',
    '            kanidm_c,\n            restore_c,\n            group_c,\n',
)
write(path, text)

# Restore API/controller implementation.
write(
    "libs/operator/src/kanidm/restore.rs",
    r'''use super::crd::Kanidm;
use super::reconcile::{CLUSTER_LABEL, statefulset::StatefulSetExt};

use kaniop_k8s_util::error::{Error, Result};

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, EnvVar, PersistentVolumeClaim,
    PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec, Volume, VolumeMount,
};
use kube::api::{DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::finalizer::{Error as FinalizerError, Event as Finalizer, finalizer};
use kube::runtime::watcher;
use kube::{Api, Client, CustomResource, Resource, ResourceExt};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone)]
struct RestoreContext {
    client: Client,
}

pub async fn run(client: Client) {
    let api = Api::<KanidmRestore>::all(client.clone());
    let ctx = Arc::new(RestoreContext { client });
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
    .map_err(|error| Error::FinalizerError("failed on KanidmRestore finalizer".to_string(), Box::new(error)))
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
            if target_pods_stopped(&target, &ctx).await? {
                set_phase(
                    &restore,
                    &ctx,
                    KanidmRestorePhase::RestoringPrimary,
                    None,
                )
                .await?;
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::RestoringPrimary => {
            let target = get_target(&restore, &ctx).await?;
            ensure_restore_config(&restore, &target, &ctx).await?;
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
                JobState::Failed => fail_after_mutation(&restore, &ctx, "database restore job failed").await?,
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
                JobState::Failed => fail_after_mutation(&restore, &ctx, "database verification failed").await?,
                JobState::Running => {}
            }
            Ok(Action::requeue(REQUEUE))
        }
        KanidmRestorePhase::RebuildingReplicas => {
            let target = get_target(&restore, &ctx).await?;
            let mut status = restore.status.clone().unwrap_or_default();
            if !status.replicas_cleared {
                delete_secondary_pvcs(&target, &ctx).await?;
                status.replicas_cleared = true;
                status.message = Some("secondary database state cleared".to_string());
                patch_status(&restore, &ctx, status).await?;
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

async fn validate(restore: &KanidmRestore, ctx: &RestoreContext) -> Result<Kanidm> {
    let target = get_target(restore, ctx).await?;
    let actual_uid = target.uid().ok_or_else(|| Error::MissingData("target Kanidm has no UID".to_string()))?;
    if actual_uid != restore.spec.target_ref.uid {
        return Err(Error::MissingData(format!(
            "target UID mismatch: expected {}, got {}",
            restore.spec.target_ref.uid, actual_uid
        )));
    }
    if !safe_basename(&restore.spec.source.local.file_name) {
        return Err(Error::MissingData("restore source fileName must be a safe basename".to_string()));
    }
    if target.spec.image != restore.spec.restore_image || mutable_image(&restore.spec.restore_image) {
        return Err(Error::MissingData(format!(
            "restoreImage must be the target's pinned Kanidm image (target image is {})",
            target.spec.image
        )));
    }
    let storage = target.spec.storage.as_ref().ok_or_else(|| Error::MissingData("restore requires persistent storage".to_string()))?;
    if storage.empty_dir.is_some() || storage.ephemeral.is_some() || storage.volume_claim_template.is_none() {
        return Err(Error::MissingData("restore requires PVC-backed Kanidm storage".to_string()));
    }
    let primaries = target.spec.replica_groups.iter().filter(|rg| rg.primary_node).count();
    if primaries != 1 {
        return Err(Error::MissingData("backup/restore requires exactly one primary replica group".to_string()));
    }

    let ns = restore.namespace().unwrap();
    let restores = Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .list(&ListParams::default())
        .await
        .map_err(|e| Error::kube_error("list", "KanidmRestore", &ns, "*", e))?;
    if restores.items.iter().any(|other| {
        other.name_any() != restore.name_any()
            && other.spec.target_ref.name == restore.spec.target_ref.name
            && !matches!(other.status.as_ref().map(|s| s.phase), Some(KanidmRestorePhase::Completed | KanidmRestorePhase::Failed))
    }) {
        return Err(Error::MissingData("another active restore targets this Kanidm".to_string()));
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
    image == "kanidm/server:latest" || image.ends_with(":latest") || (!image.contains('@') && !image.rsplit('/').next().is_some_and(|part| part.contains(':')))
}

async fn get_target(restore: &KanidmRestore, ctx: &RestoreContext) -> Result<Kanidm> {
    let ns = restore.namespace().ok_or_else(|| Error::MissingData("restore has no namespace".to_string()))?;
    Api::<Kanidm>::namespaced(ctx.client.clone(), &ns)
        .get(&restore.spec.target_ref.name)
        .await
        .map_err(|e| Error::kube_error("get", "Kanidm", &ns, &restore.spec.target_ref.name, e))
}

async fn mark_restoring(restore: &KanidmRestore, target: &Kanidm, ctx: &RestoreContext) -> Result<()> {
    let ns = target.namespace().unwrap();
    let uid = restore.uid().ok_or_else(|| Error::MissingData("restore has no UID".to_string()))?;
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

async fn clear_restoring(restore: &KanidmRestore, target: &Kanidm, ctx: &RestoreContext) -> Result<()> {
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

async fn patch_status(restore: &KanidmRestore, ctx: &RestoreContext, mut status: KanidmRestoreStatus) -> Result<()> {
    let ns = restore.namespace().unwrap();
    status.observed_generation = restore.metadata.generation;
    Api::<KanidmRestore>::namespaced(ctx.client.clone(), &ns)
        .patch_status(
            &restore.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"status": status})),
        )
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("patch status", "KanidmRestore", &ns, restore.name_any(), e))
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

async fn fail_after_mutation(restore: &KanidmRestore, ctx: &RestoreContext, message: &str) -> Result<()> {
    set_phase(restore, ctx, KanidmRestorePhase::Failed, Some(message.to_string())).await
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
        api.patch(&name, &PatchParams::default(), &Patch::Merge(json!({"spec":{"replicas":replicas}})))
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
        .patch(&name, &PatchParams::default(), &Patch::Merge(json!({"spec":{"replicas":replicas}})))
        .await
        .map(|_| ())
        .map_err(|e| Error::kube_error("scale", "StatefulSet", &ns, &name, e))
}

async fn scale_desired(target: &Kanidm, ctx: &RestoreContext) -> Result<()> {
    let ns = target.namespace().unwrap();
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    for rg in &target.spec.replica_groups {
        let name = target.statefulset_name(&rg.name);
        api.patch(&name, &PatchParams::default(), &Patch::Merge(json!({"spec":{"replicas":rg.replicas}})))
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

async fn primary_ready(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let name = target.statefulset_name(&primary_group(target)?.name);
    let sts = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns)
        .get(&name)
        .await
        .map_err(|e| Error::kube_error("get", "StatefulSet", &ns, &name, e))?;
    Ok(sts.status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0) >= 1)
}

async fn all_desired_ready(target: &Kanidm, ctx: &RestoreContext) -> Result<bool> {
    let ns = target.namespace().unwrap();
    let api = Api::<StatefulSet>::namespaced(ctx.client.clone(), &ns);
    for rg in &target.spec.replica_groups {
        let name = target.statefulset_name(&rg.name);
        let sts = api.get(&name).await.map_err(|e| Error::kube_error("get", "StatefulSet", &ns, &name, e))?;
        if sts.status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0) != rg.replicas {
            return Ok(false);
        }
    }
    Ok(true)
}

fn primary_pvc_name(target: &Kanidm) -> Result<String> {
    let rg = primary_group(target)?;
    Ok(format!("{DATA_VOLUME}-{}-0", target.statefulset_name(&rg.name)))
}

async fn delete_secondary_pvcs(target: &Kanidm, ctx: &RestoreContext) -> Result<()> {
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

fn config_map_name(restore: &KanidmRestore) -> String {
    format!("{}-config", restore.name_any())
}
fn restore_job_name(restore: &KanidmRestore) -> String {
    format!("{}-restore", restore.name_any())
}
fn verify_job_name(restore: &KanidmRestore) -> String {
    format!("{}-verify", restore.name_any())
}

async fn ensure_restore_config(restore: &KanidmRestore, target: &Kanidm, ctx: &RestoreContext) -> Result<()> {
    let ns = restore.namespace().unwrap();
    let api = Api::<ConfigMap>::namespaced(ctx.client.clone(), &ns);
    let name = config_map_name(restore);
    if api.get_opt(&name).await.map_err(|e| Error::kube_error("get", "ConfigMap", &ns, &name, e))?.is_some() {
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
    if api.get_opt(name).await.map_err(|e| Error::kube_error("get", "Job", &ns, name, e))?.is_some() {
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
        command.push(format!("{BACKUP_PATH}/{}", restore.spec.source.local.file_name));
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
                            EnvVar { name: "KANIDM_DB_PATH".to_string(), value: Some("/data/kanidm.db".to_string()), ..Default::default() },
                            EnvVar { name: "KANIDM_DOMAIN".to_string(), value: Some(target.spec.domain.clone()), ..Default::default() },
                        ]),
                        volume_mounts: Some(vec![
                            VolumeMount { name: DATA_VOLUME.to_string(), mount_path: DATA_PATH.to_string(), ..Default::default() },
                            VolumeMount { name: CONFIG_VOLUME.to_string(), mount_path: CONFIG_PATH.to_string(), read_only: Some(true), ..Default::default() },
                        ]),
                        ..Default::default()
                    }],
                    volumes: Some(vec![
                        Volume {
                            name: DATA_VOLUME.to_string(),
                            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource { claim_name: primary_pvc_name(target)?, read_only: Some(false) }),
                            ..Default::default()
                        },
                        Volume {
                            name: CONFIG_VOLUME.to_string(),
                            config_map: Some(ConfigMapVolumeSource { name: config_map_name(restore), ..Default::default() }),
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

enum JobState { Running, Complete, Failed }

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
    use super::{mutable_image, safe_basename};

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
''',
)

# Helm permissions needed by restore Jobs/PVC reset.
path = "charts/kaniop/templates/clusterrole.yaml"
replace_once(
    path,
    '    resources:\n      - configmaps\n    verbs:\n      - \'*\'\n',
    '''    resources:\n      - configmaps\n      - persistentvolumeclaims\n    verbs:\n      - '*'\n  - apiGroups:\n      - batch\n    resources:\n      - jobs\n    verbs:\n      - '*'\n''',
)

# User documentation.
write(
    "Documentation/src/usage/backup-restore.md",
    '''# Backup and Restore\n\nKaniop uses Kanidm's native logical backup format. The operator configures the online backup scheduler on exactly one primary node and stores local artifacts under `/data/backups` on the Kanidm PVC.\n\n```yaml\nspec:\n  backup:\n    schedule: "0 2 * * *"\n    versions: 7\n```\n\nLocal backups require PVC-backed storage and one `replicaGroup` with `primaryNode: true`. Kaniop intentionally does not claim PITR semantics or a globally atomic point-in-time cut across replicated writable nodes.\n\n## Restore\n\nRestore is an explicit destructive operation represented by `KanidmRestore`. Obtain the target UID with `kubectl get kanidm <name> -o jsonpath='{.metadata.uid}'`, select an existing backup basename from `/data/backups`, and use the same pinned Kanidm image as the target. `latest` and untagged images are rejected.\n\n```yaml\napiVersion: kaniop.rs/v1beta1\nkind: KanidmRestore\nmetadata:\n  name: my-idm-restore\nspec:\n  targetRef:\n    name: my-idm\n    uid: <kanidm-uid>\n  source:\n    local:\n      fileName: backup.json.gz\n  restoreImage: kanidm/server:1.10.0\n```\n\nThe controller validates the request, marks the target in maintenance, scales all Kanidm pods down, runs `kanidmd database restore`, verifies the database offline, discards stale secondary PVC data, starts the restored primary, rebuilds replicas through normal Kanidm replication, and only then resumes ordinary Kaniop reconciliation. A failure after database mutation is fail-closed: the restore remains `Failed` and the target remains marked as restoring.\n\nRestoring a historical database is followed by GitOps reconciliation. Declaratively managed Kaniop resources can therefore be recreated or changed after recovery.\n\n## S3 and infrastructure snapshots\n\nKaniop does not implement a separate S3 uploader. Native S3-compatible shipping remains deferred until Kanidm exposes its supported upstream interface. CSI/Velero snapshots can be used as an independent disaster-recovery layer, but they are not represented as equivalent to a Kanidm-native logical backup.\n''',
)

# Add unit coverage for backup config generation by inspecting generated init container settings.
path = "libs/operator/src/kanidm/reconcile/statefulset.rs"
text = read(path)
insert = r'''
    #[test]
    fn backup_enables_generated_config_and_native_online_backup_stanza() {
        use crate::kanidm::crd::{KanidmBackupSpec, KanidmStorage, PersistentVolumeClaimTemplate};
        use k8s_openapi::api::core::v1::PersistentVolumeClaimSpec;

        let (mut kanidm, mut replica_group) = create_kanidm_with_replica_group();
        replica_group.primary_node = true;
        kanidm.spec.replica_groups = vec![replica_group.clone()];
        kanidm.spec.backup = Some(KanidmBackupSpec {
            schedule: "0 2 * * *".to_string(),
            versions: 7,
        });
        kanidm.spec.storage = Some(KanidmStorage {
            volume_claim_template: Some(PersistentVolumeClaimTemplate {
                metadata: None,
                spec: Some(PersistentVolumeClaimSpec::default()),
            }),
            ..Default::default()
        });

        let sts = kanidm.create_statefulset(&replica_group, None).unwrap();
        let pod = sts.spec.unwrap().template.spec.unwrap();
        let init = pod
            .init_containers
            .unwrap()
            .into_iter()
            .find(|c| c.name == "kanidm-generate-replication-config")
            .unwrap();
        let env = init.env.unwrap();
        assert!(env.iter().any(|e| e.name == "KANIOP_BACKUP_ENABLED" && e.value.as_deref() == Some("true")));
        assert!(env.iter().any(|e| e.name == "KANIOP_BACKUP_SCHEDULE" && e.value.as_deref() == Some("0 2 * * *")));
        assert!(env.iter().any(|e| e.name == "KANIOP_BACKUP_VERSIONS" && e.value.as_deref() == Some("7")));
        let script = init.args.unwrap().join("\n");
        assert!(script.contains("[online_backup]"));
        assert!(script.contains("env.POD_NAME == env.KANIDM_PRIMARY_NODE"));
    }
'''
pos = text.rfind('\n}')
if pos == -1:
    raise RuntimeError("could not find end of statefulset test module")
text = text[:pos] + insert + text[pos:]
write(path, text)
