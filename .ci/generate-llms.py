"""Generate the Kaniop llms.txt documentation index from mdBook SUMMARY.md."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
SUMMARY = ROOT / "Documentation/src/SUMMARY.md"
TEMPLATE = ROOT / "Documentation/llms.template.txt"
OUTPUT = ROOT / "Documentation/llms.txt"
PLACEHOLDER = "{{DOCUMENTATION_INDEX}}"
LINK = re.compile(r"^(\\s*)(?:-\\s+)?\\[([^\\]]+)\\]\\(([^)]+\\.md)\\)\\s*$")
RAW_BASE = (
    "https://raw.githubusercontent.com/pando85/kaniop/"
    "{{KANIOP_REPO_REF}}/Documentation/src/"
)


def documentation_index() -> str:
    entries: list[str] = []
    for line in SUMMARY.read_text(encoding="utf-8").splitlines():
        match = LINK.match(line)
        if not match:
            continue
        whitespace, title, path = match.groups()
        depth = len(whitespace) // 2
        entries.append(f"{'  ' * depth}- [{title}]({RAW_BASE}{path})")
    if not entries:
        raise ValueError(f"no documentation links found in {SUMMARY.relative_to(ROOT)}")
    return "\\n".join(entries)


def render() -> str:
    template = TEMPLATE.read_text(encoding="utf-8")
    if template.count(PLACEHOLDER) != 1:
        raise ValueError(
            f"{TEMPLATE.relative_to(ROOT)} must contain {PLACEHOLDER!r} exactly once"
        )
    return template.replace(PLACEHOLDER, documentation_index()).rstrip() + "\\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when Documentation/llms.txt differs from generated output",
    )
    args = parser.parse_args()

    generated = render()
    if args.check:
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.exists() else ""
        if current != generated:
            print(
                "Documentation/llms.txt is stale; run make llms and commit the result.",
                file=sys.stderr,
            )
            return 1
        print("Documentation/llms.txt is up to date")
        return 0

    OUTPUT.write_text(generated, encoding="utf-8")
    print("generated Documentation/llms.txt from Documentation/src/SUMMARY.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
