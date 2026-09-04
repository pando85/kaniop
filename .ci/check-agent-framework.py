"""Validate Kaniop's file-based agent framework without external dependencies."""

from __future__ import annotations

import json
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
SKILL_NAME = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


def frontmatter(path: Path) -> dict[str, str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    if not lines or lines[0] != "---":
        raise ValueError("missing opening frontmatter delimiter")
    try:
        end = lines.index("---", 1)
    except ValueError as error:
        raise ValueError("missing closing frontmatter delimiter") from error

    values: dict[str, str] = {}
    for line in lines[1:end]:
        key, separator, value = line.partition(":")
        if separator and not line.startswith((" ", "\t")):
            values[key.strip()] = value.strip()
    return values


def load_json(path: Path) -> object:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def validate_case(path: Path, skills: set[str]) -> list[str]:
    relative = path.relative_to(ROOT)
    try:
        case = load_json(path)
    except (OSError, json.JSONDecodeError) as error:
        return [f"{relative}: invalid JSON: {error}"]

    if not isinstance(case, dict):
        return [f"{relative}: case must be a JSON object"]

    errors: list[str] = []
    required = {
        "schema_version",
        "id",
        "title",
        "source",
        "input",
        "expected_skills",
        "verification",
    }
    missing = sorted(required - case.keys())
    if missing:
        errors.append(f"{relative}: missing keys: {', '.join(missing)}")
        return errors

    case_id = case["id"]
    if not isinstance(case_id, str) or not SKILL_NAME.fullmatch(case_id):
        errors.append(f"{relative}: invalid id")
    if path.parent.name != "_template" and case_id != path.parent.name:
        errors.append(f"{relative}: id must match directory")
    if case["schema_version"] != 1:
        errors.append(f"{relative}: schema_version must be 1")

    inputs = case["input"]
    if not isinstance(inputs, dict):
        errors.append(f"{relative}: input must be an object")
    else:
        task = inputs.get("task")
        if not isinstance(task, str) or not (path.parent / task).is_file():
            errors.append(f"{relative}: input.task must name an existing file")
        if not isinstance(inputs.get("repository_commit"), str):
            errors.append(f"{relative}: input.repository_commit must be a string")

    expected_skills = case["expected_skills"]
    if not isinstance(expected_skills, list) or not all(
        isinstance(skill, str) for skill in expected_skills
    ):
        errors.append(f"{relative}: expected_skills must be a string array")
    elif path.parent.name != "_template":
        for skill in sorted(set(expected_skills) - skills):
            errors.append(f"{relative}: unknown expected skill {skill}")

    verification = case["verification"]
    if not isinstance(verification, dict):
        errors.append(f"{relative}: verification must be an object")
    else:
        for key in ("commands", "required_properties", "forbidden_changes"):
            value = verification.get(key)
            if not isinstance(value, list) or not value or not all(
                isinstance(item, str) and item for item in value
            ):
                errors.append(f"{relative}: verification.{key} must be a non-empty string array")

    return errors


def main() -> int:
    errors: list[str] = []
    skills = {
        path.parent.name for path in (ROOT / ".opencode/skills").glob("*/SKILL.md")
    }

    for path in sorted((ROOT / ".opencode/skills").glob("*/SKILL.md")):
        try:
            metadata = frontmatter(path)
            name = metadata.get("name", "")
            if not SKILL_NAME.fullmatch(name):
                errors.append(f"{path.relative_to(ROOT)}: invalid or missing name")
            if name != path.parent.name:
                errors.append(f"{path.relative_to(ROOT)}: name must match directory")
            if not metadata.get("description"):
                errors.append(f"{path.relative_to(ROOT)}: missing description")
        except (OSError, ValueError) as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")

    for path in sorted((ROOT / ".opencode/agents").glob("*.md")):
        try:
            metadata = frontmatter(path)
            if not metadata.get("description"):
                errors.append(f"{path.relative_to(ROOT)}: missing description")
            if metadata.get("mode") != "subagent":
                errors.append(f"{path.relative_to(ROOT)}: mode must be subagent")
        except (OSError, ValueError) as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")

    json_files = [
        ROOT / "opencode.json",
        ROOT / "evals/schema/case.schema.json",
        ROOT / "evals/schema/result.schema.json",
        ROOT / "evals/results/_template/result.json",
    ]
    for path in json_files:
        try:
            load_json(path)
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"{path.relative_to(ROOT)}: invalid JSON: {error}")

    for path in sorted((ROOT / "evals/cases").glob("*/case.json")):
        errors.extend(validate_case(path, skills))

    required_artifacts = {
        ROOT / "intent/_template/intent.md": ("## Problem", "## Proposed outcome"),
        ROOT / "intent/_template/spec.md": ("## Required behavior", "## Acceptance criteria"),
        ROOT / "intent/_template/plan.md": ("## Files and components", "## Verification"),
    }
    for path, headings in required_artifacts.items():
        try:
            content = path.read_text(encoding="utf-8")
            for heading in headings:
                if heading not in content:
                    errors.append(f"{path.relative_to(ROOT)}: missing {heading}")
        except OSError as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")

    try:
        configuration = load_json(ROOT / "opencode.json")
        edit_rules = configuration["permission"]["edit"]  # type: ignore[index]
        for path in ("charts/kaniop/crds/crds.yaml", "examples/**"):
            if edit_rules.get(path) != "deny":
                errors.append(f"opencode.json: edits to {path} must be denied")
    except (KeyError, TypeError) as error:
        errors.append(f"opencode.json: missing generated-file permissions: {error}")

    if errors:
        print("Agent framework validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    skill_count = len(list((ROOT / ".opencode/skills").glob("*/SKILL.md")))
    case_count = len(
        [path for path in (ROOT / "evals/cases").glob("*/case.json") if "_template" not in path.parts]
    )
    print(f"Agent framework valid: {skill_count} skills, {case_count} eval cases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
