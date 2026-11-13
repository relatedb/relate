# ADR-0511: The boolean-requiring sites raise the typed `InvalidArgumentType` — Neo4j's TypeError class

Status: Proposed
Claim: 19376b050f5397470fb8d0a25f81c513b5680284

## Context

LABS-400 (with LABS-913 item 2). ADR-0003 pinned the engine's error
vocabulary to openCypher TCK names and stated the contract: changing a
message is not a breaking change; changing a kind is. Four
boolean-requiring rejection sites in the engine still raise
`Error::syntax("InvalidArgumentType: …")`, so their kind is the
**SyntaxError** class (GQLSTATUS `42N06`, Neo4j code
`Neo.ClientError.Statement.SyntaxError`) even though the detail token
spells `InvalidArgumentType`:

- `cypherlite/cypherlite-executor/src/eval/mod.rs` — `bool_or_null`'s
  non-boolean arm. This is the one runtime policy every requiring site
  reads: the list-quantifier `WHERE` predicate (`eval/quantifier.rs`,
  context `quantifier WHERE`) and the `WHEN` predicate
  (`exec/call/conditional.rs`, context `WHEN predicate`). The four boolean
  operators layer the deprecated list/path conversion above it through
  `bool_operand` (ADR-0458 §7), so their residual non-boolean arm is the
  same site.
- `cypherlite/cypherlite-planner/src/expr_check.rs` — `check_pred`'s
  statically non-boolean predicate (`WHERE 1`, context-free: "predicate
  requires a boolean") and its entity/path variable in a predicate
  position (`WHERE n` where `n` is a node, relationship or path: "an
  entity or path is not a boolean predicate").
- `cypherlite/cypherlite-planner/src/expr_check.rs` — a statically
  non-boolean `AND`/`OR`/`XOR` operand (`RETURN 1 AND true`: "{name}
  requires a boolean"). The front-end's literal-operand validator
  (`cypherlite-front/src/validate/static_types.rs`,
  `require_boolean_literal_operand`) rejects the same queries earlier in
  the full pipeline; the planner row is the backstop for expressions the
  static-literal validator declines.

Neo4j reports every one of them as a **type error**, not a syntax error.
Probes against `neo4j:2026.09-enterprise` via `cypher-shell` (2026-11-04):

| Statement | Neo4j cause | Top-level GQLSTATUS |
| --- | --- | --- |
| `MATCH (n) WHERE 1 RETURN n` | `22NB1` — type mismatch, expected to be BOOLEAN but was 'INTEGER' | `42001` |
| `RETURN 1 AND true` | `22NB1` — type mismatch, expected to be BOOLEAN but was 'INTEGER' | `42001` |
| `MATCH (n) WHERE n RETURN n` | `22N27` — invalid entity type: "Invalid input 'NODE' for n. Expected to be BOOLEAN." | `22G03` |
| `WITH [1,2] AS xs RETURN all(x IN [1,2] WHERE xs) AS r` | `22N37` — invalid coercion, `Cannot coerce LIST to BOOLEAN.` | `42001` |

The out-of-repo spec harness's verified mapping (`clspec/errors.py`,
`GQL_TO_TCK`) accepts the **TypeError** class for all four causes —
`22NB1 → TypeError`, `22N27 → {TypeError, ArgumentError}`,
`22N37 → {TypeError, SyntaxError}`, `22G03 → {TypeError, SyntaxError,
ArgumentError}` — while `42N06` (the code these sites emit today) has no
entry, so the harness treats it as `metadata-unverified` rather than
enforcing it. Today's SyntaxError class is therefore invisible to the
harness (it passes vacuously), not correct.

The typed constructor already exists and is already classified: the user
decision on LABS-400 is "go with the neo4j type error".
`Error::invalid_argument_type(detail)` (`cypherlite-core/src/error.rs`)
renders `type error: InvalidArgumentType: <detail>` and classifies
through the one ABI v2 table (ADR-0502 §2,
`cypherlite-error/src/classification.rs`) as
`Neo.ClientError.Statement.TypeError` / GQLSTATUS `22G03`. ADR-0175
already uses it at plan time for the float-literal rejections and records
the same phase deviation this ADR accepts below.

## Decision

Every boolean-requiring site that raises the stringly `Error::syntax`
today raises `Error::invalid_argument_type` instead, in one change —
executor and planner together, so no surface can report one class for a
runtime value (executor) and another for a literal the planner sees:

1. `bool_or_null`'s non-boolean arm (`quantifier WHERE`, `WHEN
   predicate`, and the boolean operators' residual arm through
   `bool_operand`);
2. `check_pred`'s statically non-boolean predicate;
3. `check_pred`'s entity/path variable in a predicate position;
4. the statically non-boolean `AND`/`OR`/`XOR` operand — scoped to the
   operands the constant-fold rewriter keeps, so an operand it folds away
   (`1 OR true`) is exempt exactly as variable resolution exempts it.

The detail text keeps its `… requires a boolean` / `… is not a boolean
predicate` wording. Only the class prefix of the rendered message changes
(`type error:` in place of `syntax error:`); per ADR-0003 the *kind* is
the contract and the message text stays informal.

Site 3 (the entity/path predicate) converges with the rest: its own
message already names a boolean-predicate requirement, and Neo4j reports
`22N27` under `22G03` for `MATCH (n) WHERE n RETURN n` — the TypeError
family, not the syntax family.

One sibling in the family does **not** converge, and is recorded here
rather than silently left inconsistent: the front end's literal-operand
validator (`require_boolean_literal_operand`,
`cypherlite-front/src/validate/static_types.rs`) rejects `RETURN 1 AND
true` before the planner row does, by building an
`EngineError::semantic(ErrorKind::InvalidArgumentType, "Type mismatch:
expected Boolean but was …")` — already a TypeError *class* as an
`EngineError`, but flattened to `Error::syntax(error.to_string())` at the
lowering boundary (`cypherlite-front/src/lower/mod.rs`), which restores
the SyntaxError class at the ABI. Converging it means changing that
`EngineError` → `Error` boundary, and it is blocked by a TCK phase pin:
`Boolean1-4` scenarios `[8]`/`[4]` assert the error `at compile time`,
while `Error::InvalidArgumentType` classifies at the **Runtime** stage
(ADR-0502 §2) and the TCK bridge maps the variant to a runtime engine
error, so the pins would fail. A compile-time TypeError shape needs its
own stage-carrying representation — a follow-up decision, not a silent
re-pin. The observable effect today is that `RETURN 1 AND true` still
reports `kind: "syntax"` from the CLI while `MATCH (n) WHERE 1`, `MATCH
(n) WHERE n` and the runtime quantifier/`WHEN` rejections report `kind:
"type"`.

Scope is the boolean family only. The other `InvalidArgumentType` sites
keep their current constructors: `length()`/`properties()`/`type()`/
`size()` argument checks, `IN`'s list requirement, property access on a
non-map, the `DELETE` target shape, and the `SKIP`/`LIMIT` integer
checks. Their convergence, if any, is a separate decision.

## Alternatives considered

1. **Keep `Error::syntax` and rely on the `InvalidArgumentType` detail
   token.** Rejected: the class is what Bolt's failure code, the ABI v2
   error shape and every driver's exception mapping read; the detail
   token only rides the message text, so clients see a SyntaxError for a
   query Neo4j reports as TypeError, and the spec harness cannot enforce
   the class at all (`42N06` is unmapped).
2. **Add a new `Error` variant for compile-time type errors (Compile
   stage + TypeError class).** Rejected: the class is already expressed
   by the typed `InvalidArgumentType` variant and the one ABI v2 table is
   the classification seam; a second variant that differs only in stage
   would fork that table, and ADR-0175 already established the accepted
   shape for a plan-time typed rejection.
3. **Re-pin the TCK scenarios that could conflict.** Rejected: no pinned
   TCK scenario reaches any of the four converted sites (the compile-time
   `InvalidArgumentType` pins for non-boolean shapes are raised earlier by
   the front end), and the one scenario family that does pin a sibling's
   phase (`Boolean1-4` `[8]`/`[4]`, compile time) is the reason that
   sibling is left out; a re-pin would change a pin without a behavioural
   cause and hide the stage question.
4. **Converge the executor site only.** Rejected: that is exactly the
   half-converged state the change forbids — the same family would report
   TypeError when the operand is a runtime value and SyntaxError when it
   is a literal the planner sees.

## Consequences

- Every surface that classifies through the ABI v2 table — Bolt, HTTP,
  the C ABI v2 error shape, and the language wrappers — reports
  `Neo.ClientError.Statement.TypeError` (GQLSTATUS `22G03`) for these
  queries, matching Neo4j. The Bolt spec's variant table
  (`docs/api/bolt/spec.md`) already maps `InvalidArgumentType` to that
  code; no documentation surface changes.
- The typed variant's `stage` is **Runtime** (ADR-0502 §2), so the
  planner's compile-time rejections report the Runtime stage even though
  they fail before any row is produced. This is the same deviation
  ADR-0175 recorded for its plan-time float-literal rejections: the code
  and class drivers act on are right, the stage is coarser than Neo4j's.
  A per-site stage refinement would need a stage-carrying variant and is
  out of scope.
- TCK floor is unchanged: 3892 of 3892 still pass. The pinned openCypher
  scenarios that assert a compile-time `InvalidArgumentType` for
  non-boolean shapes (`Boolean1-4` `[8]`/`[4]`, `Quantifier1-4`
  `[15]`/`[16]`, `Pattern1` `[11]`) are all raised by the front end
  before the planner row, or by the front end's pattern-predicate rule,
  so none of the four changed sites is TCK-pinned.
- The front end's literal-operand sibling keeps the SyntaxError class
  (recorded under Decision), so `RETURN 1 AND true` reports `kind:
  "syntax"` while the converted sites report `kind: "type"`. Closing that
  gap needs the compile-time TypeError shape the stage discussion above
  rules out of this change.
- CIP-eval ratchet: no frozen identity drops (its error steps match any
  error class, and no scenario is keyed on this family's class).
- Tests: unit tests beside both changed sites assert the typed variant,
  the TypeError code from the ABI v2 table, and the retained
  `requires a boolean` wording; the class pins live in
  `cypherlite/cypherlite/tests/type_error_classes.rs` (the DDL class
  tables' dedicated-file pattern, one exact message fragment per site);
  the existing message-substring tests in
  `cypherlite/tests/misc_tail_forms.rs` and
  `exec/conditional_tests.rs` keep passing unchanged.

## Later references

ADR-0566 supersedes this ADR's conclusion for the *static* sites: a
statically non-boolean predicate or operator operand is Neo4j's
compile-time SyntaxError class (probed on the exception class, where this
ADR's table read the GQLSTATUS cause code). The runtime `bool_or_null`
site keeps the TypeError class this ADR chose.
