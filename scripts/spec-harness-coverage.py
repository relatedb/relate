#!/usr/bin/env python3
"""Report spec-harness coverage with a traffic light (ADR-0482).

Reads the cypher-test-harness `reports/summary.json` produced from the
PR's own Python-wheel build and answers one question: is spec coverage
still 100%?

The summary shape contract — the totals vocabulary, the category
rollups, the `@fails` exclusion, the traffic-light bands — is shared
with the ADR-0661 regression gate and lives in
`scripts/spec_harness_common.py`, imported here; this script owns only
the report: the markdown tables, the step-summary append, and the exit
codes.

Coverage = passed / (passed + failed + error) over the scenarios the
run executed — a scenario whose step crashed (behave's `error` status)
is a genuine failure exactly like an assertion failure, never context.
Three buckets never count against it, by contract with the harness's
`generate_report.py` (ADR-0482 §3): `neo4j_expected_fail`,
`nondeterministic`, and `skipped` / `untested`.

The `skipped` bucket is one opaque number on its own; the harness's
`skip_categories` rollup says *why* the scenarios did not run (the
graphengine suite skipped by design, Cypher-5-era expectations, the
skip-list process-aborting skips, …) and renders as its own small
table, so the step summary and the upserted PR comment carry the
breakdown. Visibility only: ADR-0482 §3 keeps every skip out of the
coverage number.

Reporting, not gating: the report leads with a traffic light — 🟢 at
zero genuine failures, 🟡 while the failure share stays within 10% of
the executed scenarios, 🔴 beyond it (ADR-0482 §4) — and the process
exits 0 for every band; a low number must never block a PR. The
regression gate is a separate script and a separate verdict
(`scripts/spec-harness-gate.py`, ADR-0661). The one failure mode that
still exits 1 is a summary this script cannot read at all (a shape the
ADR-0482 §3 contract does not recognize): then there is no number to
report.

The report table goes to stdout and, in GitHub Actions, is appended to
`$GITHUB_STEP_SUMMARY` (the workflow also posts it as a PR comment).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# The shared contract module sits beside this script (hyphenated names
# are not importable); scripts/ lands on sys.path for both the direct
# run and the loader-based test suites, whose sys.path[0] is
# scripts/tests/.
_SCRIPTS_DIR = str(Path(__file__).resolve().parent)
if _SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, _SCRIPTS_DIR)

from spec_harness_common import (  # noqa: E402
    BANDS,
    ShapeError,
    Summary,
    TOTALS_KEYS,
    YELLOW_LIMIT_PERCENT,
    append_step_summary,
    ordered_counts,
    parse_summary,
    read_document,
    short_sha,
)


def _non_negative_seconds(text: str) -> int:
    """The `--run-seconds` argparse type: an int of at least zero, or the
    parser's own usage error (exit 2, the one path every bad flag takes)."""
    seconds = int(text)
    if seconds < 0:
        raise argparse.ArgumentTypeError("--run-seconds must be non-negative")
    return seconds


def format_seconds(seconds: int) -> str:
    """Render a duration the way the report shows it: `47s`, `3m 12s`,
    `1h 2m`. The one spelling, so the comment and the log agree."""
    if seconds < 60:
        return f"{seconds}s"
    minutes, sec = divmod(seconds, 60)
    if minutes < 60:
        return f"{minutes}m {sec}s"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h {minutes}m"


def _report_diags(diags: list[str]) -> int:
    """The one diagnostic funnel for this report: the read's failure and
    the parse's diagnostics print through the same door and take the
    same exit 1 — the prefix, stream and code spelled once."""
    for d in diags:
        print(f"spec-harness-coverage: {d}", file=sys.stderr)
    return 1


def _count_table(header: str, counts: dict[str, int]) -> list[str]:
    """One name -> count markdown table — the shape the skip and failure
    breakdowns share, so the two cannot drift apart. The row order is
    the family's one ordered-counts helper."""
    return [
        "",
        f"| {header} | count |",
        "| --- | --- |",
        *[
            f"| {name} | {count} |"
            for name, count in ordered_counts(counts)
        ],
    ]


def render_markdown(
    summary: Summary,
    harness_sha: str | None,
    run_seconds: int | None = None,
) -> str:
    """The step-summary table: the traffic-light verdict, the not-counted
    context buckets, the per-reason skip breakdown behind the `skipped`
    total, the per-category defect breakdown (reconciled with the
    headline by the uncategorized errored row), and — when the caller
    measured it — how long the suite took."""
    legend = " · ".join(f"{emoji} {label}" for _, emoji, label in BANDS)
    lines = [
        f"## Spec-harness coverage — {summary.light}",
        "",
        f"- {summary.headline}",
        f"- scenarios run: **{summary.size}** "
        f"(passed {summary.passed}, failed {summary.failed}"
        + (f", errored {summary.errored}" if summary.errored else "")
        + ")",
        f"- bands: {legend} (ADR-0482 §4 — the light never fails the build)",
    ]
    if run_seconds is not None:
        lines.append(f"- suite runtime: {format_seconds(run_seconds)}")
    lines += [
        f"- {key} (not counted): {count}"
        for key, count in summary.context.items()
    ]
    if harness_sha:
        lines.append(f"- harness ref: `{short_sha(harness_sha)}`")
    if summary.skip_categories:
        lines += _count_table("skipped category", summary.skip_categories)
    if summary.defect_count:
        lines += _count_table("failure category", summary.categories)
        if summary.errored:
            # The headline counts errored scenarios as genuine failures;
            # this row reconciles the table with it (errored steps carry
            # no category).
            lines.append(f"| errored (no category) | {summary.errored} |")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("summary", type=Path, help="path to reports/summary.json")
    ap.add_argument(
        "--harness-sha",
        default=None,
        help="the harness ref the run executed (recorded in the report)",
    )
    ap.add_argument(
        "--run-seconds",
        type=_non_negative_seconds,
        default=None,
        metavar="N",
        help="how long the suite took, in seconds (measured by the lane; "
        "rendered as the suite-runtime row)",
    )
    args = ap.parse_args()

    # The fail-closed read boundary shared with the gate: a missing or
    # unreadable summary is a named diagnostic and exit 1 (this report's
    # no-verdict code), never a raw traceback.
    try:
        raw = read_document(args.summary, "summary")
    except ShapeError as err:
        return _report_diags([str(err)])
    summary, diags = parse_summary(raw)
    if summary is None:
        return _report_diags(diags)

    table = render_markdown(summary, args.harness_sha, args.run_seconds)
    print(table)
    append_step_summary(table)

    # Reporting, not gating (ADR-0482 §4): below 100% is the light in the
    # report, never a failed build. The status line goes to stderr — the
    # lane pipes stdout into the PR comment, which carries the table only.
    print(f"spec-harness-coverage: {summary.headline}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
