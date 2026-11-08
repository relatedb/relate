#!/usr/bin/env python3
"""Compare a memory-shape report with the frozen envelope.

A shape that does not have its fixed node and relationship counts is a
regression. There is no improvement verdict for counts.

Numeric checks are lower-is-better. A regression clears the top of the
envelope by both a band and a floor. An improvement clears the bottom the
same way. Logical bytes, disk, and attributed RSS use 15% and 1 MB. The
ratio uses 5%. An improvement warns and exits 0 (ADR-0544). A regression
and a mixed run still fail. The document still asks for a re-freeze when
the drop is real.

An environment-class or knob mismatch is incompatible. With CI_COMPARE=1
or inside GitHub Actions that is a hard failure. A local run skips the
numeric bands and still judges the counts. Until a freeze is merged the
envelope is unfrozen and the verdict is no-baseline, which exits 0.

The markdown document is written before the process exits. Every shape
section states the baseline, the validated run, and the report against
those boundaries.
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

BYTE_BAND = float(os.environ.get("MEMORY_SHAPE_BYTE_BAND", "0.15"))
BYTE_FLOOR = float(os.environ.get("MEMORY_SHAPE_BYTE_FLOOR_BYTES", str(1024 * 1024)))
RATIO_BAND = float(os.environ.get("MEMORY_SHAPE_RATIO_BAND", "0.05"))
BYTE_METRICS = ("logical_bytes", "disk_bytes", "attributed_rss_bytes")
KNOBS = (
    "logical_target_bytes",
    "attributes",
    "text_indexes",
    "point_indexes",
    "range_indexes",
)
IMPROVEMENT_NOTE = (
    "Investigate whether this improvement is a false positive. "
    "When it is real, re-freeze the envelope with the create-loadtest-baselines workflow."
)


def format_mem(value: float) -> str:
    """1024-based. GB once the magnitude reaches 1024 MB."""
    number = float(value)
    mb = abs(number) / (1024 * 1024)
    sign = "-" if number < 0 else ""
    if mb >= 1024.0:
        return f"{sign}{mb / 1024:.2f} GB"
    return f"{sign}{mb:.2f} MB"


def band_text(band: float) -> str:
    percent = band * 100.0
    if abs(percent - round(percent)) < 1e-9:
        return f"{int(round(percent))}%"
    return f"{percent:.1f}%"


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def ci_hard_fail() -> bool:
    return os.environ.get("CI_COMPARE") == "1" or _env.in_github_actions()


def knobs_of(report: dict) -> dict:
    config = report.get("config") or {}
    return {field: config.get(field) for field in KNOBS}


def shapes_of(report: dict) -> dict[str, dict]:
    found = {}
    for shape in report.get("shapes") or []:
        name = shape.get("shape")
        if name:
            found[name] = shape
    return found


def check(name: str, metric: str, verdict: str, **detail) -> dict:
    row = {"name": name, "metric": metric, "verdict": verdict}
    row.update(detail)
    return row


def judge_lower(value: float, bounds: dict, metric: str) -> str:
    band, floor = (RATIO_BAND, 0.0) if metric == "ratio" else (BYTE_BAND, BYTE_FLOOR)
    low = float(bounds["min"])
    high = float(bounds["max"])
    if value > high * (1.0 + band) and value > high + floor:
        return "regression"
    if value < low * (1.0 - band) and value < low - floor:
        return "improvement"
    return "ok"


def numeric_checks(baseline: dict, report: dict) -> list[dict]:
    rows: list[dict] = []
    found = shapes_of(report)
    for name, expect in (baseline.get("shapes") or {}).items():
        shape = found.get(name)
        scenario = (shape or {}).get("scenario") or {}
        if shape is None:
            rows.append(check(name, "presence", "regression", detail="report is missing the shape", scenario=scenario))
            continue
        got_nodes = shape.get("nodes")
        got_rels = shape.get("relationships")
        want_nodes = int(expect["nodes"])
        want_rels = int(expect["relationships"])
        counts_ok = (
            got_nodes is not None
            and got_rels is not None
            and int(got_nodes) == want_nodes
            and int(got_rels) == want_rels
        )
        got_nodes = -1 if got_nodes is None else int(got_nodes)
        got_rels = -1 if got_rels is None else int(got_rels)
        rows.append(
            check(
                name,
                "counts",
                "ok" if counts_ok else "regression",
                value={"nodes": got_nodes, "relationships": got_rels},
                expected={"nodes": want_nodes, "relationships": want_rels},
                scenario=scenario,
                measured=shape,
                bounds=expect,
            )
        )
        for metric in (*BYTE_METRICS, "ratio"):
            value = shape.get(metric)
            bounds = expect.get(metric) or {}
            if value is None or "min" not in bounds:
                rows.append(
                    check(
                        name,
                        metric,
                        "regression",
                        detail=f"report is missing {metric}",
                        scenario=scenario,
                        measured=shape,
                        bounds=expect,
                    )
                )
                continue
            rows.append(
                check(
                    name,
                    metric,
                    judge_lower(float(value), bounds, metric),
                    value=value,
                    min=bounds["min"],
                    max=bounds["max"],
                    scenario=scenario,
                    measured=shape,
                    bounds=expect,
                )
            )
    return rows


def count_checks(baseline: dict, report: dict) -> list[dict]:
    rows: list[dict] = []
    found = shapes_of(report)
    for name, expect in (baseline.get("shapes") or {}).items():
        shape = found.get(name)
        scenario = (shape or {}).get("scenario") or {}
        if shape is None:
            rows.append(check(name, "presence", "regression", detail="report is missing the shape", scenario=scenario))
            continue
        got_nodes = shape.get("nodes")
        got_rels = shape.get("relationships")
        want_nodes = int(expect["nodes"])
        want_rels = int(expect["relationships"])
        counts_ok = (
            got_nodes is not None
            and got_rels is not None
            and int(got_nodes) == want_nodes
            and int(got_rels) == want_rels
        )
        got_nodes = -1 if got_nodes is None else int(got_nodes)
        got_rels = -1 if got_rels is None else int(got_rels)
        rows.append(
            check(
                name,
                "counts",
                "ok" if counts_ok else "regression",
                value={"nodes": got_nodes, "relationships": got_rels},
                expected={"nodes": want_nodes, "relationships": want_rels},
                scenario=scenario,
                measured=shape,
                bounds=expect,
            )
        )
    return rows


def overall(rows: list[dict], incompatible: bool) -> str:
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


def metric_phrase(metric: str, value, low, high) -> str:
    if metric == "ratio":
        return f"ratio measured {float(value):.3f}x against {float(low):.3f}x–{float(high):.3f}x"
    label = {
        "logical_bytes": "logical",
        "disk_bytes": "disk",
        "attributed_rss_bytes": "attributed RSS",
    }[metric]
    return f"{label} measured {format_mem(value)} against {format_mem(low)} – {format_mem(high)}"


def shape_section(name: str, rows: list[dict]) -> list[str]:
    row = next(item for item in rows if item.get("bounds"))
    bounds = row["bounds"]
    measured = row.get("measured") or {}
    scenario = row.get("scenario") or {}
    lines = [f"## {name}", ""]
    if scenario:
        lines.append(f"**Given:** {scenario.get('given', '')}")
        lines.append("")
        lines.append(f"**When:** {scenario.get('when', '')}")
        lines.append("")
        lines.append(f"**Then:** {scenario.get('then', '')}")
        lines.append("")
    lines.append(
        "**Baseline:** logical "
        f"{format_mem(bounds['logical_bytes']['min'])} – {format_mem(bounds['logical_bytes']['max'])}, "
        f"disk {format_mem(bounds['disk_bytes']['min'])} – {format_mem(bounds['disk_bytes']['max'])}, "
        f"attributed RSS {format_mem(bounds['attributed_rss_bytes']['min'])} – "
        f"{format_mem(bounds['attributed_rss_bytes']['max'])}, "
        f"ratio {float(bounds['ratio']['min']):.3f}x–{float(bounds['ratio']['max']):.3f}x. "
        f"A reading may sit {band_text(RATIO_BAND)} outside the ratio, and "
        f"{band_text(BYTE_BAND)} or {format_mem(BYTE_FLOOR)} outside the sizes."
    )
    lines.append("")
    ratio = measured.get("ratio")
    ratio_text = f"{float(ratio):.3f}x" if ratio is not None else "-"
    lines.append(
        "**Validated:** logical "
        f"{format_mem(measured.get('logical_bytes') or 0)}, "
        f"disk {format_mem(measured.get('disk_bytes') or 0)}, "
        f"attributed RSS {format_mem(measured.get('attributed_rss_bytes') or 0)}, "
        f"ratio {ratio_text}."
    )
    lines.append("")
    failing = [item for item in rows if item["verdict"] != "ok"]
    if not failing:
        lines.append("**Report:** logical, disk, attributed RSS, and the ratio held.")
    else:
        parts = []
        for item in failing:
            if item["metric"] == "counts":
                got = item.get("value") or {}
                want = item.get("expected") or {}
                parts.append(
                    f"counts measured {got.get('nodes')} nodes and {got.get('relationships')} relationships "
                    f"against {want.get('nodes')} nodes and {want.get('relationships')} relationships"
                )
            elif item.get("detail"):
                parts.append(str(item["detail"]))
            elif "min" in item:
                parts.append(
                    f"{metric_phrase(item['metric'], item['value'], item['min'], item['max'])} ({item['verdict']})"
                )
        lines.append("**Report:** " + ". ".join(parts) + ".")
    lines.append("")
    if any(item["verdict"] == "improvement" for item in failing):
        lines.append(IMPROVEMENT_NOTE)
        lines.append("")
    return lines


def render(status: str, rows: list[dict], note: str) -> str:
    lines = ["# Memory shape", "", f"**Status:** {status}", ""]
    if note:
        lines.extend([note, ""])
    grouped: dict[str, list[dict]] = {}
    order: list[str] = []
    for row in rows:
        name = row["name"]
        if name not in grouped:
            order.append(name)
            grouped[name] = []
        grouped[name].append(row)
    if status == "ok":
        lines.append("All memory shapes held. 🟢")
        lines.append("")
        for name in order:
            if grouped[name] and grouped[name][0].get("bounds"):
                lines.extend(shape_section(name, grouped[name]))
        return "\n".join(lines)
    if not any(row["verdict"] != "ok" for row in rows) and status == "no-baseline":
        lines.append("The envelope is not frozen yet. This run is a warning, and the job stays green.")
        lines.append("")
        return "\n".join(lines)
    if not any(row["verdict"] != "ok" for row in rows) and status == "incompatible":
        lines.append("The environment class does not match the envelope, so the numeric bands were not applied.")
        lines.append("")
        return "\n".join(lines)
    for name in order:
        failing = [row for row in grouped[name] if row["verdict"] != "ok"]
        if not failing:
            continue
        if failing[0].get("bounds"):
            lines.extend(shape_section(name, grouped[name]))
            continue
        lines.append(f"## {name} — {failing[0]['metric']}")
        lines.append("")
        lines.append(f"**Verdict:** {failing[0]['verdict']}")
        lines.append("")
        if failing[0].get("detail"):
            lines.append(str(failing[0]["detail"]))
            lines.append("")
    return "\n".join(lines)


def verdict(baseline: dict, report: dict) -> tuple[str, list[dict], str]:
    if not baseline or not baseline.get("frozen"):
        return "no-baseline", [], "No frozen envelope is on this branch yet."
    if baseline.get("schema_version") != 1 or report.get("schema_version") != 2:
        return (
            "regression",
            [check("report", "schema", "regression", detail="the report or the envelope schema is not the one this gate reads")],
            "",
        )
    mismatches = _env.environment_mismatches(baseline.get("environment"), report.get("environment"))
    knob_mismatch = knobs_of(report) != (baseline.get("knobs") or {})
    incompatible = bool(mismatches) or knob_mismatch
    note = ""
    if incompatible:
        detail = mismatches[0].message if mismatches else "knobs differ from the envelope"
        note = f"Environment class mismatch ({detail}). Numeric bands were skipped."
        rows = count_checks(baseline, report)
        if ci_hard_fail():
            rows.append(check("environment", "class", "incompatible", detail=detail))
    else:
        rows = numeric_checks(baseline, report)
    status = overall(rows, incompatible)
    if status == "incompatible" and not ci_hard_fail():
        note = (note + " " if note else "") + "Local compare keeps the count checks and skips the numeric bands."
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
    parser.add_argument("--baseline", type=Path, default=Path("baselines/memory-shape.json"))
    parser.add_argument("--report", type=Path, default=Path("target/memory-shape-loadtest.json"))
    parser.add_argument("--doc", type=Path, default=Path("target/memory-shape.md"))
    parser.add_argument("--verdict", type=Path, default=Path("target/memory-shape-verdict.json"))
    args = parser.parse_args()

    if not args.baseline.is_file() or not args.report.is_file():
        missing = args.baseline if not args.baseline.is_file() else args.report
        status, rows, note = "no-baseline", [], f"{missing} is not present, so there is nothing to compare yet."
        if args.baseline.is_file() and not args.report.is_file():
            note = f"{args.report} is not present. The memory-shape run did not write a report."
            status = "regression"
            rows = [check("report", "presence", "regression", detail=note)]
            note = ""
    else:
        status, rows, note = verdict(load(args.baseline), load(args.report))

    document = render(status, rows, note)
    args.doc.parent.mkdir(parents=True, exist_ok=True)
    args.verdict.parent.mkdir(parents=True, exist_ok=True)
    args.doc.write_text(document)
    args.verdict.write_text(json.dumps({"status": status, "checks": rows}, indent=2) + "\n")
    print(f"compare-memory-shape: {status}")
    emit_checks("compare-memory-shape", rows)
    return exit_code(status)


if __name__ == "__main__":
    sys.exit(main())
