# Adjacency-payload A/B results (2026-11-21)

Harness: `testing/relate-adjacency-payload-lab` (see `README.md`). Release
build from the feature branch head, `--rounds 2`, on an **idle box (macOS
arm64, 16 cores)**: a quiet-window claim was posted to the other active agent
sessions before the CPU-saturating runs started (the 5–10-minute claim
window protocol), no other benchmark was live, and every run is stamped with
the host's 1/5/15-minute load averages. The 1-minute stamps ran 4.02–6.81
across the runs — under the quiet-box bar of 8.0 (half of the 16 cores); the
box's ambient GUI/daemon baseline is ~4–5. Two interleaved rounds per arm
(payload on, payload off, on, off); each number below is the mean of the two
runs.

This re-measures on the **fixed payload format**: the 7-column schema with a
per-row `crc32` the install verifies before a chunk lands, and no watermark
column (`payload.rs`; the previous block predates it).

The three stores: `relate-datasets` shapes tuned by their own closed-form
`census` to the target relationship count; the payload threshold
`RELATE_ADJACENCY_PAYLOAD_MIN_RELS=0` so both arms publish at every size.

| store | shape | nodes | relationships |
| --- | --- | --- | --- |
| `dense-ring-2m` | dense-ring | 100,000 | 2,000,000 |
| `mid-degree-2m` | mid-degree | 21,984 | 2,000,544 |
| `mid-degree-10m` | mid-degree | 109,904 | 10,001,264 |

The phases: **publish** = the checkpoint that writes the store (payload on
adds the payload object); **open** = the snapshot load wall, with its
adjacency-tail share (the payload install, or the rel-row re-fold) in
parentheses; **close** = the `Database` drop checkpoint after the fixed write
batch; **read**/**write** = the fixed query set and statement batch.

## dense-ring-2m — 100,000 nodes / 2,000,000 rels

| phase | payload off | payload on | delta |
| --- | --- | --- | --- |
| publish | 446 ms / 15.45 MB | 632 ms / 27.80 MB | +186 ms / +12.35 MB |
| open (tail) | 631 ms (409 ms) | 1706 ms (1483 ms) | **+1075 ms (loss)** |
| read | 116 ms | 117 ms | +1 ms |
| write | 1290 ms | 1281 ms | −8 ms |
| close | 420 ms | 523 ms | +103 ms |

Load stamps (1/5/15 min, publish then runs on/off/on/off): publish
4.02/5.15/5.15; 4.02/5.15/5.15 → 4.98/5.33/5.22 → 4.82/5.29/5.20 →
4.60/5.23/5.18.

The install (1483 ms) is **3.6× the re-fold** (409 ms): at one hop per chunk
the fold is a cheap adjacency build, while the payload pays a Parquet decode
plus a per-chunk install and a hop-ts read-back from the rel rows. At this
shape the payload is a pure loss.

## mid-degree-2m — 21,984 nodes / 2,000,544 rels

| phase | payload off | payload on | delta |
| --- | --- | --- | --- |
| publish | 423 ms / 17.77 MB | 574 ms / 23.14 MB | +151 ms / +5.37 MB |
| open (tail) | 404 ms (237 ms) | 403 ms (236 ms) | −1 ms (parity) |
| read | 33 ms | 33 ms | 0 ms |
| write | 302 ms | 299 ms | −3 ms |
| close | 388 ms | 444 ms | +56 ms |

Load stamps (1/5/15 min, publish then runs on/off/on/off): publish
4.33/5.14/5.15; 4.33/5.14/5.15 → 4.15/5.09/5.13 → 4.15/5.09/5.13 →
4.15/5.09/5.13.

Parity on the open (1 ms) while every checkpoint pays ~207 ms more (publish +
close). No case to lower the threshold to this size.

## mid-degree-10m — 109,904 nodes / 10,001,264 rels

| phase | payload off | payload on | delta |
| --- | --- | --- | --- |
| publish | 4433 ms / 88.83 MB | 4788 ms / 99.63 MB | +355 ms / +10.80 MB |
| open (tail) | 2773 ms (1380 ms) | 2283 ms (856 ms) | **−490 ms (win, 18%)** |
| read | 140 ms | 138 ms | −2 ms |
| write | 1502 ms | 1474 ms | −28 ms |
| close | 1840 ms | 2113 ms | +273 ms |

Load stamps (1/5/15 min, publish then runs on/off/on/off): publish
4.33/4.94/5.07; 4.22/4.91/5.06 → 4.61/4.98/5.08 → 4.89/5.03/5.10 →
5.46/5.14/5.14 → 6.81/5.45/5.25.

The re-fold (1380 ms) outgrows the install (856 ms) once the chunks seal: the
install copies sealed streams verbatim while the fold re-encodes them. The
open win is real (18%) but the 628 ms per-checkpoint penalty (publish + close)
is larger than it.

## Verdict

**Keep the 10,000,000-relationship default; do not lower it.** Below it the
payload buys nothing on the open — at ~2M rels the node-heavy `dense-ring`
shape is 1075 ms *slower* with it installed (1706 vs 631 ms, the install 3.6×
the re-fold) and the rel-heavy `mid-degree` shape is parity (403 vs 404 ms) —
while both sizes pay an extra 151–186 ms per checkpoint publication, 56–103 ms
per close, and 5.4–12.4 MB of store. At ~10M rels the install is material
(open 2773 → 2283 ms, 18%, tail 1380 → 856 ms) but the 490 ms open win is
still smaller than the 628 ms per-checkpoint penalty (publish +355 ms, close
+273 ms), so one open does not cover one checkpoint even there. Since an open
is a process-start event while the publication cost recurs on every
checkpoint, enabling the payload below 10M would charge every checkpoint of a
small store for an open win that is zero or negative — the default line stays
where it is, and this measurement gives no reason to move it down.
