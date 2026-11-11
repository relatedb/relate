# 0268. proptest dev-dependencies for property testing

- **Status**: Proposed
- **Renumbered**: from 0136 (shared-number cleanup)
- **Date**: 2026-09-28
- **Deciders**: CypherLite maintainers

## Context

CypherLite's durability layer (WAL, snapshots, checkpoint/reload) and its
value model have invariants that example-based tests sample only thinly:
replay determinism, checkpoint-reload equivalence, idempotent `MERGE`,
round-trips of every `CypherValue` variant. These are properties over large
input spaces, and the existing tests cannot enumerate the boundary cases.

A property-testing harness gives generated inputs, integrated shrinking, and
persisted regression seeds. Peer embedded/storage systems use one:
AletheiaDB asserts four post-recovery invariants with `proptest`; sled runs
quickcheck- and proptest-based model tests plus failpoint/crash recovery;
Polars added `proptest` round-trip tests for its Parquet decode kernels and
found real codec bugs; arrow-rs tracks the same per-`DataType` round-trip.

Two candidate harnesses were evaluated against MSRV Rust 1.88 and the
workspace's dependency policy:

- **`proptest` 1.11.0** (2026-03-24) — MSRV 1.84 (the unreleased branch moves
  to 1.88). Normal deps are `bitflags ^2.10`, `num-traits ^0.2.15`,
  `rand ^0.9`, `rand_chacha ^0.9`, `rand_xorshift ^0.4`, `unarray ^0.1.4`;
  optional `regex-syntax`, `rusty-fork`, `tempfile`, `bit-set`/`bit-vec`,
  `proptest-macro` (the derive), `x86`. No `syn`/`quote` unless the derive is
  on, no async runtime, no native/C++ build. Tests are ordinary `#[test]`s
  run by `cargo test`; failing seeds persist to `.proptest-regressions` and
  are replayed first on later runs.
- **`proptest-state-machine` 0.8.0** — depends only on `proptest ^1.10`. Runs
  a system under test against an abstract `ReferenceStateMachine` and shrinks
  a failing run to a minimal transition sequence. Sequential only.

The one friction point is `rand`. The workspace pins **`rand = "0.8"`**
(root `Cargo.toml`) and uses it in `cypherlite-executor`,
`cypherlite-datasets`, and optionally `cypherlite-cli`. proptest 1.7.0 moved
to `rand 0.9` and the unreleased branch moves to `rand 0.10`. Cargo permits
multiple semver-incompatible majors, so proptest links a second rand major
alongside the workspace's 0.8 — it compiles, but the two rand families cannot
share an RNG path.

ADR-0103's R6 rule says adding a new **heavy** external dependency (large
transitive tree, native/C++ build, networking or async runtime) to any
`cypherlite/*` crate needs its own ADR. R6 does not itself distinguish
`[dependencies]` from `[dev-dependencies]`.

Numbering note: this decision was first written as `0125`, chosen from the
then-current tree. Main merged its own `docs/adr/0125-*` ADRs during the rebase,
so `0125` collided and main's numbering runs to `0135`. This ADR was renumbered
to `0136` in the stellar-review fix pass. Numbers extend beyond the merged tree
because open PRs reserve numbers ahead of merge.

## Decision

1. **Harness.** Adopt `proptest = "1.11"` and
   `proptest-state-machine = "0.8"` as **dev-dependencies of
   `cypherlite/cypherlite-storage` only**. They are not added to any crate's
   normal dependency list and not to any other engine crate. A later need in
   a different crate gets its own ADR.

2. **rand dual-major, test build only.** Accept the second rand major. The
   workspace stays on `rand 0.8`; only the `cypherlite-storage` test build
   links `rand 0.9` through proptest. No test may pass a `rand 0.8` `Rng`
   into a proptest strategy or vice versa; a test that seeds both an engine
   RNG and a proptest generator keeps the two streams separate. Bumping the
   workspace to `rand 0.9` is a separate change and not implied by this one.

3. **Regression files committed.** `.proptest-regressions` files (one per
   test source, proptest's `SourceParallel` persistence) are committed to the
   repository. A counterexample is a permanent regression test. No
   `.proptest-regressions` file is deleted, or its entries removed, without a
   tracked reason (an issue or a later ADR); `PROPTEST_DISABLE_FAILURE_PERSISTENCE`
   must not be set in CI.

4. **R6 does not apply.** proptest is a *dev*-dependency, and its tree is
   light: no native/C++ build, no async runtime, no networking, and a handful
   of small pure-Rust crates (the six listed above, plus optional
   `regex-syntax`/`rusty-fork`/`tempfile` only if those features are enabled).
   That is below R6's heavy-dependency trigger. This ADR is still recorded
   because the rand-major split and the committed-regression-file policy are
   decisions worth having in the log, not because R6 compels it.

## Alternatives considered

- **`quickcheck` 1.1.0 (2026-02-10) as the primary harness.** One
  generator/shrinker per type via `Arbitrary`, custom generation needs a
  newtype, a single global size setting, and **no persistence of failing
  seeds** — a failure found on a random run is lost. Its shrinking is
  structurally weaker than proptest's integrated shrinking, and its own
  README points users to proptest for that reason. Rejected: the missing
  regression persistence defeats the purpose of running the tests in CI, and
  weaker shrinking leaves counterexamples hard to read.
- **`hegeltest` 0.47 (2026-09-25, Hypothesis lineage) as the primary
  harness.** Actively developed and language-agnostic, but it is in **beta**
  with breaking changes possible, and shrinking is delegated to the
  Hypothesis engine over the out-of-process Hegel protocol — an external
  process in the test build, where proptest is a pure library. Rejected:
  beta status and an external protocol runtime are not a good fit for a
  merge gate, and proptest already provides the Hypothesis ideas
  (integrated shrinking, per-type strategies) in-process.
- **`proptest-stateful` (ReadySet) 0.1.3 for the stateful tests.** Its
  `rand ^0.8.5` matches the workspace pin, and it has a strong track record
  (real ReadySet cache bugs). But it is async-only (`tokio ^1.28`) and stale
  (2023-07). Rejected: `cypherlite-storage` is synchronous, and adding
  tokio to the test build is a heavier, less maintained path than
  `proptest-state-machine` 0.8.
- **`cargo-fuzz` 0.13.2 + `arbitrary` 1.4.2 instead of proptest.** Fuzzing
  is coverage-guided and finds deep no-panic bugs, but it needs a nightly
  compiler, LLVM sanitizers, and a C++11 compiler, and is Unix-only — it
  cannot be part of the stable-toolchain merge gate. Rejected as the harness;
  kept as a **separate, out-of-band fuzzing layer** for the lexer/parser and
  the WAL/Parquet decoders, tracked by its own change. Out of scope here.
- **Add proptest workspace-wide in one change** (dev-deps on every engine
  crate). The rand-major split and the regression-file policy are decided
  once, but each crate adopting proptest should justify its own properties
  and accept its own test-build cost. Rejected as the starting point:
  `cypherlite-storage` has the highest-value properties (WAL replay,
  checkpoint/reload) and proves the setup at low blast radius.
- **Bump the workspace to `rand 0.9` first, then add proptest with a single
  rand major.** Cleaner long-term, but it touches `cypherlite-executor`,
  `cypherlite-datasets` and `cypherlite-cli` production code and their
  callers for a test-only benefit, and couples an unrelated migration to this
  decision. Rejected: keep the migration separate; accept the dual major in
  the test build only.

## Consequences

- `cypherlite/cypherlite-storage/Cargo.toml` gains a `[dev-dependencies]`
  entry for `proptest = "1.11"` and `proptest-state-machine = "0.8"`. No
  production build, published crate, or release artifact changes: dev-deps
  are not compiled into the library.
- `Cargo.lock` gains proptest and its tree, including a second `rand` major.
  `cargo test` for `cypherlite-storage` builds the extra tree; other crates
  are unaffected.
- Property tests are ordinary `#[test]`s and slot into the existing
  `cargo test -p cypherlite-storage` gate with no new runner. Case counts
  scale through `PROPTEST_CASES` for an optional heavier scheduled job.
- `.proptest-regressions` files become part of the tree; reviewers treat
  their deletion as a regression unless a tracked reason is cited.
- The rand-family separation is a standing rule for tests in this crate:
  engine RNG seeds and proptest seeds are independent and must not be mixed
  in one code path.
- Implementing the actual properties (WAL replay determinism/idempotency,
  checkpoint-reload equivalence, value round-trips) is follow-up work under
  this feature, not part of this decision.
- **Real bug found by the suites (LABS-134).** The properties landed under this
  feature caught a codec bug in the product today: `serde_json`, compiled
  without its `float_roundtrip` feature, is not correctly rounded, so
  re-decoding a serialized `f64` can return a value one ULP away from the one
  written. Through both the WAL replay path and the checkpoint-reload path, and
  on both VFS backends (`MemoryVfs` and `NativeVfs`), a float property can
  silently shift by one ULP — verified as pure ULP drift across 70 of 70
  compared value pairs. Tracked as **LABS-134**. The fix (add `float_roundtrip`
  to the workspace `serde_json` dependency) will ship as a stacked follow-up PR
  (`feat-proptest-float-roundtrip`), which will also un-`#[ignore]` the four
  blocked properties (1 in `wal_proptest.rs`, 3 in `recovery_proptest.rs`);
  until then their `#[ignore]` reasons all cite `LABS-134`.
- **Recovery-harness defect found by re-audit (node 14).** Independent of
  LABS-134, the joint-replay property's `cut == 0` branch checkpointed the
  fully-applied graph while recording `checkpoint_lsn = 0`; reopening then
  replayed records `1..=n` on top of a snapshot already at `commit_ts = n`, and
  the redo preflight rejected record 1. Fixed in the harness (that path
  checkpoints `Graph::default()` at LSN 0) on `feat-proptest`. With this fix the
  three recovery properties pass under `float_roundtrip`, so the follow-up PR's
  un-`#[ignore]` claim holds for all four.
- **Audit-fix pass (nodes 10–12).** A follow-up pass over the suites
  consolidated the shared harness into one applicability filter
  (`proptest_support::ModelGraph`) and one canonical state fingerprint
  (`state_json`), dropped dead model state, and added the tracker ids, this
  note, and the document move below.
- **Supporting documents.** The feature's research and rollout notes live at
  [`../testing/proptest-research-findings.md`](../testing/proptest-research-findings.md)
  and [`../testing/proptest-roadmap.md`](../testing/proptest-roadmap.md)
  (moved out of the repository root so the tree is not cluttered by feature
  notes).

## Later references

- **LABS-134 fix shipped (`oskar/float-roundtrip`).** The codec bug the
  suites found (the *Real bug found by the suites (LABS-134)* consequence
  above) is fixed: the workspace `serde_json` dependency now enables
  `float_roundtrip`, so f64 encode/decode is correctly rounded, and the
  four blocked properties (1 in `wal_proptest.rs`, 3 in
  `recovery_proptest.rs`) are un-`#[ignore]`d. The 1-ULP f64 drift through
  WAL replay and checkpoint reload on both VFS backends (`MemoryVfs` and
  `NativeVfs`) is eliminated.
- **Workspace rand pin superseded by ADR-0360.** ADR-0360 migrated the
  workspace to `rand 0.9`; it supersedes this ADR's §2 workspace pin (the
  workspace no longer stays on `rand 0.8`, and the storage test build unifies
  with the workspace's rand 0.9 instead of linking a second major). The
  proptest adoption and the regression-file policy stand; this ADR's original
  Decision text is unchanged.
- **Adoption extended to `cypherlite-executor` (ADR-0439).** The pin this ADR
  adopted is extended to a second engine crate: `cypherlite-executor` takes
  `proptest = "1.11"` as a dev-dependency for the commit-uniqueness admission
  property (`src/graph/constraint_keys/property_tests/`, a directory module —
  the regression file lands at
  `proptest-regressions/graph/constraint_keys/property_tests/mod.txt`,
  derived from `file!()`, ADR-0439). The
  suite generates multi-slot flush groups and drives them through the real
  `ConstraintAdmission`, comparing every slot decision and the final committed
  map against an independent reference model. Same version pin, same
  dev-only/test-build-only scope, same committed-regression-file policy; the
  Decision's "storage only" scope sentence is extended by this note rather
  than replaced as a policy.
- **Adoption extended to the executor's adjacency-model suite (ADR-0552).**
  The same pin backs the sealed+delta store's red-line guard: a generated
  create / ts-update / delete / re-add sequence compared against a plain
  `BTreeMap` reference model after every op
  (`src/graph/tests/adjacency_model_proptest.rs`; the regression file lands
  at `proptest-regressions/graph/tests/adjacency_model_proptest.txt`). Same
  dev-only/test-build-only scope, same committed-regression-file policy; no
  second proptest version enters the lockfile.
