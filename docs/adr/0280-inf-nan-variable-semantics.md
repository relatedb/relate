# 0280. inf/nan/infinity variable semantics and non-finite float JSON rendering

- **Status**: Accepted
- **Renumbered**: from 0158 (shared-number cleanup)
- **Date**: 2026-10-02
- **Deciders**: CypherLite maintainers

## Context

LABS-165 reports that variables named `inf`, `nan`, or `infinity` bind in
pattern position but resolve as float literals in expression position,
with three confusing repros: a `DETACH DELETE` of such a variable failing with
a misleading message, a property access on such a variable failing with a type
error, and `RETURN inf AS x` rendering as `{"x": null}` in JSON mode.

Live probes against Neo4j Kernel 2026.09.0 community (default language
CYPHER_25; probes re-run under CYPHER 5 with identical results) establish the
compatibility target:

1. `RETURN inf AS x, nan AS y, infinity AS z` (unbound) returns
   `Infinity`/`NaN`/`Infinity` — no "undefined variable" error.
2. `WITH 0 AS nan, 1 AS inf, 2 AS infinity RETURN nan, inf, infinity` returns
   `NaN`/`Infinity`/`Infinity` — the literal wins over the in-scope variable,
   silently, with no warning.
3. `CREATE (inf:N {v: 42}) RETURN inf.v` fails with GQLSTATUS 22NB1 (type
   mismatch, expected MAP/NODE/…, was FLOAT); backticked `` `inf`.v ``
   returns 42.
4. `… OPTIONAL MATCH (n)-[:R]->(inf:N) DETACH DELETE n, inf` fails with
   GQLSTATUS 22NB1 (expected NODE/PATH/RELATIONSHIP, was FLOAT); backticked
   `DELETE `inf`` succeeds.
5. `CREATE (nan:N) RETURN labels(nan)` fails with GQLSTATUS 22N27 (labels()
   expects NODE, got FLOAT).
6. `SET n.inf = 1` and `{nan: 1}.nan` work — property keys after a dot and
   map keys are unaffected.

So in Neo4j the float literal **always** wins in expression/operand
position, bound or not; binding such a name is legal and silent; every
expression-position use of such a variable reads the literal and fails (if at
all) at runtime with a type-mismatch GQLSTATUS, never a syntax error; and
backtick-escaping is the one workaround that forces variable resolution.

CypherLite v0.10.1 is in the same class everywhere, via these sites:

- `cypherlite-parser/src/token.rs:381-382` lexes bare `inf`/`infinity`/`nan`
  as INFINITY/NAN keyword tokens; `token.rs:261-262` (`is_ident_like`) lets
  identifier-expecting positions (pattern variables, aliases, property keys)
  accept them.
- `cypherlite-parser/src/parser/expressions.rs:1792-1798` (`parse_primary`)
  unconditionally lowers those tokens to Float literals — no scope-aware
  fallback, unlike the RETURN/NODE fallback-to-Variable arms at
  `expressions.rs:1990-2026`. Backtick-escaping
  (`cypherlite-parser/src/lexer.rs:433-462`) emits a plain Identifier token
  and rescues every position.
- Property access on the resulting Float fails at runtime
  (`cypherlite-executor/src/eval/expr.rs:63-67`, InvalidArgumentType);
  `labels(nan)` fails at runtime
  (`cypherlite-executor/src/eval/function.rs:287`, InvalidArgumentValue).
- `DELETE inf` fails **statically**: `parse_delete`
  (`cypherlite-parser/src/parser/clauses.rs:1020-1035`) parses each target as
  an expression, and `check_delete_expr`
  (`cypherlite-front/src/validate/expressions.rs:705-734`, catch-all
  `:730-733`) rejects it as a compile-time `InvalidArgumentType` with the
  message "DELETE requires a node or relationship, not an expression". That
  static check is TCK-mandated: `clauses/delete/Delete5.feature` scenario 9
  requires `DELETE 1 + 1` to be a compile-time InvalidArgumentType even on an
  empty graph, so the check cannot move to runtime. The CLI prints the error
  class as "syntax error", which reads as if the query were malformed.
- JSON rendering: `cypher_value_to_json`
  (`surfaces/cypherlite-formatter/src/lib.rs:305-307`, CLI `-json`/`-jsonl`
  modes) and the canonical wire encoder `json_f64`
  (`cypherlite/cypherlite-model/src/wire.rs:210-215`) both do
  `Number::from_f64(f).map(Number).unwrap_or(Null)`, and `from_f64` returns
  `None` for non-finite floats — so `NaN`/`±Inf` render as `null`. Table/CSV
  modes use `CypherValue`'s `Display`
  (`cypherlite/cypherlite-model/src/value.rs:515-518`) and print
  `NaN`/`Inf`/`-Inf`. The wire module documents the current rule and
  pre-commits to the fix location: "A richer representation is tracked
  separately and, if adopted, will land here so every surface changes at
  once" (`wire.rs:67-72`); the null encoding is pinned by the golden test
  `non_finite_floats_encode_as_null` (`wire.rs:987-1001`). Wire consumers:
  the C FFI (JS/Java/Go clients), the Python bindings
  (`cypherlite-python/src/convert.rs`), Arrow
  (`cypherlite-arrow/src/record_batch.rs:907`), the WASM/binding envelopes
  (`cypherlite/src/binding.rs:180-185`), CLI wire golden tests, and cookbook
  recordings (which parse wire envelopes).

This is consistent with the language's own usage examples (the literal
wins in ordinary expression position); the fresh Neo4j evidence does not
diverge from them.
The user mandate (LABS-165): align with verified Neo4j behavior, fix genuine
divergences and usability bugs, and pin everything in tests. Diagnostics are
not a compatibility surface, so improving our error text is allowed;
changing resolution semantics to diverge from Neo4j is not.

## Decision

One coherent ruling: **CypherLite keeps Neo4j-aligned float-literal resolution
semantics in every position, fixes the two genuine usability bugs (the
misleading static DELETE message and the JSON null rendering), documents the
backtick escape, and pins every ruled position in tests.**

1. **Resolution semantics — pin current behavior (matches Neo4j 2026.09 and
   its usage examples).** Per position:

   | Position | Rule | Ruling |
   |---|---|---|
   | Bare reference (`RETURN inf`) | float literal; never "undefined variable" | pin-current |
   | Bound-variable expression reference (`WITH 0 AS nan RETURN nan`) | Literal wins over the in-scope variable, silently | pin-current |
   | Property-access base (`inf.v`) | Literal wins; runtime InvalidArgumentType | pin-current |
   | Function argument (`labels(nan)`) | Literal wins; runtime type error | pin-current |
   | DELETE target (`DELETE inf`) | Rejected — statically in CypherLite (TCK Delete5 scenario 9 mandates compile-time InvalidArgumentType), at runtime in Neo4j; same outcome class, earlier failure | pin-current (see ruling 2 for the message) |
   | Binding (pattern variable `(inf:N)`, alias `AS nan`) | Legal and silent | pin-current |
   | SET target property key (`SET n.inf = 1`) | Property key after a dot; unaffected | pin-current |
   | Property key / map key (`n.inf`, `{nan: 1}.nan`) | Unaffected | pin-current |

   Backtick-escaping (`` `inf` ``) forces variable resolution in every
   position and is **the** documented workaround. No scope-aware fallback is
   added to `parse_primary`; no warning is emitted on binding (Neo4j emits
   none, and CypherLite has no warning surface).

2. **DELETE diagnostic — fix.** `check_delete_expr` gains a dedicated arm
   for non-finite float literal targets whose message names the cause and
   the workaround: `inf`/`nan`/`infinity` are float-literal keywords,
   and backtick-escaping (`` DELETE `inf` ``) references a variable of that
   name. The check stays static and keeps `ErrorKind::InvalidArgumentType`
   (TCK Delete5 scenario 9 pins the compile-time class); only the message
   text changes. Whether the validator pattern-matches non-finite float
   literals or the parser threads keyword provenance is an implementation
   choice. Runtime type errors (property access, function arguments) keep
   their current shape — they already match Neo4j's runtime type-error class
   and name expected/actual types; threading keyword provenance to runtime
   is disproportionate.

3. **JSON rendering of non-finite floats — fix.** Both encoders render
   `NaN`/`+Inf`/`-Inf` as the JSON strings `"NaN"`, `"Inf"`, `"-Inf"` —
   exactly the `CypherValue` `Display` spellings (`value.rs:515-518`), so
   JSON modes agree with table/CSV modes. The change lands in the same
   commit in the CLI formatter (`cypher_value_to_json`,
   `cypherlite-formatter/src/lib.rs:305-307`) **and** the canonical wire
   encoder (`json_f64`, `cypherlite-model/src/wire.rs:210-215`), per the
   wire module's own every-surface-at-once rule; the golden fixture
   `non_finite_floats_encode_as_null` is replaced by one pinning the string
   encoding, and the wire module doc table is updated. This is a breaking
   wire change, acceptable pre-1.0/alpha per the wire module's documented
   precedent (`wire.rs:88-89`), and gets a CHANGELOG entry. Bolt (PackStream
   `f64`) and the `.mode cypher` literal renderer (already emits parseable
   `NaN`/`Infinity`/`-Infinity`) are unaffected.

4. **Tests pin every ruling.** Engine tests cover each position in the
   ruling-1 table (including the backtick rescue and the identical-behavior
   probes 1–7); the JSON string encoding is pinned by the wire golden
   fixtures and formatter tests; the new DELETE message is pinned in
   `cypherlite-front` validation tests. If the pinned openCypher TCK corpus
   contains scenarios for these positions they join the allowlist per the
   AGENTS.md §4 procedure; no scenario moves to the skip list.

5. **Documentation.** `inf`/`nan`/`infinity` are documented as
   float-literal keywords that resolve to literals in expression position
   even when a variable of that name is in scope, with the backtick escape
   as the workaround, alongside the JSON-mode encoding note in
   `docs/api/cli/spec.md`.

## Alternatives considered

- **Scope-aware fallback** (a bound variable wins over the literal in
  expression position). Rejected: Neo4j 2026.09 probe 2 shows the literal
  wins even when bound, under both CYPHER_25 and CYPHER 5, and the
  language's usage examples say the literal wins in ordinary expression
  position.
  Diverging here breaks the compatibility target the user set.
- **Reject binding `inf`/`nan`/`infinity` as variable names.** Rejected:
  Neo4j binds them legally and silently; rejecting the bind would fail
  queries Neo4j accepts, and the backtick escape already provides a
  resolution path for anyone who binds such a name.
- **Move the DELETE rejection to runtime to match Neo4j's 22NB1 timing.**
  Rejected: TCK `clauses/delete/Delete5.feature` scenario 9 mandates a
  compile-time InvalidArgumentType for non-entity DELETE targets even on an
  empty graph, so the static check is a passing-TCK obligation; failing
  earlier is strictly better and the error class matches.
- **Keep JSON `null` and document it.** Rejected: it is silent data
  corruption — `RETURN inf AS x` renders `{"x": null}`, indistinguishable
  from a genuine null, while table mode prints `Inf`. That is LABS-165
  repro 3; documenting a corruption does not fix it.
- **Tagged-object encoding** (e.g. `{"$float": "NaN"}`). Rejected: the wire
  format is deliberately self-describing with no type tags
  (`wire.rs:12-15`); a tag object breaks column shape homogeneity (a FLOAT
  column would suddenly contain objects) and forces every client to
  special-case it, for no gain over a string a human can read.
- **Raw `NaN`/`Infinity` JSON tokens.** Rejected: not valid JSON;
  `serde_json` and every strict parser (including the recordings reader and
  browser `JSON.parse`) rejects them, so the output would be unparseable
  rather than merely lossy.
- **`"Infinity"`/`"-Infinity"` string spellings** (the keyword spelling)
  instead of the `Display` spellings. Rejected: the JSON string is a
  rendering, not a re-parseable literal — the `.mode cypher` renderer
  already owns the parseable spelling — and matching `Display`
  (`"Inf"`/`"-Inf"`/`"NaN"`) keeps JSON, table, and CSV modes consistent.
- **Fix only the CLI formatter, keep wire `null`.** Rejected: the wire
  module exists so every surface serializes identically and drift becomes a
  failing golden test (`wire.rs:1-8`); its docs pre-commit to landing a
  richer representation in `wire.rs` so every surface changes at once.
  Splitting the fix recreates exactly the drift the golden fixtures exist
  to prevent, and leaves FFI/Python/WASM consumers corrupting to `null`.

## Consequences

- No parser or planner resolution change ships; `parse_primary`'s
  unconditional literal lowering is pinned as correct, Neo4j-aligned
  behavior. The change surface is: one validation arm and message in
  `cypherlite-front/src/validate/expressions.rs`, the two float encoders
  (`cypherlite-formatter/src/lib.rs`, `cypherlite-model/src/wire.rs`),
  golden fixtures, docs, and tests.
- `check_delete_expr`'s TCK obligation (Delete5 scenario 9) is unchanged:
  the new arm keeps `ErrorKind::InvalidArgumentType` and compile-time
  timing; only message text differs, which the TCK does not pin.
- The wire encoding change propagates through `wire.rs` to the C FFI
  (JS/Java/Go clients), Python, Arrow, and WASM/binding envelopes in one
  move; those consumers see `"NaN"`/`"Inf"`/`"-Inf"` strings instead of
  `null`. Cookbook recordings render the string form (quoted) instead of
  `null` — visible rather than silently corrupting. The CHANGELOG records
  the breaking wire change under the pre-1.0/alpha precedent.
- CLI `-json`/`-jsonl` output for non-finite floats changes from `null` to
  the `Display` strings; `docs/api/cli/spec.md`'s output-mode documentation
  and the inf/nan keyword note land in the same change as the code.
- Tests pin every ruled position (engine tests, wire golden fixtures,
  formatter tests, front validation tests); the TCK allowlist only grows
  per the AGENTS.md §4 procedure and the skip list does not move.
- Adjacent gap flagged for a separate decision, out of scope here: the
  persistence encoding rejects non-finite floats outright
  (`cypherlite-model/src/serde_value.rs:39-42`, used at `:132`), so
  `NaN`/`±Inf` property values cannot round-trip through the tagged serde
  format. Rendering and persistence are separable decisions; this ADR rules
  on rendering only.
