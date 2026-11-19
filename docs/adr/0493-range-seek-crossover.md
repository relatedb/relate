---
type: adr
area: engine
claim: 1a585ccaaa918dd2e523745fdfae83914a386417
name: 0493-range-seek-crossover
status: proposed
tags:
  - index
  - planner
title: Range-seek crossover — low-selectivity windows serve from the label scan
REFERENCES:
  - "[[adr/0486-range-index-starts]]"
---

# 0493: Range-seek crossover — low-selectivity windows serve from the label scan

## Context

[ADR-0486](./0486-range-index-starts.md) priced range-index starts as exact `Estimate::Measured` counts
paying no scan floor, ranked above CONTAINS — the seekable side must win
the anchor. Measured on a 1.8M-relationship social-network graph (LABS-854, 2026-11-03): the range
seek's per-candidate cost is **~1.36 µs** (each candidate materializes a
full node — `lookup_range_seek` → `LiveNode::to_node` → `CompactProps::
to_map`) against the label scan's **~0.26 µs** per node, ~5.2×. The
break-even selectivity is therefore ~19%: `WHERE n.age >= 18` (56,802 of
100,000 users, 57%) seeks in **77 ms** where the scan plus the residual
filter answers in **26 ms** — and the executor's candidate funnel
(`node_candidates` → `Seek::Range` serve) serves the first servable seek
unconditionally, so the low-selectivity seek has always *executed*
(pre-0486 it did so invisibly; 0486's ladder made the plan name it).
Today both consumers pick the seek at any selectivity, and the plan,
the ladder, and the funnel agree on the wrong choice for
majority-selective windows.

## Decision

One crossover policy in one home — the planner's ladder module
(`cypherlite-planner/src/order/mod.rs`), imported by the executor's
funnel: **a range window at or above one quarter of its label's rows,
above a small absolute floor, is served by the label scan with the
residual filter, not the range seek.**

```text
range_seek_wins(window, label_rows) =
    window * 4 < label_rows  ||  window <= RANGE_SEEK_WIN_FLOOR (256)
```

- The **population** the window is compared against is the scan the
  seek would replace: the pattern's label conjunction — the smallest
  label it names — on BOTH sides (the planner ladder's range level and
  the executor funnel's serve), so plan narration and execution agree
  on multi-label nodes. `pattern_label_rows` sizes an absent label at
  0 (an empty scan — declining is the same rule applied to a
  measurement); a window or label population that cannot be measured
  keeps the seek (no measurement, no gate). The planner gates each
  candidate before taking the minimum, so a gated-out smallest window
  cannot hide a passing larger one.
- The **floor** keeps anchored point reads and small `IN` windows
  (windows of 1 to a few ids) on the index regardless of tiny labels,
  where the absolute cost difference is noise.
- The **fraction** (1/4) sits above the measured 19% break-even with
  margin, so the region between break-even and the crossover loses only
  the small per-row delta, never the 3× of the 57% case.
- Both consumers consult the one composition — `crossover_scan_rows`
  (the population fold) gated by `range_seek_gated` over
  `range_seek_wins`, all planner-owned: the planner's `best_range`
  level declines a window that fails the policy (the ladder falls
  through to CONTAINS / label rows), and the executor's `Seek::Range`
  serve skips it (the funnel falls through to the scan arm). The plan
  tree's seek narration inherits the gate through `best_seek`, so
  EXPLAIN/PROFILE keep naming the start that runs.

## Alternatives considered

- **Cost-based per-row constants** (`seek_row_cost × window` vs
  `scan_row_cost × label`) — rejected: calibrated per-row constants
  drift with the seek's implementation (LABS-853's id-only seek moves
  the break-even ~5×); a fraction plus a floor is auditable, measured
  once, and revisitable in one place.
- **Gate only the planner ladder, leave the funnel's opportunistic
  seek** — rejected: the funnel serves the first servable seek
  regardless of the ladder, so execution and the narrated plan would
 disagree again — exactly the invisibility [ADR-0486](./0486-range-index-starts.md) removed.
- **No gate; fix the seek's cost instead** (LABS-853 lazy entities) —
  rejected as the whole answer: even an id-only seek at 57% selectivity
  walks more candidates than the scan visits nodes; LABS-853 moves the
  crossover up but a crossover still exists.

## Consequences

- Majority-selective range predicates (`age >= 18`-class) scan and
  filter; minority-selective ones (`age > 50`, 1.4%) still seek. On
  the same graph, `aggregate_with_filter` drops from ~93 ms to the
  scan+aggregation shape of `group_by_age` (~42 ms).
- The seek narration and PROFILE stay truthful: the plan names the scan
  that runs, for the same reason it names the seek that runs.
- The crossover constants are measured facts with a named revisit
  condition: LABS-853's id-only seek is expected to raise the fraction.
- [ADR-0486](./0486-range-index-starts.md)'s ladder ranking stands unchanged for windows below the
 crossover; a **Later references** note on [ADR-0486](./0486-range-index-starts.md) records this gate.
