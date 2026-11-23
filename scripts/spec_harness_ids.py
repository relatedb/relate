"""The run-side scenario-id model (ADR-0661 §1).

Everything that turns a summary's `features` section into scenario ids
lives here: the id grammar (`feature path :: scenario name`, opaque —
built once, compared whole, never split back apart), the [`ScenarioIds`]
model a run's ids land in, the one member table that drives both the
shape check and the collection walk, and the category rollup the
verdict and the watchdog share. The baseline document and the
comparison live in `spec_harness_baseline.py`; the shared summary
contract in `spec_harness_common.py`.

The gate semantics carried by the model: the run's *not-passing* set is
its genuine assertion failures plus its step-crash errors — `@fails` /
expected-fail-list marked failures, nondeterministic and skipped
scenarios stay outside the gate exactly as they stay outside coverage
(ADR-0482 §3).
"""

from __future__ import annotations

import re
import sys
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from pathlib import Path

_SCRIPTS_DIR = str(Path(__file__).resolve().parent)
if _SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, _SCRIPTS_DIR)

from spec_harness_common import (  # noqa: E402
    CATEGORY_NAME,
    SHA1_HEX,
    ShapeError,
)

# The one scenario-id separator, stated once: the id model (feature ::
# name) and every shape check that guards it read this constant.
ID_SEP = "::"

def _require_name(value: object, what: str, where: str) -> str:
    """The one name check every id part goes through: a non-empty
    string. The `::` separator may appear *inside* a name — corpus
    scenario names are prose that describes syntax, and '::' is Cypher
    syntax — because the id is opaque to every consumer: it is built
    once, compared as a whole, never split back apart. Anything but a
    non-empty string is a shape failure naming the position (`where`),
    never the value."""
    if not isinstance(value, str) or not value:
        raise ShapeError(f"{where} has no usable {what}")
    return value


def _scenario_id(path: str, name: str) -> str:
    """The one id builder: the feature path, the separator, the scenario
    name — the model every collection site composes through, so a
    change to it is one edit."""
    return path + ID_SEP + name


def _member_entry(item: object, member: str) -> dict:
    """The one entry check: a per-scenario list member is an object —
    narrowed here once, so every collector binds a dict instead of
    assuming one. Diagnostics name the member, never the value."""
    if not isinstance(item, dict):
        raise ShapeError(f"one {member} entry is not an object")
    return item


def _member_scenario(entry: dict, member: str) -> str:
    """One narrowed entry's `scenario` name, through the one name
    check."""
    return _require_name(
        entry.get("scenario"), "scenario name", f"one {member} entry"
    )


def _category_of(entry: dict, member: str, *, required: bool) -> str:
    """One entry's `category` — the required-or-default policy in the
    one helper: a failure always carries one (absence is a shape
    failure), an errored entry may predate categories (absence renders
    as the empty string, the verdict's step-crash count); present but
    malformed is a shape failure either way."""
    if "category" not in entry:
        if required:
            raise ShapeError(f"one {member} entry has no category")
        return ""
    category = entry["category"]
    if not isinstance(category, str) or not CATEGORY_NAME.fullmatch(
        category
    ):
        raise ShapeError(f"one {member} entry has a malformed category")
    return category


@dataclass
class _Walk:
    """The mutable collection state one `features` walk feeds — the
    collectors' shared destination, so no collector hands its own
    container around. `build()` is the single conversion from the walk
    state to the frozen [`ScenarioIds`] model: positional, checked, and
    copying every container so the model owns its rollups (the walk's
    live dicts die with the walk)."""

    failing: dict[str, str] = field(default_factory=dict)
    errored: dict[str, str] = field(default_factory=dict)
    nondeterministic: set[str] = field(default_factory=set)
    skipped: set[str] = field(default_factory=set)

    def build(self) -> "ScenarioIds":
        """The one conversion to the frozen model — copies throughout,
        the ownership policy every member shares."""
        return ScenarioIds(
            dict(self.failing),
            dict(self.errored),
            frozenset(self.nondeterministic),
            frozenset(self.skipped),
        )


_Collector = Callable[[object, str, "_Walk"], None]


def is_scenario_id(text: object) -> bool:
    """The one whole-id predicate, beside the one builder: a string
    whose first `ID_SEP` splits a non-empty feature path from a
    non-empty remainder — the scenario name, which may itself contain
    the separator (corpus names are prose; the id is opaque and never
    split back). The baseline module's id lists are judged by it."""
    if not isinstance(text, str):
        return False
    path, sep, name = text.partition(ID_SEP)
    return bool(sep and path and name)


def _record_failure(item: object, path: str, walk: _Walk) -> None:
    """One failure entry into the walk's id map. The `@fails` /
    expected-fail marking is the gate's exclusion rule (ADR-0482 §3): a
    marked failure is the expected state, not a regression, so it never
    enters the not-passing set. Presence, shape and exclusion are three
    explicit facts: the marking is judged by membership (present null is
    malformed, never silently unmarked), a present value must be
    boolean true, and a marked entry returns. A genuine failure carries
    its category (the harness always writes one)."""
    entry = _member_entry(item, "failures")
    scenario = _member_scenario(entry, "failures")
    if "neo4j_fails_tag" in entry:
        marked = entry["neo4j_fails_tag"]
        if marked is not True:
            raise ShapeError(
                "one failures entry's neo4j_fails_tag is not boolean true"
            )
        return
    walk.failing[_scenario_id(path, scenario)] = _category_of(
        entry, "failures", required=True
    )


def _record_errored(item: object, path: str, walk: _Walk) -> None:
    """One errored entry into the walk's id map. A category is optional
    (the harness's error listing may not classify); an uncategorized
    entry reports under the verdict's step-crash count."""
    entry = _member_entry(item, "errored")
    scenario = _member_scenario(entry, "errored")
    walk.errored[_scenario_id(path, scenario)] = _category_of(
        entry, "errored", required=False
    )


def _collect_nondeterministic(item: object, path: str, walk: _Walk) -> None:
    """One nondeterministic entry — membership only: it gates nothing,
    it only excuses."""
    entry = _member_entry(item, "nondeterministic")
    walk.nondeterministic.add(
        _scenario_id(path, _member_scenario(entry, "nondeterministic"))
    )


def _collect_skipped(item: object, path: str, walk: _Walk) -> None:
    """One skipped entry — membership only, like its nondeterministic
    sibling."""
    entry = _member_entry(item, "skips")
    walk.skipped.add(_scenario_id(path, _member_scenario(entry, "skips")))


# The ONE member vocabulary: (member, may-be-absent, collector). The
# shape check and the collection walk in ScenarioIds.from_summary both
# iterate this table, so a member is spelled exactly once — as its row.
# Absence rules differ only for `errored`, which the harness summary
# gained with per-scenario error listing (ADR-0661 bootstrap) — an
# older harness omits it and the walk reads that as empty.
FEATURE_MEMBERS: tuple[tuple[str, bool, _Collector], ...] = (
    ("failures", False, _record_failure),
    ("nondeterministic", False, _collect_nondeterministic),
    ("skips", False, _collect_skipped),
    ("errored", True, _record_errored),
)


@dataclass(frozen=True)
class ScenarioIds:
    """The per-scenario identity of one run (ADR-0661 §1): genuine
    failures and step crashes as id -> category maps (the categories are
    the verdict's public-safe breakdown; an errored entry without one
    carries the empty string), nondeterministic and skipped scenarios as
    membership only — they gate nothing, they only excuse. Frozen and
    self-owning: built only through a [`_Walk`], which copies every
    container on the way in."""

    failing: dict[str, str]
    errored: dict[str, str]
    nondeterministic: frozenset[str]
    skipped: frozenset[str]

    @classmethod
    def from_summary(
        cls, raw: dict
    ) -> tuple["ScenarioIds | None", list[str]]:
        """Collect the ids from a summary's `features` section, or
        `(None, diagnostics)` on the first shape the contract does not
        recognize: a missing or non-object `features`; a feature entry
        that is not an object; a missing `failures` /
        `nondeterministic` / `skips` list (the harness always writes
        them — absence is a writer change, not an empty run; a present
        JSON null is the same malformation, judged by membership, not
        by a None-catch-all); an `errored` list the harness gained
        later (absent is fine); a list member without a usable
        `scenario`; an `@fails` marking that is not boolean true; a
        category that is not identifier-shaped. Positions are named,
        values never (ADR-0661 §4)."""
        walk = _Walk()
        try:
            features = raw.get("features")
            if not isinstance(features, dict):
                raise ShapeError("features is missing or not an object")
            for path, entry in features.items():
                _require_name(path, "feature path", "a feature key")
                if not isinstance(entry, dict):
                    raise ShapeError(
                        "the entry for one feature is not an object"
                    )
                # The one loop over the member vocabulary: the shape
                # rules (presence by membership, list-ness) and the
                # collection both ride the table — each member is
                # spelled once, as its row.
                for member, optional, collector in FEATURE_MEMBERS:
                    if member not in entry:
                        if optional:
                            continue
                        raise ShapeError(
                            f"one feature's {member} is missing "
                            "(required member)"
                        )
                    items = entry[member]
                    if not isinstance(items, list):
                        raise ShapeError(
                            f"one feature's {member} is not a list"
                        )
                    for item in items:
                        collector(item, path, walk)
        except ShapeError as err:
            return None, [str(err)]
        return walk.build(), []

    @property
    def not_passing(self) -> frozenset[str]:
        """The set the gate compares: genuine failures plus step
        crashes — the scenarios that did not pass and are not excused."""
        return frozenset(self.failing) | frozenset(self.errored)
