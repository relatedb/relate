# REL8-1982 round 1 — results (in progress)

Measured on `d98aee8f` with vortex 0.93.0, lab crate `testing/relate-vortex-lab`.
Every measurement carries the 1-minute load average; the box is shared
(load 13–35 during these runs), so ns figures are noisy while **bytes and
allocation counts are the load-robust signal**.

## Size: bytes per property (vortex, per-column compressed sizes)

### Comment, 151,043 rows — 14.96 MB raw -> 6.23 MB vortex (41.6%)

| column | type as loaded | B raw/value | B vortex/value |
|---|---|---|---|
| content | Str | 33.4 | **19.50** |
| creationDate | Str | 28.0 | 13.34 |
| id | Int | 8.0 | **5.12** |
| locationIP | Str | 12.7 | 2.02 |
| length | Int | 8.0 | 0.89 |
| browserUsed | Str | 9.0 | 0.38 |

### Post, 135,701 rows — 10.79 MB -> 3.46 MB (32.1%)

`content` 6.53, `creationDate` 12.60, `id` 5.11, `locationIP` 0.74,
`length` 0.40, `browserUsed` 0.05, `language` 0.05 B/value.

### Forum, 13,750 rows — 0.82 MB -> 0.44 MB (53.7%)

`id` 5.36, `title` 13.40, `creationDate` 13.40 B/value.

### Person, 1,528 rows — 66,377 B -> 43,356 B (65.3%)

`id` 8.1, `firstName` 6.5, `birthday` 6.4, `lastName` 5.5, `browserUsed` 1.1,
`gender` 0.8 B/value.

### Synthetic flat, 150,000 rows, 4 int attrs — 7.95 MB -> 1.50 MB (18.8%)

`id` (dense 0..n) **0.000 B/value** — 776 B for 150k values;
`key` (unique strings) 5.5; `attr0..3` (periodic ints) 1.0–1.3 B/value.

### Findings that matter more than the ratios

1. **Dense internal ids are free; sparse business IDs are not.** The
   synthetic `id` column (dense `0..n`) compresses to 0.000 B/value, but the
   real LDBC `id` costs **5.12 B/value** — business ids are sparse snowflake
   numbers. Retiring the engine's id-vec slots rests on *internal* dense
   node-ids; it does NOT extend to a sparse business `ID` property. Do not
   quote the 0.0 B/value figure without this distinction.
2. **Dates loaded as strings are the worst column** (12.6–13.4 B/value):
   the lab loads `creationDate`/`birthday` as `Str`, so vortex's
   `datetime_parts` encoding never applies. A `Ty::Temporal` column is a
   concrete, unmeasured win to add (the engine's wide codec keeps temporal
   values residual too, so this is a shared gap).
3. **Low-cardinality strings compress 20–100x** (`browserUsed` 0.05–0.38,
   `language` 0.05, `gender` 0.8 B/value) — the dictionary path is where the
   property-bytes win concentrates.
4. **High-cardinality text compresses only ~1.5–2x** (`content` 19.5 vs 33.4
   raw, `title` 13.4 vs 23.8) — free text is the floor of the size story and
   it dominates the Comment table's absolute bytes (2.94 MB of 6.23 MB).
5. **Repeating ints compress 6–20x** (`length` 0.40–0.89 vs 8 raw).

## Kernels O1/O2/O3 (first pass, load 13–20 — ns noisy, allocs reliable)

- O1 point read on a warm decoded column: 2,083 ns, 3 allocs/value.
- O2 three columns at the same rows: 36,555 ns, 28.5 allocs — **18x for 3x**
  (see the probe investigation; per-value string extraction is
  allocation-heavy, ~9.5 allocs per string value).
- O3 seek, 150k rows: full scan+compare 457 µs vs binary search on the
  sorted key **586 ns** (780x).
- O3 key-column decode (mmap + read_all): 1.19 ms, 1425 allocs, 182 KB.
- O10b `with_row_indices` 1024-row gather: 899 µs = 878 ns/row, 1.4
  allocs/row — suspicious (see probe investigation).

## Open

- O4–O20 kernel results (fan-out in progress).
- The two anomalies above (O2 cost scaling, O10b gather behaving like a
  re-scan).
- The engine-side A/B (CompactProps + postings) — needs the container
  harness or the `research`-feature read path; `CompactProps` is `pub(crate)`.
- `Ty::Temporal` column for date properties.
- The VFS mmap prototype (round-1 deliverable, step 60).
