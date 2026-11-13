# ADR-0335: Unbound variables fail with UndefinedVariable instead of evaluating to null

Status: Proposed
Claim: 5a3dca3f02a7be3b40e3f047cc98ccf4e536b5ef

## Context

The executor's expression evaluator fabricates `null` for a variable the
row does not bind (`cypherlite-executor/src/eval/mod.rs`):

```rust
Expr::Variable(n) => Ok(row.get(n).cloned().unwrap_or(Value::Null)),
```

and the map-projection variable item repeats it
(`MapProjectionItem::Variable { name }`). AGENTS.md bans exactly this
`unwrap_or(Value::Null)` shape, and ADR-0308 removed the identical silent
`null` from the `Expr::Parameter` arm two arms above.

The plan-time binding walks (`cypherlite-front`'s scope validation and the
planner's binding) reject an undefined variable before execution, so the
evaluator arm is only reachable for expressions the plan walk cannot see:
the `SHOW … WHERE` predicates and other catalog expressions that lower at
run time. There the fabricated `null` leaks user-visibly —
`SHOW INDEXES WHERE nosuch = 1` filters every row and returns an empty
result instead of Neo4j's undefined-variable error — and it hides a
planner/executor binding bug behind a wrong answer.

## Decision

A variable the evaluated row does not bind raises the engine's
undefined-variable error, never a `null` value.

1. **One vocabulary.** `cypherlite_core::Error::undefined_variable(name)`
   builds the error, rendering byte-identically to the front-end's
   scope-check error: `syntax error: UndefinedVariable: variable '<name>'
   not defined` (the front-end raises the same
   `EngineError::semantic(ErrorKind::UndefinedVariable, …)` text, which
   `lower_statement` maps to `Error::syntax`). The TCK bridge classifies it
   as `UndefinedVariable` through the same message scan as the plan-time
   error.
2. **Both arms.** `Expr::Variable` returns
   `Error::undefined_variable(name)` on a row miss;
   `MapProjectionItem::Variable` does the same instead of inserting
   `Value::Null`.
3. **Run-time-lowered predicates surface it.** `exec/catalog.rs`'s
   `project_catalog` WHERE filter propagates the evaluator's error instead
   of swallowing it (`matches!(eval(…), Ok(Value::Bool(true)))` treated
   every error as "row filtered out"). A `SHOW … WHERE` predicate that
   names no yielded column now fails the statement with the typed error.
4. **A bound name is unchanged.** The arm only changes a row miss; a
   variable bound to `null` still evaluates to `null`, and the row-sharing
   fast path (`eval_operand`, ADR-0170) is untouched.
5. **The two sites with deliberately incomplete rows bind their names
   explicitly.** Two executor paths evaluate expressions against rows
   that are legitimately missing names, and Neo4j reads those names as
   `null` there; each site now states that binding itself instead of
   relying on the evaluator's fallback:
   - a quantification predicate sees a non-local singleton declared later
     in the pattern as `null` (Neo4j-verified), so
     `gpm/qpp.rs` binds the predicate's unbound names to `null` for the
     evaluation row;
   - a void `OPTIONAL CALL` over a zero-row driving table yields one unit
     row on which every variable in scope is `null`
     (Neo4j-verified), so `dispatch.rs` seeds that row with the scope's
     names bound to `null`.
   A name that is neither in scope nor bound at one of these sites is
   still a typed error.

## Alternatives considered

- **Keep `null` and document it.** Rejected: it is the silent fallback
  AGENTS.md bans, it diverges from Neo4j (`Variable '<name>' not defined`
  is a compile error there), and it hides binding bugs.
- **`expect` stating the planner invariant in the evaluator.** Rejected:
  the arm is reachable from run-time-lowered catalog predicates, so a
  panic would be a crash on query input; the typed error is the honest
  result there.
- **Only add a binding check in the SHOW WHERE path, keep the `null` in
  `eval`.** Rejected: the same fabricated `null` would remain for map
  projections and any future run-time-lowered expression; the policy
  belongs to the evaluator, stated once.
- **A new typed `Error` variant for the unbound name.** Rejected for now:
  the front-end already spells this failure as a syntax-class error with
  the `UndefinedVariable` token, and a new variant would need its own
  Display/classification/surface mapping for no wire-visible difference.
  The `Error::undefined_variable` helper keeps the one spelling.

## Consequences

- `SHOW … WHERE <unbound>` now fails instead of returning an empty result.
  Every surface reports the syntax-class error through the existing
  classification; no surface mapping changes.
- A planner/executor binding bug now fails loudly instead of fabricating
  `null` rows.
- The two incomplete-row sites (QPP predicates, the void optional-call
  unit row) keep their Neo4j-verified results
  through the explicit null binding, so the frozen CIP-eval identities do
  not move.
- Tests: `eval/mod.rs` unit tests pin the error for both arms;
  `cypherlite/tests/undefined_variable.rs` pins `SHOW INDEXES WHERE nosuch
  = 1`, the void optional-call unit row and the later-declared-singleton
  read; the TCK (3892 of 3892) and CIP-eval keep their gates.
