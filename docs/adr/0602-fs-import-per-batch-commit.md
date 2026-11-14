# ADR-0602: The fs import commits per batch

Status: Proposed
Claim: 7e2315e7891836310dda53f78ffe37a0dca1452b

## Context

ADR-0157 frames fs-to-graph's whole import in one explicit transaction
(the manifest's `whole-import` policy). One transaction is one WAL
record, and the store caps a record's payload at 64 MiB (ADR-0086) — so
the cap is a ceiling on a run's *total* fs payload. `--max-content-bytes`
and `--ignore-patterns` shrink the payload, but a text-heavy tree (a
public checkout measured at ~98 MB of capped content over ~13.6k files)
crosses the ceiling at the default flags: the walk completes, the commit
refuses, nothing lands.

ADR-0392 moved git-to-graph to per-batch commits for this same failure,
measured: a transaction's whole redo is one WAL record, and the record
already fails at a quarter of a real checkout's history. Its
Consequences deliberately left fs-to-graph on whole-import, its work
"bounded by the walked tree" — the bound the measured checkout now
crosses.

The mount already bounds every `write_cypher` call: the wire kit's
chunker ships at most 1,000 rows or 8 MiB of serialized rows per call
(ADR-0430 §4), and the fs child drives each phase's rows through it.
The framing, not the call shape, is what glues those bounded calls into
one uncommittable record.

## Decision

fs-to-graph's manifest declares `per-batch` (ADR-0392's policy, the host
write layer's existing framing): every writing call — each
already-bounded chunk of a phase — commits as its own transaction. No
flag, tool, cap or host change rides along; the package's version rises
with the change.

Failure semantics follow ADR-0392 §3: a mid-import failure leaves every
committed batch in the store, and re-running the import completes the
graph — every write is MERGE-keyed by path and SET-valued (ADR-0157),
which is what makes the re-run correct. The fulltext index keeps its
point after the last write phase, `IF NOT EXISTS` unchanged.

## Alternatives considered

- **Keep `whole-import`; teach the WAL to split one redo across
  records.** Rejected in ADR-0392 and unchanged here: it bends the
  storage layer's record-per-commit contract (ADR-0014) to one importer,
  and the redo is still retained until commit.
- **One transaction per phase.** Rejected: the files phase alone carries
  a tree's whole capped content — past the record cap at exactly the
  shapes this change exists to admit. The call is the unit the wire
  already bounds.
- **Raise the record cap.** Rejected: the cap is the store's
  memory-exhaustion guard (ADR-0086); one importer's total payload is
  not a number a store should bless.

## Consequences

- fs-to-graph's failure contract weakens to per-batch: a failed run
  keeps the batches that committed (an earlier phase's graph), and a
  re-run completes it. Pinned by e2e through the real mount; the run
  flag tables document the re-run contract.
- A call's WAL record is the 8 MiB statement cap plus its statement —
  far under the record cap, which this importer can no longer reach.
  ADR-0598 scopes the whole-run stream cap to whole-import framing, so
  a per-batch session's remaining bounds are the per-call caps and the
  host store's live-graph limit — the import's other bounds, the ones
  the user steers.
- Per-call commits bound the retained redo: each call's write overlay
  drains at its own commit instead of accumulating the whole import's
  redo until one commit — the retention ADR-0392 measured at ~1.2 KB
  per relationship.
- `whole-import` remains a supported policy for an extension whose
  import is small or whose all-or-nothing atomicity matters more than
  scale. ADR-0157 and ADR-0392 gain Later references notes; their
  decision texts stand.
