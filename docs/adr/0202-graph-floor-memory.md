# 0202. Graph floor includes retained structure

- **Status**: Accepted
- **Date**: 2026-10-10
- **Deciders**: MemoryDB maintainers
- **Amends**: ADR-0042
- **Later references**: ADR-0529 (a single-version row is an inline
  slot, charged without a version list or `xmax`)

## Context

ADR-0042 caps `Database::memory_usage` with a portable formula: payload
bytes plus a few fixed proxies (a 24-byte version, a 16-byte hop). That
integer does not move with the allocator, which is what the pre-WAL
check needs. It also does not move with the containers that still own
the payload. A ring and a dense ring with the same logical size keep
very different resident graphs, because a relationship is a version
record plus two adjacency vectors, not 16 bytes of hop.

Two exclusions in ADR-0042 make the counter drop while the bytes are
still there. A delete clears the charge immediately, and the version
stays until checkpoint GC. The intern table is charged again on every
`Arc` clone if it is charged at all, so it was left out.

## Decision

`memory_usage` stays `(used, Option<limit>)`. `used` is the graph floor:
the ADR-0042 payload, plus the containers that still hold it.

**In the floor.**

- Every node and relationship version still in the chain, including
  tombstones. A delete frees the floor when checkpoint GC drops the
  version, not when the delete commits. An in-place update replaces the
  record. The first edit of a committed record keeps the old version
  and adds the new one.
- One dashmap slot each for `nodes`, `rels`, `out_adj`, and `in_adj`.
- Version lists, label-id vectors, compact property vectors, and
  `TypedAdj` chunk and hop vectors. A vector of length `n` is charged
  `reserved(n) * size_of::<T>()`, where `reserved(0) = 0` and every
  other length is the next power of two. Install paths reserve that
  same capacity. The 16-byte hop proxy is not added on the relationship;
  the hop bytes come from the adjacency vectors.
- Label-index postings, and the TEXT, POINT, and RANGE postings
  `node_account` already names. The text-gram term stays a formula of
  the live string (ADR-0063). A range index on an integer is one
  posting, so it is no longer zero.
- The string interner, once per distinct label, relationship type, or
  property name: two dashmap slots and one `Arc<str>`. A clone of that
  `Arc` is not charged again. The empty database interns `*` and `""`
  on each namespace graph.

**Out of the floor.** Bolt sessions, connection buffers, query plans
and rows, transaction overlays, WAL and checkpoint buffers, the
procedure registry, the pin table, and pages the allocator has kept
after the graph dropped the allocation. Dashmap shard tables are not
charged; the entry is. RSS stays a separate Bolt-child measurement
(ADR-0289).

The pre-commit check is unchanged: `estimate_redo_delta` uses the same
functions as the live walk, and `used_bytes == accounted_bytes`.

## Consequences

Shape pins and the `LOGICAL_MB` fit move with the floor. Fraud,
identity, and checkpoint keep calling `memory_usage`; their logical
fields change and their report schema does not. A delete no longer
makes room for a write until a checkpoint has collected the tombstones.

## Alternatives considered

- **`malloc_usable_size`.** Matches one allocator and one run, and it
  cannot be computed from the redo before the write. Rejected.
- **A second public meter for "real" bytes.** Callers of the cap and
  the pins would disagree about which number is the budget. Rejected.
