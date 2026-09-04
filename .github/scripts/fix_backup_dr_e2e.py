#!/usr/bin/env python3
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
E2E_DIR = ROOT / "tests/e2e/test/kanidm"


def block_end(lines: list[str], start: int) -> int:
    depth = 0
    for index in range(start, len(lines)):
        depth += lines[index].count("{") - lines[index].count("}")
        if index > start and depth == 0:
            return index
    raise RuntimeError(f"unterminated Rust initializer starting at line {start + 1}")


def migrate_restore_sources(path: Path) -> int:
    lines = path.read_text().splitlines(keepends=True)
    migrated = 0
    i = 0
    while i < len(lines):
        line = lines[i]
        if "KanidmRestoreSource {" not in line or line.lstrip().startswith("pub struct KanidmRestoreSource"):
            i += 1
            continue
        end = block_end(lines, i)
        block = "".join(lines[i : end + 1])
        if "external_backup:" not in block:
            field_indent = None
            for candidate in lines[i + 1 : end]:
                stripped = candidate.strip()
                if stripped and not stripped.startswith("//"):
                    field_indent = candidate[: len(candidate) - len(candidate.lstrip())]
                    break
            if field_indent is None:
                raise RuntimeError(f"{path}: could not determine restore source field indentation")
            lines.insert(end, f"{field_indent}external_backup: None,\n")
            migrated += 1
            end += 1
        i = end + 1
    path.write_text("".join(lines))
    return migrated


def migrate_backup_specs(path: Path) -> tuple[int, int]:
    text = path.read_text()
    source_re = re.compile(
        r"(?P<indent>[ \t]*)kanidm_ref: BackupKanidmRef \{\n"
        r"(?P<inner>[ \t]*)name: (?P<name>[^,\n]+),\n"
        r"(?P=inner)uid: (?P<uid>[^,\n]+),\n"
        r"(?P=indent)\},"
    )

    def source_repl(match: re.Match[str]) -> str:
        return (
            f'{match.group("indent")}source: BackupSource {{\n'
            f'{match.group("inner")}namespace: "default".to_string(),\n'
            f'{match.group("inner")}kanidm_name: {match.group("name")},\n'
            f'{match.group("inner")}kanidm_uid: {match.group("uid")},\n'
            f'{match.group("indent")}}},'
        )

    text, source_count = source_re.subn(source_repl, text)
    if "kanidm_ref: BackupKanidmRef {" in text:
        raise RuntimeError(f"{path}: an old KanidmBackup source initializer was not migrated")
    if source_count and "BackupKanidmRef" in text:
        # At this point the old type can only remain in imports/comments for these E2E files.
        text = text.replace("BackupKanidmRef", "BackupSource")

    lines = text.splitlines(keepends=True)
    removed_manifest_keys = 0
    i = 0
    while i < len(lines):
        if "KanidmBackupSpec {" not in lines[i]:
            i += 1
            continue
        end = block_end(lines, i)
        j = i + 1
        while j < end:
            if lines[j].lstrip().startswith("manifest_key:"):
                del lines[j]
                end -= 1
                removed_manifest_keys += 1
            else:
                j += 1
        i = end + 1

    path.write_text("".join(lines))
    return source_count, removed_manifest_keys


totals = {"restore_sources": 0, "backup_sources": 0, "manifest_keys": 0}
for path in sorted(E2E_DIR.glob("*.rs")):
    totals["restore_sources"] += migrate_restore_sources(path)
    source_count, manifest_count = migrate_backup_specs(path)
    totals["backup_sources"] += source_count
    totals["manifest_keys"] += manifest_count

# The shared helper keeps the legacy manifest-key parameter only so existing callers do not
# need churn; the beta catalog derives that key internally.
mod_path = E2E_DIR / "mod.rs"
text = mod_path.read_text()
old = "    manifest_key: &str,\n) -> String {\n    use kaniop_backup_core::crd::{"
if old not in text:
    raise RuntimeError("create_backup_cr_and_wait manifest_key parameter was not found")
text = text.replace(
    old,
    "    _manifest_key: &str,\n) -> String {\n    use kaniop_backup_core::crd::{",
    1,
)
mod_path.write_text(text)

# Preserve the E2E immutability assertion, but mutate immutable source provenance instead of
# the removed user-controlled object-store key.
backup_path = E2E_DIR / "backup.rs"
text = backup_path.read_text()
old = '        updated_backup.spec.manifest_key = "e2e/test/manifest-v2.json".to_string();'
new = '''        updated_backup.spec.source.kanidm_uid =
            "11111111-1111-1111-1111-111111111111".to_string();'''
if text.count(old) != 1:
    raise RuntimeError(f"backup immutability manifest-key mutation count: {text.count(old)}")
backup_path.write_text(text.replace(old, new, 1))

# Discovery no longer exposes a storage key in KanidmBackup. Assert the immutable source and
# repository identity that discovery is responsible for materializing.
transport_path = E2E_DIR / "backup_transport.rs"
text = transport_path.read_text()
old = '''            let found = list.items.iter().any(|b| {
                b.spec.kanidm_ref.name == kanidm_name
                    && b.spec.manifest_key.contains("e2e-transport/")
                    && b.spec.manifest_key.ends_with("/manifest.json")
            });'''
new = '''            let found = list.items.iter().any(|b| {
                b.spec.source.kanidm_name == kanidm_name
                    && b.spec.source.namespace == "default"
                    && b.spec.repository_ref.name == repo_name
            });'''
if text.count(old) != 1:
    raise RuntimeError(f"backup transport catalog assertion count: {text.count(old)}")
transport_path.write_text(text.replace(old, new, 1))

# Fail the migration before Rust compilation if an E2E test still references removed catalog fields.
for path in sorted(E2E_DIR.glob("*.rs")):
    text = path.read_text()
    if ".spec.manifest_key" in text:
        raise RuntimeError(f"{path}: stale .spec.manifest_key reference remains")
    if ".spec.kanidm_ref.uid" in text and "KanidmBackup" in text:
        # Do not blanket-rewrite schedule kanidmRef; surface any ambiguous residue explicitly.
        raise RuntimeError(f"{path}: stale/ambiguous backup .spec.kanidm_ref.uid reference remains")

print(
    "backup beta E2E migration applied:",
    f"restore_sources={totals['restore_sources']}",
    f"backup_sources={totals['backup_sources']}",
    f"manifest_keys={totals['manifest_keys']}",
)
