# ADR-0371: Preview Cypher feature gating via CYPHERLITE_PREVIEW

Status: Proposed

Claim: 178ecb332c29ceada32771c6ae1cb9c991605dba

## Context

Upcoming Cypher work — hybrid search `COMBINE`, local
callables, and the graph-type optional properties / default values /
implied labels — is accepted into the language but not part of our pinned dialect
floor (the openCypher TCK 2024.3 pin, ADR-0188). Two bad options existed:
shipping the syntax unconditionally (it would widen the dialect floor
before the TCK pin moves, breaking the "TCK is the floor" contract in
`docs/tck/compliance.md`), or shipping each feature behind its own ad-hoc
env var / flag (a flag grammar per feature, no single registry, no
consistent fail-closed behavior).

A language-contract decision was needed before any of those features
lands: one mechanism every later feature PR plugs into.

## Decision

One global opt-in, `CYPHERLITE_PREVIEW`: a comma-separated list of
kebab-case feature names, read once at `Database::open` (the same
config/open path as `CYPHERLITE_MEMORY_LIMIT` and the ADR-0209 bulk-batch
knobs, fail-closed on a bad value).

- **Registry**: one canonical feature-name table in the engine,
  `cypherlite-core/src/preview.rs` (`PREVIEW_FEATURE_TABLE`). Every
  spelling, lookup, and the known-names list derives from that single
  table; a feature PR adds its row and gates its syntax — nothing else
  names the feature. Names reserved now (their features land in later
  PRs): `hybrid-search`, `local-callables`, `graph-type-opt-props`,
  `graph-type-default-values`, `graph-type-implied-labels`.
- **Gate**: a typed predicate, `PreviewFeatures::enabled(feature)` /
  `require(feature)`, checked where the feature would otherwise be parsed
  or dispatched. This PR ships the mechanism only — the registry, the
  gate, the error, the docs, and the tests — with no real feature behind
  it; a representative gate fixture
  (`cypherlite/src/tests/preview.rs`) exercises the store-level set the
  open path resolves.
- **Fail-closed semantics**: unset or empty enables nothing; an unknown
  name in the list (including an empty segment) fails `Database::open`
  with a loud typed usage error naming the known names — never a silent
  ignore. A gated feature invoked while disabled fails with the authored
  error, in the house style of the ADR-0229 §2 authored text:
  `semantic error: SemanticError: preview feature '<name>' is not enabled:
  set CYPHERLITE_PREVIEW=<name>`.
- **Scope policy**: only **syntax-extending** features gate. A purely
  additive function name needs no opt-in (the dialect floor already admits
  new function names), so it never lands in the registry. Each feature PR
  states which kind it is.

## Alternatives considered

- **Ship each feature unconditionally, moving the dialect floor per
  PR.** Rejected: the TCK pin is the language contract
  (`docs/tck/compliance.md`); widening the floor ahead of the pin would
  either break the compliance claim or require re-deriving the pin per
  feature, and the syntax would be user-visible before it is contractual.
- **One env var per feature (`CYPHERLITE_HYBRID_SEARCH=1`, …).** Rejected:
  N flags for N features with no shared registry; unknown-name typos would
  silently no-op instead of failing closed; each binary/surface would
  re-document its own knob.
- **A `--preview` CLI flag instead of an env var.** Rejected: the opt-in
  belongs to the engine (every surface — CLI, Bolt, HTTP, MCP, embedded
  clients — opens the same `Database`), and it is configured once per
  store, not per CLI invocation; the env var flows through the single
  existing open-config path, and no new CLI flag grammar is invented
  (AGENTS.md CLI rules).
- **A database-level stored setting (`ALTER DATABASE … SET PREVIEW`).**
  Rejected: the supported database-administration set is fixed by
  ADR-0116, and a stored setting would make the dialect floor
  store-dependent and persistent across versions — the preview window is
  a process-level decision, not a data one.

## Consequences

- A user opting into a preview feature gets syntax ahead of the dialect
  floor; queries using it are not portable to the pinned floor and may
  change as the CIP evolves. The opt-in is explicit and loud either way.
- The registry is engine-owned (`cypherlite-core`), so the parser, planner,
  and any surface can consult it without new dependency edges
  (ADR-0103 R1).
- Each feature PR is now cheap and uniform: add the registry row, gate the
  syntax at its parse/dispatch point, extend the tests. When a feature
  reaches the pinned floor, its gate and registry row are removed in that
  same change (the feature becomes unconditional).
- This PR ships the engine wiring with no production reader of the
  resolved set yet; the `Shared` field carries a documented
  `cfg_attr(not(test), allow(dead_code))` until the first feature PR
  (hybrid search) lands its call site. No dead wrapper method ships — the
  first real call site arrives with the feature it gates.
- The env var is documented in `docs/api/cli/spec.md`'s environment table
  and in AGENTS.md's Cypher dialect section; no CLI flag is added, so no
  `--help` or flag-table change rides with feature PRs.

## Later references

- ADR-0593 records a third gate class inside ADR-0378's exception family:
  a fixed namespace's visibility gates through a namespace-registry
  mapping (the namespace twin of the procedure mapping), and its disabled
  form is the name-gate rule in namespace form — the site's ordinary
  unknown-database rejection, never the authored error. The one row is
  `system_ontologies` behind `ontology`; it is removed with ADR-0378's
  row once the spec is final.
- ADR-0378 amends the scope policy: a built-in procedure family whose
  contract tracks a non-final external spec may gate even though it does
  not extend syntax. The one row is `ontology` (the `db.ontology.*` family,
  Ontology Graph Spec 1.0.0 not final); it and its gate are
  removed once that spec is final. Syntax-only remains the default.
- ADR-0497 (the Cypher-25 catalogue alignment) adds a further exception and
  the first **name** gate: a built-in function name an approved CIP adds but
  no release ships, or that the corpus records as unreachable, is admitted
  only while its `CYPHERLITE_PREVIEW` flag is on. Admission is the front's
  `validate_function_admission`, a pre-binder AST walk over the parsed
  statement driven by the `PREVIEW_FUNCTIONS` name table, so the gate is name
  admission, not a parse/AST detector; `AGGREGATE_ALIASES` is untouched and
  needs no per-spelling gate, because the walk rejects a spelling before
  lowering can canonicalize it onto `stDev`/`stDevP`. Its disabled form is
  therefore the binder's ordinary `UnknownFunction` rejection (42N48),
  deliberately not the authored preview error above — a gate that hides a
  name cannot name it in an error without advertising it. Syntax-only remains
  the default; three rows carry the class (`coll-zip`, `coll-has`,
  `gql-stddev-aliases`), each removed with its CIP's release.
