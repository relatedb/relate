# ADR-0448: `toString()` renders temporal values at full precision — the value layer owns the policy

Status: Accepted (2026-10-29)

Claim: 40a1857f9d056df922fc98af095752f9a2d23037

## Context

PR #474 (the GQL function-alias change) introduced a
spelling-dependent rendering asymmetry: `toString(<GQL snake_case
temporal constructor call>)` — `toString(local_datetime(...))`,
`toString(zoned_datetime(...))`, `toString(local_time(...))`,
`toString(zoned_time(...))` — renders the temporal at full precision,
while the identical value built any other way (the canonical Cypher
spellings `localdatetime()` / `datetime()` / `localtime()` / `time()`,
a variable, a property) renders truncated. The mechanism is
name-sniffing in the lower layer
(`cypherlite-front/src/lower/expr.rs::is_tostring_of_gql_temporal`):
the lowering matches the argument call's *name* against the
`temporal_constructor` markers of `GQL_FUNCTION_ALIASES` and renames
the call to an internal, unadvertised `tostring_full` function whose
executor arm renders full precision.

LABS-498 (deferred from #474's stellar rounds) records the defects of
that shape: the rendering policy is a property of the **value**, not of
the call-site spelling — the pipeline bakes an asymmetry where two
expressions evaluating to the same temporal render differently, and the
distinction is enforced by string-matching the argument at lowering
time (the shape-sniffing the code-shape rules prohibit) rather than by
a typed fact.

Nothing pins the truncated rendering: the openCypher TCK corpus has no
`toString(<temporal>)` scenario (its `toString` scenarios cover
integers, booleans, nodes, lists), the CIP-eval suite has none, and the
only recorded occurrence in the docs is a cookbook cell whose recorded
output re-records with this change. Neo4j renders `toString()` on
temporals at full precision regardless of how the value was built.

## Decision

`toString()` (and `toStringOrNull()`) renders temporal values at full
precision — the value layer decides by the runtime value type, not by
the call-site spelling:

1. The rendering policy lives on the value:
   `Temporal::full_seconds_string` (cypherlite-core, beside
   `iso_string`) is the one full-precision ISO rendering, consulted by
   every string-producing surface — `Value::to_display_string`'s
   temporal arm (so a temporal nested in a map or accepted list renders
   identically to a top-level one), and the executor's shared scalar
   writer `write_scalar` (so the coercion path and the
   interpolated-string scalars render identically too).
2. The `tostring_full` internal function is deleted: the executor
   registry row and builtin, the lowering's precision rename
   (`lower_function_name`'s first policy), the
   `is_tostring_of_gql_temporal` / `is_gql_temporal_constructor`
   matchers, the `temporal_constructor` markers on
   `GQL_FUNCTION_ALIASES` (the table keeps only the alias rename
   policy), its `UNADVERTISED_FUNCTIONS` entry, and the executor-side
   `temporal_full_seconds` helper (superseded by the core method).
3. Observable deltas, accepted: the canonical constructor spellings,
   variables, and properties now render full precision where they
   rendered truncated before; temporals nested in maps and accepted
   lists, and temporals embedded in interpolated strings, render full
   precision where they rendered truncated before. The GQL spellings
   keep rendering full precision — unchanged.

## Alternatives considered

- **Encode the distinction in the parser's typed AST** (keep the
  asymmetry, move the match to where the call is typed): rejected — it
  preserves an asymmetry no spec asks for; Neo4j compatibility and the
  value-owns-the-policy principle both point the other way, and it
  keeps a second rendering path alive for a distinction nothing
  observes.
- **Keep the lowering sniff as-is** (status quo): rejected — the
  rendering policy depends on a string match of an argument's name at
  lowering time, the exact shape-sniffing the code-shape rules
  prohibit, and the asymmetry is a dialect wart.

## Consequences

- The dialect is simpler: one temporal rendering policy, owned by the
  value layer and consulted by every string-producing surface, matching
  Neo4j's behavior for all spellings — top-level, nested, interpolated,
  and `format()`'s no-pattern default render identically and cannot
  drift apart again.
- The cookbook cell recording `toString(datetime.fromEpochMillis(...))`
  re-records with the full-precision rendering (the cookbook gate
  verifies both native and WASM parity).
- Front, planner, and executor surfaces shrink: one function name
  fewer to advertise, document, and test; the GQL alias table carries
  one policy instead of two.
- Later references: none yet.
