---
type: adr
area: cypher
claim: 0961c01a3883f3b3ee8d14514fc917e1cc12737a
date: 2026-11-22
deciders: Relate maintainers
name: 0662-preview-gate-sites
status: proposed
tags:
  - dialect
title: Preview registry rows declare their gate sites; the front derives its checks from them
REFERENCES:
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0378-ontology-procedures-preview-gate]]"
  - "[[adr/0497-catalogue-corpus-alignment]]"
  - "[[adr/0593-system-ontologies-visibility-preview-gate]]"
---

# ADR-0662: Preview registry rows declare their gate sites

## Status

Proposed — refines [ADR-0371](./0371-preview-feature-gating.md)'s registry.
It extends and replaces the `GateSite` the registry already carries: that
first form recorded only the sites the front itself consults, in three
payload-free variants (`Lexer`, `Parser`, `AstDetector`), with the front's
own tests checking them.

## Context

[ADR-0371](./0371-preview-feature-gating.md) makes the preview registry
(`relate-core/src/preview.rs`) the one table of feature names. It does not
record where each feature's gate is checked. That fact lived in the pipeline
itself: the lexer, the parser, the front's AST detector loop, validation,
the function registry, procedure dispatch, the namespace registry,
expression evaluation and the executor's graph-type validation each name
the features they gate. The front's detector list therefore re-spelled the
registry's vocabulary, and nothing failed when a registry row and a
consumer disagreed: a row could gate at a site that never read it, and a
site could read a feature the registry did not list as gated there.

## Decision

1. **Each registry row declares its gate sites, typed.** A row carries,
   beside its variant, doc and kebab-case name, a list of `GateSite` values
   (empty when the gate sits somewhere no test reads). The earlier
   three-variant `GateSite` and its front-only scope are replaced by a
   typed `GateSite`: `ParseOption(ParseOption)` names the `ParseOptions` field a
   parse-site feature switches on, `AstDetector(DetectorKind)` names the
   front's AST detector, and `NameAdmission`, `ProcedureDispatch` and
   `Namespace` carry no payload. `PreviewFeature::gate_sites()` exposes the
   list, derived from the same macro invocation as the name, so a row
   cannot exist without its sites.
2. **The front derives from the rows.** The AST detector loop and the
   front's other registry-driven checks select features by asking for the
   ones whose declared sites include theirs, instead of keeping a second
   list of feature names. `relate-core` holds the `ParseOption` and
   `DetectorKind` payloads as plain data — a site names which option or
   detector it is, with no behavior attached — and `relate-front` applies
   them through total matches (`apply_parse_option` over `ParseOption`,
   `detector_uses` over `DetectorKind`), so an unhandled option or detector
   is a compile error rather than a silent skip.
3. **A consumer-consistency test pins the declaration.** For each
   site that has a readable consumer (`NameAdmission`, `ProcedureDispatch`,
   `Namespace`), a test in `relate` asserts that the features the
   consumer's own table or mapping reads equal the features whose rows
   declare that site. The `ParseOption` and `AstDetector` sites need no
   such test: the front's total matches are the compile-time guarantee that
   each row's declared site agrees with the option or detector the front
   supplies. Adding a gate without a row site, or a row site without a
   gate, fails.

### Site taxonomy

| Site | Where the gate is checked |
| --- | --- |
| `ParseOption(<ParseOption>)` | the lexer or parser reads the feature through its parse options |
| `AstDetector(<DetectorKind>)` | the front's AST detector loop rejects the syntax |
| `NameAdmission` | the function registry admits or hides a name |
| `ProcedureDispatch` | procedure dispatch gates a built-in family |
| `Namespace` | the namespace registry hides a fixed namespace |

The registry rows list which features sit at each site; this table does not.
`PreviewFeature::gate_sites()` in `relate-core/src/preview.rs` is the one
membership list — a feature's sites are read from its row, never copied
here. A feature gated elsewhere (statement validation, run-time evaluation,
the executor's graph-type layer) declares no site.

`ParseOption` is decided while the statement is parsed; every other site is
decided after the AST exists. A feature may declare several sites
(`path-construction`, `ontology`, the graph-type rows).

The taxonomy records where a gate is checked, not how it fails: the
disabled forms stay as [ADR-0371](./0371-preview-feature-gating.md),
[ADR-0378](./0378-ontology-procedures-preview-gate.md),
[ADR-0497](./0497-catalogue-corpus-alignment.md) and
[ADR-0593](./0593-system-ontologies-visibility-preview-gate.md) define them.

## Alternatives considered

- **Leave the sites in the consumers.** Status quo; the registry and the
  pipeline drift silently, as in Context.
- **A per-site feature list owned by each consumer.** A second table over
  the same vocabulary; it is the drift this decision removes.
- **Derive the sites by scanning the source.** Shape-sniffing over text;
  fragile and not checkable at compile time.

## Consequences

- A new preview feature states its sites in its registry row; the front
  picks it up without editing a detector list.
- A removed feature drops its row, and its sites go with it.
- The consistency test is the guard that the declared sites are the real
  ones; a new site variant needs a consumer and a test case.
- No Cypher syntax, error text or environment variable changes.
