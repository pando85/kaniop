"""Produce a Markdown scorecard from file-based agent evaluation results."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import statistics
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


def load_results(directory: Path) -> dict[str, dict[str, Any]]:
    results: dict[str, dict[str, Any]] = {}
    for path in sorted(directory.glob("*.json")):
        with path.open(encoding="utf-8") as source:
            result = json.load(source)
        case_id = result.get("case_id")
        if not isinstance(case_id, str) or not case_id:
            raise ValueError(f"{path}: missing case_id")
        if case_id in results:
            raise ValueError(f"{directory}: duplicate result for {case_id}")
        results[case_id] = result
    if not results:
        raise ValueError(f"{directory}: no result JSON files")
    return results


def expected_skills() -> dict[str, set[str]]:
    cases: dict[str, set[str]] = {}
    for path in (ROOT / "evals/cases").glob("*/case.json"):
        if path.parent.name == "_template":
            continue
        with path.open(encoding="utf-8") as source:
            case = json.load(source)
        cases[case["id"]] = set(case["expected_skills"])
    return cases


def percentage(numerator: int, denominator: int) -> str:
    return "n/a" if not denominator else f"{100 * numerator / denominator:.1f}%"


def metrics(results: dict[str, dict[str, Any]]) -> dict[str, str]:
    outcomes = [result["outcome"] for result in results.values()]
    findings = [
        finding
        for result in results.values()
        for finding in result["evidence"].get("findings", [])
    ]
    tokens = [
        outcome.get("input_tokens", 0) + outcome.get("output_tokens", 0)
        for outcome in outcomes
    ]

    expected = expected_skills()
    expected_count = 0
    loaded_expected_count = 0
    for case_id, result in results.items():
        case_expected = expected.get(case_id, set())
        loaded = set(result["evidence"].get("skills_loaded", []))
        expected_count += len(case_expected)
        loaded_expected_count += len(case_expected & loaded)

    return {
        "Cases": str(len(results)),
        "Pass rate": percentage(sum(item["passed"] for item in outcomes), len(outcomes)),
        "First-pass success": percentage(
            sum(item["passed"] and item["attempts"] == 1 for item in outcomes),
            len(outcomes),
        ),
        "Critical findings": str(sum(item.get("severity") == "critical" for item in findings)),
        "Important findings": str(sum(item.get("severity") == "important" for item in findings)),
        "Skill-selection recall": percentage(loaded_expected_count, expected_count),
        "Median attempts": f"{statistics.median(item['attempts'] for item in outcomes):g}",
        "Median duration (s)": f"{statistics.median(item['duration_seconds'] for item in outcomes):g}",
        "Median tokens": f"{statistics.median(tokens):g}",
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    arguments = parser.parse_args()

    try:
        baseline = load_results(arguments.baseline)
        candidate = load_results(arguments.candidate)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 1

    if baseline.keys() != candidate.keys():
        print("baseline and candidate must contain the same case IDs", file=sys.stderr)
        return 1

    baseline_metrics = metrics(baseline)
    candidate_metrics = metrics(candidate)
    regressions = sorted(
        case_id
        for case_id in baseline
        if baseline[case_id]["outcome"]["passed"]
        and not candidate[case_id]["outcome"]["passed"]
    )

    print("# Agent evaluation summary\n")
    print("| Metric | Baseline | Candidate |")
    print("|---|---:|---:|")
    for name in baseline_metrics:
        print(f"| {name} | {baseline_metrics[name]} | {candidate_metrics[name]} |")
    print(f"| Regressions | 0 | {len(regressions)} |")
    print("\n## Regressed cases\n")
    if regressions:
        for case_id in regressions:
            print(f"- `{case_id}`")
    else:
        print("None.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
