# 0077. Configurable WAL durability boundaries

- **Status**: Accepted
- **Date**: 2026-09-19
- **Amended**: 2026-09-24
- **Deciders**: CypherLite maintainers

## Context

Durable commits fsync once per group. Group commit (ADR-0019) is implemented
and effective: on the on-disk fraud loadtest, 50 workers yield **26 commits
per fsync**; fsync is 4.36 ms p50 while parse + encode + apply are ~0.2 ms,
so ~97% of write wall-time is the barrier. More batching is not the lever at
that concurrency.

CypherLite is **embedded**. The steady state is one writer, small
statements, occasional bursts; massively concurrent writes are the
exception. Optimizing batch formation optimizes the exception. The common
case has no batch to form, and single-commit latency is on the critical path
of the one working thread. The useful lever there is barrier cost, not batch
size.

Remote and network filesystems can make that barrier substantially more
expensive than local block storage. Some ingestion workloads knowingly prefer
a bounded or checkpoint-sized recovery-point objective to a barrier on every
acknowledged group.

## Decision

`OpenOptions::durability_mode`, `cypherlite --durability-mode`, and
`cypherlite-bolt --durability-mode` select one of three policies. `full`
remains the default.

1. **`full`** — append the WAL record, barrier the accepted group, apply redo,
   then acknowledge. An acknowledged commit is covered by the platform
   barrier.
2. **`everysecond`** — append, apply redo, and acknowledge immediately. A
   background worker barriers all dirty WAL records approximately once per
   second. A host, OS, or storage failure can lose acknowledged commits from
   the latest approximately one-second interval. Scheduling delays, a slow
   barrier, or a storage failure can extend that interval.
3. **`snapshot`** — append, apply redo, and acknowledge without a WAL barrier.
   Only a completed checkpoint is durable. A crash can lose every acknowledged
   commit since the latest completed checkpoint.

`full` uses the platform's cheapest correct barrier:

   | Platform | Barrier |
   | --- | --- |
   | Linux | `fdatasync` |
   | macOS | `fcntl(F_BARRIERFSYNC)` |
   | Windows | `FlushFileBuffers` |

The checkpoint clock defaults to 300 seconds. `--checkpoint-interval-secs N`
tunes the normal `snapshot` loss window; `0` disables only the timed
checkpoint. Explicit checkpoints, clean-close checkpointing, and the 64 MiB /
10,000-commit thresholds remain active.

WASM has no fsync; the debounced whole-graph flush (ADR-0051) is the
durability point, and commit points align with it.

## Alternatives considered

- **Only full durability.** Rejected as the only policy: it charges the full
  network-filesystem barrier to workloads that explicitly accept data loss.
- **Adaptive group-commit wait window** (measure fsync latency, wait up to
  `min(latency/2, ceiling)` when the leader is idle). Rejected *for now*: it
  optimizes batch formation, which matters only under concurrency — the
  exception for an embedded engine. It adds a tuning mechanism whose benefit
  is concentrated where CypherLite is least used. Revisit only if
  embedded workloads turn out to be bursty more often than assumed.
- **Call the relaxed modes “eventual consistency.”** Rejected: reads in the
  live process observe applied commits immediately. The weakened property is
  crash durability, not consistency or isolation.
- **`O_DSYNC` on the append (Linux).** Rejected: adds a barrier per write
  rather than per commit, and is generally slower than an explicit
  `fdatasync` at commit.

## Consequences

- `full` preserves the original host-return guarantee and group commit.
- `everysecond` and `snapshot` can acknowledge records that a crash later
  loses. A caller that observed those records must reconcile to the
  recovered LSN after a crash.
- Recovery trims an incomplete torn tail on any WAL segment and a corrupt
  final frame on the active segment. Corruption before the tail, or a corrupt
  final frame in a closed segment, remains fatal.
- A background `everysecond` barrier failure is sticky: later writes and
  explicit checkpoints return that storage error.
- An intentionally idle leader under low concurrency is a known non-goal
  (see the rejected adaptive window); measure before revisiting.

## Later references

- **Implemented** (2026-09-19, `feat-performance`, commits ff5e6de +
  993306b): `Wal::sync` issues the platform barrier (`durability_barrier` in
  `crates/cypherlite-storage/src/wal.rs`) and `Wal::rotate` skips a redundant
  second fsync after a synced group flush (`Wal::synced` flag), so a group
  flush fires one barrier. Measured (write-only, on-disk, 8 workers):
  sync_all p50 4.178 ms → 0.124 ms, throughput 404 → 8,457 ops/s. Details in
  [`docs/performance-notes.md`](../performance-notes.md) §11.
- **Open question carried to implementation:** `F_BARRIERFSYNC` is an
  ordering barrier; Apple's flush-to-stable-media primitive is
  `F_FULLFSYNC`. The commit barrier currently uses the faster, weaker
  variant. Decide whether the commit barrier should be `F_FULLFSYNC`
  (stronger power-loss guarantee) or stay `F_BARRIERFSYNC` and document the
  weaker guarantee.
- Method, the group-commit table, and the cross-system survey:
  [`docs/performance-notes.md`](../performance-notes.md).
- Group commit already fits the flat-combining pattern (one leader takes the
  whole queue and applies it in one unit); no change to ADR-0019/0014 is
  implied by this ADR.
- ADR-0114 narrows the recovery consequence above: a final frame on the
  active segment is trimmed only when its checksum fails. A checksum-valid
  frame that does not decode fails open.
