#!/usr/bin/env python3
"""Fold repeated memory-controls reports into a min/max envelope.

Each round is one full-profile run. Behaviour has to be identical across
rounds: the connection cap's success token, connection 129's acceptance,
the workspace-limit refusal including its limit number, and zero workload
connection refusals. Numeric metrics keep the min and max seen. Each
calibration family keeps its RSS ratio and its cgroup ratio. The
compare script applies its bands on top of that range.

The envelope is schema 1. A report that is missing its environment class,
a required case, or a stable behavioural result is refused.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REQUIRED_CASES = (
    "connection-cap",
    "unlimited-connections",
    "concurrent-logical-workspace-limit",
)
REQUIRED_WORKLOADS = ("workload-indexed", "workload-scan")
CASE_METRICS = (
    "graph_rss_bytes",
    "processing_rss_min_bytes",
    "processing_rss_max_bytes",
    "session_cost_bytes",
)
WORKLOAD_COUNTS = (
    "reads_succeeded",
    "writes_succeeded",
    "reads_limited",
    "writes_limited",
)
CALIBRATION_METRICS = (
    "max_rss_to_logical_ratio",
    "max_cgroup_to_logical_ratio",
)
KNOB_FIELDS = (
    "graph_scale",
    "workers",
    "read_parts",
    "write_parts",
    "max_connections",
    "concurrent_logical_workspace_limit",
    "ops",
    "warmup_ops",
    "seed",
    "index_mode",
    "read_query",
)
LIMIT = re.compile(r"limit=(\d+)")
OUTCOME_PHRASES = {
    "connection-cap": (
        "third connection was closed",
        "later connection was accepted",
    ),
    "unlimited-connections": ("connection 129 was accepted",),
}


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def cases_by_name(report: dict) -> dict[str, dict]:
    return {case["name"]: case for case in report.get("cases") or []}


def workloads_by_name(report: dict) -> dict[str, dict]:
    found = {}
    for workload in report.get("workloads") or []:
        name = (workload.get("case") or {}).get("name")
        if name:
            found[name] = workload
    return found


def graph_rss(case: dict) -> int:
    return int(case["baseline_rss_bytes"]) + int(case["floor_rss_bytes"])


def case_numbers(case: dict) -> dict[str, int]:
    return {
        "graph_rss_bytes": graph_rss(case),
        "processing_rss_min_bytes": int(case["processing_rss_min_bytes"]),
        "processing_rss_max_bytes": int(case["processing_rss_max_bytes"]),
        "session_cost_bytes": int(case["session_cost_bytes"]),
    }


def knobs_of(report: dict) -> dict:
    controls = report.get("controls") or {}
    return {field: controls.get(field) for field in KNOB_FIELDS}


def check(reports: list[tuple[Path, dict]]) -> list[str]:
    problems: list[str] = []
    first_path, first = reports[0]
    if first.get("schema_version") != 4 or first.get("name") != "memory-controls":
        problems.append(f"{first_path}: expected memory-controls schema 4")
    if not isinstance(first.get("environment"), dict):
        problems.append(f"{first_path}: missing environment")
    knobs = knobs_of(first)
    if any(value is None for value in knobs.values()):
        problems.append(f"{first_path}: controls are missing a knob")
    behaviour = behaviour_of(first_path, first, problems)
    for path, report in reports:
        if report.get("schema_version") != first.get("schema_version"):
            problems.append(f"{path}: schema_version differs from {first_path}")
        if report.get("environment") != first.get("environment"):
            problems.append(f"{path}: environment differs from {first_path}")
        if knobs_of(report) != knobs:
            problems.append(f"{path}: knobs differ from {first_path}")
        if behaviour_of(path, report, problems) != behaviour:
            problems.append(f"{path}: behaviour differs from {first_path}")
    return problems


def behaviour_of(path: Path, report: dict, problems: list[str]) -> dict | None:
    cases = cases_by_name(report)
    workloads = workloads_by_name(report)
    for name in REQUIRED_CASES:
        if name not in cases:
            problems.append(f"{path}: missing case {name}")
            return None
    for name in REQUIRED_WORKLOADS:
        if name not in workloads:
            problems.append(f"{path}: missing workload {name}")
            return None
    cap = cases["connection-cap"]
    unlimited = cases["unlimited-connections"]
    workspace = cases["concurrent-logical-workspace-limit"]
    cap_outcome = (cap.get("scenario") or {}).get("outcome") or ""
    unlimited_outcome = (unlimited.get("scenario") or {}).get("outcome") or ""
    for phrase in OUTCOME_PHRASES["connection-cap"]:
        if phrase not in cap_outcome:
            problems.append(f"{path}: connection-cap outcome missing {phrase!r}")
            return None
    for phrase in OUTCOME_PHRASES["unlimited-connections"]:
        if phrase not in unlimited_outcome:
            problems.append(f"{path}: unlimited-connections outcome missing {phrase!r}")
            return None
    failure = workspace.get("failure") or ""
    match = LIMIT.search(failure)
    if "concurrent-logical-workspace-limit" not in failure or match is None:
        problems.append(f"{path}: workspace limit was not refused with a limit number")
        return None
    for workload in workloads.values():
        if workload.get("connections_refused") != 0:
            problems.append(f"{path}: workload connection refusals are not 0")
            return None
    return {
        "connection-cap": {"failure": cap.get("failure")},
        "unlimited-connections": {"failure": unlimited.get("failure")},
        "concurrent-logical-workspace-limit": {
            "failure_contains": "concurrent-logical-workspace-limit",
            "limit": int(match.group(1)),
        },
        "workload": {"connections_refused": 0},
    }


def span(values: list) -> dict:
    return {"min": min(values), "max": max(values)}


def envelope(reports: list[tuple[Path, dict]]) -> dict:
    _, first = reports[-1]
    cases: dict[str, dict] = {}
    for name in REQUIRED_CASES:
        grouped = {metric: [] for metric in CASE_METRICS}
        for _, report in reports:
            numbers = case_numbers(cases_by_name(report)[name])
            for metric, value in numbers.items():
                grouped[metric].append(value)
        cases[name] = {metric: span(values) for metric, values in grouped.items()}

    workloads: dict[str, dict] = {}
    for name in REQUIRED_WORKLOADS:
        grouped = {metric: [] for metric in (*CASE_METRICS, *WORKLOAD_COUNTS)}
        for _, report in reports:
            workload = workloads_by_name(report)[name]
            numbers = case_numbers(workload["case"])
            numbers.update({count: int(workload[count]) for count in WORKLOAD_COUNTS})
            for metric, value in numbers.items():
                grouped[metric].append(value)
        workloads[name] = {metric: span(values) for metric, values in grouped.items()}

    families: dict[str, dict] = {}
    names = [
        summary["family"]
        for summary in (first.get("workspace_memory_calibration") or {}).get("summaries") or []
        if summary.get("family") != "overall"
    ]
    for family in names:
        grouped: dict[str, list] = {metric: [] for metric in CALIBRATION_METRICS}
        present = {metric: True for metric in CALIBRATION_METRICS}
        for path, report in reports:
            summary = next(
                (
                    row
                    for row in (report.get("workspace_memory_calibration") or {}).get("summaries")
                    or []
                    if row.get("family") == family
                ),
                None,
            )
            if summary is None:
                raise SystemExit(f"{path}: missing calibration family {family}")
            for metric in CALIBRATION_METRICS:
                value = summary.get(metric)
                if value is None:
                    present[metric] = False
                    continue
                grouped[metric].append(value)
        families[family] = {
            metric: span(values) for metric, values in grouped.items() if present[metric] and values
        }

    problems: list[str] = []
    behaviour = behaviour_of(reports[0][0], reports[0][1], problems)
    if behaviour is None:
        raise SystemExit("freeze-memory-controls: behaviour did not survive the check")
    return {
        "schema_version": 1,
        "name": "memory-controls",
        "frozen": True,
        "rounds": len(reports),
        "environment": first["environment"],
        "knobs": knobs_of(first),
        "behaviour": behaviour,
        "cases": cases,
        "workloads": workloads,
        "calibration": families,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("reports", nargs="+", type=Path)
    args = parser.parse_args()
    loaded = [(path, load(path)) for path in args.reports]
    problems = check(loaded)
    if problems:
        for problem in problems:
            print(f"freeze-memory-controls: {problem}", file=sys.stderr)
        return 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(envelope(loaded), indent=2) + "\n")
    print(f"freeze-memory-controls: wrote {args.out} from {len(loaded)} rounds")
    return 0


if __name__ == "__main__":
    sys.exit(main())
