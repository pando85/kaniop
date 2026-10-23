"""Validate Jekyll documentation navigation against mdBook SUMMARY.md.

The Jekyll sidebar sorts by weight and groups pages using indent.
It supports only one submenu level; deeper mdBook entries are flattened.
"""

from __future__ import annotations

from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "Documentation/src"
LINK = re.compile(r"^(?P<spaces> *)(?:- )?\[[^]]+\]\((?P<path>[^)]+\.md)\)\s*$")
FIELD = re.compile(r"^(?P<key>[a-zA-Z][\w-]*):\s*(?P<value>.*?)\s*$")


def read_metadata(path: Path) -> dict[str, str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    if not lines or lines[0] != "---":
        raise ValueError("missing YAML frontmatter")
    try:
        end = lines.index("---", 1)
    except ValueError as error:
        raise ValueError("unterminated YAML frontmatter") from error

    metadata: dict[str, str] = {}
    for line in lines[1:end]:
        match = FIELD.fullmatch(line)
        if match:
            key, value = match.group("key", "value")
            if key in metadata:
                raise ValueError(f"duplicate frontmatter field {key!r}")
            metadata[key] = value
    return metadata


def validate(source: Path) -> list[str]:
    summary = source / "SUMMARY.md"
    if not summary.is_file():
        return [f"{summary}: file not found"]

    errors: list[str] = []
    seen: set[str] = set()
    previous_weight = -1
    for line_number, line in enumerate(summary.read_text(encoding="utf-8").splitlines(), 1):
        match = LINK.fullmatch(line)
        if not match:
            if "](" in line and ".md" in line:
                errors.append(f"SUMMARY.md:{line_number}: malformed page link")
            continue

        spaces, target = match.group("spaces", "path")
        if len(spaces) % 2:
            errors.append(f"SUMMARY.md:{line_number}: use two spaces per nesting level")
        depth = len(spaces) // 2
        relative = Path(target)
        if relative.is_absolute() or ".." in relative.parts:
            errors.append(f"SUMMARY.md:{line_number}: invalid page path {target!r}")
            continue
        if target in seen:
            errors.append(f"SUMMARY.md:{line_number}: duplicate page {target!r}")
            continue
        seen.add(target)
        page = source / relative
        if not page.is_file():
            errors.append(f"SUMMARY.md:{line_number}: missing page {target!r}")
            continue

        try:
            metadata = read_metadata(page)
        except (OSError, ValueError) as error:
            errors.append(f"{target}: {error}")
            continue

        title = metadata.get("title", "").strip().strip("\"'").strip()
        if not title or title in ("~", "null", "Null", "NULL"):
            errors.append(f"{target}: missing or empty frontmatter title")

        raw_weight = metadata.get("weight", "")
        if not re.fullmatch(r"\d+", raw_weight):
            errors.append(f"{target}: weight must be a non-negative integer")
        else:
            weight = int(raw_weight)
            if weight <= previous_weight:
                errors.append(
                    f"{target}: weight {weight} must be greater than "
                    f"the preceding page's weight {previous_weight} (SUMMARY.md order)"
                )
            previous_weight = weight

        indent = metadata.get("indent")
        if indent not in (None, "true", "false"):
            errors.append(f"{target}: indent must be a YAML boolean (true/false)")
        elif (indent == "true") != (depth > 0):
            errors.append(
                f"{target}: indent must be {'true' if depth else 'false or absent'} "
                "to match SUMMARY.md nesting"
            )

    if not seen:
        errors.append("SUMMARY.md: no linked Markdown pages")
    for page in sorted(source.rglob("*.md")):
        relative = page.relative_to(source).as_posix()
        if relative != "SUMMARY.md" and relative not in seen:
            errors.append(f"{relative}: documentation page missing from SUMMARY.md")
    return errors


def main() -> int:
    errors = validate(SOURCE)
    if errors:
        print("Documentation navigation validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("Documentation navigation matches SUMMARY.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
