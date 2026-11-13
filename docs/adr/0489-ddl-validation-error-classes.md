# ADR-0489: DDL validation errors carry Neo4j's error classes

Status: Proposed
Claim: 1eb6fd3a0b7b909a709e6816af5b6cee03e57671

## Context

The Neo4j compatibility harness (`/Users/mh/d/llm/cypherlite-cypher-test-harness`)
asserts the *class* Neo4j's own acceptance tests assert (`SyntaxError`,
`ArgumentError`, …) beside the message. On the 2,994-failure run from engine
`4274a3bd`, 993 scenarios failed only on the class: the engine raised the right
message under the wrong variant. 682 of them are DDL command validation:

| family | expected | engine raised |
| --- | --- | --- |
| property-type constraint restrictions (`LIST<STRING>`, `STRING NOT NULL`, `NODE`, …) | `SyntaxError` | `SemanticError` |
| unknown `OPTIONS` key / rejected `indexProvider` (literal map) | `SyntaxError` | `SemanticError` |
| wrong property count for a single-property index kind | `SyntaxError` | `SemanticError` |
| graph-type `ALTER`/`DROP` shape and reference errors | `SyntaxError` | `SemanticError` |
| `indexConfig` value rejections (unknown setting, non-map setting, foreign config, vector config) | `ArgumentError` | `SemanticError` / `TypeError` |
| `OPTIONS $param` map rejections | `ArgumentError` | `SemanticError` |

The message text already matched; the class is a separate contract that the
Python binding exposes as `tck_class` (`binding_error_metadata`) and Bolt as the
`Neo.ClientError.*` code.

Verified against live Neo4j (`neo4j:2026.09-enterprise`, Cypher 25, driver
`Neo4jError.code`):

| statement | Neo4j code |
| --- | --- |
| `CREATE CONSTRAINT … IS TYPED LIST<STRING>` (and `STRING NOT NULL`, `NODE`, nested list) | `Neo.ClientError.Statement.SyntaxError` |
| `CREATE CONSTRAINT … OPTIONS {indexProvider: 'text-1.0'}` | `Neo.ClientError.Statement.SyntaxError` |
| `CREATE CONSTRAINT … OPTIONS {indexConfig: 'x'}` | `Neo.ClientError.General.InvalidArguments` |
| `CREATE RANGE INDEX … OPTIONS {indexProvider: 'point-1.0'}` | `Neo.ClientError.Statement.SyntaxError` |
| `CREATE RANGE INDEX … OPTIONS $options` (bad provider, non-map) | `Neo.ClientError.General.InvalidArguments` |
| ``CREATE RANGE INDEX … OPTIONS {indexConfig: {`spatial.cartesian.max`: true}}`` | `Neo.ClientError.General.InvalidArguments` |
| ``CREATE VECTOR INDEX … OPTIONS {indexConfig: {`vector.dimensions`: 9999}}`` (and unknown/badly typed settings) | `Neo.ClientError.General.InvalidArguments` |
| `CREATE TEXT INDEX … ON (n.a, n.b)` | `Neo.ClientError.Statement.SyntaxError` |
| `ALTER CURRENT GRAPH TYPE DROP { (:Person => {…}) }` / `… ALTER { :A }` / `… DROP { CONSTRAINT nope }` / `… ALTER { CONSTRAINT … }` | `Neo.ClientError.Statement.SyntaxError` |

The distinction the harness enforces is Neo4j's own: a *statement-shape* rule is
compiled by the front end and surfaces as a `SyntaxException` (even where Neo4j
internally builds a `SemanticError.*`), while an invalid `indexConfig` *value*
is the argument validator's `InvalidArgumentsException`. The same rejection
reached through a whole-map `OPTIONS $param` is also `InvalidArguments`,
because the map is only inspected at run time.

## Decision

**The class is carried as data on the engine error, and each DDL family is
classified once at its construction site.**

1. A new typed variant `Error::ArgumentError(String)` carries Neo4j's
   `ArgumentError` class. Its `Display` keeps the historical
   `semantic error: <detail>` rendering, so the message text does not move;
   only the class does. Surfaces map it in one place each:
   `binding_error_metadata` → `("ArgumentError", "ArgumentError")`, Bolt →
   `Neo.ClientError.General.InvalidArguments`, CLI → `kind: "argument"`,
   HTTP → 400. No surface scans a message prefix.
2. Statement-shape rules become `Error::syntax`: the property-type
   restrictions, a non-`indexConfig` `OPTIONS` key, the single-property index
   kinds' property count, and the whole graph-type body-validation
   family (`CREATE`/`ALTER`/`SET`/`ADD`/`DROP` reference and shape rules,
   mapped through `Error::syntax` from every `validate_graph_type_body` call
   site).
3. `indexConfig` value rejections become `Error::ArgumentError`: a non-map
   `indexConfig`, a config a Range/Text index cannot consume, and every vector
   config parse failure (unknown key, out-of-range dimension, wrong leaf type).
   A whole-map `OPTIONS $param` that is not a map, or whose provider is
   rejected, is also `ArgumentError`; the literal-map form of the same
   provider rejection stays `SyntaxError` (`index_options_shape_error` is the
   one spelling of that split).
4. Shape validation runs **before** the schema-conflict gate, on both DDL
   twins (`resolve_index_options` and `validate_constraint_options`),
   matching Neo4j's compile-time ordering: an invalid `OPTIONS` key or
   property count wins over an equivalent rule that already exists, and an
   `IF NOT EXISTS` skip can never mask it. In both validators the key
   allowlist is checked before any value expression is lowered, so an
   absent parameter in an invalid key's value slot cannot change the class.
   The equivalence-then-conflict disposition is one computation
   (`rule_disposition` → `Equivalent`/`Conflicts`/`New`) consulted by both
   the `CREATE CONSTRAINT` gate and the graph-type derived-constraint sync,
   so the two paths cannot drift; the graph-type path reports a conflict as
   `Error::syntax` (`ConstraintConflict`) while the `CREATE CONSTRAINT` path
   keeps its schema-error class.

## Alternatives considered

- **A `"ArgumentError: "` message prefix scanned by the bindings.** Rejected:
  stringly-typed discrimination; AGENTS.md's code-shape rule forbids matching
  error classes by scanning rendered text, and the repo's typed-variant
  migration (ADR-0307) exists to remove exactly that.
- **Reclassify in the Python binding only.** Rejected: Bolt, HTTP and the CLI
  would keep sending `SemanticError`, and the next client would re-derive the
  mapping.
- **Keep `SemanticError` and change the harness expectations.** Rejected: the
  harness expectations come from Neo4j's acceptance tests and are not editable
  here; live Neo4j agrees with them.
- **Give every validation error one class.** Rejected: Neo4j itself splits
  statement shape (syntax) from argument values (`InvalidArguments`), and the
  harness asserts the split.

## Consequences

- **Public error contract:** `cypherlite::Error` gains a variant (every
  exhaustive match is compiler-checked, as with ADR-0307); the Python
  `tck_class`, the Bolt failure code and the CLI JSON `kind` gain
  `ArgumentError` / `InvalidArguments` / `"argument"` values.
- The affected DDL errors' Display text keeps its detail; graph-type
  `SchemaError:`/`GraphTypeError:` messages now render under the
  `syntax error: ` prefix because their variant moved.
- openCypher TCK: unchanged — the TCK reads the `EngineError` channel
  (`ErrorKind`), not these variants, and the pinned 3,892-scenario count is
  re-gated.
- Measured on the harness: the 28 affected feature files drop from 907 to the
  residual below (the remaining failures are SHOW-output shapes and missing
  validations, tracked separately).
- The graph-type body-validation family's preview-gate failures inherit the
  family's `SyntaxError` class (they are part of the same body
  validation); preview gates outside that family keep their existing class.
- Out of scope, reported rather than forced: the 20 `CREATE CONSTRAINT …
  OPTIONS $param` scenarios, which fail at the parser before any class could
  apply (a parser gap).
