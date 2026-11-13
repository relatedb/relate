# ADR-0382: Local callables behind the `local-callables` gate

Status: Accepted

Claim: 9aeddde647d1092b1dfdb998cd197fbea79bfaa6

## Context

Local callables — accepted into the Cypher language, not yet part of the
pinned openCypher TCK floor — let one query name its own callables and
call them later in the same compilation unit: a `<definition block>` of
`DEFINE PROCEDURE
<name> ( <params> ) [ :: ( <fields> ) ] { <body> }` / `DEFINE FUNCTION
<name> ( <params> ) [ :: <type> ] ( = <expr> | { <body> } )` heads any
`<statement block>` (the top level or any braced nested body), parameters
are `<name> [ :: <type> ] [ = <default> ]`, and call sites are the
existing `<named procedure call>` (`Call name(args) YIELD ...`) and
`<function invocation>` (`name(args)`). Per ADR-0371 the construct ships
behind the already-reserved `local-callables` row of the `CYPHERLITE_PREVIEW`
registry.

The engine has no callable abstraction to extend: procedure calls are a
fixed built-in catalog, function invocation dispatches on the built-in
function registry, and no query-local name exists anywhere. What the
engine does have — and the proposal itself names as its implementation
plan ("Local Procedures are planned to be implemented through a rewrite of
`<named procedure call>`s to `<inline procedure call>`s", and a local
function's body *is* the value expression `head( COLLECT {
<procedure specification> } )`) — are scoped `CALL (vars) { … }`
subqueries, correlated `COLLECT { … }` subqueries, `WITH`, and
`FINISH`, all pre-existing and fully tested.

## Decision

Implement local callables as a parse-plus-rewrite, gated, in three layers:

- **Parser (new `parser/definitions.rs`)**: `DEFINE`/`PROCEDURE`/
  `FUNCTION`/`TYPED` are non-reserved words recognized by spelling on
  plain identifier tokens (`DEFINE` only when immediately followed by
  `PROCEDURE`/`FUNCTION`, so `define` stays a usable variable name).
  The definition block parses at the head of
  `parse_statement_block_at_current_depth` — the one ADR-0257 choke point
  every top-level and braced statement-block parse funnels through, so
  definitions land in exactly the grammar's positions and definition
  nesting is `block_depth`-guarded like every other nested-body family
  (pinned in `tests/limits.rs`). The AST gains `Statement::definitions`
  plus `LocalDefinition`/`LocalParam`/`LocalOutput`/`LocalDefinitionBody`;
  per-definition syntactic rules (duplicate parameter names 42N67, `NOT
  NULL` anywhere in a signature type 42NAL/42NAJ/42NAK, a literal default
  contradicting its parameter type 22NB1) are parse-time errors.
- **Front resolver (`front/src/local_callables/`, new)**: one walk over
  the whole statement tree, in scope order, before lowering. Each
  definition is validated (the proposal's define-time rules: name
  uniqueness per kind across the compilation unit 42I77, no `USE` in any
  body 42NAF/42NAG, no recursion or parent-definition calls, a function
  query body's scalar shape 42NAM, no updates in a function query body, a
  procedure output signature matching its body 42NAI/42NAH, aliased
  procedure return items 42N21), its body rewritten first (so nested
  definitions resolve), and then every visible call site rewritten away:
  a local function invocation becomes
  `head( COLLECT { WITH <args> AS <temps> CALL (<temps>) { WITH <temps>
  AS <params> … body } RETURN <value> } )` (zero rows → `null`, the
  proposal's scalar-subquery semantics), a local procedure call becomes
  the same argument-materialization shape around the body with the YIELD
  mapping applied to the body's own emitting RETURN and the mapped
  columns re-exported by a terminal RETURN. Fresh internal names (`_lc1`,
  …) for the materialized arguments make call-by-value collision-free
  (each argument is evaluated exactly once per outer row — the
  proposal's `foo(rand())` self-equality pin) and structurally enforce
  REFERENCE-OUTER-DEFINITION-SCOPE-NO (the body sees its parameters and
  nothing else). After the resolver, no `DEFINE` block and no local
  invocation remains: the planner and executor never see a local
  callable.
- **Gate**: the engine's statement admission (the ADR-0371 preview entry)
  fails closed with the authored error on any `DEFINE` unless
  `CYPHERLITE_PREVIEW` enables `local-callables` — the same admission
  path as the `hybrid-search` gate. Visibility is structural: a
  definition is visible only inside the statement block that contains it
  (a fresh scope frame per block — UNION arms, NEXT stages, subquery and
  conditional bodies are sibling blocks), from its own position onward,
  and never from inside its own body or a body nested within it.

Scope decisions: `YIELD … WHERE` on a local procedure call is deferred
(an authored error — the engine's subquery rebind rules cannot express
the faithful rewrite); declared parameter/output/return types are parsed
and syntactically validated but not enforced at runtime (no static
expression type checker exists here); NEXT stages inside callable bodies
and conditional statements as a function query body are rejected with
authored errors; not-visible calls fall through to the engine's own
unknown-callable errors.

## Alternatives considered

- **Planner/executor-native local callables** (a `LocalProcedure` IR
  operator and a function-registry entry per definition, executed
  natively). Rejected: it would add a callable abstraction to every layer
  (binder, planner, executor, result metadata) for a feature the
  proposal itself specifies as a rewrite; the rewrite needs zero engine
  changes, so the TCK floor and every existing contract stay untouched.
- **String/AST-level inlining of argument expressions into the body**
  (substitute each parameter reference textually). Rejected: naive
  substitution breaks the proposal's own same-name-parameter and
  shadowing examples (a parameter bound inside a nested comprehension
  would be captured), and correct binding-aware substitution would be a
  second scope engine; the fresh-internal-name materialization achieves
  the same call-by-value semantics without any substitution.
- **Parsing `DEFINE` as a reserved keyword** (a new lexer token).
  Rejected: it would break every existing query using `define` as a
  variable/label name — the TCK floor must stay byte-identical when the
  gate is off; contextual-by-spelling recognition is the house technique
  (`NEXT`/`USE`/`Let`/`FILTER`/`FINISH`).
- **Gate at the parser** (reject `DEFINE` unless a flag is threaded into
  `cypherlite-parser`). Rejected: the parser is gate-free by design
  (ADR-0371); the hybrid-search `COMBINE` gate pattern — detect the
  construct in the AST at the front's preview entry — keeps one
  admission path and one fail-closed authored error.

## Consequences

- The TCK floor is unchanged: with the opt-in unset, a `DEFINE` query
  fails closed with the authored preview error; every scenario in the
  pinned corpus parses and lowers exactly as before (all suites green).
- `Statement` gains a public `definitions` field; the shared whole-tree
  walker descends into definition bodies, so every existing whole-tree
  validator covers them too — a rule violation cannot hide inside a
  callable body.
- Local-callable errors are authored texts raised at rewrite time
  (syntax-class); the cip-eval ratchet runs leniently on error class, and
  the deviations (the 42N08/42N48 explanatory "not visible because"
  texts, the warning on shadowing a built-in name) are covered by that
  lenient error-class ratchet.
- The rewrite duplicates each definition body per call site (a clone per
  invocation); fine for a first preview, and the obvious optimization
  (a shared lowered body keyed per definition) is a later decision if
  usage shows up in profiles.
- `baselines/cip-eval.json` gains the `local-callables` entry (36/36);
  the ratchet only moves up.
- The reserved `local-callables` registry row is now live; the CLI spec's
  `CYPHERLITE_PREVIEW` row, `docs/reference/cypher-syntax.md`, the
  agent-safety skill reference, and the AGENTS.md dialect note ship in
  the same change.
