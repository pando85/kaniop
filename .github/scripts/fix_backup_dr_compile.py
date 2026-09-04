#!/usr/bin/env python3
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]


def replace_exact(path: Path, old: str, new: str, expected: int = 1) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != expected:
        raise RuntimeError(f"{path}: expected {expected} occurrences, found {count}: {old!r}")
    path.write_text(text.replace(old, new, expected))


def replace_test(text: str, name: str, replacement: str) -> str:
    pattern = re.compile(
        rf"    #\[test\]\n    fn {re.escape(name)}\(\) \{{.*?(?=\n    #\[test\])",
        flags=re.DOTALL,
    )
    updated, count = pattern.subn(replacement.rstrip(), text, count=1)
    if count != 1:
        raise RuntimeError(f"test {name}: expected one block, found {count}")
    return updated


# The validation builder now derives the manifest key and can fail on invalid catalog identity.
backup = ROOT / "libs/backup/src/controller/backup.rs"
text = backup.read_text()
old = "    let validation_job = build_validation_job(obj, &repository, namespace);"
if text.count(old) != 1:
    raise RuntimeError(f"production validation-job call count: {text.count(old)}")
text = text.replace(old, old[:-1] + "?;", 1)

marker = "#[cfg(test)]\nmod tests {"
if marker not in text:
    raise RuntimeError("backup controller test module not found")
production, tests = text.split(marker, 1)

tests = tests.replace(
    "fn make_backup(backup_id: &str, manifest_key: &str) -> KanidmBackup {",
    "fn make_backup(backup_id: &str, _manifest_key: &str) -> KanidmBackup {",
    1,
)
# This nested initializer is intentionally removed explicitly; generic regexes must not
# grow broad enough to touch ResultDocument/OperationDocument manifest keys.
stale_initializer = "                manifest_key: manifest_key.to_string(),\n"
if tests.count(stale_initializer) != 1:
    raise RuntimeError(f"nested backup test manifest_key count: {tests.count(stale_initializer)}")
tests = tests.replace(stale_initializer, "", 1)

tests = replace_test(
    tests,
    "manifest_to_backup_cr_stores_manifest_key_not_payload_key",
    '''    #[test]
    fn manifest_to_backup_cr_records_historical_source() {
        let cr = manifest_to_backup_cr(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "default",
            "corp-idm",
            "k-uid",
        );
        assert_eq!(cr.spec.backup_id, "019c7c76-f423-7a12-8f41-2bea7588a303");
        assert_eq!(cr.spec.repository_ref.name, "offsite");
        assert_eq!(cr.spec.source.namespace, "default");
        assert_eq!(cr.spec.source.kanidm_name, "corp-idm");
        assert_eq!(cr.spec.source.kanidm_uid, "k-uid");
    }''',
)

tests = replace_test(
    tests,
    "manifest_key_must_end_with_manifest_json",
    '''    #[test]
    fn manifest_key_is_derived_from_catalog_identity() {
        let backup = make_backup(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "ignored",
        );
        let repository = make_repository_with_auth(Some("w"), Some("r"), Some("d"));
        let manifest_key = backup_manifest_key(&backup, &repository).unwrap();
        assert_eq!(
            manifest_key,
            "p/v1/tenants/default/clusters/k-uid-123/backups/019c7c76-f423-7a12-8f41-2bea7588a303/manifest.json"
        );
    }''',
)

validation_call = 'let job = build_validation_job(&backup, &repository, "default");'
count = tests.count(validation_call)
if count != 5:
    raise RuntimeError(f"validation builder test call count: {count}")
tests = tests.replace(
    validation_call,
    'let job = build_validation_job(&backup, &repository, "default").unwrap();',
)

if ".spec.manifest_key" in tests or "manifest_key: manifest_key.to_string()" in tests:
    raise RuntimeError("stale KanidmBackupSpec manifest_key use remains in backup tests")
backup.write_text(production + marker + tests)


# Schedule remains alpha, but retention iterates beta KanidmBackup catalog records.
schedule = ROOT / "libs/backup/src/controller/schedule.rs"
replace_exact(
    schedule,
    ".filter(|b| b.spec.kanidm_ref.uid == kanidm_uid)",
    ".filter(|b| b.spec.source.kanidm_uid == kanidm_uid)",
)


# Discovery tests now exercise the beta catalog identity, not a user-controlled manifest key.
discovery = ROOT / "libs/backup/src/controller/discovery.rs"
text = discovery.read_text()
text = replace_test(
    text,
    "reconcile_is_idempotent_for_same_manifest_key",
    '''    #[test]
    fn reconcile_is_idempotent_for_same_backup_identity() {
        let cr1 = build_backup_cr(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "default",
            "kanidm",
            "uid",
        );
        let cr2 = build_backup_cr(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "repo",
            "default",
            "kanidm",
            "uid",
        );
        assert_eq!(cr1.metadata.name, cr2.metadata.name);
        assert_eq!(cr1.spec, cr2.spec);
    }''',
)
text = replace_test(
    text,
    "build_backup_cr_manifest_key_is_preserved",
    '''    #[test]
    fn build_backup_cr_records_historical_source() {
        let cr = build_backup_cr(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "source-ns",
            "corp-idm",
            "k-uid",
        );
        assert_eq!(cr.spec.backup_id, "019c7c76-f423-7a12-8f41-2bea7588a303");
        assert_eq!(cr.spec.repository_ref.name, "offsite");
        assert_eq!(cr.spec.source.namespace, "source-ns");
        assert_eq!(cr.spec.source.kanidm_name, "corp-idm");
        assert_eq!(cr.spec.source.kanidm_uid, "k-uid");
    }''',
)
text = replace_test(
    text,
    "build_backup_cr_does_not_set_ready_phase",
    '''    #[test]
    fn build_backup_cr_does_not_set_ready_phase() {
        let cr = build_backup_cr(
            "019c7c76-f423-7a12-8f41-2bea7588a303",
            "offsite",
            "default",
            "corp-idm",
            "k-uid",
        );
        assert!(cr.status.is_none());
    }''',
)
text = text.replace("AuthMethod, KanidmBackupPhase, KanidmBackupRepositorySpec,", "AuthMethod, KanidmBackupRepositorySpec,", 1)
if ".spec.manifest_key" in text:
    raise RuntimeError("stale KanidmBackupSpec manifest_key assertion remains in discovery tests")
discovery.write_text(text)


# The beta backup catalog no longer accepts arbitrary object-store keys. Admission
# validates immutable historical source identity instead; repository/path validation
# is performed when the controller derives the key.
webhook = ROOT / "cmd/webhook/src/handlers.rs"
text = webhook.read_text()
old_webhook = '''    if object.spec.manifest_key.is_empty() {
        return Json(review.response(AdmissionResponse::deny(uid, "manifestKey is required")));
    }

    if object.spec.manifest_key.contains("..") {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "manifestKey contains path traversal",
        )));
    }
'''
new_webhook = '''    if object.spec.source.namespace.is_empty()
        || object.spec.source.kanidm_name.is_empty()
        || object.spec.source.kanidm_uid.is_empty()
    {
        return Json(review.response(AdmissionResponse::deny(
            uid,
            "source namespace, kanidmName, and kanidmUid are required",
        )));
    }
'''
if text.count(old_webhook) != 1:
    raise RuntimeError(f"stale webhook manifestKey validation count: {text.count(old_webhook)}")
webhook.write_text(text.replace(old_webhook, new_webhook, 1))


# Example generator must emit the beta catalog shape so `make examples` remains authoritative.
example_backup = ROOT / "cmd/examples/src/backup.rs"
text = example_backup.read_text()
text, count = re.subn(
    r'\n\s*manifest_key: "v1/tenants/a81c/clusters/9e630aed/backups/019c7c76/manifest\.json"\n\s*\.to_string\(\),',
    "",
    text,
    count=1,
)
if count != 1:
    raise RuntimeError(f"backup example manifest_key removal count: {count}")
# The main beta transform already rewrites the source constructor. Do not couple this
# finalizer to its exact whitespace/type spelling; compilation and generated examples
# validate the transformed API. Preserve the transformed historical namespace value here.
example_backup.write_text(text)

example_restore = ROOT / "cmd/examples/src/kanidm_restore.rs"
replace_exact(
    example_restore,
    "                backup_ref: None,\n            },",
    "                backup_ref: None,\n                external_backup: None,\n            },",
)

# One E2E hardening fixture constructs a local restore source directly.
restore_hardening = ROOT / "tests/tests/restore_hardening.rs"
replace_exact(
    restore_hardening,
    "                backup_ref: None,\n            },\n            restore_image:",
    "                backup_ref: None,\n                external_backup: None,\n            },\n            restore_image:",
)

print("backup beta compile/test/example/webhook migration applied")
