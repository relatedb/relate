# ADR-0526: Memory controls gate

Status: Proposed
Claim: 29fa3907d6121880c5e1073f08b90c0f4cad32ac

## Context

The processing-memory run checked that connection and workspace limits
hold, and it measured resident memory per live logical workspace byte.
It printed those results and then exited. Nothing compared them with a
frozen range, so a quieter process and a leaked process looked the same
on a pull request.

The measurement only means something inside the ADR-0070 container, and
only when the ADR-0071 environment class matches. A pull request cannot
spend the full three-repeat calibration matrix on every push. The freeze
and a nightly run can.

## Decision

1. The suite is `memory-controls`. A control verification checks that a
   limit holds: `connection-cap`, `unlimited-connections`,
   `concurrent-logical-workspace-limit`, and the configured workload.
   Workspace memory calibration is the case
   `workspace-memory-calibration`. The report is schema 3 and stores the
   Given / When / Then text with each case.

2. `baselines/memory-controls.json` is a min/max envelope, schema 1,
   frozen by `make baseline-memory-controls` inside
   `create-loadtest-baselines`. Until that file is frozen, compare
   reports `no-baseline` and exits 0.

3. A behavioural miss is a regression. There is no improvement verdict
   for behaviour. Numeric checks are lower-is-better. A regression is
   above `max * (1 + band)` and above `max + floor`. An improvement is
   below `min * (1 - band)` and below `min - floor`. RSS uses 15% and
   1 MiB. Calibration ratios and slopes use 20%. Workload limited counts
   use a band of 0 and a floor of 2, so fewer refusals is an improvement.
   An improvement fails the job. The comment asks for an investigation
   and, when the improvement is real, a re-freeze.

4. An environment-class or knob mismatch is `incompatible`. `CI_COMPARE=1`
   fails the job. A local compare skips the numeric bands and still
   judges behaviour. `calibration_repeats` is not a knob in that match:
   the pull-request profile (`MEMORY_CONTROLS_PROFILE=pr`) runs one
   repeat of every family, and the freeze and the nightly
   `memory-controls-full` stage run three.

5. The pull-request job posts one self-updating comment. All clear, the
   comment says the controls and calibrations held. Otherwise it quotes
   each failing check with its verdict and its Given / When / Then text.
   The nightly full run writes the step summary and does not comment.

## Alternatives considered

- Gate only regressions, and treat an improvement as a note. Rejected:
  an improvement that is a false positive hides a broken measurement,
  and a real improvement leaves the envelope stale so the next quiet
  regression still passes.
- Run the three-repeat matrix on every pull request. Rejected: each
  observation starts a fresh server, and seventy-five of them take tens
  of minutes. One repeat still covers every family.
- Fold this suite into the fraud and identity comparator. Rejected: those
  bands are throughput and p99. This suite's pass/fail is a limit that
  held, or a memory range.

## Consequences

- The first pull request after this decision shows `no-baseline` until
  `create-loadtest-baselines` merges `baselines/memory-controls.json`.
- A real improvement needs a new freeze. The job stays red until that
  envelope lands.
- Host RSS numbers stay out of the envelope. `make baseline-memory-controls`
  refuses `BASELINE_ENV=host`.
