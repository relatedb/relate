# ADR-0553: Wide-column parts are the Parquet checkpoint format

Status: Proposed
Claim: 52b212458dcb568b3e98ee89b377fa4a5292a1b4

## Context

ADR-0534 added the wide-column part encoding as a per-store opt-in and kept
the tagged-JSON property column as the default, so both encodings shipped
behind a flag and either could be exercised in the field.

The project has no users and the repository is private: no store exists
outside development machines and CI fixtures. The compatibility cost that
justified the opt-in — existing stores, older binaries, a staged rollout —
is zero. What remains is the opt-in's own carrying cost: two live write
encodings, a dual test surface, and a knob threaded through the engine
facade, the codec-configuration slot and the CLI, none of which any store
needs to turn off.

## Decision

The wide-column part encoding **is** the Parquet codec's checkpoint format.
Every Parquet store writes wide parts; a fresh store starts wide. The
tagged-JSON part encoding is no longer written.

Reading it stays: manifests at format versions 1–4, the monolithic v1/v2
fallback and the tagged-JSON property column remain readable. The first
checkpoint of a legacy-encoded store re-encodes every part wide — the
one-time transition ADR-0534 defined — so a legacy store upgrades at its
next checkpoint, not on open: the upgrade rewrites every part, and tying
that to open would make each legacy store's first open a full re-encode at
an unpredictable moment, when the next checkpoint does the same work at a
moment the store is already writing. Migration is one-way: wide, never back
to the tagged-JSON column.

The opt-in is deleted everywhere it was exposed: the open-time
codec-configuration variant, the engine facade's flag, and the CLI value.
A store's encoding is a fact of its manifest, never of an open.

The version registry is unchanged in shape: both the legacy (3/4) and wide
(5/6) pairs stay writable-classified, because the experimental codecs
(ADR-0537) still write the legacy pair; the wide pair is simply what the
Parquet codec writes now.

## Alternatives considered

- **Flip only the default, keep the flag as a legacy opt-out** — rejected:
  two live write encodings remain, and the knob's every layer (facade,
  codec configuration, CLI, docs) outlives a need that no store has.
- **Delete legacy read support with the write path** — rejected: the legacy
  decode and the upgrade path are then exercised by nothing; keeping the
  reader costs one code path and preserves the migration test surface that
  protects future format changes.
- **Migrate legacy-encoded stores on open** (the ADR-0127 mechanism) —
  rejected: it fronts the full re-encode on every legacy store's first
  open; the next checkpoint performs the identical rewrite without
  changing when the work happens.

## Consequences

- One write encoding in the Parquet codec. The tagged-JSON column survives
  in two readers: legacy parts, and the residual tail of unpromoted keys.
- The upgrade of a legacy store re-encodes every part and marks every live
  id dirty — bounded, once per store, at its next checkpoint.
- The wide flag disappears from the public Rust API and the CLI; the CLI's
  `parquet` snapshot-format value now denotes the wide encoding.
- ADR-0534's recorded follow-ups (continuous per-key statistics,
  reader-side decode) carry over unchanged; ADR-0534 gains a
  later-references note pointing here.
- Before the repository or its binaries are public, revisit nothing: the
  readable legacy range is the only compatibility surface left, and it
  exists to upgrade development stores, not to serve anyone.
