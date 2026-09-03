#!/usr/bin/env python3
from __future__ import annotations

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
APPLY = ROOT / ".github/scripts/apply_backup_dr_beta.py"


def pre() -> None:
    lines = APPLY.read_text().splitlines(keepends=True)

    # Schedule and KanidmBackup share the same alpha print-column text. Change
    # only the KanidmBackup occurrence (the final occurrence in crd.rs).
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

    # The CA-bundle prelude exists in both validation and deletion builders.
    # Replace only the validation manifest-key injection block and preserve the
    # separate Result<Job> signature transformation.
    start = None
    for i, line in enumerate(lines):
        if (
            line.strip() == "replace(path,"
            and i + 1 < len(lines)
            and "'''    let ca_bundle_path = spec.s3.ca_bundle_ref" in lines[i + 1]
        ):
            start = i
            break
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

    # The tree-wide beta compatibility pass was over-broad. kanidm_ref is a
    # common field on unrelated CRDs, so only explicitly migrated backup code
    # may change it.
    for i, line in enumerate(lines):
        if "text = text.replace('.spec.kanidm_ref.uid'" in line:
            lines[i] = ""
        elif "text = text.replace('.spec.kanidm_ref.name'" in line:
            lines[i] = ""

    # namespace_uid -> namespace is a protocol vocabulary correction, not a
    # global Rust identifier rewrite. Protocol-bearing files are already listed
    # explicitly near the beginning of the implementation script.
    for i, line in enumerate(lines):
        if "text = path_obj.read_text().replace('namespace_uid:', 'namespace:')" in line:
            lines[i] = "    text = path_obj.read_text()\n"

    # Only KanidmBackupSpec loses public manifest_key. Keep manifest keys in
    # operation/result/internal structs.
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


def replace_exact(path: Path, old: str, new: str, expected: int = 1) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != expected:
        raise RuntimeError(f"{path}: expected {expected} occurrences of {old!r}, found {count}")
    path.write_text(text.replace(old, new, expected))


def migrate_restore_tests(path: Path) -> tuple[int, int, int, int]:
    text = path.read_text()

    old = "fn build_download_operation_doc(\n    restore: &KanidmRestore,"
    if text.count(old) == 1:
        text = text.replace(old, "fn build_download_operation_doc(\n    _restore: &KanidmRestore,", 1)

    backup_source_pattern = re.compile(
        r"(?P<indent>[ \t]*)(?:kanidm_ref|source): "
        r"kaniop_backup_core::crd::(?:BackupKanidmRef|BackupSource) \{\n"
        r"(?P<inner>[ \t]*)name: (?P<name>[^,\n]+),\n"
        r"(?P=inner)uid: (?P<uid>[^,\n]+),\n"
        r"(?P=indent)\},"
    )

    def replace_backup_source(match: re.Match[str]) -> str:
        indent = match.group("indent")
        inner = match.group("inner")
        return (
            f"{indent}source: kaniop_backup_core::crd::BackupSource {{\n"
            f'{inner}namespace: "default".to_string(),\n'
            f'{inner}kanidm_name: {match.group("name")},\n'
            f'{inner}kanidm_uid: {match.group("uid")},\n'
            f"{indent}}},"
        )

    text, migrated_sources = backup_source_pattern.subn(replace_backup_source, text)

    # Remove manifest_key only from KanidmBackupSpec initializer blocks.
    lines = text.splitlines(keepends=True)
    i = 0
    removed_manifest_keys = 0
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
        if j >= len(lines):
            raise RuntimeError("unterminated KanidmBackupSpec initializer")
        k = i + 1
        while k < j:
            if lines[k].lstrip().startswith("manifest_key:"):
                del lines[k]
                j -= 1
                removed_manifest_keys += 1
                continue
            k += 1
        i = j + 1
    text = "".join(lines)

    # Every source literal must initialize the new third union arm.
    lines = text.splitlines(keepends=True)
    i = 0
    added_external_none = 0
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
        if j >= len(lines):
            raise RuntimeError("unterminated KanidmRestoreSource initializer")
        block = "".join(lines[i : j + 1])
        if "external_backup:" not in block:
            opening_indent = re.match(r"[ \t]*", line).group(0)
            lines.insert(j, f"{opening_indent}    external_backup: None,\n")
            added_external_none += 1
            j += 1
        i = j + 1
    text = "".join(lines)

    # Old tests call the download helper without expected historical UID.
    download_call_pattern = re.compile(
        r'(build_download_operation_doc\((?:(?!\n[ \t]*\);).)*?\n'
        r'(?P<indent>[ \t]*)"[A-Za-z0-9-]+",\n)'
        r'(?P=indent)(?P<boolean>true|false),',
        flags=re.DOTALL,
    )

    def add_expected_uid(match: re.Match[str]) -> str:
        return (
            match.group(1)
            + match.group("indent")
            + '"uid",\n'
            + match.group("indent")
            + match.group("boolean")
            + ","
        )

    text, migrated_calls = download_call_pattern.subn(add_expected_uid, text)
    path.write_text(text)
    return migrated_sources, removed_manifest_keys, added_external_none, migrated_calls


def post() -> None:
    # ResultDocument::failure mirrors success provenance defaults.
    path = ROOT / "libs/backup-core/src/result.rs"
    text = path.read_text()
    pattern = r"(pub fn failure\(.*?image_digest: None,\n)(\s*error: Some\(ResultError \{)"
    replacement = (
        r"\1"
        "            source_namespace: None,\n"
        "            source_kanidm_name: None,\n"
        "            source_kanidm_uid: None,\n"
        r"\2"
    )
    text, count = re.subn(pattern, replacement, text, count=1, flags=re.DOTALL)
    if count != 1:
        raise RuntimeError(f"failure constructor provenance patch count: {count}")
    path.write_text(text)

    replace_exact(
        ROOT / "libs/backup-core/src/crd.rs",
        '            manifest_key: "v1/tenants/ns/clusters/k/backups/b/manifest.json".to_string(),\n',
        "",
    )

    # Backup controller derives all remote paths from immutable catalog identity.
    path = ROOT / "libs/backup/src/controller/backup.rs"
    text = path.read_text()
    text = text.replace(
        "let kanidm_name = &backup.spec.kanidm_ref.name;",
        "let kanidm_name = &backup.spec.source.kanidm_name;",
    )
    deletion_pattern = re.compile(
        r"    let manifest_key = &obj\.spec\.manifest_key;\n"
        r".*?"
        r"    if !repo_path\.contains_prefix\(&backup_prefix\) \{\n"
        r".*?"
        r"    \}\n\n",
        flags=re.DOTALL,
    )
    deletion_replacement = '''    let backup_prefix = repo_path
        .backup_path(
            &obj.spec.source.namespace,
            &obj.spec.source.kanidm_uid,
            &obj.spec.backup_id,
        )
        .map_err(|e| Error::MissingData(format!("invalid backup path: {e}")))?;

'''
    text, count = deletion_pattern.subn(deletion_replacement, text, count=1)
    if count != 1:
        raise RuntimeError(f"backup deletion path migration count: {count}")
    path.write_text(text)

    # Discovery uses namespace NAME as the historical source path component.
    path = ROOT / "libs/backup/src/controller/discovery.rs"
    text = path.read_text()
    old_param = "    namespace_uid: &str,\n    kanidm_uid: &str,\n) -> Job {"
    if text.count(old_param) != 1:
        raise RuntimeError(f"discover job source namespace parameter count: {text.count(old_param)}")
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
    if "namespace: namespace.to_string()," not in block:
        raise RuntimeError("build_backup_cr source namespace assignment not found")
    block = block.replace(
        "namespace: namespace.to_string(),",
        "namespace: source_namespace.to_string(),",
        1,
    )
    text = text[:fn_start] + block + text[fn_end:]
    path.write_text(text)

    restore_counts = migrate_restore_tests(ROOT / "libs/operator/src/kanidm/restore/legacy.rs")

    # Only backup-specific E2E references migrate from kanidmRef to source.
    e2e_replacements = {
        "tests/e2e/test/kanidm/backup_transport.rs": [
            ("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name"),
        ],
        "tests/e2e/test/kanidm/mod.rs": [
            ("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name"),
        ],
        "tests/e2e/test/kanidm/backup.rs": [
            ("b.spec.kanidm_ref.name", "b.spec.source.kanidm_name"),
            ("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name"),
        ],
        "tests/e2e/test/kanidm/restore.rs": [
            ("backup.spec.kanidm_ref.name", "backup.spec.source.kanidm_name"),
        ],
    }
    for file_name, replacements in e2e_replacements.items():
        file_path = ROOT / file_name
        current = file_path.read_text()
        for old, new in replacements:
            current = current.replace(old, new)
        file_path.write_text(current)

    # Guard against the original over-broad migration recurring.
    forbidden = {
        "libs/backup/src/controller/schedule.rs": ".spec.source.kanidm_name",
        "libs/oauth2/src/reconcile/status.rs": ".spec.source.kanidm_name",
        "tests/e2e/test/kanidm_ref.rs": ".spec.source.kanidm_name",
    }
    for file_name, needle in forbidden.items():
        if needle in (ROOT / file_name).read_text():
            raise RuntimeError(f"unrelated kanidmRef migration leaked into {file_name}")

    print(
        "post-migration fixes:",
        f"restore_backup_sources={restore_counts[0]}",
        f"restore_manifest_keys={restore_counts[1]}",
        f"external_none={restore_counts[2]}",
        f"download_calls={restore_counts[3]}",
    )


def main() -> None:
    if len(sys.argv) != 2 or sys.argv[1] not in {"pre", "post"}:
        raise SystemExit("usage: finalize_backup_dr_beta.py pre|post")
    if sys.argv[1] == "pre":
        pre()
    else:
        post()


if __name__ == "__main__":
    main()
