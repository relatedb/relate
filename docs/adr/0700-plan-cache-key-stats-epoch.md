---
type: adr
area: engine
claim: 1e5bfaf02e3165bed439560831ec453561f12142
name: 0700-plan-cache-key-stats-epoch
status: proposed
tags:
  - executor
  - planner
  - perf
title: A cached plan that read the per-key statistics replans when they change
REFERENCES:
  - "[[adr/0503-template-keyed-plan-cache]]"
  - "[[adr/0508-planner-stats-consult-memo]]"
  - "[[adr/0696-per-key-value-statistics]]"
---

# 0700: A cached plan that read the per-key statistics replans when they change

## Context

[ADR-0503](./0503-template-keyed-plan-cache.md) stamps every cached plan with the store's stats generation, which
advances at every applied commit; an entry is reused only while nothing
has committed since, and replans from its cached AST otherwise.

[ADR-0696](./0696-per-key-value-statistics.md) adds per-`(label, key)` value statistics that are built lazily: a
consult of an unbuilt label answers `None` and requests a build, a later
statement's completion serves it, and a label that has seen enough writes
goes stale and answers `None` again. None of those transitions is a
commit. A plan priced while a label was unmeasured (from the planner's
default table) therefore stays in the cache in a read-only workload for as
long as nothing commits, though the statistics that would price it better
now exist.

## Decision

1. **An epoch for the statistics.** The graph keeps a monotonic counter,
   `key_stats_epoch`, read with one atomic load. It moves when what a
   consult of the statistics can answer changes: a label's statistics are
   published (and any other label's keys evicted to make room), a label
   turns stale (once per transition, not per write), or the statistics are
   cleared. It is bumped after the published map changed, and a
   publication that leaves every answer as it was (every key declined)
   does not move it.
2. **A plan carries the epoch only if it read the statistics.** The
   plan-time adapter records whether planning consulted `key_stats` or
   `equality_estimate`, whatever the answer. The epoch is read before
   planning, under the publication read guard, and stored on the cache
   entry only when a consult happened (`None` otherwise). A publication
   that lands while the plan is built moves the epoch past the stamp, so
   the entry replans once; the reverse cannot happen.
3. **One freshness rule.** An entry is fresh when its commit generation is
   current and, if it carries an epoch, the epoch is current. A plan that
   never consulted the statistics ignores the epoch, so another label's
   statistics changing never replans it. A stale entry takes the existing
   replan-on-mismatch path and reports the same plan-cache outcome,
   `replan-generation-mismatch`: the outcome means the stats a plan was
   priced from moved, whichever kind moved. The replan stores its own
   stamp, so it hits afterwards.
4. **The epoch is global, not per label.** A stamped plan replans when any
   label's statistics change. Per-label tracking would need the plan to
   record which labels it consulted; the cost of the coarse rule is bounded
   (a change per label per build or ageing cycle) and falls only on plans
   that read the statistics.

## Alternatives considered

- **Advance the commit generation when statistics publish.** Replans every
  cached plan, including the ones that never read the statistics, on every
  publication. Rejected.
- **Never replan, rely on the next commit.** A read-only workload after a
  load would keep default-priced plans indefinitely. Rejected.
- **Per-label epochs.** More precise, and needs a per-plan set of consulted
  labels carried through the cache. Deferred until the coarse rule shows up
  as churn.

## Consequences

- The first statement over a label plans from the default table (the
  consult is unmeasured), the statement-completion pump builds the label,
  and the same statement text replans once and hits afterwards.
- Nothing in the planner's ordering consults the statistics yet, so no
  cached plan carries a stamp today and no behaviour changes until a
  planner rung consults them; the cache and epoch are in place before it
  does.
- A plan that read the statistics replans once per statistics change
  rather than once per commit-free interval forever.
