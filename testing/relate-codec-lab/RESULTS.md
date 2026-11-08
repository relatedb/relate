# Experimental snapshot-codec results (2026-11-07)

Harness: `testing/relate-codec-lab` (branch `feat/experimental-codecs`).
Graph: `relate-datasets` fraud generator, scale 2.0 — **29,989 nodes /
8,675,808 relationships** (rel-heavy by construction), built once and
checkpointed/reloaded by every codec. Property checksum asserted equal
across codecs on every run (a codec that loses data fails the run).

Codecs: parquet (baseline, snappy), json (full rewrite), rkyv (experimental,
columnar SoA + one lz4 block), fjall (experimental, LSM keyspace with lz4
over cypher-json rows). All measurements on the dev machine, macOS arm64,
release build, rustc 1.88.

## Store size and wall time

The current set: validated rkyv format (flat value table, decoded straight
from the archived bytes — see Findings 5) on the corrected fraud graph.
`fraud` is best-of-3; the pokec and SF0.1 runs are one pass each on a
quiet machine.

fraud scale 2.0 — **29,989 nodes / 8,695,813 relationships**:

| codec | snapshot bytes | vs parquet | checkpoint | load |
| --- | --- | --- | --- | --- |
| parquet | 353,795,439 | 1.00x | 25451 ms | 19302 ms |
| json | 1,170,987,179 | 3.31x | 9991 ms | 20348 ms |
| **rkyv** | 389,081,825 | 1.10x | **9861 ms** | **18067 ms** |
| fjall | 367,943,742 | 1.04x | 30539 ms | 21318 ms |

pokec medium — 100,000 nodes / 1,768,164 relationships (property-light):

| codec | snapshot bytes | vs parquet | checkpoint | load |
| --- | --- | --- | --- | --- |
| parquet | 47,896,169 | 1.00x | 2500 ms | 2039 ms |
| json | 155,280,298 | 3.24x | 590 ms | 1977 ms |
| **rkyv** | **32,804,033** | **0.68x** | **567 ms** | **1749 ms** |
| fjall | 319,196,390 | 6.66x | 4635 ms | 4525 ms |

LDBC SF0.1 — 327,588 nodes / 1,477,965 relationships (property-heavy):

| codec | snapshot bytes | vs parquet | checkpoint | load |
| --- | --- | --- | --- | --- |
| parquet | 55,334,125 | 1.00x | 2902 ms | 3272 ms |
| json | 214,479,291 | 3.88x | 1086 ms | 3182 ms |
| **rkyv** | **51,737,001** | **0.93x** | **1392 ms** | 3654 ms |
| fjall | 322,559,146 | 5.83x | 7202 ms | 5540 ms |

Caveat: one pass for pokec/SF0.1 (the fraud row is best-of-3), so the
load column is the noisiest — treat ±15% as the resolution there.

## Findings

1. **rkyv keeps a large checkpoint win on every shape** — 2.6x parquet on
   fraud, 4.4x on pokec medium, 2.1x on SF0.1 — because it writes one
   columnar archive instead of a shard set with per-shard overhead.
2. **Validation costs rkyv its inline-value compactness on property-heavy
   shapes.** With the validated flat value table it is 0.68x parquet on
   property-light pokec but 0.93x (SF0.1) and 1.10x (fraud) where values
   dominate; the earlier inline-value layout measured 0.71x on fraud but
   could not be validated (a recursive type cannot carry `bytecheck`
   bounds) and required unchecked access. The trade is deliberate: a
   corrupt store file is a typed error, not undefined behaviour.
3. **Load is at parity, not a rkyv win.** Decoding straight from the
   archived bytes (no owned copy) makes rkyv the fastest load on fraud
   (18067 vs 19302 ms) and pokec (1749 vs 2039 ms), but the validation
   traversal costs it on the largest archive (SF0.1: 3654 vs 3272 ms).
   One pass each — the column is the noisiest here.
4. **fjall's size is row-overhead-bound, not payload-bound**: 6.7x parquet
   on property-light pokec, 5.8x on SF0.1, against 1.04x on property-heavy
   fraud — per-row keys, JSON envelopes and LSM overhead dominate when
   rows carry little data. Its checkpoint is the slowest everywhere.
5. **The harness had been measuring a truncated graph.** The lab read the
   fraud generator's `ownership` tuples in the wrong order and silently
   dropped every `OWNS` edge (20,005 of them) — invisible to the
   cross-codec checksum, which compares codecs against each other, not
   against the generator. The fail-loud change surfaced it; every figure
   above is on the corrected graph. Measurement harnesses need the same
   "no silent fallbacks" discipline as product code.

## Binary size

Docker linux-x64 `relate-ffi` cdylib, rustc 1.88, release:

| features | cdylib bytes | MB | vs baseline |
| --- | --- | --- | --- |
| (baseline; parquet linked) | 25,658,432 | 24.5 | — |
| + `rkyv-codec` | 25,759,232 | 24.6 | **+0.1 MB** |
| + `fjall-codec` | 26,736,112 | 25.5 | **+1.0 MB** |
| + both | 26,810,632 | 25.6 | **+1.1 MB** |

So the experimental codecs are cheap in binary terms: rkyv + lz4_flex costs
~0.1 MB, fjall ~1.0 MB — against the 8.2 MB that `arrow`+`parquet` cost
(measured in the size-fix PR: cdylib 25.6 MB with parquet, 17.5 MB without).

Measurement caveat: `relate-ffi`'s `relate` dependency enables `parquet`
unconditionally, so no combo here dropped arrow (the "baseline" row still
links it). The arrow-free number is the 17.5 MB from the size-fix PR.

## Caveats

- Timings are from a **contended machine** (load average 8-11; other agent
  sessions active). Sizes are load-insensitive and solid; wall times are
  indicative and need a quiet-window re-run (repeat count > 1).
- Synthetic rel-heavy shape (30k nodes / 8.7M rels). The property-heavy
  SF0.1/SF1 LDBC shape may rank the codecs differently — the same harness
  takes it once the dataset is fetched.

## Cross-reference: LDBC SF1 (relate-storage's independent run)

Same machine, contended by this harness's builds (their RSS numbers are
clean, their wall times contended):

| measure | SF1 |
| --- | --- |
| build from CSV | 702 s |
| restore | 111 s |
| peak RSS (restore) | 21.25 GB (steady 18.7 GB) |
| store | 630 MB (legacy parquet codec, tagged-JSON property column) |

SF1's row mix is 3.18M nodes / 17.26M rels and **property-heavy on nodes** —
the opposite emphasis to this harness's shape, so the codec size ranking is
shape-sensitive and the two size numbers are not comparable directly. The
agreed apples-to-apples follow-up: run this lab on SF1's row mix (and on the
wide-column store once that writer lands) and compare against the 630 MB
legacy-parquet figure.
- rkyv archives are **validated** on load (`rkyv::access` + `bytecheck`):
  the flat value table keeps the archived type non-recursive, so every byte
  is checked and a corrupt store file is a typed error.
- fjall requires a real local directory (fails closed on `:memory:` and
  object stores) — a typed capability (`needs_native_root`), not a bug.

## Reproduce

```
cargo run --release -p relate-codec-lab -- <source> [repeats]
# fraud:2.0            default; a bare number is the same thing
# pokec:medium         synthetic pokec fixture (tiny/small/medium)
# ldbc:<dataset-dir>   extracted LDBC social_network CSV directory
```
