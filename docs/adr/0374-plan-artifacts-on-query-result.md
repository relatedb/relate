---
type: adr
area: engine
claim: d21dbc086232300867954d1f29338ca5b83a1503
name: 0374-plan-artifacts-on-query-result
status: proposed
tags:
  - observability
  - result-metadata
title: Plan artifacts on `QueryResult` as one structural fact
REFERENCES:
  - "[[adr/0344-query-plan-node-tree-and-per-operator-metrics]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
  - "[[adr/0363-query-notifications]]"
---

# ADR-0374: Plan artifacts on `QueryResult` as one structural fact

## Context

[ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) decision 2 pinned the plan of an `EXPLAIN`/`PROFILE` statement on
the result as `result.plan: Option<Box<PlanNode>>`, with each caller
destructuring the `Option` to reach the root. [ADR-0363](./0363-query-notifications.md) decision 7 then pinned
the notification list beside it: `pub notifications: Vec<Notification>` on
`QueryResult` (`None`/empty on a plain run). Both decisions were written when
the plan tree and the notification list were separate lines of work.

The two facts are not independent. [ADR-0363](./0363-query-notifications.md) decision 3 derives notifications
exactly where a `PlanNode` tree is built (`plan_tree` for `EXPLAIN`,
`profiled_tree` for `PROFILE`) and nowhere else; a plain run builds no tree
and derives no list. The sibling-field shape cannot express that dependency:
`{ plan: None, notifications: vec![…] }` and
`{ plan: Some(tree), notifications: vec![] }` both type-check, so every
consumer must know the invariant and every construction site can silently
break it. The plan tree is also heavy and rarely populated, so `None` must
stay pointer-sized and the tree cannot move onto `QueryResult` unboxed.

## Decision

1. **One field, one artifact.** `QueryResult.plan` becomes
   `Option<Box<PlanArtifacts { root, notifications }>>`: `PlanArtifacts` is
   the public record exported at the `cypherlite` root, carrying the
 [ADR-0344](./0344-query-plan-node-tree-and-per-operator-metrics.md)/[ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md) tree in `root` and the [ADR-0363](./0363-query-notifications.md) list in `notifications`.
   The field keeps its name for the `is_some`/`is_none` classification every
   surface already reads. The artifact is `Some` exactly when the statement
   was `EXPLAIN` or `PROFILE`, `None` on every plain run — and on a `PROFILE`
   whose post-run estimates read is refused because the store was poisoned
   while the statement ran (the executed rows survive that refusal
   un-enriched, never as an error).
2. **Co-variance is unrepresentable.** Notifications exist only as a field
   of the artifact, so a result can hold a notification list without the
   tree it was derived from only by constructing the artifact — i.e. never.
   There is no second `Option` or empty-vector sentinel to keep in sync.
3. **One construction path.** `QueryResult::from_exec` leaves `plan` `None`;
   `QueryResult::with_plan(root, notifications)` is the only plan-carrying
   construction, so `EXPLAIN` and `PROFILE` attach the tree and the list
   together, in one call, and a plain run never allocates either.
4. **Read accessors.** `QueryResult::plan_root() -> Option<&PlanNode>` and
   `QueryResult::notifications() -> &[Notification]` are the public reads.
   The first replaces every `result.plan.as_deref()` root destructure
 ([ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) decision 2's call sites); the second is the empty slice on a
   plain run.
5. **Surfaces are unchanged.** This is a Rust structural decision only:
   Bolt's ending summary, HTTP's `queryPlan`/`profiledQueryPlan`, the C ABI
   JSON handle, Python's attribute and the CLI's output modes read the same
 facts through the accessors ([ADR-0363](./0363-query-notifications.md) decisions 9-12). This ADR replaces
 [ADR-0363](./0363-query-notifications.md) decision 7's two sibling fields and narrows [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) decision 2's
   `Option<Box<PlanNode>>` shape; every other decision of both ADRs stands.

## Alternatives considered

- **Keep the two sibling fields** (`plan: Option<Box<PlanNode>>` +
 `notifications: Vec<Notification>`, [ADR-0363](./0363-query-notifications.md) decision 7) — rejected: it
  makes the invalid pairs `plan: None, notifications: non-empty` and
  `plan: Some(tree), notifications: empty` representable and constructible,
 so the [ADR-0363](./0363-query-notifications.md) decision 3 invariant lives only in prose and every surface
  must re-check it.
- **A second mirror field on `QueryResult` beside the artifact**
  (`pub plan: Option<Box<PlanArtifacts>>` + `pub notifications:
  Vec<Notification>`) — rejected: parallel state over one fact; the mirror
  can be updated independently of the artifact it copies, and a reader
  cannot tell which one is authoritative.
- **Keep `Option<PlanNode>` and move only the list into a method that
  re-derives it** — rejected: re-derivation walks the tree and the catalog
 per call ([ADR-0363](./0363-query-notifications.md) decisions 3 and 5), and a surface that calls the method
  twice gets two independently derived lists; the derived list is a build
  product, not a query.
- **Nest an `Option` inside the artifact** (`root: Option<PlanNode>`) —
  rejected: `EXPLAIN`/`PROFILE` always build a root, so the only absence the
  model needs is "no artifact"; a second absence axis admits an empty
  artifact no producer can build.
- **Leave the artifact unboxed** (`plan: Option<PlanArtifacts>`) —
  rejected: the tree plus the list is large, and surfaces buffer whole
  results (the Bolt row cap); `None` on the common plain run must stay
  pointer-sized.

## Consequences

- A construction site that builds a notification list without the plan tree
 no longer compiles; the compiler enforces [ADR-0363](./0363-query-notifications.md) decision 3 instead of
  review.
- `QueryResult` is one field lighter and a plain run allocates nothing for
  either fact; an `EXPLAIN`/`PROFILE` run allocates one `PlanArtifacts`.
- Call sites that destructured `result.plan` for the root move to
  `plan_root()`; callers that need the artifact itself (the serializers that
  project both facts) keep reading the public `plan` field.
- `docs/api/rust/spec.md` and the CHANGELOG describe the artifact shape and
  the two accessors; Bolt, HTTP, the C ABI, Python and the CLI keep their
  wire contracts.
