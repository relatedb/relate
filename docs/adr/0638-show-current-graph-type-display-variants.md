---
type: adr
area: cypher
claim: 73bdd2ad3aeacf69fd4dd6d4d0fa38e38e46418d
date: 2026-11-21
deciders: Relate maintainers
name: 0638-show-current-graph-type-display-variants
status: proposed
tags:
  - dialect
  - graph-type
title: SHOW CURRENT GRAPH TYPE display variants (consolidated, flattened) as pre-render transforms behind a preview gate
REFERENCES:
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0388-graph-type-trio-optional-properties-default-values-implied-labels]]"
---

# 0638: SHOW CURRENT GRAPH TYPE display variants (consolidated, flattened) as pre-render transforms behind a preview gate

## Context

`SHOW CURRENT GRAPH TYPE` returns exactly the graph type that is enforced,
as a specification string or, with `AS GRAPH`, as virtual nodes and
relationships. An accepted proposal adds two ways to look at the same state. One shows
the independent property-existence and property-type constraints as the
element types they would be, the starting point for moving a constraint-based
setup to element types. The other shows each node element type with the rules
it inherits through its implied labels spelled out, the overview that
[ADR-0388](./0388-graph-type-trio-optional-properties-default-values-implied-labels.md)'s
inheritance otherwise makes the reader assemble by hand. The proposal is
accepted but not yet part of the pinned dialect floor, so
[ADR-0371](./0371-preview-feature-gating.md) applies.

## Decision

1. **Syntax.** `SHOW CURRENT GRAPH TYPE [AS CONSOLIDATED | AS FLATTENED]
   [AS GRAPH [SINGLE | SEPARATE]] [YIELD … WHERE …]`. The two variant words
   are contextual (identifiers everywhere else), written once and before
   `AS GRAPH`. The variants are mutually exclusive: both, or a repeat, is a
   parse error, held as one optional typed field on the clause.
2. **Display only.** A variant is a pure transform of the stored graph type
   body and the constraints created outside it, run before the existing two
   renderers, which are unchanged. Nothing is altered, created or persisted;
   the plain statement and the columns are unchanged. The output may be a
   graph type that `SET` would reject.
3. **Consolidated** turns each independent single-property existence or
   type constraint -- one on a label or relationship type identifying no
   element type -- into a property of that label's element type, created
   when absent: `p :: T`, `p :: ANY NOT NULL`, or `p :: T NOT NULL` for both.
   Consolidated constraints lose their names; key and unique constraints
   stay. A second, different type for one property cannot be one definition
   and stays a constraint.
4. **Flattened** rewrites node element types only: the full transitive
   implied-label set, the effective properties from ADR-0388's resolver
   (own declaration wins, diamonds intersect, an inherited default fills a
   stated property without one), and every ancestor's undesignated
   constraints, unnamed so they render with generated names. An independent
   constraint on a non-identifying implied label inherits as if dependent:
   the resolver runs over the consolidated view while only the original
   element types are emitted. Edge types, endpoints and a label's own
   constraints are untouched.
5. **One gate.** Both variants sit behind the preview feature
   `graph-type-show-variants`, detected on the parsed statement at
   admission like the other graph-type gates, so a gated statement fails
   with the authored disabled-feature error, never a parse error.

## Alternatives considered

- **Render each variant with its own renderer.** Rejected: it duplicates
  the canonical sort, naming and quoting rules in the string and graph
  forms; transforming the input keeps one rendering of each.
- **Two flags on the clause, or a combined consolidated-and-flattened
  form.** Rejected: the combination is out of scope for the accepted
  variants, and two flags make the invalid state representable.
- **Gate at the parser.** Rejected: the statement-admission detector is the
  one place every graph-type gate already lives, and it yields the authored
  error rather than a syntax error naming the new words.

## Consequences

- The proposal's own tables differ from this engine's canonical text in
  places the variants inherit (identifying labels carry `=>`, property sets
  sort by rendered text, identified labels' constraints sort first); one
  proposal table also omits constraints its own rule inlines. Those follow
  the rule and the existing canonical form.
- Flattened output can show conflicting rules from different ancestors on
  one label; it is a view, not a type to set.
- The gate lifts when the proposal joins the pinned dialect floor.
