# virtual-plan-operators

## Destination

PROFILE and EXPLAIN show a fused MATCH as a breakdown (`NodeIndexSeek → Expand → Expand`) with an
estimate per step and metrics only where truly counted. Nothing is fabricated.

## Scope

- Index-seek-started MATCH clauses first (ADR-0486 seek path), then multi-element MATCH without a seek
  if the first slice lands cleanly.
- Virtual children are derived from the pattern in the plan-tree builder (`plan_node/`), display-only.
  Execution is untouched (ADR-0344: the clause stays the executed unit).
- Wire: virtual children ride the existing `children` array of `PlanNodeWire` with a `fused` flag and a
  `fused_into` operator id. No external clients exist (only first-party tools, all rendering through `relate-formatter`), so no compatibility shim: the wire changes directly and the table renderer adapts in the same change.
- Metrics per virtual node:
  - seek child: `seek_ns` + node reads (already in `ClauseMetrics.phases/reads`);
  - expand children: hop/rel counts only if the spike finds real per-hop counters, else `-`;
  - rows/time/memory: fused parent keeps the clause totals; a child never copies a parent number.
- Estimates: one per step from the ADR-0218 ladder (start = seek count, hop = degree estimate).
- Rung-dependent walks (chain/hub-star/frontier-NFA/batched, per `ClauseMetrics.kernel`): children show
  the logical pattern, flagged `fused`, with the rung named on the parent. No fake per-hop measurements.
- Surfaces updated in the same change: `relate-formatter` (plan_table, speedscope), `relate-http`
  (`v2_plan.rs`), Bolt metadata, MCP agent plan, `relate-eggs` art, FFI/Python/wasm if they expose the
  tree; docs under `docs/api/<surface>/` and the client-reference bundle files per AGENTS.md.
- New ADR extending ADR-0344/0486 (minted late, Status `Proposed`, with a `Later references` note added to
  the old ADRs).

## Out of scope

- Restructuring the executor into an operator tree; per-op wall time; instrumenting the hot path beyond
  counters the spike proves already exist. If honest per-hop metrics need new counters, that is a
  follow-up with its own loadtest comparison (ADR-0344 §3).
- Non-MATCH clauses.

## Decisions (confirmed by the human: children+flag wire shape, logical-pattern fused children, land early)

1. Wire shape: `children` + `fused`/`fused_into`, rejected: a sibling `steps` field (invisible to every
   existing renderer, defeats the purpose).
2. Seek-first rollout, rejected: all MATCH at once (bigger blast radius across surfaces).
3. Honest-metrics rule: no value on a virtual node unless a counter measured it.
4. Rung-dependent shapes keep the logical pattern + `fused`, rejected: collapsing to one node (loses the
   breakdown exactly where users profile).

## Open questions (for `research` nodes)

- Which per-hop counters exist today (`HOP_EXPANDS`, `NEIGHBOR_WALKS`, `StoreReads`, `PhaseTimes`) and
  can they be split by hop index without hot-path cost?
- Do any surfaces assert tree depth or one-node-per-clause (tests, Browser compatibility)?
- Does speedscope export need virtual nodes excluded or nested?
