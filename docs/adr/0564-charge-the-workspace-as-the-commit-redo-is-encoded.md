# ADR-0564: Charge the workspace as the commit redo is encoded

Status: Accepted
Claim: 9ea5823967dca6a159678c170c4f399f38bbba0c

## Context

ADR-0558 encodes a commit's redo ops once, at admission, and charges the
workspace for the finished buffer. The size is only known after encoding,
so the charge followed the allocation: the serializer's buffer grew up to
the 64 MiB record cap before any charge landed. N concurrent large writers
could therefore allocate N buffers past the workspace limit before the
first charge was attempted, and only then be refused. ADR-0558 recorded
that transient as an accepted cost; with several writers it is unbounded
in N, which defeats the purpose of the workspace limit.

## Decision

1. **Charge before every growth.** The encoder takes the caller's workspace
   charge and extends it before each growth of its buffer, then allocates
   exactly that capacity. A refused charge aborts the encoding at once with
   the workspace's own typed refusal; nothing beyond the granted charge was
   allocated.
2. **Steps.** The buffer grows in powers of two from 1 KiB, or to the next
   power of two that holds a single large write, clamped to the record cap.
3. **The copy transient is charged.** While a growth copies, the old and
   the new buffer are both resident, so both are charged for that moment
   and the charge then settles at the new capacity. Concurrent writers are
   thereby bounded by the limit itself, with no allowance for the
   reallocation.
4. **The cap is checked first, in the same sink.** The record cap is
   admitted before any growth is charged, so the cap's refusal, its
   figures and its text are unchanged, and it fires at the same write.
5. **The retained buffer is charged at its capacity.** The trimming copy of
   ADR-0558 decision 4 is dropped for records that outgrew the first step:
   their charge equals the buffer's capacity (at most twice the record),
   held until the commit's waiter completes as before. Records that fit the
   first 64 KiB are still trimmed to their length, which is a cheap copy.
6. **Every caller supplies the charge.** Admission, the replicated apply
   path, the trait-level write engine and the one-call preflight each pass
   a charge; with no workspace ledger installed it is a no-op.

## Alternatives considered

- **Charge after encoding, as in ADR-0558.** Rejected: concurrent writers
  allocate past the limit before any is refused.
- **Count the record first and charge the exact size.** Rejected: it is the
  extra serialization pass ADR-0558 removed.
- **Fixed-size steps instead of doubling.** Rejected: the copy transient is
  then small but the number of copies for a large record grows linearly,
  and charging the transient in full is simple enough.
- **Charge only the new capacity and tolerate one step of transient.**
  Rejected: the bound would then be the limit plus a step per writer.

## Consequences

- Peak memory of concurrent large commits is bounded by the workspace limit,
  checked by an allocator-counting test.
- A large commit's retained buffer can hold up to twice its record in
  charge, and a growth near the limit is refused a step earlier than the
  final size alone would be, because the old buffer is charged with the new.
- The trimming copy of large buffers is gone, which also removes its
  transient.
- Supersedes decision 4 and the transient noted in the consequences of
  ADR-0558; the rest of ADR-0558 stands.
