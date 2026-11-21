---
type: adr
area: storage
claim: 734060b72ace3da0e0332a5876b650a62831f453
date: 2026-11-21
deciders: Relate maintainers
name: 0650-configurable-wal-payload-cap
status: proposed
tags:
  - storage
  - memory
title: The per-commit WAL payload cap is 256 MiB by default and configurable at open
REFERENCES:
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0452-workspace-limit-enables-admission]]"
  - "[[adr/0494-wal-payload-cap-memory-limit-class]]"
  - "[[adr/0641-batched-generator-seeded-tails]]"
---

# ADR-0650: The per-commit WAL payload cap is 256 MiB by default and configurable at open

## Status

Proposed — lands with its implementation.

## Context

ADR-0086 bounds one commit's WAL record at 64 MiB, and that bound is a
hard constant in the engine (`Error::WAL_PAYLOAD_CAP_BYTES`): no knob, no
environment override. The refusal is typed and its message names the right
remedy — commit in smaller transactions, `CALL { … } IN TRANSACTIONS OF n
ROWS`, or client-side batches (ADR-0494).

Two facts have moved since:

1. **The bulk-load shape grew a one-transaction path.** ADR-0641 runs a
   generator-seeded, per-row-independent tail batch-major inside one
   transaction; its redo *is* the transaction's own state, deliberately
   proportional to the writes. A 656 k-relationship part of a bulk load
   commits as one record whose encoded size can approach — or cross —
   64 MiB, and the statement's only recourse today is the statement-level
   rewrite or many smaller statements.
2. **The sibling bound is configurable.** The workspace pool that admits
   concurrent large commits (`concurrent_logical_workspace_limit`, plus
   `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`, ADR-0452) is an
   open-time knob. The asymmetry is felt by exactly the user the pool
   knob serves: an agent that has sized its memory budget and wants one
   large commit in one go has no lever for the record bound.

## Decision

- The **default cap is 256 MiB** (four times ADR-0086's bound). The cap
  still exists for the same reason it always has: one record is encoded
  into a buffer before it is appended, so the record bound is a memory
  bound on a single commit's prospective size.
- The cap is **configurable at open**: `OpenOptions::wal_payload_limit`
  (bytes), with the `RELATE_WAL_PAYLOAD_LIMIT` environment variable as
  the fallback when the field is unset — the same resolution shape as the
  workspace pool knob. A non-positive or unparsable value fails the open
  loudly, never silently ignored. Unset keeps the 256 MiB default. The
  knob is bounded by the WAL frame format itself: a record's length field
  is 32 bits, so a value above 4 GiB is refused at resolution — a cap the
  format cannot encode would admit records whose persisted length
  truncates and the store could never reopen.
- The **admission** sites read the resolved value once at open: the
  bounded encode (`EncodedOps`, the storage layer) and the explicit
  transaction's commit-cap tally (the pre-check that refuses the
  *statement* that would cross the cap, so a transaction never runs to
  its last statement only to fail the commit). The storage constructors
  take the cap as a parameter — one public path per operation; the plain
  `Wal::open` is the ad-hoc default-cap form, and there are no
  default-supplying twins of the cap-taking forms.
- The **recovery** scanner is bounded by the frame format itself — the
  32-bit record length field — never by the per-open admission cap: a
  record written under a larger cap than a later open's is a valid frame,
  and refusing it at replay would make the store unopenable under a
  smaller cap. An absurd length is caught by the file-length check as an
  incomplete payload (a torn tail); only a structurally invalid zero
  length is refused outright. The same format bound refuses a configured
  cap above 4 GiB at resolution, so a record the writer can admit is
  always a frame the format can encode.
- The refusal message names the **configured value** (not the constant)
  and keeps the smaller-transaction remedy; the knob's existence is
  documented for agents in the CLI skill's operations reference beside
  the workspace-pool knob, so the two bounds are learned together.

The two knobs stay **independent and complementary**: the workspace pool
bounds concurrent admissions (how many large commits may be in flight),
the payload cap bounds one record (how large each may be). Raising the
payload cap without pool headroom refuses at admission — by design.

## Consequences

- A single commit may now hold up to 256 MiB of encoded record by
  default; the pool must have room for it or the commit is refused at
  admission (ADR-0452), which is the correct ordering.
- ADR-0086's 64 MiB figure is superseded as the default; the *existence*
  of a per-record bound stands — this ADR changes its value and makes it
  configurable, it does not remove it.
- Agents running large single commits learn one pair of knobs
  (`RELATE_WAL_PAYLOAD_LIMIT` + `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`)
  from the skill's operations reference.
- The Rust `OpenOptions` surface gains a documented field
  (`docs/api/rust/spec.md` updated in the same change).
