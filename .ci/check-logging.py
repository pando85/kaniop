#!/usr/bin/env python3
"""Enforce Kaniop's canonical tracing message convention."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

MACRO_RE = re.compile(
    r"(?<![\w:])(?:(?:tracing)::)?(?:trace|debug|info|warn|error)!\s*\("
)
FORBIDDEN_FIELD_RE = re.compile(r"\b(?:msg|message)\s*=")


def macro_body(text: str, opening_paren: int) -> tuple[str, int]:
    """Return a tracing macro body and the offset after its closing parenthesis."""
    depth = 1
    i = opening_paren + 1
    start = i
    state = "code"
    block_comment_depth = 0

    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""

        if state == "code":
            if ch == '"':
                state = "string"
            elif ch == "'":
                state = "char"
            elif ch == "/" and nxt == "/":
                state = "line_comment"
                i += 1
            elif ch == "/" and nxt == "*":
                state = "block_comment"
                block_comment_depth = 1
                i += 1
            elif ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    return text[start:i], i + 1
        elif state == "string":
            if ch == "\\":
                i += 1
            elif ch == '"':
                state = "code"
        elif state == "char":
            if ch == "\\":
                i += 1
            elif ch == "'":
                state = "code"
        elif state == "line_comment":
            if ch == "\n":
                state = "code"
        elif state == "block_comment":
            if ch == "/" and nxt == "*":
                block_comment_depth += 1
                i += 1
            elif ch == "*" and nxt == "/":
                block_comment_depth -= 1
                i += 1
                if block_comment_depth == 0:
                    state = "code"

        i += 1

    return text[start:], len(text)


def violations_in_text(text: str) -> list[tuple[int, str]]:
    found: list[tuple[int, str]] = []

    for macro in MACRO_RE.finditer(text):
        opening_paren = macro.end() - 1
        body, _ = macro_body(text, opening_paren)
        for field in FORBIDDEN_FIELD_RE.finditer(body):
            absolute = opening_paren + 1 + field.start()
            line = text.count("\n", 0, absolute) + 1
            found.append((line, field.group(0)))

    return found


def violations(path: Path) -> list[tuple[int, str]]:
    return violations_in_text(path.read_text(encoding="utf-8"))


def self_test() -> None:
    """Fail closed if the scanner stops detecting supported legacy forms."""
    forbidden = (
        'info!(msg = "legacy");',
        'tracing::warn!(message = "legacy");',
        'debug!(\n    controller = "test",\n    msg = "legacy",\n);',
        'error!(\n    error = %err,\n    message = format!("legacy {err}"),\n);',
    )
    allowed = (
        'info!("canonical message");',
        'warn!(controller = "test", "canonical message");',
        'let msg = "not a tracing field";',
    )

    for sample in forbidden:
        if not violations_in_text(sample):
            raise AssertionError(f"logging checker failed to reject: {sample!r}")
    for sample in allowed:
        if violations_in_text(sample):
            raise AssertionError(f"logging checker rejected valid source: {sample!r}")


def rust_sources() -> list[Path]:
    """Return tracked and non-ignored untracked Rust sources in the checkout.

    Including untracked sources is intentional: CI creates a temporary multiline
    legacy tracing fixture and requires this checker to reject it. Ignored build
    output (for example `target/`) remains excluded.
    """
    result = subprocess.run(
        [
            "git",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    return sorted(
        ROOT / path.decode("utf-8")
        for path in result.stdout.split(b"\0")
        if path
    )


def main() -> int:
    self_test()
    failures: list[str] = []

    for path in rust_sources():
        for line, field in violations(path):
            rel = path.relative_to(ROOT)
            failures.append(f"{rel}:{line}: forbidden tracing field `{field}`")

    if not failures:
        return 0

    print("Logging convention violations found:", file=sys.stderr)
    for failure in failures:
        print(f"  {failure}", file=sys.stderr)
    print(
        "Use the trailing tracing message syntax; see Documentation/logging.md.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
