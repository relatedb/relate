# ADR-0391: CIP-250 `FOR <variable> IN <expression>` behind the `gql-for` gate

Status: Accepted

Claim: b30c4fa0ccd22217f480d21f65a4ef48e96c5639

## Context

CIP-250 ("FOR as replacement for UNWIND", GQ10; approved by the CLG
2026-03-19, contingent on the range() example the Examples table carries)
introduces the GQL 14.8 `<for statement>` list-value-support syntax as a
Cypher clause: `FOR <binding variable> IN <expression>`, valid wherever
`UNWIND <expression> AS <binding variable>` is valid today. It is not a
keyword swap — the binding variable precedes the expression. The goal is
GQL conformance (accepting GQL-compliant queries copied from other
vendors), not new semantics: the construct is specified to normalize onto
the existing UNWIND planning and runtime infrastructure ("no new logical
plan operators"), UNWIND is explicitly not deprecated, and a non-list
source value is coerced to a singleton list exactly as UNWIND coerces it.

The feature is approved but not part of the pinned openCypher TCK floor,
so per ADR-0371 it ships behind a new `gql-for` row of the
`CYPHERLITE_PREVIEW` registry. GQ11 (`WITH ORDINALITY`), GQ24
(`WITH OFFSET`) and GQ23 (binding table references) are out of scope,
reserved for future CIPs.

The parse-side choices matter because the `FOR` token already exists (the
`SEARCH … FOR` index-specification slot, `FOREACH`'s own token) and the
front's preview admission (ADR-0046, ADR-0371) detects gated constructs in
the parsed AST before anything lowers — the CIP-278 `hybrid-search`
boolean-walk detector is the precedent.

## Decision

Implement CIP-250 as a parse-time normalization behind the `gql-for`
gate, in three pieces:

- **Parser**: `parse_for_clause` recognizes `FOR <symbolic name> IN
  <expression>` at the clause-start dispatch (`FOR` at clause start was a
  syntax error before, so the change is purely additive; `FOREACH` is its
  own token, so the two never collide, and the `SEARCH … FOR` slots are
  inside the MATCH parse, never at clause start). The clause normalizes
  into the *existing* `Clause::Unwind` — same expr/alias pair — with a
  new `UnwindClause::for_syntax` spelling flag, so validation, lowering,
  planning and execution are shared wholesale (the CIP's own "no new
  logical plan operators" rule). The binding variable reads through the
  single symbolic-name read (`parse_schema_name`, ADR-0191), so keyword
  spellings work unquoted exactly like the UNWIND alias and the FOREACH
  loop variable. `FOR` joins the clause-starter list
  `group_by_ends_clause` consults, so a grouping-key expression list ends
  before it exactly as before UNWIND. No new recursion family exists (the
  clause is a flat sibling in an existing statement body); pinned in
  `tests/limits.rs` against the shared subquery depth guard.
- **AST spelling record**: `for_syntax` is the one bit that distinguishes
  the two spellings, per the CIP's canonical-syntax rule (rendered back
  as `FOR x IN e`, never `UNWIND e AS x`). It is the parse-spelling
  record, not parallel state: every consumer except the gate treats the
  clause as a plain UNWIND.
- **Gate**: the ADR-0371 registry gains the `gql-for` row; the front's
  statement admission (`parse_and_lower_statement_with_preview`) gains
  the detector arm — one boolean whole-tree walk
  (`statement_uses_gql_for`, the CIP-278 `statement_uses_hybrid_search`
  precedent) over the spelling flag — and fails closed with the authored
  `preview feature 'gql-for' is not enabled: set
  CYPHERLITE_PREVIEW=gql-for` error before anything of the statement
  validates or lowers. The registry loop is exhaustive, so the row
  cannot exist without its detector arm.

## Alternatives considered

- **A distinct `Clause::For` AST variant lowered onto the UNWIND IR
  clause.** Rejected: the AST's `Clause` enum is matched exhaustively
  across the front's validators, the planner, the executor and the
  walkers (~30 match sites); a variant that must behave identically to
  `Unwind` everywhere would either duplicate every arm or force `_ =>
  unreachable!()`s — parallel state over the same vocabulary, exactly the
  shape the code-shape rules forbid. The parse-time normalization
  touches one construction site and zero match sites.
- **Gate at the parser** (thread a preview flag into `cypherlite-parser`
  so the FOR clause itself fails closed there). Rejected: the parser is
  gate-free by design (ADR-0371); the CIP-278/CIP-231 pattern — detect
  the construct in the AST at the front's preview entry — keeps one
  admission path and one fail-closed authored error, and leaves the
  parser usable by gateless callers.
- **Reuse the reserved `hybrid-search`-style pre-existing row or no gate
  at all.** Rejected: ADR-0371 requires every syntax-extending
  not-yet-floor feature to carry its own row; ungated shipping would put
  a non-TCK-floor spelling in the default dialect.
- **Parse `FOR` as a non-reserved contextual word.** Rejected: `FOR` is
  already a reserved keyword token (the `SEARCH … FOR` slot), so
  clause-start recognition is unambiguous with no lexical change; a
  contextual-word rewrite would touch the lexer for no benefit.

## Consequences

- The TCK floor is unchanged: with the opt-in unset, a `FOR x IN …`
  clause fails closed with the authored preview error; the pinned corpus
  (3892/3892) parses and lowers exactly as before, and the equivalent
  `UNWIND` spelling stays ungated floor.
- The planner and executor see only the shared UNWIND operator; an
  `EXPLAIN FOR x IN [1,2,3] RETURN x` plan is the UNWIND plan (the plan's
  statement echo alone carries the differing query text), exactly the
  CIP's interaction-table statement.
- `baselines/cip-eval.json` gains the CIP-250 entry (15/15, purely
  additive); the ratchet only moves up. The scenarios are doc-derived
  (deviations recorded in the CIP-250 NOTES.md).
- Engine integration coverage rides the shared process-level gate
  fixture pattern (`cypherlite/tests/gql_for.rs`,
  `common::set_preview_once`); the ungated fail-closed error is pinned
  by the registry-driven fixture in `src/tests/preview.rs`, which covers
  the new row automatically.
- The reserved `gql-for` registry row is now live; the CLI spec's
  `CYPHERLITE_PREVIEW` row, `docs/reference/cypher-syntax.md`, and the
  AGENTS.md dialect note ship in the same change. No CLI flag changed,
  so no bundle/reference drift applies.
- GQ11/GQ24/GQ23 are future CIPs; when one lands, the `for_syntax`
  spelling record is the hook its detector and renderer extend — a new
  row, not a rework of this one.

## Later references

ADR-0533 graduates the clause out of the preview registry: the reference
dialect shipped it default-on, so the `gql-for` row, its detector arm
and the gate are deleted, and the `FOR` spelling became default dialect.
The parse-time normalization and the `for_syntax` spelling record this
ADR introduced remain.
