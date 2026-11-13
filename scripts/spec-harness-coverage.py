#!/usr/bin/env python3
"""Report spec-harness coverage with a traffic light (ADR-0482).

Reads the cypher-test-harness `reports/summary.json` produced from the
PR's own Python-wheel build and answers one question: is spec coverage
still 100%?

Coverage = passed / (passed + failed + errored) over the scenarios the
run executed — a scenario whose step crashed (behave's `error` status)
is a genuine failure exactly like an assertion failure, never context.
Three buckets never count against it, by contract with the harness's
`generate_report.py` (ADR-0482 §3):

- `neo4j_expected_fail` — scenarios Neo4j itself tags `@fails`; failing
  them is the expected state, not a Relate defect,
- `nondeterministic` — `rand()` / `randomUUID()` expectations that cannot
  be verified run to run,
- `skipped` / `untested` — not executed.

The same contract keeps `@fails` failures out of `failure_categories`, so
that rollup is exactly the assertion-failure breakdown: `parse_summary`
asserts `sum(failure_categories) == failed` and returns only a validated
[`Summary`] — a shape the contract does not recognize is a diagnostic,
never a silent pass on defaults (the no-silent-fallbacks rule).

Reporting, not gating: the report leads with a traffic light — 🟢 at
zero genuine failures, 🟡 while the failure share stays within 10% of the
executed scenarios, 🔴 beyond it (ADR-0482 §4) — and the process exits 0
for every band; a low number must never block a PR. The one failure mode
that still exits 1 is a summary this script cannot read at all (a shape
the ADR-0482 §3 contract does not recognize): then there is no number to
report.

The report table goes to stdout and, in GitHub Actions, is appended to
`$GITHUB_STEP_SUMMARY` (the workflow also posts it as a PR comment).
"""

from __future__ import annotations

import argparse
import enum
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path


class TotalRole(enum.Enum):
    """What a totals key is, stated once beside the vocabulary: a
    `REQUIRED` total must be present — absent is a shape failure; an
    `ERRORED` bucket (behave's step-crash count) may be absent, and
    absent means zero — present, it counts against coverage exactly like
    a failure; a `CONTEXT` bucket may be absent (absent means not
    reported) and never counts. Misshapen values fail loudly whatever
    the role — the loud-failure contract covers the whole rollup."""

    REQUIRED = enum.auto()
    ERRORED = enum.auto()
    CONTEXT = enum.auto()


# The one totals vocabulary: (key, role). The roles are [`TotalRole`]
# members, so the walk's one dispatch is exhaustive — a new bucket is a
# new member, never a new string.
TOTALS_KEYS: tuple[tuple[str, TotalRole], ...] = (
    ("passed", TotalRole.REQUIRED),
    ("failed", TotalRole.REQUIRED),
    ("error", TotalRole.ERRORED),
    ("neo4j_expected_fail", TotalRole.CONTEXT),
    ("nondeterministic", TotalRole.CONTEXT),
    ("skipped", TotalRole.CONTEXT),
    ("untested", TotalRole.CONTEXT),
)

# The failure-share threshold that separates yellow from red, named once:
# the light reads it and the rendered legend interpolates it, so the
# number cannot drift between the decision and its prose.
YELLOW_LIMIT_PERCENT = 10.0

# The traffic-light bands (ADR-0482 §4), in order: the first row whose
# limit the failure share does not exceed wins. One table — `Summary.light`
# walks it and `render_markdown` renders its legend, so the policy is
# spelled once.
BANDS: tuple[tuple[float, str, str], ...] = (
    (0.0, "🟢", "zero failures"),
    (YELLOW_LIMIT_PERCENT, "🟡", f"failure share ≤ {YELLOW_LIMIT_PERCENT:g}%"),
    (float("inf"), "🔴", "beyond"),
)

# A failure-category name as the harness emits it. The names travel into
# the bot's PR comment, so anything else is a shape failure rather than
# text to render.
CATEGORY_NAME = re.compile(r"[a-z0-9-]+")


def _is_count(value: object) -> bool:
    """The one counting model: a non-negative int, bools excluded (a JSON
    `true` is an int subclass in Python and never a scenario count)."""
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


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


@dataclass(frozen=True)
class Summary:
    """The validated rollup the verdict reads (ADR-0482 §2/§3): the two
    required totals, behave's step-crash count (`errored` — counted like
    a failure, kept out of the category rollup), the not-counted context
    buckets reported (present and non-zero), and the assertion-failure
    category breakdown (it sums to `failed` only — errored scenarios
    carry no category; `defect_count` is the headline population).

    Built only by [`Summary.validated`], so the executed-scenario count
    is non-zero by construction and no consumer re-reads the raw dict."""

    passed: int
    failed: int
    errored: int
    categories: dict[str, int]
    context: dict[str, int]

    @classmethod
    def validated(
        cls,
        passed: int,
        failed: int,
        errored: int,
        categories: dict[str, int],
        context: dict[str, int],
    ) -> tuple[Summary | None, list[str]]:
        """The one construction path, owning the non-empty-run invariant:
        a Summary exists at all only when the run executed something, and
        it owns its rollups (copies, never the caller's live dicts)."""
        summary = cls(passed, failed, errored, dict(categories), dict(context))
        if summary.size == 0:
            return None, [
                "the run executed no scenarios "
                f"(passed {passed}, failed {failed}, errored {errored})"
            ]
        return summary, []

    @property
    def size(self) -> int:
        """Scenarios the run executed — the one derivation of that fact.
        Errored scenarios ran (their step raised), so they are in the
        denominator exactly like failures."""
        return self.passed + self.failed + self.errored

    @property
    def coverage(self) -> float:
        """Coverage over the executed scenarios, in percent."""
        return 100.0 * self.passed / self.size

    @property
    def defect_count(self) -> int:
        """Genuine failures: assertion failures plus step-crash errors —
        neither passed, so both count."""
        return self.failed + self.errored

    @property
    def failure_rate(self) -> float:
        """The genuine-failure share of the executed scenarios, in
        percent — the number the traffic-light bands read."""
        return 100.0 * self.defect_count / self.size

    @property
    def light(self) -> str:
        """The traffic light (ADR-0482 §4): the emoji of the first
        [`BANDS`] row whose limit the failure share does not exceed."""
        return next(
            emoji for limit, emoji, _ in BANDS if self.failure_rate <= limit
        )

    @property
    def headline(self) -> str:
        """The one line the verdict, the step summary and the PR comment
        all lead with — derived here, never re-spelled by a consumer."""
        lead = f"{self.light} spec coverage {self.coverage:.2f}%"
        if self.defect_count == 0:
            return f"{lead} — complete"
        return (
            f"{lead} — {self.defect_count} genuine failures "
            f"({self.failure_rate:.2f}% of the executed scenarios)"
        )


def parse_summary(raw: object) -> tuple[Summary | None, list[str]]:
    """Validate one decoded `summary.json` and return its [`Summary`], or
    `(None, diagnostics)` on the first shape the contract does not
    recognize: a non-object summary; a missing or non-object `totals`
    object; a missing or misshapen total (the roles' absence rules live on
    [`TotalRole`]); a totals key outside the [`TOTALS_KEYS`] vocabulary; a
    missing `failure_categories`, or one that is not a str -> count map of
    identifier-shaped names; a rollup that breaks the @fails exclusion
    (`sum(failure_categories) != failed` — ADR-0482 §3); or a run that
    executed no scenarios. Total over its input; never a silent default.

    The one walk over [`TOTALS_KEYS`] validates every total and collects
    everything the Summary is built from — the counted roles into one
    map, the reported context buckets beside it — so `totals` is never
    re-indexed after validation and the values the walk checked are the
    values the verdict reads."""
    if not isinstance(raw, dict):
        return None, [f"summary is not an object (got {type(raw).__name__})"]
    totals = raw.get("totals")
    if not isinstance(totals, dict):
        return None, [
            f"summary has no totals object (got {type(totals).__name__})"
        ]
    # The counted map starts from the table's own ERRORED members —
    # absent means zero is the role's policy, derived, not re-spelled as
    # a literal key here; the walk fills every present counted role.
    counted: dict[str, int] = {
        key: 0 for key, role in TOTALS_KEYS if role is TotalRole.ERRORED
    }
    context: dict[str, int] = {}
    for key, role in TOTALS_KEYS:
        if key not in totals:
            if role is TotalRole.REQUIRED:
                return None, [f"totals.{key} is missing (required total)"]
            continue
        value = totals[key]
        if not _is_count(value):
            return None, [
                f"totals.{key} is not a non-negative int (got {value!r})"
            ]
        if role is TotalRole.CONTEXT:
            if value:
                context[key] = value
        else:
            counted[key] = value
    # The vocabulary boundary is checked in both directions: an unknown
    # total (a harness-side rename like `skipped` -> `not_run`) must not be
    # silently ignored — the contract's vocabulary is exhaustive.
    unknown = totals.keys() - {key for key, _ in TOTALS_KEYS}
    if unknown:
        return None, [
            f"totals.{key} is not in the coverage contract's totals "
            "vocabulary (ADR-0482 §3)"
            for key in sorted(unknown)
        ]
    # Absence is named as absence, like the totals walk above: a missing
    # `failure_categories` is a different fact from a present-but-malformed
    # one, and each carries its own diagnostic.
    if "failure_categories" not in raw:
        return None, ["failure_categories is missing (required member)"]
    categories = raw["failure_categories"]
    if not isinstance(categories, dict) or not all(
        isinstance(name, str)
        and CATEGORY_NAME.fullmatch(name)
        and _is_count(count)
        for name, count in categories.items()
    ):
        return None, [
            "failure_categories is not a map of identifier-shaped names "
            "to non-negative ints"
        ]
    # The ADR-0482 §3 contract: @fails failures stay out of the category
    # rollup, so it sums to exactly the assertion-failure count. A mismatch
    # means the harness's rollup changed — update this gate with it.
    summed = sum(categories.values())
    failed = counted["failed"]
    if summed != failed:
        return None, [
            f"failure_categories sums to {summed}, totals.failed is "
            f"{failed}: the harness summary no longer keeps @fails "
            "failures out of the category rollup (ADR-0482 §3 contract)"
        ]
    return Summary.validated(
        counted["passed"], failed, counted["error"], categories, context
    )


def render_markdown(
    summary: Summary,
    harness_sha: str | None,
    run_seconds: int | None = None,
) -> str:
    """The step-summary table: the traffic-light verdict, the not-counted
    context buckets, the per-category defect breakdown (reconciled with
    the headline by the uncategorized errored row), and — when the caller
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
        lines.append(f"- harness ref: `{harness_sha}`")
    if summary.defect_count:
        lines += [
            "",
            "| failure category | count |",
            "| --- | --- |",
        ]
        lines += [
            f"| {name} | {count} |"
            for name, count in sorted(
                summary.categories.items(), key=lambda kv: (-kv[1], kv[0])
            )
        ]
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

    summary, diags = parse_summary(json.loads(args.summary.read_text()))
    if summary is None:
        for d in diags:
            print(f"spec-harness-coverage: {d}", file=sys.stderr)
        return 1

    table = render_markdown(summary, args.harness_sha, args.run_seconds)
    print(table)
    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with open(step_summary, "a", encoding="utf-8") as f:
            f.write(table + "\n\n")

    # Reporting, not gating (ADR-0482 §4): below 100% is the light in the
    # report, never a failed build. The status line goes to stderr — the
    # lane pipes stdout into the PR comment, which carries the table only.
    print(f"spec-harness-coverage: {summary.headline}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
