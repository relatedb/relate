#!/usr/bin/env python3
"""Fold repeated memory-shape reports into a min/max envelope.

Each round loads the same fixed graph per shape. The node and relationship
counts have to match the loadtest targets on every round. Logical bytes,
disk, attributed RSS, and the ratio keep the min and max seen. The compare
script applies its bands on top of that range.

The envelope is schema 1. A report that is missing its environment class,
a shape, or the fixed counts is refused.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# The fixed graphs in testing/relate-datasets/src/memory_shape.rs.
TARGETS = {
    "base": (0, 0),
    "flat": (120_000, 0),
    "fanout": (52_016, 52_000),
    "ring": (46_000, 46_000),
    "dense ring": (5_000, 100_000),
}
METRICS = ("logical_bytes", "disk_bytes", "attributed_rss_bytes", "ratio")
KNOBS = (
    "logical_target_bytes",
    "attributes",
    "text_indexes",
    "point_indexes",
    "range_indexes",
)


def load(path: Path) -> dict:
    return json.loads(path.read_text())


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


def span(values: list) -> dict:
    return {"min": min(values), "max": max(values)}


def check(reports: list[tuple[Path, dict]]) -> list[str]:
    problems: list[str] = []
    first_path, first = reports[0]
    if first.get("schema_version") != 2:
        problems.append(f"{first_path}: report schema is {first.get('schema_version')}, want 2")
    if first.get("environment") is None:
        problems.append(f"{first_path}: missing environment")
    knobs = knobs_of(first)
    for path, report in reports:
        if report.get("schema_version") != first.get("schema_version"):
            problems.append(f"{path}: schema_version differs from {first_path}")
        if report.get("environment") != first.get("environment"):
            problems.append(f"{path}: environment differs from {first_path}")
        if knobs_of(report) != knobs:
            problems.append(f"{path}: knobs differ from {first_path}")
        found = shapes_of(report)
        if set(found) != set(TARGETS):
            problems.append(f"{path}: shape set is {sorted(found)}, want {sorted(TARGETS)}")
            continue
        for name, (nodes, rels) in TARGETS.items():
            shape = found[name]
            got_nodes = shape.get("nodes")
            got_rels = shape.get("relationships")
            if got_nodes is None or got_rels is None or int(got_nodes) != nodes or int(got_rels) != rels:
                problems.append(
                    f"{path}: {name} has {shape.get('nodes')} nodes and "
                    f"{shape.get('relationships')} relationships, want {nodes} and {rels}"
                )
            for metric in METRICS:
                if shape.get(metric) is None:
                    problems.append(f"{path}: {name} is missing {metric}")
    return problems


def envelope(reports: list[tuple[Path, dict]]) -> dict:
    _, last = reports[-1]
    shapes: dict[str, dict] = {}
    for name, (nodes, rels) in TARGETS.items():
        grouped = {metric: [] for metric in METRICS}
        for _, report in reports:
            shape = shapes_of(report)[name]
            for metric in METRICS:
                grouped[metric].append(shape[metric])
        shapes[name] = {
            "nodes": nodes,
            "relationships": rels,
            **{metric: span(values) for metric, values in grouped.items()},
        }
    return {
        "schema_version": 1,
        "name": "memory-shape",
        "frozen": True,
        "rounds": len(reports),
        "environment": last["environment"],
        "knobs": knobs_of(last),
        "shapes": shapes,
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
            print(f"freeze-memory-shape: {problem}", file=sys.stderr)
        return 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(envelope(loaded), indent=2) + "\n")
    print(f"freeze-memory-shape: wrote {args.out} from {len(loaded)} rounds")
    return 0


if __name__ == "__main__":
    sys.exit(main())
