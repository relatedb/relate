# ADR-0593: Hide the `system_ontologies` namespace behind the ontology preview opt-in

Status: Proposed

Claim: 1a477fc2a38fd2b9a1f1dbf41b2e066877a51ef2

## Context

ADR-0116 gave every store three fixed namespaces; ADR-0229 kept
`system_ontologies` listed but not queryable, its 42N17 text pointing at
the `db.ontology.*` procedures; ADR-0378 gated those procedures behind
the `ontology` row of `RELATE_PREVIEW` because their contract tracks a
non-final external spec (the Ontology Graph Spec and the
ontology-privileges proposal).

The namespace itself still advertises that feature unconditionally.
`SHOW DATABASES` lists the row in the flag-off default, the 42N17 text
names the procedures, and `USE`, session connect and `ALTER DATABASE`
all accept the name — a caller can discover and target a surface whose
shape upstream may re-cut, exactly the condition ADR-0378 gated the
procedures for. The default listing also matches nothing a live Neo4j
edition reports: no edition ships a `system_ontologies` database, while
the flag-off two-row shape (the user database and `system`) is exactly
Community Edition's listing.

## Decision

While the `ontology` feature is disabled, `system_ontologies` is
invisible everywhere the dialect can name, list or select a database.

- **`SHOW DATABASES` omits the row** — the flag-off default is CE's
  two-row listing — and the named `SHOW DATABASE` form returns no rows.
- **Every route to the name fails with the site's ordinary
  unknown-database rejection**: `USE system_ontologies` (the element-id
  segment route included) with 42N00 `Graph not found`; session connect,
  typed and by name, and `set_namespace` the same; `ALTER DATABASE
  system_ontologies` as an unknown target, its `IF EXISTS` form a no-op.
  Never the authored preview error — ADR-0371's name-gate rule: an error
  must not advertise a name the catalog does not list.
- **The mechanism is a mapping, not inline checks.** A
  fixed-namespace → preview-feature table in the namespace registry —
  the namespace twin of the executor's procedure-to-feature mapping
  (ADR-0378) — with the visibility predicate derived from it. Every
  resolution site consults the one table; a future gated namespace adds
  one row and no new machinery. ADR-0378's exception class widens from
  a procedure family to the feature's whole dialect surface: the
  procedures and the namespace hosting the stored document are one
  surface, one registry row, one opt-in.
- **Gate placement.** The visibility arm runs after the procedures
  preview gate — ADR-0378's pinned first position stands — and before
  `CALL` validation and the later gates, so a hidden namespace is
  indistinguishable from a name that never resolved.
- **Store format unchanged.** The databases catalog keeps its durable
  `system_ontologies` entry — the opt-in is per-open, not durable — so
  an existing store lists two rows while off and three while on; the
  42N17 text is unchanged and reachable only with the opt-in set.
- **Removal rides with ADR-0378's**: when the spec and the proposal are
  final, the mapping row and the gates are deleted with the registry
  row.

## Alternatives considered

- **Keep the namespace unconditionally listed.** Rejected: the listing
  and the 42N17 text advertise the same non-final surface ADR-0378 hid;
  discovery must agree with callability.
- **Fail hidden routes with the authored preview error.** Rejected: it
  advertises the name, the exact form ADR-0371's name-gate rule forbids;
  the ordinary rejection is the honest disabled form.
- **Remove the namespace until the spec is final.** Rejected: ADR-0229
  chose listed-not-queryable over removal; the durable catalog row, the
  ontology WAL format and the procedures' target semantics depend on
  the namespace existing, and removal breaks stored databases.
- **Hide the listing but keep the name addressable.** Rejected: a
  half-visible namespace keeps advertising through the 42N17 text and
  answers `USE` with a gate no catalog row explains.
- **A second registry row for visibility.** Rejected: one feature, one
  opt-in; two rows admit combinations (procedures without the
  namespace, or the reverse) the contract cannot explain.
- **Gate inside the planner's `USE` resolution.** Rejected: the planner
  is deliberately preview-blind; the gate arm over the resolved
  namespace is observably identical — same 42N00 kind and text —
  without widening the planner's inputs.

## Consequences

- **Breaking in the flag-off default:** the listing loses the row and
  every name route to the namespace fails 42N00; a `CHANGELOG` entry
  under Unreleased records it and the remedy.
- **CE parity improves by default** — two rows is Community Edition's
  exact listing.
- **WASM** has no process environment, so the namespace is invisible
  there like the procedures; stored documents still open and reload.
- **The spec harness** (separate repository) runs its multi-database
  scenarios with the opt-in; a flag-off run reports a listing diff —
  reporting-only, never a gate.
- **Tests:** suites pinning the visible namespace opt in through the
  shared preview fixture; the fail-closed suite pins the listing,
  `USE`, connect, `set_namespace`, `ALTER DATABASE` and the element-id
  route; golden-store compatibility pins an old store flag-off — two
  listed rows, durable catalog row intact.
- **Client surfaces need no code change:** names stay valid wire shape
  and the engine refusal renders as the ordinary connect error. Docs,
  examples and the CLI skill references change in the same change.
