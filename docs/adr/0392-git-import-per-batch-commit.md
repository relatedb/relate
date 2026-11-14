# ADR-0392: The git import commits per batch over a streamed plan

Status: Accepted
Claim: e65df3fdb2494c37856798c44e5915fde785ff0f
Later references: ADR-0394 adds the fresh-store `CREATE` edge statements on top of this ADR's batch design — a store with no nodes writes `CREATE` (the rows are distinct by construction), every populated store keeps this ADR's `MERGE` path and its re-run completion. ADR-0602 moves fs-to-graph onto this ADR's per-batch framing, superseding this ADR's consequence that kept fs-to-graph on whole-import and its rollback.

## Context

ADR-0376 pins `cypherlite lab git-to-graph`'s schema, path rule, flags,
index set and one explicit transaction around the whole import; ADR-0377
pins its resource caps (134,217,728 bytes of `git log` stdout, 500,000
distinct `CO_CHANGED` pairs, `--max-changeset` at most 1,000) and that
every refusal fires before the write; ADR-0380 §5 corrects the peak
arithmetic to ~2.7 GiB of materialised rows at those caps.

The feature's write-retention probe (feature `git-import-peak-cut`,
release-build, synthetic in-process histories driven through the same
write path field-for-field) measured two facts that make that design
unworkable at the scale of a real checkout's whole history:

1. **One explicit transaction cannot commit at that scale.** A
   transaction's whole redo serialises as a single WAL record, and the
   store caps a record's payload at 64 MiB
   (`cypherlite-storage/src/wal.rs:21`, ADR-0086). `commit()` fails from
   60,000 five-path commits (~460,000 relationships) in both the
   row-materialising and the row-streaming shape; 45,000 commits commit
   cleanly. The mechanism is not the row vectors: it is the transaction
   itself.
2. **Streaming rows alone does not restore the memory budget.** The
   transaction's `WriteScratch` (overlay + redo log, created at `begin()`
   and drained only at `commit()`) retains every write until commit:
   measured 1,181.8 bytes per relationship of overlay + redo retention,
   9,880.1 bytes per commit pre-commit, which extrapolates to ~2.8 GiB
   (2,978,004,992 B) at ADR-0377's 300,000-commit caps shape — against
   the ~0.7 GiB transient heap ADR-0377's Consequences recorded and
   ADR-0380 §5 corrected away. The row-materialising design's measured
   peak slopes extrapolate to ~4.1 GiB (4,369,694,720 B) at the same
   shape.

The target the feature measured is a real checkout's whole history under
the 128 MiB log cap: the neo4j shape, ~92,000 commits at ~12.8 paths per
commit, ~2.6M distinct `CO_CHANGED` pairs, ~3.9M relationships. That
shape is refused by the 500,000-pair cap after the whole log is read, and
would be uncommittable and unaffordable under the current write path even
without the cap: its single WAL record is ~8x the record that already
fails, and its materialised rows alone would be ~3.4 GiB at ADR-0380 §5's
911 bytes per row.

The constraints are unchanged: ADR-0376's schema, path rule and
`MERGE`/`SET` semantics; ADR-0377's refusal texts and their
constant-derived numbers; no new flag; no new dependency; every refusal
before the store is first written; and fs-to-graph's behaviour unchanged
(ADR-0157).

## Decision

### 1. `git-to-graph` commits each 1,000-row batch as its own explicit transaction

The shared phase runner (`lab/write.rs::run_phases`) takes a
`CommitPolicy`: `WholeImport` (ADR-0157's one transaction around every
phase — what fs-to-graph keeps, its work bounded by the walked tree) or
`PerBatch` (begin, write, commit per [`BATCH`]-row batch — what
git-to-graph now uses). The runner stays one canonical helper for both
importers; the policy is their one difference.

A 1,000-row batch's WAL record is tens of KB — the measured run's WAL
averages well under 100 KB per batch record, three orders of magnitude
below the 64 MiB payload cap — and at the neo4j shape the import's ~4,000
batch transactions all commit cleanly, where the one-transaction design's
single record already fails at a quarter of the shape (the probe's
60,000-commit evidence). `PerBatch` is what admits the shape at all.

### 2. Index order, re-anchored to the batch order

The three range indexes are created before the first batch and the
`commit_subject` full-text index after the last batch commits — ADR-0376's
order, unchanged, with "the write transaction" now meaning the batch
sequence. `IF NOT EXISTS` keeps a re-import and a re-run safe.

### 3. Failure semantics: committed batches stay, a re-run completes

A mid-import failure under `PerBatch` leaves every batch before it
committed and rolls back only the failing batch. Because every write is
`MERGE`-keyed and `SET`-valued, and `CO_CHANGED` counts are set, never
incremented (ADR-0376), re-running the same command over the partially
imported store completes the history to exactly the node, edge and
`CO_CHANGED` counts of a clean import — pinned by test. This supersedes
ADR-0376's whole-import rollback for git-to-graph only, and ADR-0377 §2's
"no partial import to resume" statement.

### 4. Rows are generated lazily from a bounded plan

`build_rows` (six whole-phase row vectors) is replaced by `build_plan`:
the bounded pre-write aggregates ADR-0377 already allowed — the people
map, the distinct interned paths and the interner, the pair counts — plus
one compact per-commit plan (interned handles, counts and one
path-handle vector per commit; no parsed git record, no row). The parsed
history is dropped when the plan is built. Each phase's rows are
generated lazily from the plan (`write::RowSource`), so only the batch in
flight is ever materialised — pinned by a test whose row source panics if
the runner holds more than one batch. The import-wide memory above the
graph is the plan plus one batch; a phase's source is re-derivable, which
is what makes the re-run of §3 work.

### 5. `MAX_CO_CHANGED_PAIRS` rises to 5,000,000

The pair cap rises from 500,000 to 5,000,000 so the measured neo4j shape
imports whole. It stays one constant; the error text, `--help`, the spec
and every test read it from there, and the message wording is otherwise
unchanged (ADR-0377 §4's contract). The raise is admissible only because
of §1 and §4: under the old write path 5,000,000 pairs are ~4.2 GiB of
materialised rows and an uncommittable WAL record.

### 6. Every refusal still fires before the store is first written

The ADR-0377 refusals — `LogTooLarge`, `TooManyPairs`, `UnsafePath`, and
the `--max-changeset` parse error — all fire before the range-index DDL
and the first batch transaction: the first three inside the plan build,
the parse error before the command runs at all. A refused import creates
no index, no node, no edge — pinned by the refusal suite's no-index
assertions.

### 7. The transient-overhead budget, measured

At the neo4j shape the import's transient overhead — its peak RSS above
the resident graph it leaves behind (the store open on the imported
history, the plan dropped) — stays below 734,003,200 bytes (700 MiB),
ADR-0377's Consequences figure, restored. The pin is an ignored
release-build measurement test
(`lab/git_to_graph/peak_run.rs`, `cargo test --release -p cypherlite-cli
--bin cypherlite -- --ignored --nocapture neo4j_scale`) that generates
the shape in-process, drives the real import path, and samples RSS the
way the feature's probe did (`cypherlite-test-support`'s
`process_rss_bytes` checkpoints and `process_peak_rss_bytes`
high-water). Its recorded breakdown at the measured shape (92,000
commits, 2,400 files, 1,177,600 TOUCHED rows, 2,621,968 distinct pairs,
~3.9M relationships, a ~27-minute release run):
`{"transient_bytes": 191283200, "graph_bytes": 2197733376,
"peak_bytes": 2389016576}` — 182 MiB of transient overhead above a
2.05 GiB resident graph, 3.8x under the budget, with the parsed history
peaking at ~112 MB and the plan at ~349 MB over the ~9 MB process
baseline.

## Alternatives considered

- **Chunk the WAL record instead of committing per batch.** Keep one
  transaction, teach the WAL to split a redo across records. Rejected: it
  changes the storage layer's record-per-commit contract (ADR-0014,
  ADR-0086) for one importer's benefit; the redo is still retained until
  commit, so the measured 1,181.8 bytes-per-relationship overlay
  retention (~2.7 GiB at the caps shape) stands; and it puts an engine
  change in service of a surface the layering rules keep apart.
- **Stream the rows but keep the one transaction.** Rejected on the
  probe's own numbers: pre-commit retention stays 9,880.1 bytes per
  commit (~2.8 GiB at the caps shape) because the `WriteScratch` overlay
  and redo live until `commit()`, and the single WAL record still cannot
  commit from ~50,000 commits.
- **Keep the 500,000-pair cap.** Rejected: the cap exists to bound a
  hostile input, not to reject a real checkout's whole history — the
  measured neo4j shape is 2.6M distinct pairs under the 128 MiB log cap,
  and the import would read the entire log and then refuse.
- **Raise the cap under the current write path.** Rejected twice over:
  5,000,000 pairs are ~4.2 GiB of materialised rows at ADR-0380 §5's 911
  bytes per row, and the one-transaction WAL record rejects the shape
  outright — the raise and the write-path restructure land together or
  not at all.
- **One transaction per phase instead of per batch.** Rejected: the
  touched phase alone is ~1.18M rows at the neo4j shape, one record over
  the cap again at modestly larger shapes and a phase-long overlay
  retention; the 1,000-row batch is the unit the write path already
  bounds (`BATCH`, ADR-0376), so the batch is the commit unit.
- **A progress sidecar that auto-resumes from the last committed
  batch.** Rejected: `MERGE`/`SET` idempotence already makes a plain
  re-run correct and complete; a sidecar adds durable state to keep
  honest, a new drift failure mode, and no user-visible gain over
  "re-run the same command".

## Consequences

- fs-to-graph's behaviour is unchanged — same phases, same whole-import
  rollback (ADR-0157), pinned by its existing mid-import rollback test —
  and `run_phases` remains the one phase runner both importers share.
- git-to-graph's failure contract weakens to per-batch: a killed import
  leaves a partial graph that the same command completes on re-run. This
  is documented in `--help`, `docs/api/cli/spec.md`,
  `docs/reference/cli-flags.md` and the bundled skill reference, and
  pinned by the re-run test.
- The store's 64 MiB WAL payload cap is no longer reachable by this
  importer: a batch's record is tens of KB, and at the neo4j shape
  every one of the ~4,000 batch commits succeeds. The cost is ~4,000
  small WAL records and their fsyncs where the old design wrote one —
  absorbed by the segmented WAL and its checkpoint thresholds.
- ADR-0376, ADR-0377 and ADR-0380 gain `Later references` notes naming
  this ADR; their decision texts are untouched. ADR-0377's error texts
  are unchanged except the pair number, which every surface reads from
  the one `MAX_CO_CHANGED_PAIRS` constant; ADR-0380 §5's itemised
  subtraction is superseded by §7's direct measurement.
- The lazy-row and at-most-one-batch guarantees are pinned by
  `phases_materialise_at_most_one_batch_at_a_time`; the refusal order by
  the refusal suite's no-index assertions; the re-run completion by
  `a_re_run_completes_a_partially_imported_history_to_clean_counts`; the
  budget by the ignored measurement test of §7.
- The ignored measurement test is deliberately outside CI's default
  lanes: it is a minutes-long, multi-gigabyte release-build run, like the
  full-scale dataset tests. Its recorded numbers live in the feature's
  record; re-running it re-measures them.
- No new flag, no new dependency, no engine change: AGENTS.md R6 and the
  R1–R5 boundaries are untouched, and the import surface (flags,
  defaults, exits) is exactly ADR-0376/0377/0383's plus the cap number.
