# ADR-0378: Gate the `db.ontology.*` procedures behind `CYPHERLITE_PREVIEW`

Status: Proposed

Claim: 74fe11e0a20154c0acbd649101a091ea1698a860

## Context

LABS-402 asks to gate the ontology procedures behind the
`CYPHERLITE_PREVIEW` flag. ADR-0371 introduced that flag as one global,
fail-closed opt-in (`CYPHERLITE_PREVIEW=<kebab-name>,…`, read once at
`Database::open`) and set a deliberate scope policy: only
**syntax-extending** features gate; a purely additive function or
procedure name needs no opt-in and never enters the registry.

ADR-0121 §6 put the four procedures (`db.ontology.set`, `get`, `drop`,
`apply`) in every build on purpose — they are part of the Cypher surface
in all CLI bundles, the Rust crate, the language bindings, Bolt and WASM,
and they are explicitly not a Cargo feature.

Both rules collide for this one family because its contract does not sit
on our own pin. The procedures validate and return documents of the
Ontology Graph Spec 1.0.0, which is still a draft: ADR-0227 §6 pinned
`neo4j/graph-spec` @ `2f8dcbdf2dab51ebf2d760e00904a8f2b0518f59`, and
ADR-0364 had to move the pin to `main` (`066515593ae59317d758bcf2cef54195e74bde39`)
because upstream's draft and `main` both call themselves 1.0.0 while
differing in 25 structural places. The ontology-privileges proposal is
likewise still under review (first version 6 Oct 2026, revised 8 Oct
2026). A caller can therefore write documents, parse `get`'s output, or
build tooling against a shape that upstream may change again — the same
"surface may change under us" condition that made ADR-0229 remove the
`system_ontologies` Cypher surface, except that ADR-0229 kept the
procedures as the only Cypher route to the stored ontology.

The question this ADR answers is whether that family may gate under
ADR-0371 despite not extending syntax, or must ship unconditionally
under ADR-0121 §6.

## Decision

The `db.ontology.*` family is the first, narrow exception to ADR-0371's
syntax-only scope policy, and the exception is recorded where the policy
lives.

- **Registry row.** `PreviewFeature::Ontology` is appended to
  `PREVIEW_FEATURE_TABLE` in `cypherlite/cypherlite-core/src/preview.rs`
  with the kebab-case name `ontology`; its doc comment names this ADR and
  the removal condition below. No other spelling of the name exists.
- **All four procedures gate**, not only the writers:
  `db.ontology.set`, `db.ontology.get`, `db.ontology.drop` and
  `db.ontology.apply`.
- **One owner for the resolved set.** The set lives on the store-level
  executor services (`StoreServices`, ADR-0116 §10), installed at open and
  carried by a follow reload. `Shared.preview` is gone; the statement
  admission that gates hybrid-search `COMBINE` (ADR-0379) captures
  `Graph::preview()` under a read guard before parsing, and the `CALL`
  gate and the `SHOW PROCEDURES` catalog read the same bundle. ADR-0379's
  "first production reader of `Shared.preview`" sentence is superseded
  here; its Later references note points at this decision.
- **The disabled error** is ADR-0371's authored text, unchanged:

  ```
  semantic error: SemanticError: preview feature 'ontology' is not enabled: set CYPHERLITE_PREVIEW=ontology
  ```
- **Placement.** The gate is a compile-time check in
  `Connection::plan_gate_classify` (`cypherlite/cypherlite/src/gate.rs`),
  after planning and **before** compile-time `CALL` validation. The
  preview error therefore precedes arity and `YIELD` errors and the
  ADR-0116 §4 / ADR-0229 namespace gates: with the gate off, a call that
  is also malformed or routed to `system_ontologies` reports the preview
  error; with the gate on, every later gate runs exactly as today (the
  42N17 text included). Because it sits above the EXPLAIN early-return
  and is reached by every statement path, it covers nested calls
  (`CALL { … }` bodies, expression subqueries) and `EXPLAIN`/`PROFILE`
  wrappers in implicit and explicit transactions alike. The plan walk is
  the existing full-descent one, so no new pass is added.
- **Discovery agrees with callability.** While disabled, `SHOW
  PROCEDURES` omits the four rows in every composed form — plain,
  `YIELD`/`WHERE`, `UNION`, and `EXISTS`/`COUNT` subqueries; while
  enabled it lists them unchanged. The catalog and the call gate read the
  same store-level set, so no composition can list a row that cannot be
  called.
- **One set per store.** The preview set lives on the store-level
  executor services — the bundle that already shares the LOAD CSV policy
  across all three namespace graphs (ADR-0116 §10) — installed at open
  and re-installed on follow reload, so every namespace and every
  session/transaction of the store sees the same opt-in.
- **Persistence is unaffected.** The gate governs the Cypher procedure
  surface, not stored state: open, WAL replay (including `MDBWAL02`
  segments), checkpoint and follow reload keep working with the gate
  off, and a store that holds an ontology opens fine.
- **The hidden Rust facades are not gated.** `Database::ontology_document`,
  `Database::commit_ontology_set` and `Database::commit_ontology_drop`
  keep working without the opt-in; they exist for the engine's own
  persistence tests and for hosts that manage the document directly, and
  they are `#[doc(hidden)]`, not the public procedure contract.
- **42N17 is unchanged.** The ADR-0229 §2 text (`This database is not
  queryable. Use the db.ontology.* procedures on the user database
  'neo4j' instead.`) keeps its wording and its position after the
  preview gate.
- **Scope-policy amendment.** ADR-0371's policy gains one narrow clause: a
  built-in procedure family whose contract tracks a **non-final external
  spec** may gate even though it does not extend syntax. The exception is
  not general — additive function names and procedure families on our own
  pinned contract still do not gate — and it ends with this family.
- **Removal.** Once the Ontology Graph Spec is final and the
  document/format contract is pinned, the registry row and the gate are
  removed in that same change and the four procedures become
  unconditional again, exactly as ADR-0371 requires when a feature
  reaches its floor.

## Alternatives considered

- **Leave the procedures ungated.** Rejected: LABS-402 is precisely the
  ask, and a caller would keep building document tooling against a draft
  upstream has already re-cut once (ADR-0364's 25 structural changes),
  with breakage arriving silently at the next re-pin.
- **A Cargo feature (`--features ontology`).** Rejected: ADR-0121 §6
  already rejected it — every build ships the procedures and the feature
  only multiplies build/test combinations. It also cannot express a
  runtime opt-in for a single binary or embedded host, and it would
  bifurcate the ABI/WASM artifact.
- **Gate only the write procedures (`set`, `drop`, `apply`).** Rejected:
  `get` returns and converts documents in the same non-final format
  (ADR-0227 §2/§3), so a read-only caller would still pin the draft
  shape; it also splits one family's availability into two rules that
  every surface would have to document and test.
- **Keep listing the four rows in `SHOW PROCEDURES` while disabled.**
  Rejected: discovery would advertise procedures that cannot be called,
  and a client that enumerates the catalog before calling would hit the
  preview error with no prior signal; `SHOW` must agree with
  callability.
- **Fail with `ProcedureNotFound` instead of the authored error.**
  Rejected: it hides both the reason and the remedy. The caller would
  conclude the procedure does not exist rather than that this build
  needs `CYPHERLITE_PREVIEW=ontology`, and the fail-closed contract of
  ADR-0371 exists to make that remedy explicit.
- **A dedicated env var such as `CYPHERLITE_ONTOLOGY`.** Rejected:
  ADR-0371 rejected per-feature env vars — N flags for N features, no
  shared registry, typos that silently no-op instead of failing closed;
  the one comma-separated registry is the whole point of the mechanism.
- **Gate at executor dispatch instead of at plan time.** Rejected: an
  `EXPLAIN` would succeed while the statement cannot run, arity and
  `YIELD` errors would mask the preview error, and conditionally
  executed nested calls could escape the check entirely.
- **Remove the procedures until the spec is final.** Rejected: ADR-0121
  §6 keeps the code in every build and ADR-0229 made the procedures the
  only Cypher route to the stored ontology; removing them would make the
  stored state unreachable for Bolt and embedded callers instead of
  merely opt-in.

## Consequences

- **Breaking change for existing callers.** A `db.ontology.*` call that
  worked before now fails with the authored preview error until the
  caller sets `CYPHERLITE_PREVIEW=ontology`. A `CHANGELOG` entry under
  Unreleased records the opt-in, the error and the four affected
  procedures.
- **WASM is unreachable.** The browser build has no process environment,
  so the open path resolves no preview features and the four procedures
  cannot be called there; stored documents still open, replay,
  checkpoint and reload. A future WASM opt-in would need its own
  mechanism, not a second gate.
- **Tests and the Rust ontology example opt in.** The integration tests
  covering the procedures and `cypherlite/cypherlite/examples/ontology.rs`
  set `CYPHERLITE_PREVIEW=ontology` before `Database::open`; dedicated
  fail-closed tests keep the unset case pinned, and the plan-gate tests
  assert the error ordering (preview error before arity/`YIELD` and
  42N17).
- **CLI agent skill guidance changes.** The CLI skill bundle's client
  reference must say that ontology management needs
  `CYPHERLITE_PREVIEW=ontology` (the env table already documents the
  variable per ADR-0371), so an agent does not diagnose the authored
  error as a missing build capability.
- **The code still ships in every build.** ADR-0121 §6 holds for the
  code — the crate dependency, the four signatures and their executor
  arms remain compiled into every bundle, binding and WASM artifact; only
  callability is gated. No `cfg`, no feature flag.
- **The exception is temporary by construction.** The registry row and
  the gate are deleted when the spec is final, so the registry does not
  accumulate procedure families that stay gated past their contract.
