# ADR-0527: Write engine seam

Status: Proposed

## Context

Durability is a logical redo log. The commit timestamp is assigned when
the log appends (ADR-0015). Group commit, checkpoint, follow, and change
capture are wired to the concrete log inside the facade. A clustered
log would assign order differently and would still have to feed the
same redo into the same query engine.

The snapshot codecs reach into the live graph. A write engine cannot be
replaced while that edge exists.

## Decision

The redo operation is the change unit shared by query engines and write
engines. A write engine owns durable ordering and the commit clock:
open and recovery, the durable commit of redo groups, the checkpoint
taken from an export, follow, change-data capture, and the writer
lease.

The facade keeps group-commit admission, conflict checks, apply, and
the order in which a commit is published. A replacement write engine
replaces the durable part only.

Today's log, checkpoint, follow, and change capture become the local
write engine, behind that contract. The file layer, the snapshot
codecs, and the log record format stay a toolkit a write engine may
use. No alternate write engine is added.

The store manifest records the write engine identity beside the query
engine identity (ADR-0526). A store written before this change opens as
the local engine. Opening a store whose recorded identity is not the
engine being opened fails. The identities are optional manifest fields;
redo records and snapshot payloads keep their encoding.

## Alternatives considered

- **Clustering code inside the current log, behind a flag** — rejected:
  the two durability models would share one implementation and break
  each other.
- **The write engine also applies redo and publishes snapshots** —
  rejected: visibility and pins belong to the query engine, and the
  facade is what orders apply against publication.
- **A page buffer that writes dirty pages before commit** — rejected:
  that is a different durability model and needs its own engine, which
  this change does not build.

## Consequences

- The local write engine lives in its own crate. The facade names the
  concrete engines in one module.
- The contract type replaces the log record type without changing the
  JSON encoding of a record.
- The contract does not assume threads, so the browser build keeps the
  same traits with timers and follow compiled out.
