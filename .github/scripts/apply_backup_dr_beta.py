#!/usr/bin/env python3
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]


def p(path: str) -> Path:
    return ROOT / path


def read(path: str) -> str:
    return p(path).read_text()


def write(path: str, text: str) -> None:
    p(path).write_text(text)


def replace(path: str, old: str, new: str, count: int | None = None) -> None:
    text = read(path)
    actual = text.count(old)
    if count is not None and actual != count:
        raise RuntimeError(f"{path}: expected {count} occurrences, found {actual}: {old[:80]!r}")
    if actual == 0:
        raise RuntimeError(f"{path}: replacement source not found: {old[:80]!r}")
    write(path, text.replace(old, new))


def regex(path: str, pattern: str, repl: str, count: int = 0) -> None:
    text = read(path)
    out, n = re.subn(pattern, repl, text, count=count, flags=re.MULTILINE | re.DOTALL)
    if n == 0:
        raise RuntimeError(f"{path}: regex did not match: {pattern[:100]!r}")
    write(path, out)


# Wire protocol: this is an intentional hard alpha -> beta cut.
for path in [
    "libs/backup-core/src/manifest.rs",
    "libs/backup-core/src/operation.rs",
    "libs/backup-core/src/result.rs",
    "libs/operator/src/kanidm/restore/legacy.rs",
    "libs/backup/src/controller/backup.rs",
    "libs/backup/src/controller/discovery.rs",
    "libs/operator/src/kanidm/reconcile/transport.rs",
    "cmd/data-mover/src/commands/upload.rs",
    "cmd/data-mover/src/commands/upload_shared.rs",
    "cmd/data-mover/src/commands/download.rs",
    "cmd/data-mover/src/commands/discover.rs",
    "cmd/data-mover/src/commands/transport.rs",
]:
    text = read(path)
    text = text.replace("backup.kaniop.rs/v1alpha1", "backup.kaniop.rs/v1beta1")
    write(path, text)

# namespaceUid was always populated with the namespace NAME. Fix the protocol vocabulary.
for path in [
    "libs/backup-core/src/manifest.rs",
    "libs/backup-core/src/operation.rs",
    "libs/backup-core/src/paths.rs",
    "cmd/data-mover/src/commands/upload.rs",
    "cmd/data-mover/src/commands/upload_shared.rs",
    "cmd/data-mover/src/commands/download.rs",
    "cmd/data-mover/src/commands/discover.rs",
    "cmd/data-mover/src/commands/transport.rs",
    "libs/operator/src/kanidm/reconcile/transport.rs",
]:
    text = read(path)
    text = text.replace("namespace_uid", "namespace")
    text = text.replace("namespaceUid", "namespace")
    write(path, text)

# Backup manifest source and beta version are intentionally incompatible with alpha.
# (The mechanical rename above updates source.namespace_uid -> source.namespace.)

# ResultDocument carries source provenance from the validated manifest so restore status can record it.
path = "libs/backup-core/src/result.rs"
replace(path,
'''    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResultError>,''',
'''    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kanidm_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kanidm_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResultError>,''', 1)
replace(path,
'''            image_digest: None,
            error: None,''',
'''            image_digest: None,
            source_namespace: None,
            source_kanidm_name: None,
            source_kanidm_uid: None,
            error: None,''')

# Download result is the trusted provenance bridge into KanidmRestore.status.resolvedSource.
path = "cmd/data-mover/src/commands/download.rs"
replace(path,
'''    result.kanidm_version = Some(manifest.source.kanidm_version.clone());
    result.image_digest = manifest.source.image_digest.clone();
    result
}''',
'''    result.kanidm_version = Some(manifest.source.kanidm_version.clone());
    result.image_digest = manifest.source.image_digest.clone();
    result.source_namespace = Some(manifest.source.namespace.clone());
    result.source_kanidm_name = Some(manifest.source.kanidm_name.clone());
    result.source_kanidm_uid = Some(manifest.source.kanidm_uid.clone());
    result
}''', 1)

# KanidmBackup beta catalog: historical source identity, no user-controlled manifest key.
path = "libs/backup-core/src/crd.rs"
replace(path, 'version = "v1alpha1",\n    kind = "KanidmBackup",', 'version = "v1beta1",\n    kind = "KanidmBackup",', 1)
replace(path, 'printcolumn = r#"{\\"name\\":\\"Kanidm\\",\\"type\\":\\"string\\",\\"jsonPath\\":\\".spec.kanidmRef.name\\"}"#,',
              'printcolumn = r#"{\\"name\\":\\"Kanidm\\",\\"type\\":\\"string\\",\\"jsonPath\\":\\".spec.source.kanidmName\\"}"#,', 1)
replace(path,
'''pub struct KanidmBackupSpec {
    pub backup_id: String,
    pub kanidm_ref: BackupKanidmRef,
    pub repository_ref: BackupRepositoryRef,
    pub manifest_key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BackupKanidmRef {
    pub name: String,
    pub uid: String,
}
''',
'''pub struct KanidmBackupSpec {
    pub backup_id: String,
    pub source: BackupSource,
    pub repository_ref: BackupRepositoryRef,
}

/// Immutable provenance of the Kanidm instance that produced a backup.
/// The namespace is the Kubernetes namespace name; Kanidm UID is the immutable lineage identity.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BackupSource {
    pub namespace: String,
    pub kanidm_name: String,
    pub kanidm_uid: String,
}
''', 1)

# Backup controller: derive storage keys from immutable catalog identity.
path = "libs/backup/src/controller/backup.rs"
replace(path,
'''use crate::crd::{
    BackupKanidmRef, BackupRepositoryRef, KanidmBackup, KanidmBackupPhase, KanidmBackupRepository,
    KanidmBackupStatus,
};''',
'''use crate::crd::{
    BackupRepositoryRef, BackupSource, KanidmBackup, KanidmBackupPhase, KanidmBackupRepository,
    KanidmBackupStatus,
};''', 1)
replace(path,
'''pub fn build_validation_job(
    backup: &KanidmBackup,
    repository: &KanidmBackupRepository,
    namespace: &str,
) -> Job {''',
'''pub fn build_validation_job(
    backup: &KanidmBackup,
    repository: &KanidmBackupRepository,
    namespace: &str,
) -> Result<Job> {''', 1)
replace(path,
'''    let ca_bundle_path = spec.s3.ca_bundle_ref.as_ref().map(|_| ca_bundle_path());

    let operation_json = serde_json::json!({''',
'''    let ca_bundle_path = spec.s3.ca_bundle_ref.as_ref().map(|_| ca_bundle_path());
    let manifest_key = backup_manifest_key(backup, repository)?;

    let operation_json = serde_json::json!({''', 1)
replace(path, '"manifestKey": backup.spec.manifest_key,', '"manifestKey": manifest_key,', 1)
replace(path, '"expectedKanidmUid": backup.spec.kanidm_ref.uid,', '"expectedKanidmUid": backup.spec.source.kanidm_uid,', 1)
# First Job-return closing in build_validation_job only.
needle = '''    Job {
        metadata: ObjectMeta {
            name: Some(job_name),'''
replace(path, needle, '''    Ok(Job {
        metadata: ObjectMeta {
            name: Some(job_name),''', 1)
# Close the first Ok(Job block just before build_deletion_job.
replace(path, '''        ..Default::default()
    }
}

pub fn build_deletion_job(''', '''        ..Default::default()
    })
}

pub fn build_deletion_job(''', 1)

# Replace catalog constructor.
regex(path,
      r'''pub fn manifest_to_backup_cr\(.*?\n\}\n\nfn job_is_complete''',
'''pub fn manifest_to_backup_cr(
    backup_id: &str,
    repository_name: &str,
    source_namespace: &str,
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
                    ("kaniop.rs/repository".to_string(), repository_name.to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            ..Default::default()
        },
        spec: crate::crd::KanidmBackupSpec {
            backup_id: backup_id.to_string(),
            source: BackupSource {
                namespace: source_namespace.to_string(),
                kanidm_name: kanidm_name.to_string(),
                kanidm_uid: kanidm_uid.to_string(),
            },
            repository_ref: BackupRepositoryRef {
                name: repository_name.to_string(),
            },
        },
        status: None,
    }
}

fn backup_manifest_key(
    backup: &KanidmBackup,
    repository: &KanidmBackupRepository,
) -> Result<String> {
    RepositoryPath::new(&repository.spec.s3.bucket, &repository.spec.s3.prefix)
        .and_then(|path| {
            path.manifest_key(
                &backup.spec.source.namespace,
                &backup.spec.source.kanidm_uid,
                &backup.spec.backup_id,
            )
        })
        .map_err(|error| Error::MissingData(format!("invalid backup repository path: {error}")))
}

fn job_is_complete''', count=1)

# Drop manifestKey validation; validate historical source instead.
regex(path,
      r'''\n    if spec\.manifest_key\.is_empty\(\) \{.*?\n    \}\n\n    let mut status =''',
'''\n    if spec.source.namespace.is_empty()
        || spec.source.kanidm_name.is_empty()
        || spec.source.kanidm_uid.is_empty()
    {
        return Err(Error::MissingData(
            "backup source namespace, kanidmName, and kanidmUid are required".to_string(),
        ));
    }

    let mut status =''', count=1)
# Wherever validation job is created, unwrap Result with ?.
text = read(path)
text = text.replace('let job = build_validation_job(&obj, &repository, &namespace);',
                    'let job = build_validation_job(&obj, &repository, &namespace)?;')
# Same-source backup deletion prefix: derive from spec rather than manifestKey parent.
text = text.replace('obj.spec.manifest_key.trim_end_matches("manifest.json")',
                    '&RepositoryPath::new(&repository.spec.s3.bucket, &repository.spec.s3.prefix)\n                .and_then(|path| path.backup_path(&obj.spec.source.namespace, &obj.spec.source.kanidm_uid, &obj.spec.backup_id))\n                .map_err(|error| Error::MissingData(format!("invalid backup repository path: {error}")))?')
write(path, text)

# Discovery: namespace NAME is the path locator. Remove the unused Namespace UID API lookup.
path = "libs/backup/src/controller/discovery.rs"
replace(path,
'''use crate::crd::{
    BackupKanidmRef, BackupRepositoryRef, KanidmBackup, KanidmBackupRepository,
    KanidmBackupSchedule, KanidmBackupSpec,
};''',
'''use crate::crd::{
    BackupRepositoryRef, BackupSource, KanidmBackup, KanidmBackupRepository, KanidmBackupSchedule,
    KanidmBackupSpec,
};''', 1)
replace(path,
'''            let namespace_uid = kanidm.metadata.namespace.as_deref().unwrap_or_default();
            let kanidm_uid = kanidm.metadata.uid.as_deref().unwrap_or_default();

            if namespace_uid.is_empty() || kanidm_uid.is_empty() {''',
'''            let source_namespace = kanidm.metadata.namespace.as_deref().unwrap_or_default();
            let kanidm_uid = kanidm.metadata.uid.as_deref().unwrap_or_default();

            if source_namespace.is_empty() || kanidm_uid.is_empty() {''', 1)
regex(path,
      r'''\n            let ns_obj = get_namespace_uid\(client, &namespace\)\.await\?;.*?\n            \}\n\n            match process_discovery_for_schedule''',
'''\n            match process_discovery_for_schedule''', count=1)
replace(path, '                namespace_uid,\n                kanidm_uid,', '                source_namespace,\n                kanidm_uid,', 1)
regex(path,
      r'''\nasync fn get_namespace_uid\(.*?\n\}\n\n#\[allow\(clippy::too_many_arguments\)\]''',
'''\n#[allow(clippy::too_many_arguments)]''', count=1)
# Rename only function parameter/local occurrences in discovery where it represented namespace name.
text = read(path)
text = text.replace('    namespace_uid: &str,\n    kanidm_uid: &str,\n    metrics:', '    source_namespace: &str,\n    kanidm_uid: &str,\n    metrics:')
text = text.replace('&namespace_uid,\n', '&source_namespace,\n')
text = text.replace('namespace_uid,\n', 'source_namespace,\n')
# Existing catalog identity and manifest-key tracking.
text = text.replace('b.spec.kanidm_ref.uid == kanidm_uid', 'b.spec.source.kanidm_uid == kanidm_uid')
# convert manifest-key set to backup-id set where possible
text = text.replace('.map(|b| b.spec.manifest_key.clone())', '.map(|b| b.spec.backup_id.clone())')
# constructor fields in this file
text = text.replace('kanidm_ref: BackupKanidmRef {\n                    name: kanidm_name.to_string(),\n                    uid: kanidm_uid.to_string(),\n                },',
                    'source: BackupSource {\n                    namespace: namespace.to_string(),\n                    kanidm_name: kanidm_name.to_string(),\n                    kanidm_uid: kanidm_uid.to_string(),\n                },')
# remove manifest_key field from discovered backup specs
text = re.sub(r'\n\s*manifest_key: manifest_key\.to_string\(\),', '', text)
write(path, text)

# Any call to manifest_to_backup_cr in discovery/controller tests uses new signature; make obvious source namespace insertion.
text = read(path)
text = text.replace('manifest_to_backup_cr(\n            manifest_key,\n            backup_id,\n            repository_name,',
                    'manifest_to_backup_cr(\n            backup_id,\n            repository_name,\n            namespace,')
write(path, text)

# Restore API: exactly one of local/backupRef/externalBackup.
path = "libs/operator/src/kanidm/restore/legacy.rs"
replace(path,
'''            "message": "exactly one of local or backupRef must be set",
            "rule": "(has(self.local) ? 1 : 0) + (has(self.backupRef) ? 1 : 0) == 1"''',
'''            "message": "exactly one of local, backupRef, or externalBackup must be set",
            "rule": "(has(self.local) ? 1 : 0) + (has(self.backupRef) ? 1 : 0) + (has(self.externalBackup) ? 1 : 0) == 1"''', 1)
replace(path,
'''    /// Remote cataloged backup reference. Mutually exclusive with `local`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_ref: Option<KanidmRestoreBackupRefSource>,
}''',
'''    /// Remote cataloged backup reference for a same-lineage restore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_ref: Option<KanidmRestoreBackupRefSource>,
    /// Historical backup in a repository, used for cross-cluster disaster recovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_backup: Option<KanidmRestoreExternalBackupSource>,
}''', 1)
replace(path,
'''pub struct KanidmRestoreBackupRefSource {
    /// Name of a KanidmBackup resource in the same namespace representing a committed remote backup.
    pub name: String,
}
''',
'''pub struct KanidmRestoreBackupRefSource {
    /// Name of a KanidmBackup resource in the same namespace representing a committed remote backup.
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreExternalBackupSource {
    pub repository_ref: KanidmRestoreExternalRepositoryRef,
    pub source: KanidmRestoreExternalSourceIdentity,
    pub backup_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreExternalRepositoryRef {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreExternalSourceIdentity {
    /// Kubernetes namespace name of the historical source.
    pub namespace: String,
    /// Immutable UID of the historical Kanidm CR.
    pub kanidm_uid: String,
}
''', 1)
# Status resolvedSource.
replace(path,
'''    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_target_uid: Option<String>,
    #[serde(default)]''',
'''    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_target_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_source: Option<KanidmRestoreResolvedSource>,
    #[serde(default)]''', 1)
replace(path,
'''pub struct ReplicaCountEntry {
    pub group: String,
    pub replicas: i32,
}
''',
'''pub struct ReplicaCountEntry {
    pub group: String,
    pub replicas: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KanidmRestoreResolvedSource {
    pub repository: String,
    pub backup_id: String,
    pub namespace: String,
    pub kanidm_name: String,
    pub kanidm_uid: String,
    pub created_at: String,
    pub kanidm_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}
''', 1)
# Remote source + source validation.
replace(path, 'restore.spec.source.backup_ref.is_some()\n}',
              'restore.spec.source.backup_ref.is_some() || restore.spec.source.external_backup.is_some()\n}', 1)
regex(path,
      r'''fn validate_source\(restore: &KanidmRestore\) -> Result<\(\)> \{.*?\n\}\n\nfn validate_safety_backup_config''',
'''fn validate_source(restore: &KanidmRestore) -> Result<()> {
    let source = &restore.spec.source;
    let count = usize::from(source.local.is_some())
        + usize::from(source.backup_ref.is_some())
        + usize::from(source.external_backup.is_some());
    if count != 1 {
        return Err(Error::MissingData(
            "source must specify exactly one of local, backupRef, or externalBackup".to_string(),
        ));
    }
    if let Some(local) = &source.local {
        if !safe_basename(&local.file_name) {
            return Err(Error::MissingData(
                "restore source fileName must be a safe basename".to_string(),
            ));
        }
    }
    if let Some(backup_ref) = &source.backup_ref {
        if backup_ref.name.is_empty() {
            return Err(Error::MissingData(
                "source.backupRef.name must not be empty".to_string(),
            ));
        }
    }
    if let Some(external) = &source.external_backup {
        if external.repository_ref.name.is_empty()
            || external.source.namespace.is_empty()
            || external.source.kanidm_uid.is_empty()
        {
            return Err(Error::MissingData(
                "externalBackup repositoryRef.name, source.namespace, and source.kanidmUid are required".to_string(),
            ));
        }
        uuid::Uuid::parse_str(&external.backup_id).map_err(|_| {
            Error::MissingData("externalBackup.backupId must be a UUID".to_string())
        })?;
    }
    Ok(())
}

fn validate_safety_backup_config''', count=1)

# Same-cluster backupRef now validates catalog source identity.
text = read(path)
text = text.replace('backup.spec.kanidm_ref.uid', 'backup.spec.source.kanidm_uid')
text = text.replace('backup.spec.kanidm_ref.name', 'backup.spec.source.kanidm_name')
text = text.replace('KanidmBackup kanidmRef.uid', 'KanidmBackup source.kanidmUid')
text = text.replace('KanidmBackup kanidmRef.name', 'KanidmBackup source.kanidmName')
write(path, text)
# Add source namespace invariant for backupRef.
replace(path,
'''    if backup.spec.source.kanidm_name != restore.spec.target_ref.name {
        return Err(Error::MissingData(format!(
            "KanidmBackup source.kanidmName '{}' does not match target name '{}'",
            backup.spec.source.kanidm_name, restore.spec.target_ref.name
        )));
    }
    let repo =''',
'''    if backup.spec.source.kanidm_name != restore.spec.target_ref.name {
        return Err(Error::MissingData(format!(
            "KanidmBackup source.kanidmName '{}' does not match target name '{}'",
            backup.spec.source.kanidm_name, restore.spec.target_ref.name
        )));
    }
    if backup.spec.source.namespace != ns {
        return Err(Error::MissingData(format!(
            "KanidmBackup source.namespace '{}' does not match target namespace '{}'",
            backup.spec.source.namespace, ns
        )));
    }
    let repo =''', 1)

# External source preflight validates repository availability. Manifest/domain/version are verified in PreparingSource before mutation.
insert_before = 'fn has_accepted_condition(conditions: &[Condition]) -> bool {'
replace(path, insert_before,
'''async fn validate_external_backup(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
) -> Result<()> {
    let external = restore
        .spec
        .source
        .external_backup
        .as_ref()
        .ok_or_else(|| Error::MissingData("externalBackup source not set".to_string()))?;
    let ns = restore.namespace().unwrap();
    let repo = Api::<KanidmBackupRepository>::namespaced(ctx.client.clone(), &ns)
        .get(&external.repository_ref.name)
        .await
        .map_err(|e| {
            Error::kube_error(
                "get",
                "KanidmBackupRepository",
                &ns,
                &external.repository_ref.name,
                e,
            )
        })?;
    if !has_accepted_condition(
        &repo.status.as_ref().map(|s| &s.conditions).cloned().unwrap_or_default(),
    ) {
        return Err(Error::MissingData(format!(
            "KanidmBackupRepository '{}' configuration has not been accepted",
            external.repository_ref.name
        )));
    }
    RepositoryPath::new(&repo.spec.s3.bucket, &repo.spec.s3.prefix)
        .and_then(|path| {
            path.manifest_key(
                &external.source.namespace,
                &external.source.kanidm_uid,
                &external.backup_id,
            )
        })
        .map_err(|error| Error::MissingData(format!("invalid external backup path: {error}")))?;
    Ok(())
}

fn has_accepted_condition(conditions: &[Condition]) -> bool {''', 1)
# Need RepositoryPath import.
replace(path, 'use kaniop_backup_core::image::data_mover_image;\n',
              'use kaniop_backup_core::image::data_mover_image;\nuse kaniop_backup_core::paths::RepositoryPath;\n', 1)
# validate dispatch.
replace(path,
'''    if is_remote_source(restore) {
        validate_backup_ref(restore, &target, ctx).await?;
    }''',
'''    if restore.spec.source.backup_ref.is_some() {
        validate_backup_ref(restore, &target, ctx).await?;
    } else if restore.spec.source.external_backup.is_some() {
        validate_external_backup(restore, ctx).await?;
    }''', 1)

# Common remote source resolver used by both same-cluster and DR paths.
insert_before = 'async fn ensure_source_prep_job(\n'
replace(path, insert_before,
'''struct ResolvedRemoteSource {
    repository: KanidmBackupRepository,
    repository_name: String,
    manifest_key: String,
    backup_id: String,
    source_namespace: String,
    source_kanidm_uid: String,
    expected_payload_sha256: Option<String>,
}

async fn resolve_remote_source(
    restore: &KanidmRestore,
    ctx: &RestoreContext,
) -> Result<ResolvedRemoteSource> {
    let ns = restore.namespace().unwrap();
    if let Some(reference) = &restore.spec.source.backup_ref {
        let backup = Api::<KanidmBackup>::namespaced(ctx.client.clone(), &ns)
            .get(&reference.name)
            .await
            .map_err(|e| Error::kube_error("get", "KanidmBackup", &ns, &reference.name, e))?;
        let repository_name = backup.spec.repository_ref.name.clone();
        let repository = Api::<KanidmBackupRepository>::namespaced(ctx.client.clone(), &ns)
            .get(&repository_name)
            .await
            .map_err(|e| Error::kube_error("get", "KanidmBackupRepository", &ns, &repository_name, e))?;
        let manifest_key = RepositoryPath::new(&repository.spec.s3.bucket, &repository.spec.s3.prefix)
            .and_then(|path| path.manifest_key(
                &backup.spec.source.namespace,
                &backup.spec.source.kanidm_uid,
                &backup.spec.backup_id,
            ))
            .map_err(|error| Error::MissingData(format!("invalid backup path: {error}")))?;
        return Ok(ResolvedRemoteSource {
            repository,
            repository_name,
            manifest_key,
            backup_id: backup.spec.backup_id.clone(),
            source_namespace: backup.spec.source.namespace.clone(),
            source_kanidm_uid: backup.spec.source.kanidm_uid.clone(),
            expected_payload_sha256: backup.status.as_ref().and_then(|s| s.payload_sha256.clone()),
        });
    }

    let external = restore.spec.source.external_backup.as_ref().ok_or_else(|| {
        Error::MissingData("remote restore source not configured".to_string())
    })?;
    let repository_name = external.repository_ref.name.clone();
    let repository = Api::<KanidmBackupRepository>::namespaced(ctx.client.clone(), &ns)
        .get(&repository_name)
        .await
        .map_err(|e| Error::kube_error("get", "KanidmBackupRepository", &ns, &repository_name, e))?;
    let manifest_key = RepositoryPath::new(&repository.spec.s3.bucket, &repository.spec.s3.prefix)
        .and_then(|path| path.manifest_key(
            &external.source.namespace,
            &external.source.kanidm_uid,
            &external.backup_id,
        ))
        .map_err(|error| Error::MissingData(format!("invalid external backup path: {error}")))?;
    Ok(ResolvedRemoteSource {
        repository,
        repository_name,
        manifest_key,
        backup_id: external.backup_id.clone(),
        source_namespace: external.source.namespace.clone(),
        source_kanidm_uid: external.source.kanidm_uid.clone(),
        expected_payload_sha256: None,
    })
}

async fn ensure_source_prep_job(
''', 1)
# Replace start of ensure_source_prep_job's backup/ref/repo resolution block.
regex(path,
      r'''    let backup_name = restore.*?    let endpoint = &repo\.spec\.s3\.endpoint;\n    let region = &repo\.spec\.s3\.region;''',
'''    let source = resolve_remote_source(restore, ctx).await?;
    let repo = &source.repository;
    let endpoint = &repo.spec.s3.endpoint;
    let region = &repo.spec.s3.region;''', count=1)
# Replace build_download args and auth references in this function.
text = read(path)
text = text.replace('&backup.spec.manifest_key,\n        &backup.spec.backup_id,',
                    '&source.manifest_key,\n        &source.backup_id,\n        &source.source_kanidm_uid,')
text = text.replace('&backup.spec.repository_ref.name,\n                                AuthRole::Reader,',
                    '&source.repository_name,\n                                AuthRole::Reader,')
write(path, text)

# build_download_operation_doc: pass historical UID rather than target UID.
replace(path,
'''    expected_backup_id: &str,
    bucket: &str,''',
'''    expected_backup_id: &str,
    expected_kanidm_uid: &str,
    bucket: &str,''', 1)
replace(path, '"expectedKanidmUid": restore.spec.target_ref.uid,', '"expectedKanidmUid": expected_kanidm_uid,', 1)

# Source prep result: carry verified provenance.
replace(path,
'''struct VerifiedSourcePrepResult {
    #[allow(dead_code)]
    manifest_key: String,
    payload_sha256: String,
}''',
'''struct VerifiedSourcePrepResult {
    #[allow(dead_code)]
    manifest_key: String,
    payload_sha256: String,
    namespace: String,
    kanidm_name: String,
    kanidm_uid: String,
    created_at: String,
    kanidm_version: String,
    image_digest: Option<String>,
}''', 1)
replace(path,
'''    Ok(VerifiedSourcePrepResult {
        manifest_key,
        payload_sha256,
    })''',
'''    let namespace = result_doc.source_namespace.clone().ok_or_else(|| {
        Error::ParseError("result document missing sourceNamespace".to_string())
    })?;
    let kanidm_name = result_doc.source_kanidm_name.clone().ok_or_else(|| {
        Error::ParseError("result document missing sourceKanidmName".to_string())
    })?;
    let kanidm_uid = result_doc.source_kanidm_uid.clone().ok_or_else(|| {
        Error::ParseError("result document missing sourceKanidmUid".to_string())
    })?;
    let created_at = result_doc.created_at.clone().ok_or_else(|| {
        Error::ParseError("result document missing createdAt".to_string())
    })?;
    let kanidm_version = result_doc.kanidm_version.clone().ok_or_else(|| {
        Error::ParseError("result document missing kanidmVersion".to_string())
    })?;
    Ok(VerifiedSourcePrepResult {
        manifest_key,
        payload_sha256,
        namespace,
        kanidm_name,
        kanidm_uid,
        created_at,
        kanidm_version,
        image_digest: result_doc.image_digest.clone(),
    })''', 1)

# PreparingSource completion: resolve expected source independently of target and persist manifest-derived provenance.
regex(path,
      r'''                        let backup_id = restore.*?                        let result =\n                            read_source_prep_result\(&restore, &ctx, &name, expected_backup_id\)\n                                \.await;''',
'''                        let source = resolve_remote_source(&restore, &ctx).await?;
                        let result = read_source_prep_result(
                            &restore,
                            &ctx,
                            &name,
                            &source.backup_id,
                        )
                        .await;''', count=1)
# Replace old backup.status SHA block through status creation with source-aware validation/persistence.
regex(path,
      r'''                            Ok\(verified\) => \{\n                                if let Some\(expected_sha256\) = backup.*?                                let mut status = restore\.status\.clone\(\)\.unwrap_or_default\(\);''',
'''                            Ok(verified) => {
                                if let Some(expected_sha256) = source.expected_payload_sha256.as_deref() {
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
                                if verified.namespace != source.source_namespace
                                    || verified.kanidm_uid != source.source_kanidm_uid
                                {
                                    resume_before_mutation(&restore, &ctx).await?;
                                    set_phase(
                                        &restore,
                                        &ctx,
                                        KanidmRestorePhase::Failed,
                                        Some("resolved manifest source does not match requested historical source".to_string()),
                                    )
                                    .await?;
                                    return Ok(Action::requeue(REQUEUE));
                                }
                                validate_resolved_source_compatibility(&restore, &target, &verified)?;
                                let mut status = restore.status.clone().unwrap_or_default();
                                status.resolved_source = Some(KanidmRestoreResolvedSource {
                                    repository: source.repository_name.clone(),
                                    backup_id: source.backup_id.clone(),
                                    namespace: verified.namespace.clone(),
                                    kanidm_name: verified.kanidm_name.clone(),
                                    kanidm_uid: verified.kanidm_uid.clone(),
                                    created_at: verified.created_at.clone(),
                                    kanidm_version: verified.kanidm_version.clone(),
                                    image_digest: verified.image_digest.clone(),
                                });''', count=1)
# add target to PreparingSource scope if not already there exists at top yes target declared.

# Compatibility helper for external/same remote manifests, before build_download_operation_doc.
insert_before = '#[allow(clippy::too_many_arguments)]\nfn build_download_operation_doc('
replace(path, insert_before,
'''fn validate_resolved_source_compatibility(
    restore: &KanidmRestore,
    target: &Kanidm,
    source: &VerifiedSourcePrepResult,
) -> Result<()> {
    if let Some(target_version) = target
        .status
        .as_ref()
        .and_then(|status| status.version.as_ref())
        .map(|version| version.image_tag.as_str())
        && !target_version.is_empty()
        && !source.kanidm_version.is_empty()
        && target_version != source.kanidm_version
    {
        return Err(Error::MissingData(format!(
            "backup Kanidm version '{}' does not match target version '{}'",
            source.kanidm_version, target_version
        )));
    }
    if let Some(digest) = source.image_digest.as_deref()
        && !digest.is_empty()
        && restore.spec.restore_image.contains('@')
        && !restore.spec.restore_image.contains(digest)
    {
        return Err(Error::MissingData(format!(
            "restore image digest does not match backup image digest '{digest}'"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_download_operation_doc(''', 1)

# Safety upload namespace vocabulary.
text = read(path).replace('namespace_uid: restore.namespace().unwrap_or_default(),',
                          'namespace: restore.namespace().unwrap_or_default(),')
write(path, text)

# Add external_backup: None to existing restore source struct literals throughout Rust source/tests.
for path_obj in ROOT.rglob('*.rs'):
    text = path_obj.read_text()
    if 'KanidmRestoreSource {' not in text:
        continue
    # Add only where a literal has backup_ref and no external field before its closing brace.
    pattern = re.compile(r'(KanidmRestoreSource\s*\{(?:(?!\n\s*\}).)*?\n\s*backup_ref:\s*[^\n]+,)(\n\s*\})', re.DOTALL)
    def add_external(m):
        block = m.group(0)
        if 'external_backup:' in block:
            return block
        indent = re.search(r'\n(\s*)backup_ref:', block).group(1)
        return m.group(1) + f'\n{indent}external_backup: None,' + m.group(2)
    out = pattern.sub(add_external, text)
    path_obj.write_text(out)

# Restore module exports new API/status types.
path = "libs/operator/src/kanidm/restore/mod.rs"
replace(path,
'''    KanidmRestore, KanidmRestoreBackupRefSource, KanidmRestoreLocalSource, KanidmRestorePhase,
    KanidmRestoreSource, KanidmRestoreSpec, KanidmRestoreStatus, KanidmRestoreTargetRef,
    RESTORE_ANNOTATION, ReplicaCountEntry, SafetyBackupConfig, SafetyBackupRepositoryRef,
};''',
'''    KanidmRestore, KanidmRestoreBackupRefSource, KanidmRestoreExternalBackupSource,
    KanidmRestoreExternalRepositoryRef, KanidmRestoreExternalSourceIdentity,
    KanidmRestoreLocalSource, KanidmRestorePhase, KanidmRestoreResolvedSource,
    KanidmRestoreSource, KanidmRestoreSpec, KanidmRestoreStatus, KanidmRestoreTargetRef,
    RESTORE_ANNOTATION, ReplicaCountEntry, SafetyBackupConfig, SafetyBackupRepositoryRef,
};''', 1)

# Fix backup CR constructors/usages across the tree. The beta API is intentionally breaking.
for path_obj in ROOT.rglob('*.rs'):
    if '.github/scripts' in str(path_obj):
        continue
    text = path_obj.read_text()
    text = text.replace('BackupKanidmRef', 'BackupSource')
    text = text.replace('.spec.kanidm_ref.uid', '.spec.source.kanidm_uid')
    text = text.replace('.spec.kanidm_ref.name', '.spec.source.kanidm_name')
    # Common constructor shape after type rename.
    text = re.sub(
        r'kanidm_ref:\s*BackupSource\s*\{\s*name:\s*([^,]+),\s*uid:\s*([^,]+),\s*\}',
        r'source: BackupSource { namespace: "default".to_string(), kanidm_name: \1, kanidm_uid: \2 }',
        text,
        flags=re.DOTALL,
    )
    # Legacy field names in test literals.
    text = re.sub(r'\n\s*manifest_key:\s*[^,\n]+,', '', text)
    path_obj.write_text(text)

# Operation namespace field literals after type rename in tests/source.
for path_obj in ROOT.rglob('*.rs'):
    if '.github/scripts' in str(path_obj):
        continue
    text = path_obj.read_text().replace('namespace_uid:', 'namespace:')
    path_obj.write_text(text)

# Webhook backup validation: no arbitrary manifestKey; validate source fields instead.
path = "cmd/webhook/src/handlers.rs"
text = read(path)
text = re.sub(r'''\n    if object\.spec\.manifest_key\.is_empty\(\) \{.*?\n    \}\n    if object\.spec\.manifest_key\.contains\("\.\."\) \{.*?\n    \}''', '', text, count=1, flags=re.DOTALL)
# Add source validation after backupId validation if identifiable.
marker = 'if object.spec.backup_id.is_empty() {'
pos = text.find(marker)
if pos != -1:
    # insert after the first full if block by locating next '\n    }'
    end = text.find('\n    }', pos)
    if end != -1:
        end += len('\n    }')
        snippet = '''
    if object.spec.source.namespace.is_empty()
        || object.spec.source.kanidm_name.is_empty()
        || object.spec.source.kanidm_uid.is_empty()
    {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "source namespace, kanidmName, and kanidmUid are required",
        )));
    }'''
        text = text[:end] + snippet + text[end:]
write(path, text)

# Generated/user-facing API version strings for KanidmBackup only.
for path_obj in [ROOT / 'cmd/webhook/src/handlers.rs', ROOT / 'libs/backup/src/controller/backup.rs']:
    text = path_obj.read_text().replace('"kaniop.rs/v1alpha1",\n        "kind": "KanidmBackup"',
                                        '"kaniop.rs/v1beta1",\n        "kind": "KanidmBackup"')
    path_obj.write_text(text)

# Transport/discovery operator field vocabulary.
path = "libs/backup/src/controller/discovery.rs"
text = read(path).replace('"namespaceUid":', '"namespace":')
write(path, text)
path = "libs/operator/src/kanidm/reconcile/transport.rs"
text = read(path).replace('"namespaceUid":', '"namespace":')
write(path, text)

# Documentation: accepted ADR and implementation plan.
write("docs/adr/0002-cross-cluster-backup-restore.md", '''# ADR 0002: First-class cross-cluster backup restore

- Status: Accepted
- Date: 2026-09-03
- Supersedes: the source-identity assumptions in ADR 0001 where noted below

## Context

Kaniop's remote backup protocol originally coupled a `KanidmBackup` to the live
`Kanidm` CR that produced it. A normal restore required the backup's Kanidm UID
to equal the target Kanidm UID. That is a useful same-cluster safety invariant,
but it prevents a declarative disaster-recovery restore after loss of the
original Kubernetes cluster because the recovered `Kanidm` necessarily has a
new Kubernetes UID.

The alpha protocol also called the namespace component `namespaceUid` even
though Kaniop always wrote the Kubernetes namespace **name** into that field.
Discovery additionally fetched the real Namespace UID but did not use it.
Keeping that accidental vocabulary would make a beta contract misleading.

## Decision

### Separate historical source identity from destructive target identity

`KanidmRestore.spec.targetRef` continues to identify the exact current Kanidm
object that may be mutated:

```yaml
targetRef:
  name: kanidm
  uid: <current-target-uid>
```

Remote backup provenance is independent:

```yaml
source:
  namespace: identity-prod
  kanidmName: kanidm
  kanidmUid: <historical-source-uid>
```

The namespace is a Kubernetes namespace **name** (`String`). The Kanidm UID is
the immutable lineage discriminator. A Namespace UID is not part of the backup
identity model.

### Make KanidmBackup a historical catalog record

`KanidmBackup` moves to `kaniop.rs/v1beta1` and stores `spec.source` instead of
`spec.kanidmRef`. The public `manifestKey` field is removed. Object-store keys
are derived from repository configuration, source namespace, source Kanidm UID,
and backup ID. A catalog record therefore remains meaningful if the original
Kanidm or namespace no longer exists.

### Add an explicit external backup restore source

`KanidmRestore` remains `kaniop.rs/v1beta1` and accepts exactly one of
`local`, `backupRef`, or `externalBackup`.

`backupRef` is the normal same-lineage path and requires source namespace,
Kanidm name, and Kanidm UID to match the current target.

`externalBackup` selects a historical repository lineage explicitly:

```yaml
source:
  externalBackup:
    repositoryRef:
      name: production-backups
    source:
      namespace: identity-prod
      kanidmUid: <old-kanidm-uid>
    backupId: <backup-uuid>
```

A source UID different from the target UID is expected for this mode. No
`ignoreUid`, `force`, or generic disaster-recovery boolean weakens normal
restore validation.

### Keep application-level checks strict

Before `databaseMutationStarted`, the data mover validates the beta manifest,
backup ID, historical source UID, target Kanidm domain, repository confinement,
encryption metadata, payload size, and payload SHA-256. Kaniop validates Kanidm
version/image compatibility and persists `status.resolvedSource` from the
validated manifest result. The requested historical namespace and UID must
match the resolved manifest source.

### Keep the physical repository layout

This ADR does not rename `v1/tenants/.../clusters/...`. The namespace component
already contains the namespace name, so changing directory aesthetics would
add migration risk without improving the identity model.

### Hard alpha-to-beta protocol cut

`KanidmBackupManifest`, `OperationDocument`, and `ResultDocument` move from
`backup.kaniop.rs/v1alpha1` to `backup.kaniop.rs/v1beta1`. Alpha manifests are
not read by the beta implementation. This is intentional while the subsystem
is experimental.

## Consequences

- A fresh cluster can restore an existing remote backup without recreating old
  Kubernetes UIDs.
- Same-cluster restore retains its strict UID guard.
- Users no longer provide arbitrary object-store manifest keys.
- Namespace names remain readable and operationally useful while Kanidm UID
  provides immutable lineage separation.
- Existing alpha `KanidmBackup` objects and remote manifests require the hard
  migration documented in the changelog.
- Native Kanidm backup payload files are not invalidated by this API/protocol
  change; the incompatibility is in Kaniop's remote catalog/manifest contract.
''')

write("docs/plans/cross-cluster-backup-restore-beta.md", '''# Cross-cluster backup restore beta implementation plan

## Goal

Make clean-cluster disaster recovery a first-class declarative workflow without
weakening the identity checks used by ordinary restores, while correcting the
alpha `namespaceUid` naming mistake before the backup catalog becomes beta.

## Implementation

1. **Beta catalog contract**
   - Move `KanidmBackup` to `v1beta1`.
   - Replace `kanidmRef` with immutable `source { namespace, kanidmName, kanidmUid }`.
   - Remove public `manifestKey`; derive repository keys internally.
   - Keep Repository and Schedule at `v1alpha1`.

2. **Beta wire protocol**
   - Move Manifest, OperationDocument, and ResultDocument to `backup.kaniop.rs/v1beta1`.
   - Rename `namespaceUid` to `namespace` everywhere in the backup protocol.
   - Preserve the current physical S3 layout.
   - Return validated source provenance in download results.

3. **Discovery and catalog reconciliation**
   - Stop fetching the unused Kubernetes Namespace UID.
   - Discover by namespace name + Kanidm UID.
   - Reconstruct immutable beta `KanidmBackup` catalog resources from repository state.

4. **Restore API and resolver**
   - Extend `KanidmRestore.source` with `externalBackup`.
   - Resolve both `backupRef` and `externalBackup` to a common internal remote source.
   - Derive the manifest key from repository + historical source + backup ID.
   - Keep `backupRef` same-lineage identity checks strict.
   - Allow external historical UID/namespace to differ from the target.

5. **Pre-mutation verification and auditability**
   - Verify manifest source UID and target domain in the data mover.
   - Verify payload size/SHA and encryption before mutation.
   - Validate Kanidm version/image compatibility.
   - Persist `status.resolvedSource` from the verified manifest result.

6. **Migration and docs**
   - Document the intentional alpha incompatibility and CRD stored-version transition.
   - Regenerate CRDs/examples.
   - Add/adjust unit and e2e coverage for beta source identity and external restore.

## Acceptance criteria

- `namespaceUid` does not appear in the beta backup protocol/API.
- A `KanidmBackup` does not require the source Kanidm CR to exist.
- Same-cluster `backupRef` rejects a different target UID.
- `externalBackup` accepts a historical UID different from the target UID.
- External restore cannot select an arbitrary S3 key.
- Requested external namespace/UID/backup ID are verified against the manifest.
- Domain, version/image, encryption, size, and SHA validation happen before database mutation.
- The resolved historical source is visible in restore status.
- `make crdgen`, `make examples`, formatting, lint, and tests pass.
''')

# Add a concise user-facing DR section; leave broader document structure intact.
path = "Documentation/src/usage/backup-restore.md"
text = read(path)
if '## Cross-cluster disaster recovery' not in text:
    text += '''\n\n## Cross-cluster disaster recovery\n\n`KanidmRestore.spec.source.externalBackup` restores a historical backup into a\nnew Kanidm CR without requiring the old Kubernetes UID to be recreated. The\nsource namespace is the historical namespace **name**; `kanidmUid` identifies\nthe historical backup lineage.\n\n```yaml\napiVersion: kaniop.rs/v1beta1\nkind: KanidmRestore\nmetadata:\n  name: recover-production\nspec:\n  targetRef:\n    name: kanidm\n    uid: <current-target-uid>\n  source:\n    externalBackup:\n      repositoryRef:\n        name: production-backups\n      source:\n        namespace: identity-prod\n        kanidmUid: <historical-kanidm-uid>\n      backupId: <backup-uuid>\n  restoreImage: <the-target-pinned-kanidm-image>\n  safetyBackup:\n    repositoryRef:\n      name: production-backups\n```\n\nThe external source UID is expected to differ from `targetRef.uid`. Kaniop does\nnot disable identity verification: it derives the manifest key itself and\nverifies the requested historical source, target domain, payload integrity,\nencryption metadata, and version/image compatibility before database mutation.\nThe verified manifest provenance is recorded in `status.resolvedSource`.\n'''
write(path, text)

# Changelog migration notice under the first heading.
path = "CHANGELOG.md"
text = read(path)
notice = '''\n### Breaking: backup catalog and remote protocol beta\n\n- `KanidmBackup` moves to `kaniop.rs/v1beta1`, replaces `spec.kanidmRef` with\n  historical `spec.source`, and removes public `spec.manifestKey`.\n- Kaniop's remote Manifest/Operation/Result protocol moves to\n  `backup.kaniop.rs/v1beta1`; the misnamed `namespaceUid` field is now\n  `namespace` and continues to contain the Kubernetes namespace name.\n- Existing experimental alpha catalog objects and remote manifests are not\n  compatible. Before upgrading, suspend backup schedules, remove old\n  `KanidmBackup` resources, preserve or remove alpha remote objects (or start\n  with a new repository prefix), complete the documented CRD `storedVersions`\n  transition, upgrade, then resume backups. Native Kanidm `.json.gz` backup\n  payloads are not invalidated by this Kaniop protocol change.\n- `KanidmRestore.spec.source.externalBackup` adds first-class clean-cluster\n  disaster recovery from a historical namespace + Kanidm UID + backup ID.\n\n'''
if '### Breaking: backup catalog and remote protocol beta' not in text:
    # Insert after top-level changelog title.
    first_nl = text.find('\n')
    text = text[:first_nl+1] + notice + text[first_nl+1:]
write(path, text)

# Clean placeholder/probe files created while preparing the branch.
for rel in [
    "docs/adr/.keep-beta-dr",
    "docs/plans/.keep-beta-dr",
    "docs/plans/.tool-check",
    "docs/plans/.tool-check-2",
    "docs/plans/.tool-check-3",
    "docs/plans/.tool-check-4",
]:
    q = p(rel)
    if q.exists():
        q.unlink()

print("backup DR beta patch applied")
