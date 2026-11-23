---
type: adr
area: engine
claim: efa0ebb6641ecda9fed34173a52ecce9e58fa3ef
date: 2026-11-22
deciders: Relate maintainers
name: 0664-load-threads-and-open-peak-memory
status: accepted
tags:
  - memory
  - open
  - perf
title: A load thread setting and a reported peak for opening a store
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0137-wal-follow]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0517-rename-to-relate]]"
  - "[[adr/0599-adjacency-floor-arena]]"
---

# 0664: A load thread setting and a reported peak for opening a store

## Context

Opening a checkpointed store decodes the snapshot and rebuilds the
adjacency structures from it. Both phases allocate temporary memory beyond
the graph they produce: decoded column batches, the shard partitions of the
rebuild, the label cache. On a large store that transient peak, not the
final graph, is what a host sizes its memory for, and until now it was
neither bounded by a setting nor visible to the host.

The load phases now run on one shared worker pool, and every transient
allocation they make is charged to a per-open tracker. What is missing is
the public surface: a way to choose how many threads an open may use, and a
way to read the peak the tracker saw.

## Decision

1. **One setting for the load thread count.** `OpenOptions::load_threads`
   (`Option<usize>`, default `None`) caps the threads the load phase may
   use. `None` reads `RELATE_LOAD_THREADS`, then falls back to the
   machine's available parallelism. It resolves the way the sibling limits
   do: option, then environment variable, then default.
2. **Loud on bad input, before the lease.** `Some(0)`, and an environment
   value that is zero, not an integer, or not valid UTF-8, fail the open
   with `Error::Semantic` naming the source. The check runs in the open
   preflight, so a failed open leaves no store directory behind. A blank
   variable counts as unset.
3. **The retired spelling is rejected.** A set `CYPHERLITE_LOAD_THREADS`
   fails the open through the same guard as every other retired
   `CYPHERLITE_*` name ([ADR-0517](./0517-rename-to-relate.md)), naming
   `RELATE_LOAD_THREADS`.
4. **The peak is read from the handle.** `Database::open_peak_transient_bytes`
   returns the most transient bytes the latest load held at once. The open
   runs inside a load scope and latches the tracker's peak when it ends. It
   is `0` for an empty `:memory:` store, which loads nothing, and positive
   for a populated, checkpointed open.
5. **A follower's gap reload is a load too.** The Follow reload
   ([ADR-0137](./0137-wal-follow.md)) runs in a scope with the same thread
   count and replaces the latched value with its own peak, so the number
   always describes the graph the handle currently serves.
6. **A custom slot-pinned pool, not a general one.** `relate-core` owns
   the `LoadPool` that runs the load phases. Its tasks borrow the caller's
   stack, so a scoped borrow needs a guarantee no off-the-shelf pool gives
   here: `WaitOnDrop` blocks until every sent job has finished, including
   while the caller unwinds, so no job outlives what it borrowed. Tasks run
   on slots `0..k` only, where `k` is the open's thread count, and the
   slots persist and are reused across opens. The pool therefore holds at
   most as many worker threads, and their thread-local heaps, as the
   largest `k` any open has asked for, a process-wide bound that does not
   grow with the number of stores opened.
7. **A measurement, not a limit.** The peak is reported, never enforced:
   the live graph cap ([ADR-0042](./0042-graph-memory-limit.md)) is
   unchanged and does not count transient bytes.
8. **An over-wall build stays in the delta tier.** The sealed arena's byte
   offsets are `u32`, so its streams stop at 4 GiB ([ADR-0599](./0599-adjacency-floor-arena.md)).
   A wholesale build that would cross that wall keeps each run that does
   not fit in the delta tier, the same as a run at the span-length wall,
   and the open returns normally: it never panics on the wall. Each shard's
   stream checks the wall on its own bytes; when the shards fit alone but
   their sum does not, the side is built again with every run in the delta
   tier. The graph reads back the same either way.

## Alternatives considered

- **A general-purpose thread pool.** Rejected: it would not give the
  scoped-borrow guarantee or the process-wide bound on worker slots that
  decision 6 sets, and would add a dependency to `relate-core`.

- **Read the thread count only from the environment.** Rejected: an
  embedding host should not have to mutate its process environment to size
  one open.
- **A CLI flag.** Rejected for now: the setting is advanced, and the
  environment variable covers the CLI and `serve`. A flag can follow if a
  need appears.
- **Clamp a bad value to the default.** Rejected: a silent fallback hides a
  misconfigured limit, the rule every memory knob already follows.
- **Report the peak per phase.** Rejected: one number answers the sizing
  question, and a breakdown is a diagnostic that can be added later without
  changing it.
- **Enforce the peak as a cap.** Rejected: refusing an open halfway through
  a decode is a different feature, with its own failure and retry story.

## Consequences

- A host can bound the parallelism, and so the transient memory, of an
  open, and can read what the open actually used.
- One more open-time setting, documented beside the other limits.
- `mimalloc` is a dev-dependency of the `relate` crate only, for the
  `open_peak_rss` regression gate. It is a new native dependency, so
  AGENTS.md R6 asks for the rationale here. The gate opens and closes many
  stores on four threads and checks the process's peak RSS; the system
  allocator retains freed transient memory and would hide the open's peak,
  so the gate installs mimalloc to reproduce a host that embeds it. No
  Relate release binary links it, and it is never a normal dependency.
- The reported peak covers the charged allocations only; it is a lower
  bound on the process's true transient footprint.
