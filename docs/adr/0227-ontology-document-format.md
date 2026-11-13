# 0227. Ontology document format: JSON or YAML, stored verbatim

- **Status**: Accepted
- **Date**: 2026-10-12
- **Deciders**: CypherLite maintainers (Linear LABS-132)

## Context

ADR-0120 §1 stores "the validated ontology JSON for `neo4j`, kept
verbatim", and §5 carries it as one immutable
`ontology/<128-bit-hex>.json` snapshot object. ADR-0121 §1 defines
`db.ontology.set` as validating the document against a supported Ontology
Spec version — a newer spec version failing while naming the highest
supported version — and `db.ontology.get` as returning the stored document.
Neither ADR pins the accepted syntaxes, the parser, the validation
mechanism, or the version gate's exact rule. ADR-0121 §5's mapping deferral
is resolved by ADR-0226, whose mappings need a validated, parsed document
model to build on.

Since those ADRs, Neo4j has published its ontology-privileges proposal
(first version 6 Oct 2026, revised 8 Oct 2026 — the
revision that removed the database argument from the procedures). Its
procedure surface is `db.ontology.set(ontology :: STRING)`,
`db.ontology.get(format = "yaml" :: STRING) :: (ontology :: STRING)`,
`db.ontology.drop()` and `db.ontology.apply()`: no database argument — the
procedures act on the database they are executed against (the session
database, or the target of a `USE` clause) — and `get` returns the stored
ontology *in the requested format*, defaulting to YAML because it is more
compact in agent context. An optional database argument is a documented
future extension ("can be added later without breaking anything"). Per
ADR-0121 §1's own rule — "Where Neo4j's final signatures differ …
CypherLite follows Neo4j" — CypherLite follows the proposal. Its privilege
machinery (`SHOW ONTOLOGY`, `SET ONTOLOGY`, `ONTOLOGY MANAGEMENT`,
`EXECUTE BOOSTED`) stays out of scope per ADR-0121 §4.

The Ontology Graph Spec's proposal writes every example in YAML while its
schema is JSON Schema (draft 2020-12); both syntaxes denote the same
documents, and the pinned draft schema (`neo4j/graph-spec` @
`2f8dcbdf2dab51ebf2d760e00904a8f2b0518f59`) validates a JSON-equivalent
value model either way. The mapping code lives in
`cypherlite/cypherlite-ontology` (ADR-0121 §6, an engine crate), and the
workspace today has no YAML dependency, so the crate choice is an R6
heavy-dependency decision (ADR-0103) recorded here.

## Decision

1. **Signatures follow the proposal.** `db.ontology.set(ontology :: STRING)`,
   `db.ontology.get(format = "yaml" :: STRING) :: (ontology :: STRING)`,
   `db.ontology.drop()` and `db.ontology.apply()`. There is no database
   argument: each procedure acts on the database it is executed against —
   the session database or the `USE` target (ADR-0116 §3). Executed against
   `system` or `system_ontologies`, the procedures fail with ADR-0121 §2's
   `Ontologies can only be stored for the user database 'neo4j'.`, now
   applied to the session/USE target rather than to a database argument; an
   unknown `USE` target fails at routing with 42N00 (ADR-0116 §3) before
   this surface is reached. The optional database argument may return as a
   future extension, without breaking these signatures.
2. **JSON or YAML in; the stored bytes stay verbatim.** `set` accepts a
   document in JSON or YAML 1.2. The submitted string is stored verbatim —
   ADR-0120 §1's "the validated ontology JSON kept verbatim" is amended to
   "the validated ontology document, JSON or YAML, kept verbatim" — and the
   ADR-0120 §3 16 MiB envelope limit applies to the submitted string
   regardless of syntax. The store records no JSON-vs-YAML marker — only
   the bytes.
3. **`get` returns the requested format.** `format` is `"yaml"` (the
   default — the proposal's choice: more compact than JSON, so cheaper in
   agent context) or `"json"`; any other value fails with an argument error
   naming the two. When the requested format matches the stored one, `get`
   returns the stored bytes unchanged — YAML in, YAML out, comments and
   formatting included; likewise JSON. Otherwise it parses the stored
   document into the value model of (4) and serializes it canonically in
   the requested format: compact UTF-8 for JSON, block style with
   two-space indentation for YAML, in both cases preserving the document's
   key order, which the value model carries. Conversion never touches the
   stored bytes; a later `get` in the stored format still returns them
   unchanged. The stored format is detected at read time by a strict JSON
   parse: a stored document that parses as JSON counts as JSON, anything
   else as YAML.
4. **One parse path.** The string is parsed once as YAML 1.2 (a superset of
   JSON) into a JSON-equivalent value model: maps with string keys in
   source order, lists, strings, integers, floats, booleans, null. Source
   key order is preserved — ADR-0226's deterministic id assignment and
   (3)'s canonical serialization depend on it. Anything outside that model
   — non-string mapping keys, custom tags, non-finite floats — is rejected
   at parse time. Validation, version gating, graph derivation and
   format conversion all consume this one model, so both syntaxes behave
   identically after parsing.
5. **YAML crate: `saphyr`.** `cypherlite/cypherlite-ontology` depends on
   `saphyr` (with `saphyr-parser`) for YAML parsing and serialization. R6
   justification: pure safe Rust, no build script and no native code, a
   small transitive tree, YAML 1.2 semantics matching the spec's examples,
   and the actively maintained line of the yaml-rust family. `set` never
   re-serializes; the only product-path serialization is a converting
   `get` (3), which emits from the parsed value model and leaves the stored
   bytes alone.
6. **Hand-rolled validation against the vendored schema.** The pinned draft
   schema is vendored as `ontology-graph-spec.schema.json` in
   `cypherlite/cypherlite-ontology` (source: `neo4j/graph-spec` @
   `2f8dcbdf2dab51ebf2d760e00904a8f2b0518f59`, the same pin ADR-0226's
   mappings target). Validation is hand-rolled structural checking of
   exactly that schema, over the value model from (4): the required root
   fields (`schema`, `id`, `version`); `additionalProperties: false` at the
   levels the schema pins; `version`, `dimension` and the endpoint count
   fields as integers with their minimums; the property `type` token
   grammar (bare scalars, `ANY`, `LIST<…>`, `LIST<ANY>`,
   `VECTOR<INTEGER|FLOAT>`) parsed directly; the `constraint_type` enum
   with a non-empty `properties` list; non-empty property names; endpoint
   objects with required `node`; and the custom extension envelope (`type`
   required). `format: uri` / `format: regex` are annotations and are not
   checked, as JSON Schema prescribes; extension payloads are never
   validated, which is the schema's own rule. Fixture tests pin the
   validator against the vendored schema's example document and per-clause
   cases.
7. **Spec-version gate.** The root `schema` field must equal exactly
   `https://neo4j.com/ontology-graph-spec/1.0.0/schema.json` — the pinned
   1.0.0 draft — and is checked before structural validation, so a
   newer-version document fails with the version error rather than
   incidental structural errors. Any other value fails with
   `Unsupported ontology spec version: '<uri>'. This version of CypherLite
   supports up to Ontology Graph Spec 1.0.0
   ('https://neo4j.com/ontology-graph-spec/1.0.0/schema.json').`
   Supporting a newer spec version is a new ADR: a new vendored schema, a
   validator update, and a raised gate.

## Alternatives considered

- **Keep the database argument** (ADR-0121 §1 as written). Rejected:
  The proposal's 8 Oct 2026 revision removed it, and ADR-0121 §1 binds
  CypherLite to Neo4j's signatures; the session/USE-target rule preserves
  §2's `neo4j`-only restriction without an argument.
- **JSON only.** Rejected: the spec's human wire format reads as YAML — its
  proposal writes every example in YAML, and its `get` defaults to
  YAML — and verbatim storage makes YAML support cheap: it changes the
  parse step, not the store.
- **Normalize to canonical JSON on `set` and return that from `get`.**
  Rejected: it breaks round-trip fidelity (comments, key order cosmetics,
  YAML syntax) and ADR-0120's verbatim principle, for a normalization no
  consumer needs — the derived graph is already canonical (ADR-0226), and
  `get` converts on request.
- **Record a format marker in the WAL or snapshot.** Rejected: it would
  change ADR-0120's store formats for one bit that a strict-JSON sniff
  derives exactly — any stored document that parses as JSON is JSON.
- **Convert on every `get`, even when the formats match.** Rejected: it
  breaks the verbatim round-trip for no benefit; the proposal asks for the
  ontology in a format, not for normalization.
- **`serde_yaml`** (the original). Rejected: the repository was archived in
  2024 and receives no fixes; a new engine dependency must not start
  unmaintained.
- **`serde_yaml_ng`.** Rejected: it pulls `unsafe-libyaml` — C transpiled
  to unsafe Rust — into an engine crate, and inherits libyaml's YAML 1.1
  lineage, whose scalar resolution differs from the spec's YAML 1.2
  examples; its serde pipeline buys nothing because validation is
  hand-rolled.
- **`yaml-rust2`.** Rejected in favor of its successor: maintenance of the
  yaml-rust2 line has moved to `saphyr`, the actively developed,
  YAML-1.2-focused fork with the same pure-Rust, no-unsafe profile.
- **A full `jsonschema` crate.** Rejected: a complete JSON Schema 2020-12
  engine and its transitive tree (regex machinery included) to validate
  against one fixed, vendored schema. Hand-rolled checks of exactly that
  schema are smaller and audit-friendly; the correctness risk is bounded by
  fixture tests that pin validator behavior per schema clause.
- **No validation** (store any well-formed document). Rejected: ADR-0120
  stores the *validated* document, and ADR-0226's derived-graph build
  assumes the schema shape.
- **Lenient version handling** (accept any `1.x`, or ignore the URI).
  Rejected: silently misreading a newer document's semantics is worse than
  failing loudly; the error names the highest supported version
  (ADR-0121 §1).

## Consequences

- Round-trip fidelity on every surface (Rust, the C ABI clients, CLI, Bolt,
  WASM): users who ask for the stored format get their own bytes back, YAML
  or JSON; a cross-format `get` gets a canonical conversion with the
  document's key order intact.
- The workspace gains its first YAML dependency; this ADR is its R6 record.
  `cargo xtask check-boundaries` is unaffected: `cypherlite-ontology` is an
  engine crate depending on engine crates plus `saphyr`, `serde` and
  `serde_json`.
- Bumping the supported spec version is an explicit, reviewable act (a new
  ADR), never a dependency upgrade.
- Because YAML 1.2 is a JSON superset, a JSON document and its YAML twin
  validate and derive identically; only the stored bytes differ.
- Older binaries already refuse stores holding an ontology (ADR-0120 §8),
  so YAML documents need no extra format gate.
- The procedures compose with ADR-0116 routing: `USE system_ontologies
  CALL db.ontology.get()` fails with §2's ontology text, and `USE nope …`
  fails with 42N00 at routing.
- Tests cover: JSON and YAML round trips through `set`/`get`, both
  conversion directions with key order preserved, the `"yaml"` default,
  the stored-format sniff, the pinned schema's example document, the
  version gate's error text, rejection of non-string keys and custom tags,
  per-clause validator fixtures, and the `system`/`system_ontologies`
  failures.
- ADR-0120 and ADR-0121 gain Later references notes.

## Later references

- ADR-0229 tightens §1's `system_ontologies` composition rule: the
  procedures routed there now fail ADR-0116 §4's namespace gate with
  ADR-0229's 42N17 text (`This database is not queryable. …`) before
  `Ontologies can only be stored for the user database 'neo4j'.` is
  reached — that text now fires only on `system`. The Consequences
  composition example `USE system_ontologies CALL db.ontology.get()`
  fails with the new text. The `system` half and the rest of §1 stand.
- ADR-0364 raises §7's gate to main's
  `https://neo4j.com/docs/ontology-graph-spec/1.0.0/schema.json` and
  replaces §6's vendored draft schema and hand-rolled validation with
  `neo4j/graph-spec` main @ `066515593ae59317d758bcf2cef54195e74bde39`.
  The exact-URI gate survives with the new URI as CypherLite policy
  (main leaves `schema` an unconstrained string); the old URI fails.
  Typed extension members are now validated deeply, superseding §6's
  "extension payloads are never validated" for everything except
  `custom` objects. §2/§3 (verbatim store, format conversion) stand, and
  stored documents are never re-validated against either schema.
