# 0094. Preflight deterministic WAL acceptance before append

- **Status**: Proposed
- **Date**: 2026-09-24
- **Deciders**: CypherLite maintainers

## Context

ADR-0019 batches independent WAL records under one durability barrier, then
applies their redo in order. ADR-0086 limits each encoded WAL JSON payload to
64 MiB and refuses an oversized *individual* record before writing it.
`flush_group` currently appends each eligible slot as it walks the batch; an
oversized second slot can make the leader return an error to every waiter even
though the first slot's bytes were appended (and may survive reopen). Similarly,
`Database::apply_replicated` checks redo IDs, then appends and syncs before
`Graph::apply_redo`; a `CreateRel` with valid numeric IDs but a missing endpoint
can fail only after the record is durable. An endpoint deleted earlier in the
same redo has the same problem. ID validation alone cannot establish that redo
is applicable. A post-sync apply failure may also follow malformed index detail
options or invalid catalog operations. ADR-0086 and the ID guard do not make
the whole group or replicated record deterministically safe to accept.

The group contains independent *records*, not an atomic WAL transaction.
Partial writes, failed barriers, and failures after a barrier have different
outcomes from a known invalid record: an I/O error does not prove that the
record is absent or that its bytes are not durable. ADR-0020 permits snapshot
dumping while writers proceed, so fencing only future checkpoint starts does
not stop a previously pinned worker from publishing. The storage-hardening
review also found a separate destructive-reset alias issue; the reset policy
is amended in Proposed ADR-0090, not decided by this WAL ADR.

## Decision

1. **One pre-append acceptance boundary.** While holding the serialization
   boundary covering WAL order and the graph state being checked, select the
   group's candidate slots in queue order with ADR-0019's existing conflict
   checks against the graph and earlier eligible slots and the existing
   per-slot memory-capacity reservation policy. Conflict and capacity errors
   stay per waiter; those slots do not enter the WAL preflight. Assign the
   would-be commit timestamps and LSNs in that same order, rejecting invalid
   timestamps, LSN overflow, unsupported IDs, serialization errors and an
   encoded JSON payload outside ADR-0086's 1..=64 MiB range **for every
   candidate before the first append**. Include the exact prospective LSN,
   timestamp and ops in each size calculation. Preflight ordered redo
   applicability against the stable graph plus earlier candidate operations
    with **one pure, ordered `Graph::validate_redo_applicability`**. It must
    examine ops at their actual execution point, not merely find an endpoint
    anywhere in the batch. Use a scratch view of touched live nodes and
    relationships (including detach-delete cascades), and touched catalog
    names/schemas over the existing graph/catalog. `CreateRel` requires both
    endpoints live at that point, whether created in an earlier op or deleted
    earlier in the record/group. A record's `commit_ts` must be strictly
    greater than the graph's current commit timestamp and every preceding
    candidate's timestamp, nonzero and less than `u64::MAX`. The graph
    baseline for untouched node/relationship visibility is evaluated at that
    *proposed* timestamp, exactly as installation does; the ordered scratch
    view still takes precedence for earlier ops. Reject zero, repeated, or
    backward timestamps before changing graph write state or installing any
    prefix. WAL append uses the same increasing, nonzero timestamp contract,
    and startup/replay checks the persisted sequence across segments before
    handing records to graph apply. This preserves the existing replica
    contract of retaining a source timestamp when it advances the destination
    clock (including gaps); a source timestamp at or behind the destination is
    rejected rather than rewritten or replayed out of order. `DeleteNode`
    currently applies with
   `detach = true`; model that cascade, rather than incorrectly imposing the
   non-detach deletion restriction. Apply the same ordered idempotency and
   duplicate-name/equivalent-schema rules as catalog index and constraint
   insert/drop, including duplicates introduced by earlier ops. Decode each
   `CatalogIndexDetail.options` using the same strict tagged-JSON decoder as
   apply. Cover every deterministic error-producing apply path, including
   timestamp and all persisted IDs (not only CreateNode/CreateRel); future
   fallible redo variants must extend this validator before they can be
   persisted. Exhaustive `RedoOp` ID preflight remains a single RedoOp-owned
   rule shared by WAL and graph (the follow-up to ADR-0086); the Graph
   validator calls it, rather than reimplementing it. Keep validation free of
   graph mutations and WAL writes. Direct `Graph::apply_redo` calls the same
   validator **before** setting write state or installing any prefix; group
   and replication call it on the stable graph with an ordered scratch view
   before append. Validation is not a trial `apply_redo` followed by rollback
    or a separate, approximate validation implementation. At each record
    boundary, a newly created named node/relationship label-existence
    constraint must have matching nonempty derived enforcement metadata from
    that record or already known theater state. A missing or mismatched
    companion is a typed deterministic rejection **before append**; group and
    replicated writes follow the same rule. Preserve the ability to replay
    historical WAL records created before this admission rule: recovery tracks
    their exact unenforced legacy definitions for ADR-0095 checkpoint omission,
    without granting new direct or replicated writes that exception.

   If any candidate fails deterministic preflight, append **none** of the
   candidate records. Complete every candidate waiter with the preflight
   failure (including earlier candidates), and complete already excluded
   conflict/capacity waiters with their own errors; no waiter hangs. Queued
   waiters not yet drained follow the existing leader failure handling. This
   changes only deterministic *batch* failure, not first-committer-wins,
   capacity admission, or the per-record success/one-fsync contract of
   ADR-0019. A passing batch appends in the planned order, performs its one
   durability barrier, applies in order and reports success only after that
   barrier and application. Hold the WAL/write serialization boundary across
   preflight, append, sync and graph application for both group and replica;
   no concurrent writer or replica may invalidate the checked state. Applying
   a previously checked record may defensively re-enter this *same* Graph
   validator against the now-applied prefix, but must not introduce another
   validation rule or rely on mutating the graph to test applicability.

2. **Bound the preflight rather than buffering a group.** Use the exact WAL
   serde representation twice: first serialize each candidate to a
   byte-counting, limit-enforcing sink that stops at 64 MiB + 1 without
   retaining JSON, then serialize that same immutable candidate at append
   through the same encoder with a 64 MiB bound. Distinguish a size-limit
   error from a serde error. The append still needs only one bounded payload
   at a time to compute its CRC and frame it; never retain a `Vec` of encoded
   payloads for the whole group. Track only the touched IDs/catalog state
   needed for ordered applicability, proportional to the current group's
   input rather than cloning whole graph/property maps. Keep the size guard in
    the low-level WAL append for direct callers. Preflight returns an opaque
    proof borrowing the immutable ops and carrying the assigned LSN, timestamp
    and exact counted size. A preflighted append checks the proof against the
    current next LSN/timestamp, encodes once into its bounded payload, and
    compares the actual size to the proof before writing a frame. Direct append
    still counts then encodes. Preflight and append share the encoding definition;
    no caller can mutate ops through the proof between passes.

3. **Replica before-sync applicability.** At `Database::apply_replicated`,
   under the same stable serialization boundary, preflight the exact record's
   timestamp, ID range, LSN, encoded size/serde result and full ordered redo
   applicability through that same Graph validator before WAL append or sync.
   `CreateRel` must see both live endpoints after all preceding operations in
   its record; a missing endpoint or one deleted earlier in the redo is a
   typed pre-append rejection, with unchanged LSN/WAL bytes and prior data
   reopenable. A valid mixed record, including pull-identity updates, keeps
   its one append/sync/apply sequence. Direct `Graph::apply_redo` and replay
   reject an invalid record without installing even a valid prefix. This is
   a record-level promise, not rollback of earlier, independently committed
   WAL records.

4. **Uncertain physical publication and a shared fence.** The all-or-nothing
   rejection promise applies to *deterministic preflight errors before any
   append*. Remember the **last safe commit horizon**: the LSN and timestamp
   of the last *contiguous* WAL prefix whose barrier was confirmed and whose
   redo was fully applied, before this attempted group/replica publication
   (not the numerically highest timestamp of any later record). Once the
   first frame write is attempted, a write or sync failure (including an error
   reported after a barrier), an unexpected apply failure, or a failed
   post-append WAL rotation is **outcome uncertain**, not a declaration that
   every record was rejected. Bytes from a complete prefix may be recoverable
   even after a failed sync; an apply failure may leave part of the current
   record in memory. Advance the safe horizon only after the entire planned
   group (or replica record) is durably synced and fully applied; freeze the
   previous horizon on any uncertainty, even when a prefix has a confirmed
   barrier and full application. Do not infer that later uncertain records
   were rejected.
    A direct WAL handle also becomes sticky-poisoned on any physical append,
    sync or rotation failure, including a short write or a barrier error after
    publication. Reject subsequent append, sync, rotate and recycle on that
    handle until reopen; deterministic preflight rejection leaves it usable.
    Rotation commits its in-memory segment transition only after the new header
    is written and the append handle opens. A failed new-header write may leave
    a short highest-segment magic: ADR-0086 permits the tightly scoped recovery
    after validating the predecessor, but never an in-process retry.
    Set a sticky poisoned state on `Shared` before releasing the serialization
   boundary; return/report a distinguishable uncertain-outcome storage error
   to every affected waiter, including queued writers. No success response
   precedes a confirmed barrier and full application.

   While poisoned, refuse new query admission (reads as well as writes),
   replication, explicit checkpoint, timed/scheduled checkpoint,
   WAL rotation/recycling and persist-on-drop. Admit read-only queries under a
   short shared publication guard: check health and pin the graph's commit
    timestamp **and snapshot the named indexes and constraints and all
    graph-backed SHOW theater state (index details/options, derived constraints,
    graph types, current graph type and databases) together under that guard**,
    then release the guard before executing on that pinned view. SHOW INDEXES,
    SHOW CONSTRAINTS, SHOW GRAPH TYPE, SHOW CURRENT GRAPH TYPE and SHOW DATABASES
    consume this admitted view rather than consulting live catalog/theater
    during execution; a commit after admission cannot mix new schema rows
    with old entity rows. Capture catalog and theater in separate, non-overlapping
    critical sections under the publication guard: no catalog lock is held
    while taking theater or vice versa. An explicitly opened transaction already holds its
    entity pin from `begin`; each statement checks health and snapshots the
    schema and theater at **statement admission**, incorporating its own write
    overlay during execution. Schema is therefore statement-consistent rather than
    fixed at transaction begin, while entity visibility remains pinned at
    begin. A read admitted before the poison may finish on its earlier safe
    snapshot (even if it delivers the result afterward); it cannot see a
    partially applied redo. Keep the guard only through admission, never
    through SHOW row generation or other query execution. Direct executor
    callers supplying an already pinned timestamp must also supply the schema
    captured at that admission boundary; the executor must not repin. Theater
    mutations from DDL remain private in the transaction's redo overlay until
    successful commit publication; rollback leaves the committed theater
    unchanged, and other connections cannot observe uncommitted CREATE/DROP
    INDEX metadata (or database/graph-type/derived-constraint changes). The
    overlay is applied to the statement-admitted theater state in redo order,
    including index detail/options creation and index drops. Committed theater
    changes are installed with catalog changes inside the publication write
    guard. Direct executor callers without embedded publication coordination
    remain responsible for serializing their own admission against writers.
   Writes and new queries fail closed after the fence. Guard other graph-backed
   reads, including pull lookups, with fail-closed behavior for their existing
   return types. No checkpoint may publish a snapshot or recycle WAL
   containing an uncertain suffix; capping a checkpoint LSN at the last safe
   horizon alone is insufficient if the in-memory graph may be partially
   applied.

   Before the first frame write, close checkpoint admission and **drain any
   already running checkpoint through its publication/recycle completion**.
   Do not wait for that worker while holding the WAL mutex or any lock it
   needs. Recheck the stable graph/WAL state before preflight and append after
   the drain; serialize this handoff with other writers and checkpoint
   admission so no checkpoint can start in the gap. During the physical
   publication and application window, new checkpoints wait or skip; after a
   confirmed success they can resume, but after uncertainty they stay fenced.
   This makes an earlier checkpoint's published cut provably pre-uncertainty,
   without allowing one that was merely pinned before failure to publish later.
   If an already running checkpoint fails, return its failure before
   attempting this append; it cannot silently count as a successfully drained
   publication. That failed-checkpoint signal is scoped to the writer(s) blocked
   on that drain and is consumed when delivered; it must not become a permanent
   fence. A standalone `Database::checkpoint()` failure (for example a
   deterministic invalid persisted value, or an I/O error while writing the
   snapshot) is returned to that call's caller and leaves later writes
   unaffected: a checkpoint that fails with no writer waiting on its admission
   does not poison or fence the handle, precisely as before this ADR. Only the
   physical WAL uncertainty in item 4 above sets the sticky poisoned state.
   Do not retry an uncertain record automatically, truncate a
   possibly durable prefix, or clear the fence in-process: close/drop without
   persist-on-drop, then reopen and replay/reconcile the WAL under normal
   recovery rules.

## Alternatives considered

- **Serialize every candidate once into a group-sized `Vec` of JSON bytes.**
  Rejected: ADR-0086 caps an individual frame, not a group or segment; a large
  group could retain arbitrarily many 64 MiB payloads on top of the redo and
  invalidate the bounded-memory recovery/write envelope.
- **Count with an independent hand-written size estimate.** Rejected: JSON
  escaping, enum tags, numeric formats and LSN field widths can diverge from
  actual serde output; a second slot could still fail deterministically after
  an earlier append.
- **Stream the serialized payload directly into the WAL once.** Rejected:
  the existing frame starts with CRC and length, which must be known before
  writing; spooling/re-reading or patching headers introduces storage writes
  and new partial-frame crash states without eliminating applicability
  preflight. It does not make a later invalid candidate safe to reject.
- **Validate each group slot just before its own append.** Rejected: a later
  oversized or inapplicable slot can still cause an earlier appended slot's
  waiter to receive an error despite recoverable bytes.
- **Reject old WAL records missing derived enforcement.** Rejected: existing
  durable databases must remain loadable with their known historical behavior;
  the stricter admission rule applies to new writes, not legacy replay.
- **Treat every append/sync error as proof of rollback.** Rejected: an I/O
  error cannot establish whether a complete WAL prefix reached durable media;
  pretending the commit was rejected permits duplicate retries or a checkpoint
  to discard an unacknowledged record.
- **Guard only writes and future checkpoint starts after I/O failure.** Rejected:
  a checkpoint already dumping can still publish/recycle after the failure,
  and an in-memory partial apply can leak through reads and pull lookups.
- **Read live catalog after pinning entities.** Rejected: a concurrent schema
  commit can add or drop SHOW rows after read admission, returning a catalog
   from a different commit than the entity snapshot or even after poison.
- **Lock catalog while cloning theater.** Rejected: graph-type DDL can acquire
  theater before asking for named constraints, creating an ABBA deadlock with
  admission. The publication guard provides the cross-component boundary.
- **Mutate shared theater during speculative DDL.** Rejected: another connection
  can observe an index definition with missing TEXT/VECTOR details, and rollback
  cannot restore the committed metadata without racing other readers/writers.
- **Hold the publication guard through SHOW execution.** Rejected: long-running
  catalog pipelines would block WAL sync and application rather than just
  briefly delaying them for admission.
- **Freeze schema at explicit transaction begin.** Rejected: changing the
  existing per-statement schema visibility would prevent later statements
  from observing catalog changes that the transaction currently sees.
- **Let an already pinned checkpoint finish at a clamped safe LSN.** Rejected:
  its snapshot inputs can include partial graph/catalog changes after a failed
  apply; a safe WAL cut does not prove a safe snapshot. Draining it before the
  first write is simpler, at the cost of waiting behind a current checkpoint.

## Consequences

- A deterministic invalid candidate rejects the eligible drained group before
  any WAL byte/LSN changes; conflicts and capacity retain their per-slot
  policy. A passing group still pays one data-segment durability barrier and
  retains separate record LSNs and ordered commits (ADR-0019).
- The counting pass adds serialization CPU, including on serial commits, and
  lengthens WAL-serialization occupancy. Peak *additional* encoded-payload
  memory stays at one <=64 MiB payload rather than scaling with group size;
  the input batch and touched-ID/catalog validation state still consume memory.
  A commit may wait for a previously running checkpoint before attempting its
  first append; read execution must coordinate with apply/fence publication.
  Measure fraud/identity group-commit throughput, WAL mutex wait, p99 and
   memory against the ADR-0070 baseline environment before accepting the
   implementation; do not claim a performance regression is free or freeze a
   host-native envelope.
- Read admission copies the named schema and full graph-backed SHOW theater state once per statement,
  under the short publication guard. SHOW row production can run after the
  guard is released, including after a later commit or poison. Explicit
  transactions keep their begin-pinned entity view and admit schema per
  statement. The direct pinned-read executor API must carry both the entity
  timestamp and the schema snapshot; update its Rust documentation and
   compiled example alongside the signature.
  Theater DDL is staged in the private redo overlay and becomes globally visible
  only on successful publication; transaction statements apply their own redo
  to the freshly admitted committed theater, not to the transaction-begin view.
  Two-thread lock-order and cross-connection CREATE/DROP/rollback tests must
  cover the catalog/theater boundary and SHOW classifications.
 - Tests must cover valid-first/oversized-second slots with all waiters
  completed, unchanged WAL bytes/LSN and clean reopen; conflict/capacity
  exclusions; valid groups with one barrier; replica missing/deleted endpoints
  before append, and valid ordered replication including pull identity. Direct
  `Graph::apply_redo` tests must put an invalid op *after* a valid op and prove
  no prefix was installed: missing or earlier-deleted relationship endpoints,
  conflicting/equivalent catalog index and constraint inserts after ordered
  drops/creates, malformed `CatalogIndexDetail` options, and invalid IDs on
  non-create ops. Check idempotent catalog inserts and valid create/delete/
  recreate sequences against group and replica preflight too. Fault-injected
  partial append, failed barrier, and post-sync apply errors must assert the
  distinguishable uncertain outcome, completed waiters, unchanged safe horizon
  and a sticky shared fence against new writes, replication, queries/reads,
  explicit/timed/scheduled checkpoints, rotation, and drop persistence.
  Race a pre-existing checkpoint against an attempted append: it must finish
  before any frame write, or the write must wait; after poisoning, manifest and
  recycle cut must not advance. Verify reopen/replay recovers the surviving
  prefix without an in-process retry or checkpoint of partial state.
- ADR-0086's per-record payload bound still applies to direct WAL callers;
  replay and direct graph apply continue validating malformed inputs. Previously
  accepted zero or non-increasing WAL timestamps now fail open/replay rather
  than being applied with inconsistent MVCC visibility. Public
  Rust API error/lifecycle documentation must describe the distinction when
  this policy is implemented.

## Later references

ADR-0116 replaces the SHOW DATABASES theater rows with three fixed namespaces:
`neo4j`, `system` and `system_ontologies`. Read admission still snapshots
their stored settings with the rest of the schema theater under the
publication guard.

- **Round-2 review of PR #200 replaced the count-then-encode pass.** The
  opaque proof now owns the payload encoded once at preflight, and a
  preflighted append writes those bytes as the frame without re-encoding or
  re-checking their size. A group retains each candidate's encoded payload
  until its append consumes it, so pre-append encoded-payload memory scales
  with the eligible group; each payload keeps the 64 MiB bound.
- ADR-0558 moves the single encoding of a commit's redo to
  admission, so the group leader no longer serializes the ops: it stamps the
  record header into the buffer the committing thread already produced.
