### Changed

- Cached plans now survive commits that do not touch them: a statement's plan
  is re-planned only when a schema change or a graph replacement lands, when a
  label, relationship type or property key the plan reads drifts past twice its
  planned cardinality, or when a drifted plan's measured execution work degrades
  past twice its baseline. Previously every applied commit invalidated every
  cached plan.
- The `planCache` result metadata every surface reports (CLI, Bolt, HTTP, Rust)
  spells its re-plan outcome `replan` instead of `replan-generation-mismatch`,
  and the value now means one of four causes rather than any commit: a schema
  change or graph replacement, a commit that created an absent name the plan
  resolved, a degraded plan grade, or the per-key value statistics the plan
  read changing (ADR-0700).
