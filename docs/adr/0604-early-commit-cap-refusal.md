# ADR-0604: An over-cap transaction is refused at the statement that crosses the cap

Status: Proposed
Claim: aef7b02158add6c20a7db9f314690b6e288a68c3

## Context

ADR-0086 bounds one WAL frame's JSON payload at 64 MiB and refuses an
oversized commit before any frame byte is written. An explicit transaction
(ADR-0028) accumulates its whole write set and commits as one frame, so its
redo is bounded by that cap — and the refusal can only be raised where the
encoder runs, at COMMIT.

That leaves the natural bulk-load shape — one `BEGIN … COMMIT` around the
load — failing after every statement has run: the client streams the whole
transaction, then learns at COMMIT that it was too large for the log. The
work is lost, and on a large load the lost work is hours.

The transaction knows its size from its first statement: the redo log it
accumulates is exactly what the commit will encode.

## Decision

**A statement run inside an explicit transaction measures the
transaction's prospective commit record as it goes, and the statement that
takes it over the ADR-0086 cap is refused — with the same capacity refusal
(ADR-0494) the commit would have produced.** The transaction is failed
there, so the session rolls back rather than committing.

1. The measurement is the record's own figure — header slack, array
   framing, each op's encoded length — taken one op at a time as it is
   appended, so a transaction that stays under the cap pays one encode per
   op and never re-encodes what it already counted.
2. The refusal is `Error::Capacity` in `CapacityKind::WalPayload`, with
   `used` the counted payload at the crossing and `limit` the cap: the same
   variant, fields and rendering as the commit-time refusal, which stays as
   the backstop for any path reaching the encoder without the check.
3. The check runs at the statement boundary, so it reaches every surface
   through the one place statements run, and it is stated once rather than
   per surface.
4. Auto-commit statements are unaffected: they commit at their own
   statement boundary — a `CALL … IN TRANSACTIONS` batch drains as it goes
   (ADR-0210) — so their refusal already lands where the work is.

## Alternatives considered

- **Multi-frame commit records** — one logical commit spanning several
  frames with a commit-group marker, so a transaction is bounded by memory
  rather than by one frame. Removes the limit for bulk loads rather than
  moving the failure earlier. Rejected: it changes the frame format, replay
  and the read-side length check for a limit only a single-transaction bulk
  load meets, and that load has a batching remedy today.
- **Refuse at COMMIT only, documented** — no code change; the limit and the
  batching remedy are stated in the surfaces' docs. Rejected: it leaves the
  failure after all the work, which is the defect, not the message.
- **Refuse on an in-memory size proxy** — bound the accumulated redo by the
  scratch's resident byte count instead of the encoded figure. Rejected: the
  two differ by a wide, op-dependent factor, so the proxy would refuse
  transactions that commit and pass ones that do not.

## Consequences

- A bulk load in one explicit transaction fails at the statement that
  crosses the cap, with the size limit and the batching remedy in the
  error: the caller loses the statements since the last commit, not the
  whole load.
- The surfaces' explicit-transaction documentation states that the cap is
  the transaction's and where the refusal arrives.
- Every statement in an explicit transaction pays one encode of the ops it
  appended; measured against the statement's own work, that is below the
  run-to-run noise.
