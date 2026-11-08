#!/usr/bin/env python3
"""Compare a memory-controls report with the frozen envelope.

Behaviour that no longer holds is a regression: a missing case, a cap
that did not close the third connection, connection 129 refused, an
overlapping write admitted, idle residue above 8 MiB, or workload
connection refusals. Those checks have no improvement.

Numeric checks are lower-is-better. A regression clears the top of the
envelope by both a band and a floor. An improvement clears the bottom
the same way. RSS uses 15% and 1 MiB. Each family is gated on its RSS
ratio and its cgroup ratio: the frozen min and max, and 5% outside that
range. Slopes in an older envelope are ignored. Workload limited counts
use a band of 0 and a floor of 2: fewer refusals is an improvement and
needs a fresh envelope. An improvement warns and exits 0 (ADR-0544).
A regression, a mixed run, and an incompatible class in CI still exit 1.

An environment-class or knob mismatch is incompatible. With CI_COMPARE=1
or inside GitHub Actions that is a hard failure. A local run skips the
numeric bands and still judges behaviour. Until a freeze is merged the
envelope is unfrozen and the verdict is no-baseline, which exits 0.

The markdown document is written before the process exits.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys
from pathlib import Path

_ENV = Path(__file__).resolve().parent / "loadtest_env.py"
_spec = importlib.util.spec_from_file_location("loadtest_env", _ENV)
_env = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
_spec.loader.exec_module(_env)

RSS_BAND = float(os.environ.get("MEMORY_CONTROLS_RSS_BAND", "0.15"))
RSS_FLOOR = float(os.environ.get("MEMORY_CONTROLS_RSS_FLOOR_BYTES", str(1024 * 1024)))
CALIBRATION_BAND = float(os.environ.get("MEMORY_CONTROLS_CALIBRATION_BAND", "0.05"))
LIMITED_BAND = float(os.environ.get("MEMORY_CONTROLS_LIMITED_BAND", "0"))
LIMITED_FLOOR = float(os.environ.get("MEMORY_CONTROLS_LIMITED_FLOOR", "2"))
IDLE_RESIDUE = 8 * 1024 * 1024


def format_mem(value: float) -> str:
    """1024-based. GB once the magnitude reaches 1024 MB."""
    number = float(value)
    mb = abs(number) / (1024 * 1024)
    sign = "-" if number < 0 else ""
    if mb >= 1024.0:
        return f"{sign}{mb / 1024:.2f} GB"
    return f"{sign}{mb:.2f} MB"

RSS_METRICS = {
    "graph_rss_bytes",
    "processing_rss_min_bytes",
    "processing_rss_max_bytes",
    "session_cost_bytes",
}
LIMITED_METRICS = {"reads_limited", "writes_limited"}
CALIBRATION_METRICS = {
    "max_rss_to_logical_ratio",
    "max_cgroup_to_logical_ratio",
}
# Observation field that produced the family ratio, then the bytes it retained.
RATIO_BYTES = {
    "max_rss_to_logical_ratio": ("rss_to_logical_ratio", "rss_delta_bytes"),
    "max_cgroup_to_logical_ratio": ("cgroup_to_logical_ratio", "cgroup_delta_bytes"),
}
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

IMPROVEMENT_NOTE = (
    "Investigate whether this improvement is a false positive. "
    "When it is real, re-freeze the envelope with the create-loadtest-baselines workflow."
)


def ci_hard_fail() -> bool:
    return os.environ.get("CI_COMPARE") == "1" or _env.in_github_actions()


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def cases_by_name(report: dict) -> dict[str, dict]:
    return {case.get("name"): case for case in report.get("cases") or []}


def workloads_by_name(report: dict) -> dict[str, dict]:
    found = {}
    for workload in report.get("workloads") or []:
        name = (workload.get("case") or {}).get("name")
        if name:
            found[name] = workload
    return found


def scenario_of(report: dict, name: str) -> dict:
    case = cases_by_name(report).get(name)
    if case and case.get("scenario"):
        return case["scenario"]
    workload = workloads_by_name(report).get(name)
    if workload and workload.get("scenario"):
        return workload["scenario"]
    if name != "workspace-memory-calibration":
        calibration = cases_by_name(report).get("workspace-memory-calibration")
        if calibration and name in {
            row.get("family")
            for row in (report.get("workspace_memory_calibration") or {}).get("summaries") or []
        }:
            return (calibration or {}).get("scenario") or {}
    return (case or {}).get("scenario") or {}


def band_text() -> str:
    percent = CALIBRATION_BAND * 100.0
    if abs(percent - round(percent)) < 1e-9:
        return f"{int(round(percent))}%"
    return f"{percent:.1f}%"


def retained_bytes(report: dict, family: str, metric: str) -> int | None:
    """Bytes retained by the observation that set this family's ratio."""
    spec = RATIO_BYTES.get(metric)
    if spec is None:
        return None
    ratio_key, byte_key = spec
    best_ratio = None
    best_bytes = None
    observations = (report.get("workspace_memory_calibration") or {}).get("observations") or []
    for row in observations:
        if family != "overall" and row.get("family") != family:
            continue
        ratio = row.get(ratio_key)
        size = row.get(byte_key)
        if ratio is None or size is None:
            continue
        ratio = float(ratio)
        if best_ratio is None or ratio > best_ratio:
            best_ratio = ratio
            best_bytes = int(size)
    return best_bytes


def ratio_sentence(row: dict) -> str:
    label = "cgroup" if "cgroup" in str(row.get("metric")) else "RSS"
    bounds = f"{float(row['min']):.3f}x–{float(row['max']):.3f}x"
    measured = f"{float(row['value']):.3f}x"
    sentence = (
        f"Expected {label} ratio {bounds}. "
        f"A reading may sit {band_text()} outside that range. "
    )
    if row.get("retained_bytes") is not None:
        sentence += f"Measured {format_mem(row['retained_bytes'])}, ratio {measured}."
    else:
        sentence += f"Measured ratio {measured}."
    return sentence


def band_for(metric: str) -> tuple[float, float]:
    if metric in LIMITED_METRICS:
        return LIMITED_BAND, LIMITED_FLOOR
    if metric in CALIBRATION_METRICS:
        return CALIBRATION_BAND, 0.0
    return RSS_BAND, RSS_FLOOR


def judge_lower(value: float, bounds: dict, metric: str) -> str:
    band, floor = band_for(metric)
    low = float(bounds["min"])
    high = float(bounds["max"])
    if value > high * (1.0 + band) and value > high + floor:
        return "regression"
    if value < low * (1.0 - band) and value < low - floor:
        return "improvement"
    return "ok"


def check(name: str, metric: str, verdict: str, **detail) -> dict:
    row = {"name": name, "metric": metric, "verdict": verdict}
    row.update(detail)
    return row


def behaviour_checks(baseline: dict, report: dict) -> list[dict]:
    rows: list[dict] = []
    cases = cases_by_name(report)
    workloads = workloads_by_name(report)
    behaviour = baseline.get("behaviour") or {}
    for name, expect in behaviour.items():
        if name == "workload":
            if not workloads:
                rows.append(check("workload", "presence", "regression", detail="report is missing the workload"))
                continue
            for workload_name, workload in workloads.items():
                got = workload.get("connections_refused")
                wanted = expect.get("connections_refused")
                verdict = "ok" if got == wanted else "regression"
                rows.append(
                    check(
                        workload_name,
                        "connections_refused",
                        verdict,
                        value=got,
                        expected=wanted,
                        scenario=workload.get("scenario") or {},
                    )
                )
            continue
        case = cases.get(name)
        if case is None:
            rows.append(check(name, "presence", "regression", detail="report is missing the case"))
            continue
        scenario = case.get("scenario") or {}
        if name == "concurrent-logical-workspace-limit":
            failure = case.get("failure") or ""
            needle = expect.get("failure_contains") or ""
            limit = expect.get("limit")
            held = needle in failure and (limit is None or f"limit={limit}" in failure)
            rows.append(
                check(
                    name,
                    "behaviour",
                    "ok" if held else "regression",
                    value=failure,
                    expected=f"{needle} limit={limit}",
                    scenario=scenario,
                )
            )
        else:
            phrases = {
                "connection-cap": ("third connection was closed", "later connection was accepted"),
                "unlimited-connections": ("connection 129 was accepted",),
            }.get(name, ())
            outcome = scenario.get("outcome") or ""
            failure_ok = case.get("failure") == expect.get("failure")
            phrases_ok = all(phrase in outcome for phrase in phrases)
            rows.append(
                check(
                    name,
                    "behaviour",
                    "ok" if failure_ok and phrases_ok else "regression",
                    value=outcome,
                    expected=expect.get("failure"),
                    scenario=scenario,
                )
            )
        idle = case.get("idle_rss_bytes") or []
        if idle:
            first = idle[0]
            growth = max(sample - first for sample in idle)
            rows.append(
                check(
                    name,
                    "idle-residue",
                    "ok" if growth <= IDLE_RESIDUE else "regression",
                    value=growth,
                    expected=IDLE_RESIDUE,
                    scenario=scenario,
                )
            )
    for workload_name, workload in workloads.items():
        idle = (workload.get("case") or {}).get("idle_rss_bytes") or []
        if not idle:
            continue
        growth = max(sample - idle[0] for sample in idle)
        rows.append(
            check(
                workload_name,
                "idle-residue",
                "ok" if growth <= IDLE_RESIDUE else "regression",
                value=growth,
                expected=IDLE_RESIDUE,
                scenario=workload.get("scenario") or {},
            )
        )
    return rows


def numeric_checks(baseline: dict, report: dict) -> list[dict]:
    rows: list[dict] = []
    cases = cases_by_name(report)
    for name, metrics in (baseline.get("cases") or {}).items():
        case = cases.get(name)
        if case is None:
            continue
        measured = {
            "graph_rss_bytes": int(case["baseline_rss_bytes"]) + int(case["floor_rss_bytes"]),
            "processing_rss_min_bytes": case.get("processing_rss_min_bytes"),
            "processing_rss_max_bytes": case.get("processing_rss_max_bytes"),
            "session_cost_bytes": case.get("session_cost_bytes"),
        }
        for metric, bounds in metrics.items():
            value = measured.get(metric)
            if value is None:
                rows.append(check(name, metric, "regression", detail="report is missing the metric"))
                continue
            rows.append(
                check(
                    name,
                    metric,
                    judge_lower(float(value), bounds, metric),
                    value=value,
                    min=bounds["min"],
                    max=bounds["max"],
                    scenario=case.get("scenario") or {},
                )
            )
    workloads = workloads_by_name(report)
    for name, metrics in (baseline.get("workloads") or {}).items():
        workload = workloads.get(name)
        if workload is None:
            rows.append(check(name, "presence", "regression", detail="report is missing the workload"))
            continue
        case = workload.get("case") or {}
        measured = {
            "graph_rss_bytes": int(case["baseline_rss_bytes"]) + int(case["floor_rss_bytes"]),
            "processing_rss_min_bytes": case.get("processing_rss_min_bytes"),
            "processing_rss_max_bytes": case.get("processing_rss_max_bytes"),
            "session_cost_bytes": case.get("session_cost_bytes"),
            "reads_succeeded": workload.get("reads_succeeded"),
            "writes_succeeded": workload.get("writes_succeeded"),
            "reads_limited": workload.get("reads_limited"),
            "writes_limited": workload.get("writes_limited"),
        }
        for metric, bounds in metrics.items():
            if metric not in LIMITED_METRICS and metric not in RSS_METRICS:
                continue
            value = measured.get(metric)
            if value is None:
                rows.append(check(name, metric, "regression", detail="report is missing the metric"))
                continue
            if metric not in LIMITED_METRICS and metric not in RSS_METRICS:
                continue
            rows.append(
                check(
                    name,
                    metric,
                    judge_lower(float(value), bounds, metric if metric in LIMITED_METRICS else "graph_rss_bytes"),
                    value=value,
                    min=bounds["min"],
                    max=bounds["max"],
                    scenario=workload.get("scenario") or {},
                )
            )
    summaries = {
        row.get("family"): row
        for row in (report.get("workspace_memory_calibration") or {}).get("summaries") or []
    }
    calibration_case = cases.get("workspace-memory-calibration") or {}
    for family, metrics in (baseline.get("calibration") or {}).items():
        summary = summaries.get(family)
        if summary is None:
            rows.append(check(family, "presence", "regression", detail="report is missing the calibration family"))
            continue
        for metric, bounds in metrics.items():
            if metric not in CALIBRATION_METRICS:
                continue
            value = summary.get(metric)
            if value is None:
                rows.append(check(family, metric, "regression", detail="report is missing the metric"))
                continue
            rows.append(
                check(
                    family,
                    metric,
                    judge_lower(float(value), bounds, metric),
                    value=value,
                    min=bounds["min"],
                    max=bounds["max"],
                    retained_bytes=retained_bytes(report, family, metric),
                    scenario=calibration_case.get("scenario") or {},
                )
            )
    return rows


def knobs_of(report: dict) -> dict:
    controls = report.get("controls") or {}
    return {field: controls.get(field) for field in KNOB_FIELDS}


def overall(rows: list[dict], incompatible: bool, hard: bool) -> str:
    kinds = {row["verdict"] for row in rows if row["verdict"] != "ok"}
    if "regression" in kinds and "improvement" in kinds:
        return "mixed"
    if "regression" in kinds:
        return "regression"
    if "improvement" in kinds:
        return "improvement"
    if incompatible:
        return "incompatible"
    return "ok"


def render(status: str, rows: list[dict], note: str) -> str:
    lines = ["# Memory controls and workspace memory calibration", "", f"**Status:** {status}", ""]
    if note:
        lines.extend([note, ""])
    if status == "ok":
        lines.append("All memory controls and workspace memory calibrations held. 🟢")
        lines.append("")
        seen = []
        for row in rows:
            if row["name"] not in seen:
                seen.append(row["name"])
        for name in seen:
            lines.append(f"- {name}")
        lines.extend(calibration_lines(rows))
        return "\n".join(lines)
    failing = [row for row in rows if row["verdict"] != "ok"]
    if not failing and status == "no-baseline":
        lines.append("The envelope is not frozen yet. This run is a warning, and the job stays green.")
        lines.append("")
        return "\n".join(lines)
    if not failing and status == "incompatible":
        lines.append("The environment class does not match the envelope, so the numeric bands were not applied.")
        lines.append("")
        return "\n".join(lines)
    for row in failing:
        lines.append(f"## {row['name']} — {row['metric']}")
        lines.append("")
        lines.append(f"**Verdict:** {row['verdict']}")
        lines.append("")
        scenario = row.get("scenario") or {}
        if scenario:
            lines.append(f"**Given:** {scenario.get('given', '')}")
            lines.append("")
            lines.append(f"**When:** {scenario.get('when', '')}")
            lines.append("")
            lines.append(f"**Then:** {scenario.get('then', '')}")
            lines.append("")
        metric = str(row.get("metric") or "")
        if metric in CALIBRATION_METRICS and "min" in row:
            lines.append(f"**Outcome:** {ratio_sentence(row)}")
            lines.append("")
            if row["verdict"] == "improvement":
                lines.append(IMPROVEMENT_NOTE)
                lines.append("")
            continue
        if scenario:
            lines.append(f"**Outcome:** {scenario.get('outcome', '')}")
            lines.append("")
        size = metric.endswith("_bytes") or metric == "idle-residue"
        if "min" in row:
            if size:
                shown = (
                    f"Measured {format_mem(row['value'])} against the envelope "
                    f"{format_mem(row['min'])} – {format_mem(row['max'])}."
                )
            else:
                shown = (
                    f"Measured {row.get('value')} against the envelope "
                    f"{row.get('min')} – {row.get('max')}."
                )
            lines.append(shown)
        elif row.get("detail"):
            lines.append(str(row["detail"]))
        elif "expected" in row:
            if size:
                lines.append(
                    f"Measured {format_mem(row['value'])} against {format_mem(row['expected'])}."
                )
            else:
                lines.append(f"Measured {row.get('value')!r} against {row.get('expected')!r}.")
        lines.append("")
        if row["verdict"] == "improvement":
            lines.append(IMPROVEMENT_NOTE)
            lines.append("")
    return "\n".join(lines)


def calibration_lines(rows: list[dict]) -> list[str]:
    chosen = [row for row in rows if row.get("metric") in CALIBRATION_METRICS and "min" in row]
    if not chosen:
        return []
    lines = ["", "Workspace memory calibration", ""]
    for row in chosen:
        lines.append(f"- {row['name']}: {ratio_sentence(row)}")
    lines.append("")
    return lines


def verdict(baseline: dict, report: dict) -> tuple[str, list[dict], str]:
    if not baseline or not baseline.get("frozen"):
        return "no-baseline", [], "No frozen envelope is on this branch yet."
    if baseline.get("schema_version") != 1 or report.get("schema_version") != 4:
        return (
            "regression",
            [check("report", "schema", "regression", detail="the report or the envelope schema is not the one this gate reads")],
            "",
        )
    rows = behaviour_checks(baseline, report)
    mismatches = _env.environment_mismatches(baseline.get("environment"), report.get("environment"))
    knob_mismatch = knobs_of(report) != (baseline.get("knobs") or {})
    incompatible = bool(mismatches) or knob_mismatch
    note = ""
    if incompatible:
        detail = mismatches[0].message if mismatches else "knobs differ from the envelope"
        note = f"Environment class mismatch ({detail}). Numeric bands were skipped."
        if ci_hard_fail():
            rows.append(check("environment", "class", "incompatible", detail=detail))
    else:
        rows.extend(numeric_checks(baseline, report))
    status = overall(rows, incompatible, ci_hard_fail())
    if status == "incompatible" and not ci_hard_fail():
        note = (note + " " if note else "") + "Local compare keeps the behaviour checks and skips the numeric bands."
    return status, rows, note.strip()


def exit_code(status: str) -> int:
    if status in {"regression", "mixed"}:
        return 1
    if status == "incompatible" and ci_hard_fail():
        return 1
    return 0


def emit_checks(prefix: str, rows: list[dict]) -> None:
    for row in rows:
        if row["verdict"] == "ok":
            continue
        message = f"{prefix}: {row['name']} {row['metric']} {row['verdict']}"
        if row["verdict"] == "improvement" and _env.in_github_actions():
            print(f"::warning::{message}", file=sys.stderr, flush=True)
        else:
            print(message, file=sys.stderr, flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--baseline", type=Path, default=Path("baselines/memory-controls.json"))
    parser.add_argument("--report", type=Path, default=Path("target/memory-controls.json"))
    parser.add_argument("--doc", type=Path, default=Path("target/memory-controls.md"))
    parser.add_argument("--verdict", type=Path, default=Path("target/memory-controls-verdict.json"))
    args = parser.parse_args()

    if not args.baseline.is_file() or not args.report.is_file():
        missing = args.baseline if not args.baseline.is_file() else args.report
        status, rows, note = "no-baseline", [], f"{missing} is not present, so there is nothing to compare yet."
        if args.baseline.is_file() and not args.report.is_file():
            note = f"{args.report} is not present. The memory-controls run did not write a report."
            status = "regression"
            rows = [check("report", "presence", "regression", detail=note)]
            note = ""
    else:
        status, rows, note = verdict(load(args.baseline), load(args.report))

    document = render(status, rows, note)
    args.doc.parent.mkdir(parents=True, exist_ok=True)
    args.verdict.parent.mkdir(parents=True, exist_ok=True)
    args.doc.write_text(document)
    args.verdict.write_text(
        json.dumps({"status": status, "checks": rows}, indent=2) + "\n"
    )
    print(f"compare-memory-controls: {status}")
    emit_checks("compare-memory-controls", rows)
    return exit_code(status)


if __name__ == "__main__":
    sys.exit(main())
