# Integration-test binaries

Cargo builds every file directly under a crate's `tests/` as its own test
binary, and each one statically links the whole engine. In a local debug
build that is roughly 450 MB and several seconds of linking per file. So
`relate/relate` and `surfaces/relate-cli` keep their shareable test files
in one shared binary each: `tests/integration/`, built from
`tests/integration/main.rs`. Files that must stay separate (the rules
below) remain their own binaries directly under `tests/` — for `relate`
that is 46 of 195 files, for `relate-cli` 5 of 60.

## Running one file's tests

The file name is now a module path inside the `integration` binary:

```bash
cargo test -p relate --test integration clone::
```

```bash
cargo test -p relate-cli --test integration e2e_bundles::test_one_shot_query_runs_in_every_bundle -- --exact
```

## Adding a test file

Put it in `tests/integration/` and add a `mod <name>;` line to
`tests/integration/main.rs`. The shared helpers are reached as
`use crate::common;` (relate) or `use crate::support;` (relate-cli), not
re-declared with `mod`. The `every_file_is_declared` test — a call into
`relate_test_support::assert_every_file_is_declared`, the guard's one
home — fails if a file in the directory has no `mod` line, because an
undeclared file would otherwise never compile or run.

Put it directly in `tests/` instead, as a binary of its own, when sharing
a process with the other tests would change what it tests. Under plain
`cargo test` a binary's tests run as threads of one process:

- it reads or sets the process-wide preview opt-in: `RELATE_PREVIEW` is
  read once per `Database::open`, and `set_preview_once` (through
  `common::ontology::preview_ontology` or
  `common::show_variants::show_variants_connection`) sets it for the whole
  process — a permanent `Once`, never restored. So both sides of a gate
  must be separate binaries: a file that enables a feature breaks a
  sibling that pins it closed. This rule is the one that also reads
  comments, because a file declares the dependence in prose ("this file is
  its own test binary: `RELATE_PREVIEW` is process-global"); a CLI test
  that only passes the variable to a spawned child process is separate too,
  since the scan cannot tell the two spellings apart and over-keeping is
  the safe direction.
- it mutates the process environment: `set_var`, the pattern-engine switch
  (`open_with_engine`, `for_each_engine`), `EnvRestore`, or the bulk-batch
  window (`BatchEnv`). `Database::open` reads these variables, so a sibling
  test would observe the change — including one that opens a `Database`
  directly instead of through `common::env_seam`.
- it samples process-wide state, such as RSS.
- it re-executes its own test binary as a child.
- it uses the test crate root as its own (`crate::` paths, `#[path]`
  submodules), as `constraint_validation.rs` does.
- it must also compile on its own outside this tree. `golden_stores.rs`
  is copied into old release checkouts (`testdata/stores/README.md`).

`scripts/merge-test-binaries.py <crate-dir>` applies these rules with a
conservative text scan — comments are blanked first, so a rule named in
prose does not keep a file separate (except the preview rule above) — and
moves the files that pass. The files kept for a reason no scan can see are
recorded in the script's `KEEP_ALWAYS` table, so a bare re-run
(`scripts/merge-test-binaries.py relate/relate`) reproduces the committed
layout. The move is idempotent, so
re-running it after new files land directly in `tests/` folds in the
shareable ones.

The committed `main.rs` files are the generator's output, byte for byte —
generated, not editable: `scripts/tests/test_merge_test_binaries.py` (the
`test-merge-test-binaries` Makefile target, a `test-scripts` member) fails
if either drifts from what the script renders, and it also holds the layout
invariant (a bare re-run moves nothing; no merged file would be kept
separate). Change the generator, not `main.rs`.

Under nextest (one process per test — the CI bundle lanes), the merge
does not change how tests are isolated at runtime.
