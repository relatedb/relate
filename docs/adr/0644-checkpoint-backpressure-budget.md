---
type: adr
area: engine
claim: 8ea7c7f04d3312d73464db5270cedfca389e4621
date: 2026-11-21
deciders: the maintainers
name: 0644-checkpoint-backpressure-budget
status: proposed
tags:
  - checkpoint
  - wal
  - perf
title: Checkpoint back-pressure is bounded by the closed-segment backlog
REFERENCES:
  - "[[adr/0020-segmented-wal-concurrent-checkpoint]]"
  - "[[adr/0643-checkpoint-cuts-before-the-dump]]"
---

# 0644. Checkpoint back-pressure is bounded by the closed-segment backlog

## Context

[ADR-0643](./0643-checkpoint-cuts-before-the-dump.md) took a checkpoint's dump
off the commit path: a checkpoint freezes its cut under the publication guard,
then dumps beside commits, so a commit pays at most the cut's rotate barrier
instead of the whole dump.

That leaves the WAL's closed-segment backlog unbounded. A checkpoint's cut
rotates and closes the open segment; commits running during the dump keep
appending, and each one that crosses a rotation threshold closes another
segment. Only a checkpoint's recycle deletes closed segments, and the running
dump recycles only up to its own cut — the segments closed after the cut wait
for the next checkpoint. A dump held open while a fast writer keeps crossing
thresholds therefore grows the backlog by one segment per threshold with
nothing to bound it, and each segment holds up to a rotation threshold's
records. The WAL on disk then grows with the dump's duration instead of with
the data.

## Decision

A commit group waits for the in-flight checkpoint only when the closed-segment
backlog is over its budget: a fixed number of closed segments (four). Under the
budget a dump costs a commit nothing; over it the group waits for the running
checkpoint to finish its dump and recycle, then starts.

- The wait holds no lock — the checkpoint's cut needs the publication guard a
  waiting commit has not taken — so it cannot deadlock the dump it waits for.
- The gate runs before the group's publication and WAL admission, so a batch
  that waits is neither applied nor refused; a store that fails its health
  check while the writer waits fails the batch like every other pre-apply
  refusal.
- One segment is capped by the rotation threshold, so the segment count bounds
  the backlog's bytes too.

## Alternatives considered

- **Leave the backlog unbounded.** Rejected: the WAL's disk use then scales
  with the dump's duration times the write rate, with no ceiling.
- **Wait for the checkpoint's cut instead of its recycle.** Rejected: the cut
  is already short, and the backlog grows during the dump — the recycle is what
  bounds it.
- **Chunk or interleave the dump so it recycles as it goes.** Rejected: that
  rewrites the checkpoint's publication, where a bound on the writer needs only
  a gate.
- **Bound by closed bytes instead of segments.** Rejected: the segment count
  bounds the bytes within the threshold factor, and the count is the fact the
  WAL already keeps.
- **Refuse the commit over the budget.** Rejected: a refusal is a new error
  the writer must handle; waiting is the back-pressure the WAL needs, and the
  commit resumes from it.

## Consequences

- A writing load that outruns the dump is back-pressured at four closed
  segments instead of growing the WAL without bound; every commit under the
  budget still pays no wait.
- The budget is a compile-time constant, not an `OpenOptions` knob.
- A writer waiting over the budget waits for the whole remaining dump; the
  group behind it finds the backlog recycled or the next checkpoint due.
