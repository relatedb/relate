"""The shared fixtures of the spec-harness suites (ADR-0661).

The three suites — `test_spec_harness_ids.py` (the run-side id
contract), `test_spec_harness_baseline.py` (the baseline document and
the comparison), `test_spec_harness_gate.py` (the CLI) — judge the one
family, so the module loaders, the sha constants, the fixture builders
and the one subprocess harness live here exactly once, imported as a
plain sibling module (the CI lanes run each suite directly with
`python3`, so `sys.path[0]` is `scripts/tests/`).
"""

import subprocess
import sys

from child_env import REPORTING_ENV, child_env
from script_loader import ROOT, load


common = load(
    "spec_harness_common", ROOT / "scripts" / "spec_harness_common.py"
)
ids_script = load(
    "spec_harness_ids", ROOT / "scripts" / "spec_harness_ids.py"
)
baseline_script = load(
    "spec_harness_baseline", ROOT / "scripts" / "spec_harness_baseline.py"
)
gate_script = load(
    "spec_harness_gate", ROOT / "scripts" / "spec-harness-gate.py"
)

SCRIPT = ROOT / "scripts" / "spec-harness-gate.py"

TOTALS_KEYS = common.TOTALS_KEYS


def run_script(argv, env=None):
    """The one subprocess harness for this suite: the interpreter, the
    script under test, and the scrubbed reporting env — the plumbing
    every CLI case (gate, writer, usage errors) goes through, exactly
    once."""
    return subprocess.run(
        [sys.executable, str(SCRIPT), *argv],
        capture_output=True, text=True,
        env=child_env(REPORTING_ENV, env),
    )

# Full 40-hex shas the fixtures share; the verdict renders them short.
ENGINE_SHA = "1f" * 20
HARNESS_SHA = "2a" * 20
OTHER_SHA = "3b" * 20

def failure(name, category="result-mismatch", marked=False):
    """One `features[].failures` entry. `marked` carries the `@fails`
    boolean the harness writes for expected failures (ADR-0482 §3)."""
    item = {
        "scenario": name,
        "category": category,
        "location": "features/tck/a.feature:12",
        "message": "first failure: row count differs",
    }
    if marked:
        item["neo4j_fails_tag"] = True
    return item


def nondet(name):
    return {
        "scenario": name,
        "functions": ["rand"],
        "category": "result-mismatch",
        "location": "features/tck/a.feature:20",
        "message": "uses rand() -- the expectation cannot be verified",
    }


def skip(name, reason="admin-command"):
    return {"scenario": name, "reason": reason, "detail": reason}


def errored(name, category=None):
    """One `features[].errored` entry — a step crash (ADR-0661
    bootstrap: the harness lists them per scenario)."""
    item = {"scenario": name, "location": "features/tck/a.feature:31"}
    if category is not None:
        item["category"] = category
    return item


def summary(features=None, totals=None, categories=None):
    """A summary.json in the harness's shape: the prefilled totals
    vocabulary (a run that executed nothing is a shape failure, so the
    default fixture executed a hundred scenarios), the genuine-failure
    rollup defaulting to whatever sums to `failed`, and the `features`
    map the gate's id walk reads."""
    values = {key: 0 for key, _ in TOTALS_KEYS}
    # Presence by identity, not truthiness: an explicit empty totals
    # dict is a supplied totals object (and the parser's shape failure
    # to judge), never silently replaced by the default.
    values.update({"passed": 100} if totals is None else totals)
    if categories is None:
        failed = values["failed"]
        categories = {} if failed == 0 else {"result-mismatch": failed}
    return {
        "generated": "2026-11-22T00:00:00+00:00",
        "command": "behave",
        "totals": values,
        "suites": {},
        "failure_categories": categories,
        "skip_categories": {},
        "features": {} if features is None else features,
    }


def features(
    failures=(), nondeterministic=(), skips=(), errored=(), path="a.feature"
):
    """One feature's entry, in the harness `build_summary` shape. The
    `errored` member is the one the gate tolerates absent (older
    harnesses predate the per-scenario error listing)."""
    return {
        f"features/tck/{path}": {
            "suite": "tck",
            "totals": {},
            "failures": list(failures),
            "nondeterministic": list(nondeterministic),
            "skips": list(skips),
            "errored": list(errored),
        }
    }


def baseline(
    failing=(),
    errored_ids=(),
    nondet_ids=(),
    skip_ids=(),
    engine=ENGINE_SHA,
    harness=HARNESS_SHA,
    schema=baseline_script.BASELINE_SCHEMA,
    totals=None,
):
    """A baseline document in the writer's shape: sorted unique id lists
    by construction, so a fixture never violates the invariant it is
    meant to test."""
    values = {key: 0 for key, _ in TOTALS_KEYS}
    # Presence by identity, not truthiness: an explicitly supplied
    # empty totals object is a supplied object (and the parse's shape
    # failure to judge), never silently replaced by the default.
    values.update(
        {"passed": 100, "failed": len(failing), "error": len(errored_ids)}
        if totals is None
        else totals
    )
    return {
        "schema": schema,
        "engine_sha": engine,
        "harness_sha": harness,
        "generated": "2026-11-22T00:00:00+00:00",
        "totals": values,
        "failure_categories": (
            {} if not failing else {"result-mismatch": len(failing)}
        ),
        "skip_categories": {},
        "failing": sorted(failing),
        "errored": sorted(errored_ids),
        "nondeterministic": sorted(nondet_ids),
        "skipped": sorted(skip_ids),
    }


def ids_of(names, path="a.feature"):
    """The id spelling for whole scenarios of the default feature —
    `<feature path>::<scenario name>`, the one the writer and the gate
    share (ADR-0661 §1)."""
    return [f"features/tck/{path}::{name}" for name in names]


def _validated(parse, data):
    """The one body the two parse helpers share: a contract-honoring
    fixture's parsed value, asserted against its diagnostics — the
    parameterized twin of assert_parse_failure."""
    parsed, diags = parse(data)
    assert parsed is not None, diags
    return parsed


def collect(data):
    """The ScenarioIds for a contract-honoring fixture (the shape tests
    call `ScenarioIds.from_summary` directly for the broken shapes)."""
    return _validated(ids_script.ScenarioIds.from_summary, data)


def parse_baseline(data):
    """The Baseline for a contract-honoring fixture."""
    return _validated(baseline_script.Baseline.from_json, data)


def assert_parse_failure(parse, data, needle):
    """The one shape-failure assertion every broken-shape case shares,
    parameterized by the parse callable: the parse yields nothing and a
    diagnostic naming `needle`. Values never appear in the diagnostic
    (ADR-0661 §4) — the assertion checks the needle, not the message."""
    parsed, diags = parse(data)
    if parsed is None:
        assert any(needle in d for d in diags), diags
    else:
        raise AssertionError(
            f"parse accepted a shape {needle!r} rejects: {parsed!r}"
        )


