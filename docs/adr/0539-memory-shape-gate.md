# ADR-0539: Memory-shape gate

Status: Proposed
Claim: 0bad150d5f87bf2b90805b12a876a82824c53ff4

## Context

The memory-shape loadtest filled each shape until the 32 MB logical
limit. A cheaper graph loaded more nodes, so two runs were not the same
graph. The old worst-of-N file keeps one snapshot. A later run had no
min and max to sit inside, so a quieter graph and a leaked graph looked
the same on a pull request.

The measurement only means something inside the ADR-0070 container, and
only when the ADR-0071 environment class matches.

## Decision

1. The loadtest stops at fixed counts. Flat is 120,000 nodes. Fanout is
   52,016 nodes and 52,000 relationships. Ring is 46,000 nodes and
   46,000 relationships, including the closing edge. Dense ring is
   5,000 nodes and 100,000 relationships. Base stays empty. The 32 MB
   logical limit stays as a ceiling. Reaching it before the count fails
   the run. The report is schema 2. Each shape stores Given / When /
   Then / Outcome. Sizes print as MB, or GB once a value reaches 1024 MB.

2. `baselines/memory-shape.json` is a min/max envelope, schema 1, frozen
   by `make baseline-memory-shape` inside `create-loadtest-baselines`.
   It is separate from `baselines/memory-shape-loadtest.json`, which
   `make baseline-loadtests` still owns. The freeze runs the loadtest
   `BASELINE_RUNS` times and keeps min and max per shape for logical
   bytes, disk, attributed RSS, and the ratio. The ratio is attributed
   RSS divided by logical bytes.

3. `make compare-memory-shape` judges one run. Counts that miss the
   targets are a regression. Logical bytes, disk, and attributed RSS are
   lower-is-better, with a 15% band and a 1 MB floor. The ratio is
   lower-is-better, with a 5% band. A regression clears the top by both
   the band and the floor. An improvement clears the bottom the same
   way and fails until the envelope is re-frozen. The document states
   Baseline, Validated, and Report for every shape, including a green
   run. An environment-class or knob mismatch is incompatible. A local
   mismatch skips the numeric bands. `CI_COMPARE=1` fails the job. No
   frozen envelope yet is `no-baseline` and exits 0.

4. The pull-request job is the Sonic stage `memory-shape` on
   `vars.LOADTEST_RUNNER`. It posts one comment marked
   `<!-- relate:memory-shape -->`. The nightly cadence runs the same
   stage, writes the summary, and does not comment. A local freeze is
   not the envelope. The first pull request reports `no-baseline` until
   the freeze runs on the x86_64 runner.

## Consequences

A graph that uses less memory fails until the envelope is re-frozen.
That is the point of keeping the bottom of the range. The old
worst-of-N file stays where `make baseline-loadtests` writes it.
