# ADR-0563: The Parquet row-group cap is 1024 rows

Status: Proposed
Claim: 74979dd119bb422695147797b06ef165f2cdcbc8

## Context

ADR-0553 made the wide-column part encoding the Parquet codec's only
write encoding, and ADR-0534's leaf count grew with it: every promoted
column is a leaf, and ADR-0180's 64-row row group made every leaf write a
dictionary page and a data page per 64 rows. At the wide worst case the
page envelope (ADR-0086, ADR-0534) held only 59,904 node / 56,640
relationship rows per file, so a checkpoint published 8–10× more part
files than the fixed-schema era — each paying create, two fsyncs, footer
preflight and stat. Measured with the codec lab (best of 3, fresh
stores): parquet's checkpoint was the slowest codec on the fraud shape
(3,406 ms vs json 1,797 / rkyv 1,779) across 43 part files; pokec:medium
held 36.

## Decision

Raise `ROW_GROUP_CAP` from 64 to 1024. The page envelope scales by the
cap, so a file's row bound rises to 958,464 node / 906,240 relationship
rows — still under the 1,000,000-row file cap (ADR-0086) and the
1,000,000-id `MAX_WINDOW` (ADR-0183). Row groups close on the row cap
alone, as before; the byte split and decoded-envelope discipline of
ADR-0180 Decisions 1–2 are unchanged. The reader takes row groups from
each file's footer, so stores written at 64-row groups stay readable and
copyable, and the copy path's batch size — the cap — stays within the
public decode bound of 1024-row batches.

## Alternatives considered

- **Keep 64.** Rejected: the measured write tax stands on every wide
  write, and no other remedy moves the file count.
- **256 (measured).** Rejected: it takes roughly half to three quarters
  of the checkpoint win (pokec 1,477.7 → 757.5 ms, fraud 3,406.0 →
  1,978.6 ms) but stays behind 1024 on load, directory size and part
  count on both shapes (14/18 parts vs 4/4).
- **A width-aware cap, or planning the row bound by a part's actual
  column count (ADR-0534's recorded refinement).** Rejected for this
  change: strictly better shape, but it is a derivation redesign with
  its own verification; the flat raise is measured now and does not
  preclude it.

## Consequences

Measured (same lab, same box, cap 64 → 1024): pokec:medium checkpoint
1,477.7 → 559.3 ms (−62%), load 1,276.7 → 1,000.8 ms (−22%), directory
47.65 → 32.02 MB (−33%), parts 36 → 4; fraud:1.0 checkpoint 3,406.0 →
1,590.7 ms (−53%), load 2,573.3 → 1,620.6 ms (−37%), directory 93.17 →
64.93 MB (−30%), parts 43 → 4. Parquet's fraud checkpoint is now at
parity-or-better with json and rkyv (1,590.7 vs 1,691.0 / 1,745.9 ms);
on the property-light pokec shape it remains above them (559.3 vs
~390 ms) — the residual is per-part schema derivation and per-column
page work, ADR-0534's open refinement.

One row group's write-side peak grows with the cap: the group's Arrow
buffers are live together, bounded by the part's byte target exactly as
before (a byte-heavy part still cuts early). Decode granularity
coarsens for range reads that decode a whole group to reach a few rows;
full restores decode everything and measured faster. Fewer, larger
files concentrate first-checkpoint file-create cost into fewer, bigger
barriers.
