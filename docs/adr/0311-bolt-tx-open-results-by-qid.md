---
type: adr
area: surface
claim: fc6485ad97289a279047a047afba6be56078c756
name: 0311-bolt-tx-open-results-by-qid
status: proposed
tags:
  - bolt
  - transactions
title: Bolt explicit transactions keep one open result per RUN, addressed by qid
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0309-bolt-summary-metadata-neo4j-4-4]]"
  - "[[adr/0314-bolt-protocol-breaches-close]]"
---

# ADR-0311: Bolt explicit transactions keep one open result per RUN, addressed by qid

## Context

[ADR-0110](./0110-bolt-connection-query-governance.md) gives each Bolt session one buffered result: "`DISCARD` or a new
`RUN` discards pending rows", and PULL ignores the `qid` field. Bolt 4.x
lets an explicit transaction hold several open results at once: each RUN
SUCCESS carries `qid`, and `PULL {n, qid}` / `DISCARD {n, qid}` address one
result (`qid` absent or `-1` means the most recent RUN).

The LABS-323 probes found the damage (F-23, F-24, D-41). With two RUNs in
one transaction before any PULL, cypherlite-bolt serves the second result
to `PULL {qid: 0}`, and neo4rs interleaving two streams with fetch size 1
gets stream two's rows on stream one. That is wrong data, not a missing
feature.

[ADR-0110](./0110-bolt-connection-query-governance.md) is still `Proposed`, but it is the implemented, cited contract,
so this ADR records the change and [ADR-0110](./0110-bolt-connection-query-governance.md) gains a Later references note.

## Decision

1. **Inside an explicit transaction every RUN opens its own result.** A
 later RUN never discards an earlier open one. This reverses [ADR-0110](./0110-bolt-connection-query-governance.md)'s
   "a new RUN discards pending rows" for explicit transactions.
2. **`qid`.** RUN SUCCESS carries `qid: Int`, 0 for the first RUN after
   BEGIN and one more for each RUN in that transaction. A failed RUN uses
   no qid.
3. **Addressing.** `PULL {n, qid}` and `DISCARD {n, qid}` address the
   result with that qid. `qid` absent or `-1` means the most recent RUN's
   result. Each result keeps its own cursor and its own ending SUCCESS
 ([ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md)). DISCARD honours `n` like PULL (`n = -1` discards the
   rest), so a DISCARD that leaves rows answers `{has_more: true}`.
4. **A qid that names no open result** while other results are open
   answers FAILURE `Neo.ClientError.Request.Invalid` with the message
   `no open result with qid <qid>`, and the session enters the failed
   state. A PULL or DISCARD with no open result at all is a protocol
 breach ([ADR-0314](./0314-bolt-protocol-breaches-close.md)).
5. **Lifetime.** A result closes when its ending SUCCESS or its FAILURE is
   sent. COMMIT, ROLLBACK and RESET drop every result still open. BEGIN
   starts with no open results and restarts qids at 0.
6. **Auto-commit is unchanged.** An auto-commit RUN SUCCESS carries no
   `qid`, and a new auto-commit RUN replaces the previous result.
7. **Memory cap.** The [ADR-0110](./0110-bolt-connection-query-governance.md) `--max-result-bytes` cap bounds the sum of
   the logical bytes of all open results on the connection. A RUN whose
 result would push the sum over the cap fails with the [ADR-0110](./0110-bolt-connection-query-governance.md) message.
   Engine change: `cypherlite_core::logical_bytes_within` returns the
   summed logical bytes when they stay within a budget, with the same early
   stop as `logical_bytes_exceed`, which now delegates to it.

## Alternatives considered

- **Keep one result and buffer the earlier one's rows client-side.** That
  is what drivers do for Bolt 3. Rejected: Bolt 4.4 drivers send `qid` and
  expect the server to honour it, and neo4rs does not buffer.
- **A per-result cap instead of a connection-wide sum.** Rejected: with
  many open results a per-result cap no longer bounds a connection's
  memory, which is the reason the cap exists.
- **Make an unknown qid a protocol breach that closes the connection.**
  Rejected: it is a recoverable request error (the transaction is still
  usable after RESET), and Neo4j answers it with a FAILURE.

## Consequences

- Drivers that interleave results in one transaction (neo4rs streams,
  the Python driver's multiple `tx.run` results) get each result's own
  rows.
- A session holds a list of open results instead of one. COMMIT,
  ROLLBACK and RESET clear it.
- [ADR-0110](./0110-bolt-connection-query-governance.md) gains a Later references note pointing here.
- Pinned by `tests/compat/scripts/protocol/tx-two-open-results.bolt` and
  the `protocol_tx_results_tests.rs` unit tests.
