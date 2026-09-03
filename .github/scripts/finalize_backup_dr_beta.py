#!/usr/bin/env python3
from __future__ import annotations

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
APPLY = ROOT / ".github/scripts/apply_backup_dr_beta.py"


def pre() -> None:
    lines = APPLY.read_text().splitlines(keepends=True)

    # Change only the KanidmBackup print column; Schedule remains alpha.
    for i, line in enumerate(lines):
        if "printcolumn = r#" in line and "kanidmRef.name" in line:
            lines[i] = (
                "text = read(path)\n"
                "old = '.spec.kanidmRef.name'\n"
                "idx = text.rfind(old)\n"
                "if idx < 0:\n"
                "    raise RuntimeError('KanidmBackup printcolumn not found')\n"
                "write(path, text[:idx] + '.spec.source.kanidmName' + text[idx + len(old):])\n"
            )
            if i + 1 < len(lines) and "spec.source.kanidmName" in lines[i + 1]:
                lines[i + 1] = ""
            break
    else:
        raise RuntimeError("KanidmBackup printcolumn patch not found")

    # Inject derived manifest key only into build_validation_job.
    start = next(
        (
            i
            for i, line in enumerate(lines)
            if line.strip() == "replace(path,"
            and i + 1 < len(lines)
            and "'''    let ca_bundle_path = spec.s3.ca_bundle_ref" in lines[i + 1]
        ),
        None,
    )
    if start is None:
        raise RuntimeError("validation manifest-key injection block not found")
    end = start
    while end < len(lines) and "''', 1)" not in lines[end]:
        end += 1
    if end >= len(lines):
        raise RuntimeError("validation manifest-key injection block end not found")
    lines[start : end + 1] = [
        "regex(path,\n",
        "      r'''(pub fn build_validation_job\\(.*?let ca_bundle_path = spec\\.s3\\.ca_bundle_ref\\.as_ref\\(\\)\\.map\\(\\|_\\| ca_bundle_path\\(\\)\\);\\n)''',\n",
        "      r'''\\1    let manifest_key = backup_manifest_key(backup, repository)?;\\n''', count=1)\n",
    ]

    # Do not rewrite kanidm_ref fields on unrelated CRDs.
    for i, line in enumerate(lines):
        if "text = text.replace('.spec.kanidm_ref.uid'" in line:
            lines[i] = ""
        elif "text = text.replace('.spec.kanidm_ref.name'" in line:
            lines[i] = ""

    # namespace_uid -> namespace is a protocol change, not a tree-wide Rust rename.
    for i, line in enumerate(lines):
        if "text = path_obj.read_text().replace('namespace_uid:', 'namespace:')" in line:
            lines[i] = "    text = path_obj.read_text()\n"

    # Remove manifest_key only from KanidmBackupSpec initializers.
    comment = next(
        (i for i, line in enumerate(lines) if "# Legacy field names in test literals." in line),
        None,
    )
    if comment is None:
        raise RuntimeError("legacy manifest-key cleanup comment not found")
    for i in range(comment + 1, min(comment + 8, len(lines))):
        if "re.sub" in lines[i] and "manifest_key" in lines[i]:
            lines[i] = (
                "    text = re.sub(r'(KanidmBackupSpec\\s*\\{(?:(?!\\n\\s*\\}).)*?)"
                "\\n\\s*manifest_key:\\s*[^,\\n]+,', r'\\1', text, flags=re.DOTALL)\n"
            )
            break
    else:
        raise RuntimeError("broad manifest-key cleanup not found")

    APPLY.write_text("".join(lines))


def migrate_restore_tests(path: Path) -> tuple[int, int, int, int]:
    text = path.read_text()
    text = text.replace(
        "fn build_download_operation_doc(\n    restore: &KanidmRestore,",
        "fn build_download_operation_doc(\n    _restore: &KanidmRestore,",
        1,
    )

    source_re = re.compile(
        r"(?P<indent>[ \t]*)(?:kanidm_ref|source): "
        r"kaniop_backup_core::crd::(?:BackupKanidmRef|BackupSource) \{\n"
        r"(?P<inner>[ \t]*)name: (?P<name>[^,\n]+),\n"
        r"(?P=inner)uid: (?P<uid>[^,\n]+),\n(?P=indent)\},"
    )

    def source_repl(m: re.Match[str]) -> str:
        return (
            f'{m.group("indent")}source: kaniop_backup_core::crd::BackupSource {{\n'
            f'{m.group("inner")}namespace: "default".to_string(),\n'
            f'{m.group("inner")}kanidm_name: {m.group("name")},\n'
            f'{m.group("inner")}kanidm_uid: {m.group("uid")},\n'
            f'{m.group("indent")}}},'
        )

    text, migrated_sources = source_re.subn(source_repl, text)

    lines = text.splitlines(keepends=True)
    removed_manifest_keys = 0
    i = 0
    while i < len(lines):
        if "KanidmBackupSpec {" not in lines[i]:
            i += 1
            continue
        depth = 0
        j = i
        while j < len(lines):
            depth += lines[j].count("{") - lines[j].count("}")
            if j > i and depth == 0:
                break
            j += 1
        k = i + 1
        while k < j:
            if lines[k].lstrip().startswith("manifest_key:"):
                del lines[k]
                j -= 1
                removed_manifest_keys += 1
            else:
                k += 1
        i = j + 1
    text = "".join(lines)

    lines = text.splitlines(keepends=True)
    added_external_none = 0
    i = 0
    while i < len(lines):
        line = lines[i]
        if "KanidmRestoreSource {" not in line or line.lstrip().startswith("pub struct KanidmRestoreSource"):
            i += 1
            continue
        depth = 0
        j = i
        while j < len(lines):
            depth += lines[j].count("{") - lines[j].count("}")
            if j > i and depth == 0:
                break
            j += 1
        if "external_backup:" not in "".join(lines[i : j + 1]):
            indent = re.match(r"[ \t]*", line).group(0)
            lines.insert(j, f"{indent}    external_backup: None,\n")
            added_external_none += 1
            j += 1
        i = j + 1
    text = "".join(lines)

    call_re = re.compile(
        r'(build_download_operation_doc\((?:(?!\n[ \t]*\);).)*?\n'
        r'(?P<indent>[ \t]*)"[A-Za-z0-9-]+",\n)'
        r'(?P=indent)(?P<boolean>true|false),',
        flags=re.DOTALL,
    )
    text, migrated_calls = call_re.subn(
        lambda m: m.group(1)
        + m.group("indent")
        + '"uid",\n'
        + m.group("indent")
        + m.group("boolean")
        + ",",
        text,
    )
    path.write_text(text)
    return migrated_sources, removed_manifest_keys, added_external_none, migrated_calls


def post() -> None:
    # ResultDocument failure constructor.
    path = ROOT / "libs/backup-core/src/result.rs"
    text = path.read_text()
    text, count = re.subn(
        r"(pub fn failure\(.*?image_digest: None,\n)(\s*error: Some\(ResultError \{)",
        r"\1            source_namespace: None,\n            source_kanidm_name: None,\n            source_kanidm_uid: None,\n\2",
        text,
        count=1,
        flags=re.DOTALL,
    )
    if count != 1:
        raise RuntimeError(f"failure provenance patch count: {count}")
    path.write_text(text)

    # Final stale alpha-only CRD test field.
    path = ROOT / "libs/backup-core/src/crd.rs"
    text = path.read_text()
    stale = '            manifest_key: "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),\n'
    if text.count(stale) != 1:
        raise RuntimeError(f"stale CRD manifest_key count: {text.count(stale)}")
    path.write_text(text.replace(stale, "", 1))

    # Backup controller deletion derives the prefix from historical source identity.
    path = ROOT / "libs/backup/src/controller/backup.rs"
    text = path.read_text().replace(
        "let kanidm_name = &backup.spec.kanidm_ref.name;",
        "let kanidm_name = &backup.spec.source.kanidm_name;",
    )
    text, count = re.subn(
        r"    let manifest_key = &obj\.spec\.manifest_key;\n.*?"
        r"    if !repo_path\.contains_prefix\(&backup_prefix\) \{\n.*?    \}\n\n",
        '''    let backup_prefix = repo_path
        .backup_path(
            &obj.spec.source.namespace,
            &obj.spec.source.kanidm_uid,
            &obj.spec.backup_id,
        )
        .map_err(|e| Error::MissingData(format!("invalid backup path: {e}")))?;

''',
        text,
        count=1,
        flags=re.DOTALL,
    )
    if count != 1:
        raise RuntimeError(f"backup deletion path migration count: {count}")
    path.write_text(text)

    # Discovery: historical namespace is a namespace name, and catalog comparison
    # is by backup ID rather than the removed public manifest key.
    path = ROOT / "libs/backup/src/controller/discovery.rs"
    text = path.read_text()
    old_param = "    namespace_uid: &str,\n    kanidm_uid: &str,\n) -> Job {"
    if text.count(old_param) == 1:
        text = text.replace(
            old_param,
            "    source_namespace: &str,\n    kanidm_uid: &str,\n) -> Job {",
            1,
        )
    text = text.replace(
        "if existing_manifest_keys.contains(manifest_key) {",
        "if existing_manifest_keys.contains(&backup_id) {",
    )
    old_call = '''        let backup_cr = build_backup_cr(
            manifest_key,
            &backup_id,
            &repo_name,
            kanidm_name,
            kanidm_uid,
        );'''
    new_call = '''        let backup_cr = build_backup_cr(
            &backup_id,
            &repo_name,
            namespace,
            kanidm_name,
            kanidm_uid,
        );'''
    if text.count(old_call) != 1:
        raise RuntimeError(f"build_backup_cr call migration count: {text.count(old_call)}")
    text = text.replace(old_call, new_call, 1)
    old_sig = '''fn build_backup_cr(
    manifest_key: &str,
    backup_id: &str,
    repository_name: &str,
    kanidm_name: &str,
    kanidm_uid: &str,
) -> KanidmBackup {'''
    new_sig = '''fn build_backup_cr(
    backup_id: &str,
    repository_name: &str,
    source_namespace: &str,
    kanidm_name: &str,
    kanidm_uid: &str,
) -> KanidmBackup {'''
    if text.count(old_sig) != 1:
        raise RuntimeError(f"build_backup_cr signature migration count: {text.count(old_sig)}")
    text = text.replace(old_sig, new_sig, 1)
    fn_start = text.index("fn build_backup_cr(")
    fn_end = text.index("\n}\n\nfn merge_conditions", fn_start)
    block = text[fn_start:fn_end]
    if "namespace: source_namespace.to_string()," not in block:
        if "namespace: namespace.to_string()," not in block:
            raise RuntimeError("build_backup_cr source namespace assignment not found")
        block = block.replace(
            "namespace: namespace.to_string(),",
            "namespace: source_namespace.to_string(),",
            1,
        )
    text = text[:fn_start] + block + text[fn_end:]
    path.write_text(text)

    counts = migrate_restore_tests(ROOT / "libs/operator/src/kanidm/restore/legacy.rs")

    # Backup-specific E2E references only.
    replacements = {
        "tests/e2e/test/kanidm/backup_transport.rs": [("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name")],
        "tests/e2e/test/kanidm/mod.rs": [("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name")],
        "tests/e2e/test/kanidm/backup.rs": [
            ("b.spec.kanidm_ref.name", "b.spec.source.kanidm_name"),
            ("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name"),
        ],
        "tests/e2e/test/kanidm/restore.rs": [("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name")],
    }
    for file_name, pairs in replacements.items():
        p = ROOT / file_name
        current = p.read_text()
        for old, new in pairs:
            current = current.replace(old, new)
        p.write_text(current)

    # Protect unrelated CRDs from accidental migration.
    for file_name in [
        "libs/backup/src/controller/schedule.rs",
        "libs/oauth2/src/reconcile/status.rs",
        "tests/e2e/test/kanidm_ref.rs",
    ]:
        if ".spec.source.kanidm_name" in (ROOT / file_name).read_text():
            raise RuntimeError(f"unrelated kanidmRef migration leaked into {file_name}")

    print(
        "post-migration fixes:",
        f"restore_backup_sources={counts[0]}",
        f"restore_manifest_keys={counts[1]}",
        f"external_none={counts[2]}",
        f"download_calls={counts[3]}",
    )


def main() -> None:
    if len(sys.argv) != 2 or sys.argv[1] not in {"pre", "post"}:
        raise SystemExit("usage: finalize_backup_dr_beta.py pre|post")
    pre() if sys.argv[1] == "pre" else post()


if __name__ == "__main__":
    main()
