"""The gate baseline document and the comparison against it (ADR-0661).

The machine-written baseline (`baselines/engine-main.json` in the
harness repository) and the verdict a run earns against it are one
contract with two sides, so they live here together: the parse (a
[`Baseline`] from one decoded document, every writer invariant
enforced), the emit (`emit_baseline`, the merge run's write side — the
id derivation and the document spelling in one home, so writer and
reader cannot drift), and the comparison ([`gate_verdict`] /
[`Verdict`], the set semantics ADR-0661 §1 pins, which the watchdog's
delta reads structurally).

The run-side id model lives in `spec_harness_ids.py`; this module
imports [`ScenarioIds`] for its comparison signatures and touches
nothing else of it. The shared summary contract and the one
[`ShapeError`] taxonomy live in `spec_harness_common.py`.
"""

from __future__ import annotations

import enum
import json
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import TypeVar

_SCRIPTS_DIR = str(Path(__file__).resolve().parent)
if _SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, _SCRIPTS_DIR)

from spec_harness_common import (  # noqa: E402
    SHA1_HEX,
    ShapeError,
    Summary,
    category_counts,
    rollup_member,
    short_sha,
    validate_totals,
)
from spec_harness_ids import (  # noqa: E402
    ScenarioIds,
    is_scenario_id,
)

_T = TypeVar("_T")

# The baseline document's schema version. A bump is a writer/gate
# contract change — the parse refuses older and newer alike, loudly.
BASELINE_SCHEMA = 1

# The id members the document carries, in the one vocabulary the reader
# validates and the writer emits: (member, model field). Stated once —
# the reader's loop and the writer's loop both iterate it.
BASELINE_ID_MEMBERS: tuple[tuple[str, str], ...] = (
    ("failing", "failing"),
    ("errored", "errored"),
    ("nondeterministic", "nondeterministic"),
    ("skipped", "skipped"),
)


class FailureKind(enum.Enum):
    """How a scenario failed — the partition the verdict's one new-failing
    map carries beside each category: an assertion failure or a step
    crash (ADR-0661 §1)."""

    ASSERTION = "assertion"
    STEP_CRASH = "step-crash"


def _unwrap(value: _T | None, diags: list[str]) -> _T:
    """The one bridge from the shared tuple-validators to the raising
    model: a validator's None return raises its diagnostics, a parsed
    value passes back through — the callers bind the validated value
    directly, no ad-hoc keyed round-trips."""
    if value is None:
        raise ShapeError("; ".join(diags))
    return value


def _sha_field(raw: dict, key: str) -> str:
    """One baseline sha, validated and returned — the 40-hex check and
    the binding in one place."""
    value = raw.get(key)
    if not isinstance(value, str) or not SHA1_HEX.fullmatch(value):
        raise ShapeError(f"baseline.{key} is not a 40-hex sha")
    return value


def _baseline_ids(
    raw: dict, member: str
) -> tuple[frozenset[str] | None, list[str]]:
    """One baseline id list through the writer's invariants: a list of
    id-shaped strings (judged by the id model's one predicate), sorted
    and unique. The order is part of the contract (machine-written,
    byte-stable for a byte-identical run), so an unsorted or duplicated
    list is named as corruption."""
    if member not in raw:
        return None, [f"baseline.{member} is missing (required member)"]
    value = raw[member]
    if not isinstance(value, list) or not all(
        is_scenario_id(t) for t in value
    ):
        return None, [
            f"baseline.{member} is not a list of scenario ids"
        ]
    if value != sorted(set(value)):
        return None, [
            f"baseline.{member} is not the writer's sorted unique list"
        ]
    return frozenset(value), []


@dataclass(frozen=True)
class Baseline:
    """The machine-written regression baseline (ADR-0661 §2), modeled as
    what the gate reads: the shas it was measured at and the four id
    sets the comparison and the excusals use. The document carries more
    — generated stamp, totals, category rollups — for the humans who
    read the harness repository; the parse validates every member (a
    corrupt totals object or rollup is a hard failure, the fail-closed
    stance) without mirroring it onto the model: a field comes back with
    its reader. Built only by [`Baseline.from_json`], which also enforces
    the writer's invariants — sorted, unique, id-shaped lists."""

    engine_sha: str
    harness_sha: str
    failing: frozenset[str]
    errored: frozenset[str]
    nondeterministic: frozenset[str]
    skipped: frozenset[str]

    @classmethod
    def from_json(cls, raw: object) -> tuple[Baseline | None, list[str]]:
        """Validate one decoded baseline, or `(None, diagnostics)` on the
        first broken invariant: a non-object document; a schema other
        than the one this gate reads; a sha that is not 40-hex; a totals
        object outside the ADR-0482 §3 vocabulary; a malformed category
        rollup; or an id list that is not sorted, unique and id-shaped —
        the writer's invariants, so their violation is corruption, not
        content. Local checks raise, the shared tuple-validators go
        through the one [`_unwrap`] bridge, and a single except owns the
        return — the linear shape the features walk demonstrates."""
        try:
            if not isinstance(raw, dict):
                raise ShapeError(
                    f"baseline is not an object "
                    f"(got {type(raw).__name__})"
                )
            schema = raw.get("schema")
            if schema != BASELINE_SCHEMA:
                raise ShapeError(
                    f"baseline schema {schema!r} is not "
                    f"{BASELINE_SCHEMA} — the writer contract moved; "
                    "update this gate with it"
                )
            engine_sha = _sha_field(raw, "engine_sha")
            harness_sha = _sha_field(raw, "harness_sha")
            generated = raw.get("generated")
            if not isinstance(generated, str) or not generated:
                raise ShapeError(
                    "baseline.generated is not a non-empty string"
                )
            counted, _context, diags = validate_totals(
                raw.get("totals"), owner="baseline"
            )
            # The corruption stance, not a mirror: every member is
            # validated (a malformed one raises through _unwrap), and
            # the parsed values the gate never reads are discarded —
            # they ride in the document for the humans who read the
            # harness repository, not on the model.
            _unwrap(counted, diags)
            failure_categories, diags = rollup_member(
                raw, "failure_categories", required=True
            )
            _unwrap(failure_categories, diags)
            skip_categories, diags = rollup_member(
                raw, "skip_categories", required=True
            )
            _unwrap(skip_categories, diags)
            # The one id-member vocabulary drives the read: each row's
            # list validates into the field the row names.
            id_fields = {
                field: _unwrap(*_baseline_ids(raw, member))
                for member, field in BASELINE_ID_MEMBERS
            }
            return (
                cls(
                    engine_sha=engine_sha,
                    harness_sha=harness_sha,
                    **id_fields,
                ),
                [],
            )
        except ShapeError as err:
            return None, [str(err)]

    @property
    def known(self) -> frozenset[str]:
        """The set a PR's failures are excused by: everything the
        baseline carries as not passing, nondeterministic or skipped —
        a scenario that never passed on main cannot regress. Composed
        over [`not_passing`], never re-derived beside it."""
        return self.not_passing | self.nondeterministic | self.skipped

    @property
    def not_passing(self) -> frozenset[str]:
        """What the baseline carries as genuinely not passing — the
        population a PR can shrink (its `fixed` count)."""
        return self.failing | self.errored


@dataclass(frozen=True)
class Verdict:
    """The comparison's answer, derived once and rendered twice (the
    comment table on stdout, the status line on stderr). The new-failing
    population is ONE map — id -> ([`FailureKind`], category) — so every
    population question (red, the count, the per-kind breakdown, the
    watchdog's rollup) reads the same data, never a re-derived union of
    parallel maps. Counts and categories only — never an id (ADR-0661
    §4). Both shas are the baseline's own provenance: the engine main
    and harness revision it measured."""

    new: dict[str, tuple[FailureKind, str]]
    fixed: int
    carried: int
    engine_sha: str
    harness_sha: str

    @property
    def red(self) -> bool:
        """Red iff anything new failed."""
        return bool(self.new)

    @property
    def new_count(self) -> int:
        """New failing scenarios: assertion failures plus step
        crashes — the headline number of a red verdict."""
        return len(self.new)

    def _of_kind(self, kind: FailureKind) -> dict[str, str]:
        """The new-failing ids of one kind, as the id -> category map the
        renderers break down."""
        return {
            id_: category
            for id_, (found, category) in self.new.items()
            if found is kind
        }

    @property
    def new_failed(self) -> dict[str, str]:
        """The new assertion failures."""
        return self._of_kind(FailureKind.ASSERTION)

    @property
    def new_errored(self) -> dict[str, str]:
        """The new step crashes."""
        return self._of_kind(FailureKind.STEP_CRASH)

    @property
    def counts_phrase(self) -> str:
        """The per-kind breakdown both renderers interpolate — one
        spelling, so the table and the status line cannot drift."""
        return (
            f"{len(self.new_failed)} assertion failures, "
            f"{len(self.new_errored)} step crashes"
        )

    @property
    def fixed_phrase(self) -> str:
        """The fixed-on-this-PR phrase both renderers interpolate, with
        its zero-suppression policy stated once: nothing fixed renders
        nothing, not a zero."""
        return f"{self.fixed} fixed on this PR" if self.fixed else ""

    @property
    def light(self) -> str:
        """The verdict's traffic light, derived once for both renderers
        — the same discipline the coverage report's `Summary.light`
        carries for its bands."""
        return "🔴" if self.red else "🟢"


def gate_verdict(ids: ScenarioIds, baseline: Baseline) -> Verdict:
    """The one comparison (ADR-0661 §1) — the single home of the
    excusal derivation, read by the PR verdict and by the watchdog's
    delta alike: the new-failing map carries the run's failures and
    step crashes that the baseline does not know, each with its kind;
    fixed is the baseline's not-passing minus the run's. The shas the
    verdict renders are the baseline's own — the run main measured."""
    known = baseline.known
    new: dict[str, tuple[FailureKind, str]] = {}
    for id_, category in ids.failing.items():
        if id_ not in known:
            new[id_] = (FailureKind.ASSERTION, category)
    for id_, category in ids.errored.items():
        if id_ not in known:
            new[id_] = (FailureKind.STEP_CRASH, category)
    return Verdict(
        new=new,
        fixed=len(baseline.not_passing - ids.not_passing),
        carried=len(baseline.not_passing & ids.not_passing),
        engine_sha=baseline.engine_sha,
        harness_sha=baseline.harness_sha,
    )


def emit_baseline(
    ids: ScenarioIds,
    summary: Summary,
    *,
    engine_sha: str,
    harness_sha: str,
    path: Path,
) -> str:
    """Write the baseline document (ADR-0661 §2) and return the one
    'wrote' line for the CLI to print — the document module writes
    documents, the CLI owns every output line. Every value comes from
    the validated [`Summary`] and the walk's ids — the rollups from the
    model's own fields, the totals from the contract's one
    totals-document derivation, the id lists through the one member
    table — so nothing re-indexes a raw document and the emitted file
    is written from validated data. The shas are keyword-only: two
    adjacent same-typed provenance strings cannot be swapped by a
    positional call. A byte-identical run writes a byte-identical
    file."""
    document = {
        "schema": BASELINE_SCHEMA,
        "engine_sha": engine_sha,
        "harness_sha": harness_sha,
        "generated": datetime.now(timezone.utc).isoformat(),
        "totals": summary.totals,
        "failure_categories": summary.categories,
        "skip_categories": summary.skip_categories,
        **{
            member: sorted(getattr(ids, field))
            for member, field in BASELINE_ID_MEMBERS
        },
    }
    path.write_text(json.dumps(document, indent=1) + "\n", encoding="utf-8")
    return (
        f"spec-harness-baseline: wrote {path} — engine "
        f"{short_sha(engine_sha)}, harness {short_sha(harness_sha)}, "
        f"{len(ids.failing)} failing, {len(ids.errored)} errored, "
        f"{len(ids.nondeterministic)} nondeterministic, "
        f"{len(ids.skipped)} skipped"
    )


def previous_delta(
    previous: Baseline | None, ids: ScenarioIds, harness_sha: str
) -> str:
    """The watchdog's one machine-readable line (ADR-0661 §6) — the
    single home of the format: both arms derive the four field values
    (the bootstrap's zeros and `-` included) and render them through
    the one f-string, so the line's spelling exists once.
    `previous=None` is the bootstrap's first write. The delta is the
    comparison's own verdict against the previous baseline — agreement
    is structural, not re-derived — and the lane's writer step only
    reads the line and files the issue when `new=` is non-zero."""
    if previous is None:
        new_count, fixed, moved, categories = 0, 0, 0, "-"
        suffix = " (no previous baseline — first write)"
    else:
        verdict = gate_verdict(ids, previous)
        new_count = verdict.new_count
        fixed = verdict.fixed
        moved = int(previous.harness_sha != harness_sha)
        breakdown = category_counts(
            category for _kind, category in verdict.new.values()
        )
        categories = (
            ",".join(f"{name}:{count}" for name, count in breakdown)
            if breakdown
            else "-"
        )
        suffix = ""
    return (
        f"spec-harness-baseline: delta new={new_count} fixed={fixed} "
        f"harness_moved={moved} new_categories={categories}{suffix}"
    )
