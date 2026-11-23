"""The spec-harness summary contract shared by the engine-side readers.

`scripts/spec-harness-coverage.py` (the ADR-0482 traffic-light report),
`scripts/spec-harness-gate.py` (the ADR-0661 regression gate) and
`scripts/spec_harness_baseline.py` (the machine-written baseline and the
comparison against it) all read the cypher-test-harness documents and
all must reject the same shapes loudly — the totals vocabulary, the
category rollups, the `@fails` exclusion — so the contract lives here
exactly once, imported by all three. No reader re-states a rule this
module owns.

The contract itself (ADR-0482 §2/§3):

- Coverage = passed / (passed + failed + error) over the scenarios the
  run executed — behave's `error` bucket (step crashes) counts against
  coverage exactly like an assertion failure.
- Three buckets never count against it: `neo4j_expected_fail`
  (scenarios Neo4j itself tags `@fails`, or the harness's
  expected-fail-list markings), `nondeterministic` (`rand()` /
  `randomUUID()` expectations that cannot be verified run to run), and
  `skipped` / `untested`.
- The `@fails` failures stay out of `failure_categories`, so that rollup
  sums to exactly the assertion-failure count — the invariant the parse
  asserts. A shape the contract does not recognize is a diagnostic,
  never a silent pass on defaults (the no-silent-fallbacks rule).

The traffic-light bands (ADR-0482 §4) live here too: they are part of
the same contract table, read by [`Summary.light`], and the report
renders them from this one home.

The document readers live here as well: [`ShapeError`] is the one
failure type every shape check raises (a diagnostic naming a position,
never a value — corpus-derived text does not render in this
repository, ADR-0661 §4), [`MissingDocument`] distinguishes absence
from corruption, and [`read_document`] owns the fail-closed read
boundary both readers translate into their own exit code.
"""

from __future__ import annotations

import enum
import json
import os
import re
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path


class ShapeError(Exception):
    """The one failure type every shape check raises: the message is a
    diagnostic naming a position, and the CLI caller turns it into its
    own no-verdict exit code."""


class MissingDocument(ShapeError):
    """The read's absent-file case, distinct from corruption on purpose:
    only the previous baseline's absence is a green bootstrap line
    (ADR-0661 §2); a document that is present but unreadable is a hard
    failure everywhere, never a silent default."""


def read_document(path: Path, what: str, missing: str | None = None) -> object:
    """The one read for the harness documents: a readable file holding
    one JSON value. Absence (`FileNotFoundError`) raises
    [`MissingDocument`] with the caller's `missing` diagnostic (the
    baseline's is the bootstrap remedy, ADR-0661 §5; the summary's names
    the path) — absence is its own fact, distinct from corruption. The
    read fetches bytes (only the `OSError` family arises there) and the
    decode sits beside the parse, so every present-but-unreadable shape
    — another `OSError`, a non-UTF-8 stream, undecodable JSON — raises
    [`ShapeError`] naming the document: a hard failure everywhere, never
    a raw traceback (which a CI step would read as a red verdict)."""
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        raise MissingDocument(missing or f"the {what} is missing: {path}")
    except OSError as err:
        raise ShapeError(f"the {what} is unreadable ({err})")
    try:
        return json.loads(data.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as err:
        raise ShapeError(f"the {what} is not valid JSON ({err})")


def append_step_summary(text: str) -> None:
    """The one Actions step-summary append, shared by the readers: in
    GitHub Actions the report table and the gate verdict land in the
    job's step summary through this door, and nowhere else. The write is
    an append, never a truncate — a lane that runs both scripts keeps
    both tables."""
    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with open(step_summary, "a", encoding="utf-8") as f:
            f.write(text + "\n\n")


def short_sha(sha: str) -> str:
    """The one short-sha spelling the family renders: the first seven
    hex digits — the verdict's provenance line, the writer's line and
    the coverage report all interpolate it, so the width cannot drift
    between them."""
    return sha[:7]


def ordered_counts(counts: dict[str, int]) -> list[tuple[str, int]]:
    """The one count-then-name row order every count table in the reader
    family shares: rows by descending count, ties by name — stated
    once, so the coverage report's tables and the gate's breakdowns
    cannot drift apart."""
    return sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))


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
# walks it and the report renders its legend, so the policy is spelled
# once.
BANDS: tuple[tuple[float, str, str], ...] = (
    (0.0, "🟢", "zero failures"),
    (YELLOW_LIMIT_PERCENT, "🟡", f"failure share ≤ {YELLOW_LIMIT_PERCENT:g}%"),
    (float("inf"), "🔴", "beyond"),
)


# A git sha as the documents record one: 40 lowercase hex digits —
# document grammar, beside the category vocabulary it keeps company
# with (both the baseline reader and the gate's sha flags read it).
SHA1_HEX = re.compile(r"[0-9a-f]{40}")

# A failure-category name as the harness emits it. The names travel into
# the bot's PR comment, so anything else is a shape failure rather than
# text to render.
CATEGORY_NAME = re.compile(r"[a-z0-9-]+")


def is_count(value: object) -> bool:
    """The one counting model: a non-negative int, bools excluded (a JSON
    `true` is an int subclass in Python and never a scenario count)."""
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def rollup_member(
    raw: dict, member: str, *, required: bool
) -> tuple[dict[str, int] | None, list[str]]:
    """The one lookup for a name -> count rollup member, owning every
    policy the members share: the absence rule (`required` — a missing
    required member is named as absence, an optional one absent is the
    empty rollup) and the shape check with its diagnostic. A valid member
    is a map of identifier-shaped names to non-negative ints; anything
    else (a list, a null, a non-identifier name, a bool, a negative
    count) is a shape failure. A third rollup member is one more call,
    never another block."""
    if member not in raw:
        if required:
            return None, [f"{member} is missing (required member)"]
        return {}, []
    value = raw[member]
    if not isinstance(value, dict) or not all(
        isinstance(name, str)
        and CATEGORY_NAME.fullmatch(name)
        and is_count(count)
        for name, count in value.items()
    ):
        return None, [
            f"{member} is not a map of identifier-shaped names "
            "to non-negative ints"
        ]
    return value, []


def validate_totals(
    totals: object, *, owner: str
) -> tuple[dict[str, int] | None, dict[str, int], list[str]]:
    """The one walk over [`TOTALS_KEYS`]: validate every total in one
    pass and return the counted map (required + errored roles, absent
    errored read as zero) beside the reported context buckets (present
    and non-zero only) — or `(None, {}, diagnostics)` on the first shape
    the contract does not recognize. Shared by the summary parse and the
    baseline parse (`owner` names the document in the diagnostics), so
    the two are judged by the one vocabulary; `totals` is never
    re-indexed after validation."""
    if not isinstance(totals, dict):
        return None, {}, [
            f"{owner} has no totals object (got {type(totals).__name__})"
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
                return None, {}, [
                    f"totals.{key} is missing (required total)"
                ]
            continue
        value = totals[key]
        if not is_count(value):
            return None, {}, [
                f"totals.{key} is not a non-negative int (got {value!r})"
            ]
        if role is TotalRole.CONTEXT:
            if value:
                context[key] = value
        elif role is TotalRole.REQUIRED or role is TotalRole.ERRORED:
            counted[key] = value
        else:
            # The one dispatch over the role vocabulary must be exhaustive
            # for real: a new TotalRole member has to be classified here
            # rather than absorbed by an `else`.
            raise AssertionError(f"unhandled TotalRole {role}")
    # The vocabulary boundary is checked in both directions: an unknown
    # total (a harness-side rename like `skipped` -> `not_run`) must not
    # be silently ignored — the contract's vocabulary is exhaustive.
    unknown = totals.keys() - {key for key, _ in TOTALS_KEYS}
    if unknown:
        return None, {}, [
            f"totals.{key} is not in the coverage contract's totals "
            "vocabulary (ADR-0482 §3)"
            for key in sorted(unknown)
        ]
    return counted, context, []


@dataclass(frozen=True)
class Summary:
    """The validated rollup the verdict reads (ADR-0482 §2/§3): the two
    required totals, behave's step-crash count (`errored` — counted like
    a failure, kept out of the category rollup), the not-counted context
    buckets reported (present and non-zero), the assertion-failure
    category breakdown (it sums to `failed` only — errored scenarios
    carry no category; `defect_count` is the headline population), and
    the per-reason skip breakdown behind the `skipped` total (visibility
    only — skips never count).

    Built only by [`Summary.validated`], so the executed-scenario count
    is non-zero by construction and no consumer re-reads the raw dict."""

    passed: int
    failed: int
    errored: int
    categories: dict[str, int]
    context: dict[str, int]
    skip_categories: dict[str, int]

    @classmethod
    def validated(
        cls,
        passed: int,
        failed: int,
        errored: int,
        categories: dict[str, int],
        context: dict[str, int],
        skip_categories: dict[str, int],
    ) -> tuple[Summary | None, list[str]]:
        """The one construction path, owning the non-empty-run invariant:
        a Summary exists at all only when the run executed something, and
        it owns its rollups (copies, never the caller's live dicts)."""
        summary = cls(
            passed,
            failed,
            errored,
            dict(categories),
            dict(context),
            dict(skip_categories),
        )
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

    @property
    def totals(self) -> dict[str, int]:
        """The one totals-document derivation: the counted members from
        this model's own fields, the reported context buckets beside
        them — the `error` ↔ `errored` rename lives here, beside the
        vocabulary it maps, so no writer re-spells document keys."""
        return {
            "passed": self.passed,
            "failed": self.failed,
            "error": self.errored,
            **self.context,
        }


def parse_summary(raw: object) -> tuple[Summary | None, list[str]]:
    """Validate one decoded `summary.json` and return its [`Summary`], or
    `(None, diagnostics)` on the first shape the contract does not
    recognize: a non-object summary; a missing or non-object `totals`
    object; a missing or misshapen total (the roles' absence rules live
    on [`TotalRole`]); a totals key outside the [`TOTALS_KEYS`] vocabulary; a
    missing `failure_categories`, or one that is not a str -> count map of
    identifier-shaped names; a `skip_categories` that is not that shape
    (absent is fine — older harnesses predate it); a rollup that breaks
    the @fails exclusion (`sum(failure_categories) != failed` —
    ADR-0482 §3); or a run that executed no scenarios. Total over its
    input; never a silent default.

    The per-scenario `features` section is NOT this parse's subject — the
    regression gate owns it (ADR-0661): coverage reads counts only."""
    if not isinstance(raw, dict):
        return None, [f"summary is not an object (got {type(raw).__name__})"]
    counted, context, diags = validate_totals(
        raw.get("totals"), owner="summary"
    )
    if counted is None:
        return None, diags
    # Both rollup members go through the one lookup: they differ only in
    # their absence rule — the failure breakdown is required (the @fails
    # contract is read off it), the skip breakdown is optional (a harness
    # older than the rollup omits it) — while a present-but-malformed
    # member of either is the same shape failure, named as such.
    categories, diags = rollup_member(raw, "failure_categories", required=True)
    if categories is None:
        return None, diags
    skip_categories, diags = rollup_member(
        raw, "skip_categories", required=False
    )
    if skip_categories is None:
        return None, diags
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
        counted["passed"],
        failed,
        counted["error"],
        categories,
        context,
        skip_categories,
    )


def category_counts(categories: Iterable[str]) -> list[tuple[str, int]]:
    """The public-safe breakdown of a new-failure set, in the family's
    one count-then-name order: category -> count with uncategorized
    entries (the empty string) under the no-category spelling — derived
    once from the categories alone, so the verdict table and the
    watchdog line cannot drift apart. Takes any iterable of category
    strings, the id maps' values included."""
    counts: dict[str, int] = {}
    for category in categories:
        key = category or "no-category"
        counts[key] = counts.get(key, 0) + 1
    return ordered_counts(counts)
