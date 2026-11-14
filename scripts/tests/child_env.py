"""The one reporting-env scrub policy for the scripts/tests/ suites.

A suite that runs a scripts/ file under test must not inherit the
reporting environment of the runner it happens to execute on: the subject
appends to `$GITHUB_STEP_SUMMARY` whenever it is set, reads
`GITHUB_ACTIONS` to pick its rendering, and shells out to `gh` with
whatever `GITHUB_*`/`GH_*` state it finds. An unscrubbed env would write
fabricated rows into a real job's step summary and make the suite's verdict
depend on the machine it runs on.

Each suite owns its scrub vocabulary — the names its subject reads and
must therefore see unset, or see set deliberately — and passes the values
it wants on top. The policy itself (copy `os.environ`, remove the scrub
list, apply the overrides) lives here, exactly once. `REPORTING_ENV` is
the pair every suite whose subject touches the step summary must scrub;
suites with more to scrub derive from it rather than re-spelling the pair.

This is the reporting-env policy specifically. A suite with a different
ambient vocabulary names it at its own call site — the comparator suites'
knob prefixes (`LOADTEST_*`, `QUERY_LATENCY_*`) are matched in
comparator_runner.scrubbed_env, which then routes the matched names through
`child_env` so the copy-scrub-override shape stays here alone.

The suites previously carried a copy each — `test_spec_harness_coverage.py`
and `test_compare_baselines.py` the reporting pair, `test_update_pr_comment.py`
a twelve-name list, `adr_fixtures.py` the git-hook variables — and they had
drifted apart, so the policy could not be read in one place.

Imported as a plain sibling module: the Makefile runs each test file
directly with `python3`, so `sys.path[0]` is `scripts/tests/`.
"""

from __future__ import annotations

import os
from collections.abc import Iterable, Mapping

# The Actions reporting pair: the step-summary target a subject appends its
# report to, and the Actions marker that travels with it. A suite whose
# subject writes a report scrubs both; a suite with more to scrub derives
# its list from this tuple.
REPORTING_ENV = ("GITHUB_ACTIONS", "GITHUB_STEP_SUMMARY")


def child_env(
    scrub: Iterable[str] = (),
    overrides: Mapping[str, str | None] | None = None,
) -> dict[str, str]:
    """The environment for one scripts/ file under test.

    Starts from the test process's own environment, removes every name in
    `scrub` (the inherited runner state the subject must not see), then
    applies `overrides` in order. An override of `None` removes the name
    instead of setting it, so a case that needs one of the suite's own
    defaults absent — `unset` in test_update_pr_comment.py — states it in
    the same call rather than in a second pop loop beside it.

    `overrides` is one mapping rather than `**kwargs` on purpose: every
    call site merges dicts into it (`BASE_ENV`, the caller's `extra_env`),
    and a keyword spelled both explicitly and inside a `**` unpacking is a
    TypeError at the call, not a last-wins merge.
    """
    env = os.environ.copy()
    for key in scrub:
        env.pop(key, None)
    for key, value in (overrides or {}).items():
        if value is None:
            env.pop(key, None)
        else:
            env[key] = value
    return env
