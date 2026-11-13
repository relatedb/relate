# ADR-0211: Graph-type elements with relaxed property declarations, and `SET GRAPH TYPE <name>`

Status: Proposed

## Context

Three gaps in the graph-type surface:

1. **Property-declared elements fail to parse.** `CREATE GRAPH TYPE
   SOGraph AS { (:Tag => {name IS STRING}) }` fails: the `{ <property
   types> }` block's property declarations only accepted the `::`
   (or TYPED) introducer, so the GQL-standard `IS <value type>` spelling a
   user reaches for first was rejected. (The ticket's original example
   dropped the `=>` marker too — `(:Tag {name IS STRING})` — but current
   Neo4j rejects that form as well: the marker is what makes the label an
   explicitly denoted identifying label, so it stays
   mandatory and the parse error now says so.)
2. **A declared (named) graph type can never become current.**
   `CREATE GRAPH TYPE X { ... }` registers a catalog artifact that `SHOW
   GRAPH TYPE X` reports, but nothing moves it into the database's single
   "current" graph type: `SHOW CURRENT GRAPH TYPE` still reports `{}`, and
   the natural `ALTER CURRENT GRAPH TYPE SET GRAPH TYPE X` failed to parse
   (the `SET` grammar expected a `{` body immediately).
3. **`SHOW CURRENT GRAPH TYPE AS GRAPH`** — Neo4j's visual form
   from the Cypher cheat sheet — was already implemented
   (`cypherlite-executor/src/graph_type_visualize.rs`) but had no
   end-to-end coverage at the `cypherlite` crate level.

Neo4j's semantics: `CREATE GRAPH TYPE` registers a named type only; the
current graph type is managed solely through `ALTER CURRENT GRAPH TYPE
SET/ADD/ALTER/DROP { ... }`. Neo4j offers no statement that promotes a
named type to current, so `ALTER CURRENT GRAPH TYPE SET GRAPH TYPE <name>`
is **not** a Neo4j form — but it is the smallest extension consistent with
the established `ALTER CURRENT GRAPH TYPE` verb surface, and the ticket
asks for a supported path if none exists.

## Decision

1. **Element grammar keeps the `=>` requirement.** In a node/edge type
   specification the `{ <property types> }` block is accepted only after
   the `=>`/`IMPLIES` marker — current Neo4j behavior, pinned by the
   negative scenarios: without the marker the label is not
   an explicitly denoted identifying label and the statement is a
   compile-time syntax error (the error message says exactly that).
   Canonical rendering is unchanged (`(:`Tag` => {`name` :: STRING})`).
2. **`IS` as a property-type introducer.** A property declaration accepts
   `<name> IS [::] <value type>` as equivalent to `<name> :: <value type>`.
   Disambiguation against the existing inline-constraint form is lexical:
   `IS` followed by `KEY`/`UNIQUE` (optionally preceded by the
   `NODE`/`REL`/`RELATIONSHIP` qualifier) keeps its inline-constraint
   meaning; anything else after `IS` starts the value type.
3. **`ALTER CURRENT GRAPH TYPE SET GRAPH TYPE <name>`.** A new `SET`
   sub-form copies the named, previously `CREATE GRAPH TYPE`-declared
   type's body into the current graph type, passing the same validation and
   derived-constraint installation as `SET { ... }`. An unknown name fails
   with `GraphTypeNotFound` (the same error `SHOW GRAPH TYPE <name>`
   reports). `CREATE GRAPH TYPE` still does not make its type current —
   declared and current remain separate (matching Neo4j's
   declared-vs-current separation); the new sub-form is the supported path
   from one to the other.
4. **Documentation.** `docs/reference/cypher-syntax.md`'s graph-types
   section documents the relaxed property declarations, `SET GRAPH TYPE
   <name>`, and `SHOW CURRENT GRAPH TYPE AS GRAPH [SINGLE | SEPARATE]`.

## Alternatives considered

- **Also drop the `=>` requirement (accept `(:Tag {name IS STRING})`).**
  Rejected: current Neo4j (docs, cheat sheet, and the
  acceptance corpus) rejects a property block without the marker — the marker denotes
  the identifying label. Accepting it would diverge from Neo4j and fail
  the CIP-eval gate, for a convenience the `=>` spelling already provides.
- **Make `CREATE GRAPH TYPE` implicitly set the current graph type.**
  Rejected: it diverges from Neo4j's declared-vs-current separation and
  gives a catalog statement a hidden side effect (installing constraints).
  Promotion is explicit via `SET GRAPH TYPE <name>`.
- **A new statement (`SET CURRENT GRAPH TYPE <name>`) instead of extending
  `ALTER CURRENT GRAPH TYPE`.** Rejected: `ALTER CURRENT GRAPH TYPE SET` is
  the established verb surface for replacing the current type; a parallel
  statement duplicates it.
- **Only `IS ::` (not bare `IS`) as the introducer.** Rejected: the
  ticket's form is bare `IS <value type>`; accepting both is the same
  disambiguation rule either way.

## Consequences

- `cypherlite-parser`'s `AlterCurrentGraphTypeClause` gains a `target:
  AlterCurrentGraphTypeTarget` enum (`Body` / `Named`) replacing the verb
  body for the named form; the front/planner pass the clause through
  opaquely, so no other layer changes.
- Previously-failing spellings now parse: `(:Tag => {name IS STRING})`,
  `(q)-[:TAGGED => {weight IS FLOAT}]->(t)` (with `IS`/`IS ::` property
  types), `ALTER CURRENT GRAPH TYPE SET GRAPH TYPE <name>`. No
  previously-valid spelling changes meaning — the relaxed productions only
  accept inputs that were errors before.
- `SHOW CURRENT GRAPH TYPE AS GRAPH` gains crate-level test
  coverage.
