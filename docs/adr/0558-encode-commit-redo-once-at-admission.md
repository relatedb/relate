# ADR-0558: Encode each commit's redo once, at admission

Status: Accepted
Claim: 7cd7773e69e2402637a056bd4284754328dc6b98

## Context

ADR-0094 bounds WAL acceptance with a count pass over the exact record
serialization followed by a capture pass, and a later amendment moved the
capture to the preflight proof. In the commit path that still serializes a
commit's redo ops three times: the admission reservation on the caller's
thread (a count at maximum-width LSN and timestamp), then a second count and
the capture, both inside the serialized group leader. The leader's passes
sit on the path every writer waits behind, and the second count repeats work
admission already did. ADR-0094 gates any change to preflight cost on the
ADR-0070 baseline environment.

## Decision

1. **The ops are serialized once, at admission.** The committing thread
   encodes the `ops` array of the record into one buffer, off the serial
   path and in parallel with other committers. The buffer begins with fixed
   slack for the record header sized for maximum-width LSN and timestamp,
   and ends with the record's closing brace.
2. **The leader only stamps the header.** When the group assigns LSNs and
   timestamps, the header is written into the slack in place and the frame is
   the bytes from the header to the end. The payload is byte-identical to the
   record the single-pass encoder produced; the record format, checksum, and
   frame layout are unchanged. No JSON is produced inside the leader.
3. **The cap and its refusal are unchanged.** The bound is checked while
   encoding, over the whole prospective record including the maximum-width
   header slack, so a record is refused at the same size with the same
   typed capacity refusal and the same figures as the admission count
   produced. The refusal now happens before the commit joins the queue;
   the leader can no longer meet an oversized record.
4. **Accounting keeps its shape, with one shift.** The workspace reservation
   is the buffer's size, equal to the previous admission figure, taken
   before the commit enqueues and held until its waiter completes. Because
   the size is only known after encoding, the reservation follows the
   encode rather than preceding it; the serializer's growth is bounded by
   the cap, and the buffer is trimmed to its length before it is charged.
5. **One canonical path.** Every writer reaches the WAL through the same
   encoder: group commit, the replicated apply path, direct append, and
   bulk batches. The direct append entry point encodes with the shared
   helper and then runs the same preflight; there is no second copy of the
   size policy. The count-only sink is removed.
6. **Typed ops remain.** Apply continues to consume the typed redo; this
   decision does not make apply read the encoded form.
7. **The encoding owns its ops' facts.** Encoding runs the redo ID and
   ontology-envelope validation and records whether an ontology op is
   present, so the preflight takes no typed ops and a body cannot be paired
   with ops it was not made from. Invalid redo is now refused at admission.
8. **Unchanged:** the ontology segment rotation rule, stale-proof checks,
   read-only and poisoned behavior, and the ADR-0094 acceptance and
   uncertain-outcome rules.

## Alternatives considered

- **Keep count-then-capture in the leader.** Rejected: it is the cost this
  decision removes, paid serially by every group.
- **Thread the admission count into the leader and capture there.**
  Rejected: it still encodes inside the serial section.
- **Capture into a capped sink in the leader with no count.** Rejected:
  still serial, and it gives up the committing threads' parallelism.
- **Concatenate header and body into a second buffer.** Rejected: an
  unreserved copy of up to the cap per record; the in-place header avoids it.
- **Count first at admission to reserve before allocating.** Rejected: that
  is the extra pass being removed.

## Consequences

- Encoding CPU moves from the leader to the committing threads; the serial
  section per record shrinks to validation, header stamp, and write.
- Peak memory per in-flight commit is unchanged in steady state (the
  retained buffer equals the prior reservation) but a transient up to the
  serializer's growth slack precedes the charge; trimming adds one copy of
  the buffer for large records.
- A public storage entry point that exposed the count reservation is
  replaced by the encoded-ops type and a preflight taking it.
- Whether apply could run from the encoded form, dropping the typed redo
  held in the commit slot, is left open for a later decision.
