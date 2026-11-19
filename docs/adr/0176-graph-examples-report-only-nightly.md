---
type: adr
area: infra
date: 2026-10-07
deciders: Oskar Hane (decided in conversation), CypherLite maintainers
name: 0176-graph-examples-report-only-nightly
status: accepted
tags:
  - ci
  - conformance
title: Graph-examples nightly is report-only — diffs never fail the run
REFERENCES:
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0204-nightly-pull-retry-and-diagnostics]]"
SUPERSEDES:
  - "[[adr/0144-graph-examples-differential-suite]]"
---

# 0176. Graph-examples nightly is report-only — diffs never fail the run

## Context

[ADR-0144](./0144-graph-examples-differential-suite.md) gave the graph-examples differential suite a "cip-eval-style
ratchet" Reporting decision: a committed baseline of known diffs
(`testing/cypherlite-graph-examples/baselines/graph-examples.txt`), where
any diff not in the baseline fails the run and new passes must update the
baseline (`CYPHERLITE_GRAPH_EXAMPLES_UPDATE_BASELINE=1`).

That contract has not worked in practice. The baseline file was never
seeded, so every nightly run is red. The diffs it would ratchet against
are real, long-lived engine gaps (LABS-182 through LABS-187 were filed
from run 37567417871), not regressions — the nightly is a *signal* about
where cypherlite diverges from Neo4j on real data, not a merge gate, and
its per-query reference timeboxes are environment-sensitive, so a
committed baseline flaps even when nothing changed. A permanently red
nightly teaches everyone to ignore it, which is worse than no red at all.

## Decision

The graph-examples differential nightly is **report-only**. It reports
every outcome and fails only on infrastructure failures.

**Never fails the run:**

- **DIFF** outcomes — a row/content mismatch between the Neo4j reference
  and the pulled cypherlite copy. These are the engine gaps the suite
  exists to surface; they are reported and tracked in Linear (e.g.
  LABS-182..187), not gated on.
- **Order-only notes** — rows that match as a bag but differ in order.
- **Skips** — `gds`, gated `apoc`, and `dialect-skip` classifications
 ([ADR-0144](./0144-graph-examples-differential-suite.md)), and clean self-skips when the cache is cold and
  network/docker are absent.
- **Inconclusive reference-timebox outcomes** — a statement whose Neo4j
  reference run exceeds its timebox has no trustworthy expected result,
  so there is nothing to diff; it is reported as inconclusive.

**Fails the run (infrastructure failures only):**

1. **A dataset's setup fails**: the dump or adoc fetch fails or its
   sha256 mismatches, database registration or the online-poll fails,
   `cypherlite pull` fails, times out, or is killed, or the pulled copy
   fails to open. Without the data in place there is nothing to
   differentially test, and a green report would be a lie.
2. **The reference container fails to boot** (already a panic today):
   with no reference, no result is interpretable.
3. **A pull-verification count mismatch**: pulled node/rel counts differ
   from the reference counts. This **does fail the run** — it is an
   infrastructure failure, not a diff, because every downstream diff for
   that dataset is then uninterpretable: the comparison would be against
   a different graph.
4. **A dataset whose adoc yields zero runnable statements** (already an
   error today): the suite's only input is the adoc; zero statements
   means the extraction broke, not that there is nothing to test.

**The ratchet machinery is removed.** The committed baseline
(`testing/cypherlite-graph-examples/baselines/graph-examples.txt`) and
the `CYPHERLITE_GRAPH_EXAMPLES_UPDATE_BASELINE` env mode are deleted.
The per-statement id (`dataset:adoc-line:sha256-prefix`, as today) stays
in the report, so nightly reports remain diffable across runs and a
new or resolved diff is visible by comparing report artifacts.

## Alternatives considered

- **Keep the ratchet baseline.** Rejected: per-query reference timeboxes
  are environment-sensitive, so the baseline flaps between runs even when
  no engine behavior changed; the nightly is signal, not a merge gate,
  and a ratchet is the wrong shape for signal.
- **Fail on any diff (no baseline at all).** Rejected: the run would be
  permanently red, because real engine gaps are long-lived and tracked in
  Linear (LABS-182..187); a permanently red nightly gets ignored and the
  signal is lost.
- **Keep the baseline but seed it from CI.** Rejected: churn without
  value — the seeded baseline would need re-seeding on every environment
  shift, hitting the same flap problem, while adding no information the
  report itself does not already carry.

## Consequences

- The nightly goes green whenever the harness itself is healthy,
  regardless of how many engine diffs exist; red now means "the suite is
  broken", which is actionable, instead of "cypherlite is not Neo4j",
  which is known.
- Engine diffs are tracked in Linear (LABS-182..187 and successors);
  closing one is verified by the diff disappearing from the nightly
  report, not by a baseline edit.
- `testing/cypherlite-graph-examples` loses the baseline file, the
  `CYPHERLITE_GRAPH_EXAMPLES_UPDATE_BASELINE` mode, and the
  baseline-comparison panic; report generation and the stable
  per-statement ids stay.
- [ADR-0144](./0144-graph-examples-differential-suite.md) is immutable; it gains a **Later references** note pointing
  here, and its Reporting decision bullet is superseded by this ADR.

## Later references

[ADR-0204](./0204-nightly-pull-retry-and-diagnostics.md) hardens the nightly harness this ADR defines: a pull that fails
with a transient mid-statement stream disconnect is retried once from
scratch instead of immediately becoming an infrastructure failure (kinds
1/3/4), and the harness captures the reference container's logs into the
report dir before the failure-path panic's cleanup removes the container,
so the diagnostics upload is populated on graceful-but-failing exits too.
