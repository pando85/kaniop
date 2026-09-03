#!/usr/bin/env python3
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]


def replace_test(text: str, name: str, replacement: str) -> str:
    pattern = re.compile(
        rf"    #\[test\]\n    fn {re.escape(name)}\(\) \{{.*?(?=\n    #\[test\]|\n    fn [a-zA-Z_])",
        flags=re.DOTALL,
    )
    updated, count = pattern.subn(replacement.rstrip(), text, count=1)
    if count != 1:
        raise RuntimeError(f"test {name}: expected one block, found {count}")
    return updated


# backup_validator: test the beta immutable provenance contract rather than a removed key.
path = ROOT / "cmd/webhook/src/backup_validator.rs"
text = path.read_text()
text = replace_test(
    text,
    "backup_immutable_spec_same_spec_passes",
    '''    #[test]
    fn backup_immutable_spec_same_spec_passes() {
        let old = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some("kb-test".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupSpec {
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                source: BackupSource {
                    namespace: "default".to_string(),
                    kanidm_name: "corp-idm".to_string(),
                    kanidm_uid: "uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
            },
            status: None,
        };
        let new = old.clone();
        assert!(validate_backup_immutable_spec(&old, &new).is_ok());
    }''',
)
text = replace_test(
    text,
    "backup_immutable_spec_changed_spec_fails",
    '''    #[test]
    fn backup_immutable_spec_changed_spec_fails() {
        let old = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some("kb-test".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupSpec {
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                source: BackupSource {
                    namespace: "default".to_string(),
                    kanidm_name: "corp-idm".to_string(),
                    kanidm_uid: "uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
            },
            status: None,
        };
        let mut new = old.clone();
        new.spec.source.kanidm_uid = "uid-456".to_string();
        assert!(validate_backup_immutable_spec(&old, &new).is_err());
    }''',
)
text = replace_test(
    text,
    "backup_immutable_spec_changed_backup_id_fails",
    '''    #[test]
    fn backup_immutable_spec_changed_backup_id_fails() {
        let old = KanidmBackup {
            metadata: kube::api::ObjectMeta {
                name: Some("kb-test".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: kaniop_backup::crd::KanidmBackupSpec {
                backup_id: "019c7c76-f423-7a12-8f41-2bea7588a303".to_string(),
                source: BackupSource {
                    namespace: "default".to_string(),
                    kanidm_name: "corp-idm".to_string(),
                    kanidm_uid: "uid-123".to_string(),
                },
                repository_ref: BackupRepositoryRef {
                    name: "offsite".to_string(),
                },
            },
            status: None,
        };
        let mut new = old.clone();
        new.spec.backup_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string();
        assert!(validate_backup_immutable_spec(&old, &new).is_err());
    }''',
)
if "manifest_key:" in text[text.index("#[cfg(test)]"):]:
    raise RuntimeError("stale manifest_key initializer remains in backup_validator tests")
path.write_text(text)


# handlers: beta JSON and helper use source provenance; update mutation tests accordingly.
path = ROOT / "cmd/webhook/src/handlers.rs"
text = path.read_text()
text = text.replace(
    "fn test_backup(name: &str, backup_id: &str, manifest_key: &str) -> KanidmBackup {",
    "fn test_backup(name: &str, backup_id: &str, _manifest_key: &str) -> KanidmBackup {",
    1,
)
text = re.sub(
    r'\n\s*manifest_key: manifest_key\.to_string\(\),',
    "",
    text,
    count=1,
)

# Convert the two admission JSON fixtures to the beta catalog schema.
text = text.replace(
    '"kanidmRef": {"name": "corp-idm", "uid": "uid-123"},\n                        "repositoryRef": {"name": "offsite"},\n                        "manifestKey": "v1/manifest.json"',
    '"source": {"namespace": "default", "kanidmName": "corp-idm", "kanidmUid": "uid-123"},\n                        "repositoryRef": {"name": "offsite"}',
)
text = text.replace(
    '"kanidmRef": {"name": "corp-idm", "uid": "uid-123"},\n                        "repositoryRef": {"name": "offsite"},\n                        "manifestKey": "v1/old-manifest.json"',
    '"source": {"namespace": "default", "kanidmName": "corp-idm", "kanidmUid": "uid-123"},\n                        "repositoryRef": {"name": "offsite"}',
)
text = text.replace(
    'assert_eq!(old.spec.manifest_key, "v1/old-manifest.json");',
    'assert_eq!(old.spec.source.kanidm_uid, "uid-123");',
    1,
)
text = text.replace(
    'new.spec.manifest_key = "v1/new.json".to_string();',
    'new.spec.source.kanidm_uid = "uid-456".to_string();',
    1,
)

# Ensure the test import follows the beta catalog type. The main migration normally does
# this already, but normalize it explicitly to keep this helper self-contained.
text = text.replace("BackupKanidmRef, BackupRepositoryRef, KanidmBackupSpec", "BackupSource, BackupRepositoryRef, KanidmBackupSpec")

if ".spec.manifest_key" in text[text.index("#[cfg(test)]"):]:
    raise RuntimeError("stale manifest_key field access remains in handler tests")
if "manifest_key: manifest_key.to_string()" in text:
    raise RuntimeError("stale manifest_key initializer remains in handler tests")
path.write_text(text)

print("backup beta webhook test migration applied")
