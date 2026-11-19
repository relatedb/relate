---
type: adr
area: infra
claim: 5ebf5c5ae57e030046c7a172402071b7d6701580
name: 0512-single-source-loadtest-runner-class
status: proposed
tags:
  - ci
  - loadtest
title: Single-source loadtest runner class (`vars.LOADTEST_RUNNER`)
REFERENCES:
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
SUPERSEDES:
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
---

# ADR-0512: Single-source loadtest runner class (`vars.LOADTEST_RUNNER`)

## Context

[ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) made wall-clock loadtest comparison valid only when the frozen
envelope's `environment` matches the per-PR report's, and named the runner
class as one of the class fields. The class was then pinned **by hand in
six places**: `runs-on` and the `BASELINE_RUNNER` stamp in `ci.yml`'s
query-latency lane, the same pair in `ci.yml`'s loadtest-baselines lane,
and the `runner` input default plus stamp in
`create-loadtest-baselines.yml`. Nothing tied them together.

They drifted, and the drift has a treadmill shape: the two comparison
lanes ran on **different classes** (query-latency on `linux-4-core-x86`
per the LABS-310 rationale; loadtest-baselines on `ubuntu-24.04`), while
the freeze stamps **all six envelopes from one input**. Every freeze
therefore matched one suite and broke the other — a default (ubuntu)
freeze flipped the query-latency baselines away from its 4-core lane, a
4-core freeze did the reverse to fraud/identity — and the standing state
was envelopes frozen on `linux-4-core-x86` beside an `ubuntu-24.04`
fraud/identity lane, INCOMPATIBLE on every PR (LABS-946 item 1).

## Decision

1. **One class, one source: the repo Actions variable
   `LOADTEST_RUNNER`.** GitHub Actions has no cross-file constants, but
   both `runs-on` and `env` accept the `vars` context — so the variable
   is the one place the class label lives. Every stamping consumer reads
   it:
   - `ci.yml` `query-latency`: `runs-on: ${{ vars.LOADTEST_RUNNER }}`,
     `BASELINE_RUNNER: github-actions:${{ vars.LOADTEST_RUNNER }}`
   - `ci.yml` `loadtest-baselines`: the same pair (this moves the lane
     off its hand-pinned `ubuntu-24.04` onto the shared class — the
     current envelopes already carry `linux-4-core-x86`, so the
     comparison matches again with **no re-freeze**)
   - `create-loadtest-baselines.yml`: `runs-on: ${{ inputs.runner ||
     vars.LOADTEST_RUNNER }}` and the stamp from the same expression —
     the `runner` input default is now **empty**; a default freeze always
     lands on the class the comparisons run on, and the input remains the
     deliberate class-change tool
2. **The class is `linux-4-core-x86`** (the variable's value): the
   LABS-310 compile-bound rationale already put query-latency there, a
   quieter host steadies the timed suites, and the committed envelopes
   are frozen on it.
3. **A class change is a two-step, not a hunt**: update the variable,
   dispatch the freeze (default input — it reads the same variable).
   A missing or empty variable fails the workflow loudly (an empty
   `runs-on`), never silently falls back to a class.
4. Non-stamping lanes are untouched: `loadtest-smoke` (no comparison) and
   everything else keep their own labels.

## Alternatives considered

- **Re-freeze on ubuntu-24.04 (the old default)** — rejected: it matches
  only fraud/identity and breaks the query-latency comparison whose lane
  is 4-core; the treadmill continues.
- **Two variables, one per suite** — rejected: the freeze workflow stamps
  all envelopes from one runner; two classes can never both match a
  single freeze, so the classes must converge first.
- **Stamping from the actual runner inside the job** — rejected: the
  `runner` context exposes `os`/`arch`, not the `runs-on` label, so the
  class label must be named somewhere — one variable is the smallest
  naming surface.

## Consequences

- The fraud/identity loadtest comparison works again on every PR, against
  the envelopes as committed — no re-freeze needed for this change.
- `BASELINE_RUNNER` and `runs-on` can no longer disagree: they are one
  expression in every lane.
- A future class change (say 8-core) touches one repo variable plus one
  freeze dispatch; no workflow edits.
- [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md) §4's "both use `ubuntu-24.04`" is superseded by this ADR: the
 shared class is whatever the variable says — the invariant [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)
  wanted (PR job and freeze share a class) finally holds **structurally**
  instead of by hand-copying a label into six sites.
