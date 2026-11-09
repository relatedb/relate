# ADR-0541: Resolve label, relationship-type and property-key names to token ids once per plan

Status: Accepted
Claim: 8591f1611b539fc1d47ee18bf6d77bfdd68fb45a

## Context

Plans name labels, relationship types and property keys as strings. The
store interns all three into one append-only string-to-id table, and the
executor looks names up in it at execution time — once per stage on the
contracted kernels (ADR-0473), once per step for relationship types, but
once per candidate and name on the walkers' node admission and property
reads (ADR-0355 kept that cost deliberately). Profiles of expansion-heavy
queries still show string hashing and interner probes inside the
per-element loop.

The planner has the same gap in the other direction. Its statistics seam
takes names, so every cardinality, index and degree probe resolves the
name again inside the store before it can look anything up by id.

Two facts make an earlier resolution point sound. The interner is
append-only: an id, once handed out, never changes or disappears for the
life of the store. And the plan cache is per store and re-plans from the
cached statement whenever any commit has landed since the plan was
compiled (ADR-0503). A plan therefore never outlives the store state it
was resolved against, and a name that was absent at planning time cannot
become present without a commit that forces a re-plan.

The one exception is a write statement's own overlay: names a statement
creates are interned only at commit, so within that statement they exist
only as strings.

## Decision

1. **Resolve names to token ids once per plan compile.** A resolution
   step runs at the start of planning, over every label, relationship
   type and property key the statement mentions, against the store's
   interner. The cached, store-independent statement tree stays id-free;
   ids live only in the compiled plan.
2. **One token space, one resolver seam.** The id type and a narrow
   resolver trait live in the lowest shared engine crate; the store
   implements the trait, and the planner receives it beside its statistics
   seam. The planner never depends on the executor.
3. **A resolved name keeps its text.** Operators carry the name and an
   optional id. Absent means "no committed element carries this name":
   the executor fails the check without a store read, except where a
   write overlay is present, in which case the string arm answers. The
   text stays for plan display and error messages.
4. **Statistics and index probes take ids.** The planner's statistics
   methods gain id-keyed forms so a cardinality or index decision costs no
   string lookup. Name-keyed forms remain for callers that have no plan.
5. **Executors consume ids without re-resolving.** Per-stage resolution in
   the kernels and per-hop resolution in the walkers collapse into reading
   the plan's ids; no executor path interns a name from a plan.

## Alternatives considered

- **Keep resolving per stage in the executor** (ADR-0473 and the
  per-hop hoist). Cheaper than per candidate, but each executor
  re-implements the hoist, the planner still probes by string, and the
  design cannot reach walker paths that have no stage. Rejected: it fixes
  one consumer at a time.
- **A separate pass that rewrites the statement tree with ids before
  planning.** Gives the planner ids from its first step, but the rewritten
  tree is store-bound while the cache keeps the statement store-independent
  and re-plans it with fresh statistics. Rejected: it forces a second
  tree or re-resolution on every re-plan.
- **Resolve lazily on first use inside each operator.** No up-front
  pass, but resolution moves into execution, needs interior mutability in
  shared cached plans, and gives the planner nothing. Rejected.
- **Make the interner hash cheaper** (a keyless fast hash). Barred by
  ADR-0355's rule against it for keys a client can choose, and it would
  still pay a hash per candidate. Rejected.

## Consequences

- The per-candidate string hash and interner probe leave the walker hot
  path; planner statistics become id lookups.
- The plan IR gains a token type and the planner and statistics seam grow
  a resolver parameter: a cross-crate change that lands in slices (type
  and seam, then planner, then each executor consumer).
- Plan size grows by one small id per name; the plan cache's byte
  accounting must include it.
- A plan is now valid only for the store and generation it was resolved
  at. Any new path that reuses a plan across stores or generations must
  re-resolve; the cache already guarantees this for the current paths.
- Write statements keep a string arm for overlay-created names, so that
  code path stays and needs its own tests.
- Plan-display and `EXPLAIN` output must remain name-based and
  byte-identical.

## Later references

Decision 4 landed as one method per statistics probe taking the name beside its
token, not separate id-keyed forms: the store impl reads the id for a resolved
token, answers an absent one without a store read, and falls back to the name
only for an unresolved one.
