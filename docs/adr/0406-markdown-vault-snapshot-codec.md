# ADR-0406: Markdown vault snapshot codec — the database runs on a folder of markdown files

Status: Proposed

Claim: 9e072ad4a56b1918616b8915300e11c1a13ed0c9

## Context

CypherLite's durable form is a fsync'd WAL plus a snapshot written by a
snapshot codec. Two codecs exist (ADR-0050): Parquet (sharded) and JSON
(`graph.json` full rewrite), selected by `SnapshotCodecKind` and dispatched
at exactly two sites (`checkpoint_at` in `cypherlite-storage/src/lib.rs`,
`load_with_checkpoint_lsn` in `src/open.rs`). The VFS below them is a
`dyn Vfs` trait object; Native directories, `:memory:` and `.cl` bundles
are the routes today. The WAL is codec-independent.

A prior exploration (the `markdown-mode` branch of the lpgfs project)
established the graph↔markdown shape this decision builds on —
`/<Label>/<name>.md` with YAML frontmatter, wikilink relationship keys,
an Obsidian-compatible layout — as a **read-only view** layer. It was
never released; this decision is not bound to its formats, only informed
by them. The compatibility target is **OKF v0.2** (Open Knowledge Format,
`GoogleCloudPlatform/knowledge-catalog/okf/SPEC.md`): `type` the only
required frontmatter key, arbitrary producer keys tolerated by
consumers, `index.md`/`log.md` reserved at any hierarchy level, concept
identity defined as the file path.

The requested feature: a CypherLite database whose **actual storage is
the folder of markdown files** — read at open, written at checkpoint,
exactly the role `graph.json` or the Parquet shards play today — so an
Obsidian vault (or any folder of markdown files with YAML frontmatter)
is a live database: open it with CypherLite, query it with Cypher,
write through it, and still open the same folder in Obsidian.

Constraints found in the codebase that shape the decision:

- Snapshot codecs are enum-dispatched function sets, not trait objects;
  a third codec is a new arm in a small number of known matches
  (`checkpoint_at`, `load_with_checkpoint_lsn`, `snapshot_data_bytes`,
  `resolve_codec`, `read_envelope`, `kind.rs`).
- A checkpoint carries more than nodes and relationships:
  `next_node_id`/`next_rel_id`, `commit_ts`, `checkpoint_lsn`, indexes,
  constraints, theater, graph type, and the ontology document. None of
  that has a home in a markdown file.
- The canonical property-value encoding already exists:
  `props_to_cypher_json` / `props_from_cypher_json` (tagged values,
  ADR-0120-era `value_encoding`) — shared by the JSON codec and
  validated fail-closed for non-persistable values. A third encoding for
  YAML frontmatter would be parallel state.
- The graph-type machinery (element types, property types, key
  constraints; ADR-0226/0227) already enforces non-null/unique per
  property — the vault can lean on it instead of inventing filename
  fallbacks.
- The catalog (labels, relationship types, property keys) is
  serialized store state — it can classify frontmatter keys by type,
  not by shape.
- `saphyr` / `saphyr-parser` (YAML 1.2, pure Rust) are already workspace
  dependencies (ADR-0227); no markdown parser is, and none is needed:
  the body is opaque text between the frontmatter fence and EOF.
- Public Obsidian vaults exist for validation at real scale, e.g.
  `obsidianmd/obsidian-help` (official docs vault), `community-archive/
  obsidian-hub`, `doctorfree/Obsidian-Books-Vault`,
  `obsidian-pkm-vault/awesome-obsidian-vault`.

## Decision

Add a **third snapshot codec, `SnapshotCodecKind::Markdown`**, whose
snapshot is a folder of markdown files (the "vault"), with the engine's
own state in a hidden `.cypherlite/` sidecar directory inside the vault
— the same "dot-directory next to the content" convention Obsidian
itself uses with `.obsidian/`. The markdown↔graph mapping lives in a
new crate, `cypherlite/cypherlite-markdown`; the codec arms live in
`cypherlite-storage` beside the JSON codec.

### 1. Crate split

`cypherlite-markdown` holds the pure markdown domain: frontmatter
parse (saphyr-parser) and a hand-rolled deterministic emitter, filename
sanitization, wikilink parse/render, the node-file render and parse
rules, and the YAML↔tagged-JSON value bridge. It depends only on
`cypherlite-core` and `cypherlite-model` (R1-clean) and on the existing
`saphyr-parser` workspace dependency — no new external dependency
(ADR-0227's parser, reused). `cypherlite-storage` depends on it and adds
the codec arms; the engine and surfaces see only `SnapshotCodecKind`.

### 2. Vault layout (OKF v0.2)

- **Label directories and key properties.** Every element type (label)
  has a **key property**, declared in the graph type — non-null and
  unique, enforced by the existing key-constraint machinery. The default
  is `name`. The node's path is `<vault>/<PrimaryLabel>/<key>.md`, where
  the key is the sanitized value of **the primary label's** key
  property; the primary label is the alphabetically first label, and
  remaining labels round-trip under a reserved `labels:` key — the path
  is owned by the primary label alone, and secondary labels never
  create additional paths (one node, one file). Because the constraint
  is non-null and unique, every node has a stable, collision-free
  path: **no `_n<id>` fallback form and no `_nid` key exist.** A node
  written without its label's key property fails at commit with an
  authored error ("the markdown vault requires a non-null unique
  '<key>' on :<Label>") — key-less nodes are refused loudly at write
  time, never silently renamed at checkpoint. A key value that
  sanitizes to an empty string fails closed at render.
- **Arbitrary hierarchy: the key is the full relative path** under the
  primary label directory — subdirectory segments plus the filename,
  `/`-joined (`06 - Inbox/Projects/Alpha.md` → label `06 - Inbox`, key
  `Projects/Alpha`). OKF's unconstrained hierarchy is absorbed into the
  key, which was already "the path is the identity"; writes recreate
  the same nesting deterministically, and wikilink targets name the
  full key. Subfolders are organization, not taxonomy — even a
  folders-as-labels reading cannot drop the subpath from identity
  (two same-named files under different subpaths of one label would
  collide) — so **folders-as-labels is a future additive option, not an
  identity switch**: a per-store graph-type declaration mapping a
  label's subpath segments to additional labels, recorded in the
  manifest so the same vault always decodes the same way, layered on
  top of path identity and never replacing it. A global setting that
  changed identity would make the same bytes decode into two different
  graphs; there is none.
- **Root-level `.md` files** (no label directory — every real vault has
  README/CHANGELOG/dashboard files at root) load under a **reserved
  implicit label, default `:Vault`**, configurable per store and
  recorded in the graph type so the mapping is store state, not a
  global switch — the same vault always decodes the same way. Load
  maps root files to the root label; the engine never *writes* a node
  to the vault root (a `:Vault` node created in Cypher fails closed
  with an authored error naming the label directory it needs), so
  engine-created nodes never depend on the implicit mapping.
- **Case-insensitive identity.** Labels and keys compare
  case-insensitively: two labels or keys differing only in case are the
  same label/node — a data-quality signal, not a real difference (and
  case-insensitive filesystems such as default APFS would collide into
  one file anyway). On load, a case-only collision resolves to the first
  spelling in deterministic path order and emits a **warning**; on
  write, a case-only collision with an existing node fails the unique
  constraint.
- **Load is the mirror**: the filename *is* the node's key value, the
  OKF "Concept ID" (path) made data-derived; the directory names the
  label. A frontmatter value for the key property that differs from the
  filename fails closed (two identities in one file) rather than one
  silently winning.
- **Key inference — zero user burden.** A fresh external vault has no
  declared graph type; opening one bootstraps it: a two-pass scan
  builds the implied schema. The key *value* always comes from the OKF
  Concept ID's last path segment (the filename); the key *name* is
  inferred per label: prefer `name`, then `title` (OKF's recommended
  display key), then the first property whose values correspond 1:1 to
  the label's filenames (non-null, unique, equal to each file's last
  path segment after sanitization); if no property matches, the key
  name defaults to `name` and its value is taken from the path with the
  frontmatter key simply absent. The relationship-type vocabulary is
  inferred from unambiguously-shaped link keys (§4). The result is
  recorded as a **generated graph type**
  in the sidecar (§3) and is the explicit schema every later load
  consults. A label with no usable key candidate fails closed at open
  with an authored error naming the label and the requirement; a
  property-key/rel-type ambiguity that a single file cannot resolve on
  a sidecar-less first open (a map-form link list for a type no other
  file evidences) fails closed with a declare-or-use-a-bare-link error.
  The generated type is **descriptive, not prescriptive**: drift (a new
  key or shape the type does not know) extends the census and
  regenerates the type at checkpoint — the vault stays authoritative,
  and the codec never refuses the vault it learned from. Drift emits a
  **warning** and leaves the editing path to the user; the census
  refreshes at open and checkpoint only, never mid-session. A user who
  wants enforcement pins the type (or writes their own); then mismatches
  are authored graph-type errors.
- **Frontmatter key order** (deterministic): `type: <PrimaryLabel>`
  first (the OKF-required key; a colliding `type` property — Obsidian
  users use `type` as free metadata — is demoted to `type_property`);
  then `labels` / `labels_property` (same demotion rule); then all
  remaining properties in sorted order; then one key per outgoing
  relationship type, in sorted order, each a sorted list of entries
  (§4): a quoted wikilink `"[[Label/<key>]]"` for a prop-less
  relationship, or a map with a `target` wikilink plus the
  relationship's properties.
- **Body**: exactly the `body` property, rendered bare; on load the bare
  body becomes the `body` property. Headings inside the body are
  markdown text, not structure — there is one body property, no
  section-to-property mapping.
- **Relationships**: topology is the outgoing `TYPE:` keys only —
  every edge is declared once, by its source node; incoming edges are
  derived, never stored. Wikilinks are the link form (native Obsidian:
  resolves in the graph view). **Two spellings parse; one is written.**
  The codec writes only the full form `[[Label/<key>]]` — unambiguous,
  and Obsidian resolves it. On load it also accepts the **short form**
  `[[Note]]` — the spelling real Obsidian vaults actually use —
  resolved through the generated graph type's tables (§2): a short
  form matches the unique node whose key (or last key segment)
  case-insensitively equals it; a target naming a secondary label
  resolves through the type's primary→secondary label mapping to the
  primary-label path; an ambiguous short form fails closed listing
  the candidates; an unresolvable target materializes a stub (§4).
- **Values**: frontmatter values round-trip through the canonical
  tagged encoding (`props_to_cypher_json` / `props_from_cypher_json`)
  rendered as YAML — the vocabulary is exactly `CypherValue`'s
  persistable set (scalars, temporals, points, lists; tagged-map
  transport), nothing broader; the emitter and the loader consult one
  value-vocabulary table. Long strings render as YAML block scalars
  past a deterministic threshold, with escape-safe quoting below it.
  Vector-valued properties are **embeddings — derived engine state,
  never frontmatter**: a property named by a vector index is skipped at
  render and lives in the sidecar (§3), so re-embedding a corpus does
  not churn the markdown files.
- **Reserved filenames**: `index.md` and `log.md` are reserved at any
  hierarchy level (OKF §3.1), skipped on load — never concept nodes —
  and regenerated at checkpoint as derived views: `index.md` a
  directory listing (progressive disclosure), `log.md` a human-readable
  changelog with OKF's ISO `YYYY-MM-DD` date headings. Both are
  wholesale derived from engine truth; external edits to them are
  overwritten with a warning. They are views, never load-bearing state.

### 3. Engine sidecar `<vault>/.cypherlite/`

- `manifest.json` — the envelope (`codec: markdown`, `checkpoint_lsn`,
  `commit_ts`) plus everything the vault cannot hold: `next_node_id`,
  `next_rel_id`, indexes, constraints, theater, graph type (including
  the generated/learned type and its key-property declarations),
  `value_encoding`, the inline ontology document (JSON-codec rules), the
  vault digest, and **embeddings** — the vector values of every
  property named by a vector index, keyed by `(vault path, property)`,
  the node's primary-key identity (§2), stable because the key
  constraint is non-null and unique; a key change remaps the embedding
  key in the same checkpoint. Embeddings are derived data: a vault
  copied without `.cypherlite/` loses them and they are re-computed,
  not lost source data. Restored onto the nodes at load. Published
  last, atomically, like every codec's manifest.
- `wal/` — the WAL (the default WAL directory for a markdown store is
  `<vault>/.cypherlite/wal`, not `<vault>/wal`), so nothing engine-owned
  but hidden clutter appears at vault root.
- **Namespaces: one vault is one database** — the vault files are the
  user database (`neo4j`); the `system` and `system_ontologies`
  namespaces (ADR-0116) are engine state and live entirely in the
  sidecar, never as markdown files. A process opens one vault per
  `Database`; serving several vaults as differently-named databases is
  a future surface, not part of this codec.

### 4. Relationship properties and unresolved links

Wikilinks cannot carry properties in their string form without breaking
Obsidian semantics (`|` is the display-alias separator), and moving
topology out of frontmatter (HTML link attributes in the body,
Pandoc-style attribute blocks) breaks YAML tooling and is no
Obsidian-native round-trip form. Relationship properties therefore stay
**in the frontmatter, beside the link**: a relationship-type key's list
holds bare wikilink strings for prop-less relationships and maps for the
rest — `target: "[[Label/<key>]]"` plus the relationship's properties:

```yaml
KNOWS:
  - "[[Person/Alan]]"
  - target: "[[Person/Grace]]"
    since: 2020
```

Entries are sorted by target, properties render through the same
canonical tagged encoding as node properties, and a property literally
named `target` fails closed at render (the mirror-key rule from §2).
Parallel edges between the same pair are representable: entries with
the same target and differing properties are distinct relationships.
Relationships carry **set semantics**: an edge is identified by
`(start, type, end, properties)` and exact duplicates (same target,
same properties — or two bare links to one target) deduplicate on both
write and load, so no `occurrence` disambiguator exists and none is
needed. Relationships are vault-authoritative end to end —
git-diffable, surviving a vault copy without `.cypherlite/` — with no
sidecar state to drift.

**Classification is schema-driven, not shape-sniffed.** A frontmatter
key is a link list iff it names a relationship type in the catalog
(explicit: the persisted catalog/generated type; inferred: the
two-pass bootstrap vocabulary of §2). The value's shape (bare strings /
maps with `target`) is a validation of the entry, not the
discriminator — a property whose value merely *looks* like links
(`records: [{target: ..., url: ...}]` with `records` not a rel type)
stays a property; the only fail-closed case is a property key
colliding with a catalog rel-type name. One canonical classifier
exists, a set lookup shared by load and render.

Unresolved wikilinks (idiomatic Obsidian, tolerated by OKF) never
survive into engine state as danglers: **a link whose target has no
node materializes a stub node** on load — the same semantics as
clicking a dangling link in Obsidian creates the note — so the
invariant "no dangling links in the engine" holds by construction and
external vaults with aspirational links load cleanly. A stub is a real
node (its label from the wikilink, its key value from the wikilink
target); it gets a real file at the next checkpoint, ready to be
filled in. Only a malformed (unparseable) target fails closed at
load. The engine's own writes never produce a dangling link, since a
Cypher relationship requires an existing end node.

### 5. Open, checkpoint, and the external-edit contract

- **Load**: read `.cypherlite/manifest.json` (or bootstrap §2 when
  absent), walk the vault's `*.md` files, build the graph (ids in
  deterministic path order; reserved files skipped), union the links,
  materialize stub nodes for unresolved targets, restore sidecar
  embeddings, then replay the WAL from `checkpoint_lsn` exactly like
  the other codecs.
- **Checkpoint is copy-on-write journaled.** Render every node file,
  compare with the current file content, and — before overwriting a
  changed file — copy its current content into
  `.cypherlite/checkpoint-journal/<relpath>` (only changed files; the
  compare rule keeps the set small). Fsync journal and new files,
  write only changed files atomically (`atomic_write`: temp + fsync +
  rename + dir sync — Obsidian never sees a partial file), delete
  files whose node is gone, prune empty label directories, publish the
  sidecar manifest with the new vault digest, then delete the journal.
  **Crash recovery**: an open that finds a journal knows the mismatch
  is an engine crash, not an external edit — either every new file
  landed (verify digest, publish the manifest forward) or restore the
  journaled old contents, drop to the old `checkpoint_lsn`, and replay
  the WAL as normal. Without the journal, WAL-first ordering alone
  cannot recover a half-updated vault: the log survives, but the base
  it replays onto was destructively updated in place across many
  files, and the overwritten old contents are gone.
- **External edits while the engine is closed**: when the WAL has no
  records after `checkpoint_lsn`, the vault is authoritative — edits
  made in Obsidian while the engine was closed are picked up at the
  next open. When the WAL *is* non-empty, the vault digest is
  recomputed at open — stat-first (path, size, mtime; full crc32 of
  changed files only) so a nothing-changed open stays a stat pass —
  and a mismatch fails closed with an authored error instead of
  replaying deltas over a moved base.
- **External edits while the engine is open** are detected, not
  merged: at every checkpoint the vault is stat-checked; unexpected
  external writes fail closed with an authored error advising the
  close-edit-reopen cycle (v1 does not merge concurrent writers —
  silently overwriting the Obsidian edit would be silent data loss).
- **Opt-in**: a plain folder of markdown files never auto-detects as a
  store (a Parquet store directory may legitimately contain markdown
  docs). The codec activates via `--snapshot-format markdown`,
  `CYPHERLITE_SNAPSHOT_FORMAT=markdown`, or
  `OpenOptions::snapshot_codec`, and via detection of an existing
  `.cypherlite/manifest.json` envelope that names it. Mixed-store
  errors follow the existing `resolve_codec` rules.

### 6. Fail-closed rules (and warnings)

Real vaults contain **broken frontmatter** (@user handles, Templater
placeholders — 15 files in obsidian-hub alone; Obsidian itself
tolerates them). Such a file loads **body-only** and its raw frontmatter
is preserved **verbatim in the sidecar, keyed by vault path** — the
raw text is the user's data, and the codec never rewrites what it
cannot parse. Loading emits a per-file warning. Writes are
**compare-before-write whole-file**: an unchanged node produces zero
file writes (the raw frontmatter, the historic attribute order,
everything survives untouched); a node the engine changes is rewritten
whole in the canonical deterministic form. An engine modification of a
node whose frontmatter is unparsable **fails closed** naming the file —
the codec cannot re-emit what it cannot parse, and silently dropping it
would be silent data loss. (Patch-style surgical edits are rejected as
a mechanism: they would break the determinism and atomic-write
guarantees; diff cleanliness comes from zero-writes for unchanged
nodes, not from in-place patching.)

No silent fallbacks (LABS-311). Authored errors: a label-less node at
checkpoint ("the markdown vault requires at least one label"), a node
missing its label's key property at commit, a duplicate key value (the
existing unique-constraint error), a key value that sanitizes to an
empty string, a frontmatter key value differing from the filename it
mirrors, a non-persistable property value (the existing
`validate_persistable` errors), a property key colliding with a catalog
rel-type name, a property literally named `target` on a relationship,
a WAL-over-edited-vault digest conflict, and an external write while
the engine is open (§5). `reset_uri` on a markdown store refuses
rather than deleting a hand-authored vault. Two distinct labels whose
sanitized directory names collide are a **warning, not an error** (the
checkpoint reports "labels X and Y both map to directory 'Z'"); labels
that close are a data smell, not data loss.

### 7. Validation

Unit and roundtrip tests mirror the JSON codec's
(`codec/json/tests/roundtrip.rs` on Native and Memory VFS): full
graph-level round-trip, key-property filenames, map-form relationship
properties, set-semantics dedup, schema-driven classification
(link-shaped properties stay properties), unresolved-link stub
materialization and writeback, external-edit pickup, digest conflict,
journaled-checkpoint crash recovery, index/log regeneration, mixed
stores. The 27-file **Odyssey vault** is vendored as an owned e2e
fixture (`tests/fixtures/odyssey-vault/`, transferred from the lpgfs
exploration — kept as our test data, not as format compatibility):
`title:` keys matching filenames 1:1 (the inference path),
full-form wikilinks under per-rel-type keys, bodies, root-level
`README.md` (the `:Vault` rule), and `index.md`/`log.md` at root *and*
inside label directories (reserved at any hierarchy level); its
`import.cypher` documents the expected graph shape. The test asserts
load → checkpoint → reload stability and an idempotent re-checkpoint.
The public vaults validate real-world shape via an env-gated
test (`CYPHERLITE_MARKDOWN_VAULTS=<dir>…`: open, count, checkpoint,
verify idempotent re-checkpoint) — `obsidianmd/obsidian-help`,
`community-archive/obsidian-hub`, `doctorfree/Obsidian-Books-Vault`,
`obsidian-pkm-vault/awesome-obsidian-vault` — and their frontmatter
shape census (property keys, value kinds, nesting) is recorded as a
fixture: it is the inbound-acceptance test for the value vocabulary
and the realism source for generated vaults. Thresholds (open /
checkpoint / digest cost vs. vault size) are measured, not guessed: a
`markdown-vault-loadtest` target (the `memory-shape-loadtest` pattern,
`LOGICAL_MB`-sized, baseline env) generates vaults at 1k / 10k / 100k
/ 1M nodes sampling the observed shape census, and freezes the
results as a baseline envelope; cap/warn thresholds are decided from
those numbers.

## Alternatives considered

- **A `Vfs` implementation that translates markdown** (a "markdown
  filesystem" at the byte level). Rejected: the VFS layer moves opaque
  bytes for WAL segments and snapshot objects; WAL records and shard
  bytes cannot be markdown, so a translating VFS would either fake it
  (a real binary store hidden under generated names — not what "the
  files are the storage" means) or be impossible. The snapshot codec
  is the layer where "the durable form is markdown" is true.
- **No new crate: the mapping inside `cypherlite-storage`.** Rejected:
  the markdown domain (frontmatter grammar, sanitization, wikilinks,
  body rules) is separable and independently testable, and the request
  names a `cypherlite-markdown` crate. Keeping it dep-light
  (core + model only) also keeps it reusable by surfaces later.
- **A read-only view layer** (render the graph into the vault, import
  separately — the lpgfs shape). Rejected: not bidirectional storage;
  the vault would drift from the engine's truth instead of being it.
- **lpgfs format compatibility** (`in_TYPE` view keys, bundle-relative
  link paths, `## <name>` body sections). Rejected: lpgfs was an
  unreleased test branch; carrying its formats forever costs three
  parse arms, a reserved `in_` namespace (a legal rel type `in_KNOWS`
  would silently flip direction on reload), and an ambiguous
  section-to-property mapping, for zero users. OKF v0.2 is the
  compatibility target; Obsidian-native wikilinks are the one link
  form. (Its Odyssey fixture is kept — as our owned e2e test data,
  §7, not as a compatibility obligation.)
- **`_n<id>` derived filenames for name-less nodes** (the original
  draft). Rejected: key-less Cypher writes are legal, but the fallback
  exposed engine ids in filenames, made path-order id assignment
  unstable under external edits, and needed a `_nid` key plus a
  masquerade guard (a node named `_n42`). The per-label key-property
  constraint (§2) refuses key-less nodes loudly at write time instead
  — the vault is a schema-constrained store, not arbitrary-graph
  storage.
- **A fixed global `name` key.** Rejected: labels want different keys
  (`:Book(title)`, `:Product(sku)`); the key is declared per element
  type, and the bootstrap infers it (`name` → `title` → first unique
  string) so no user burden is added.
- **Requiring the user to declare the graph type before first open.**
  Rejected: zero-burden bootstrap — the two-pass scan infers labels'
  keys and the rel-type vocabulary, records the generated type, and
  every later load uses it; failing only when inference genuinely
  cannot (a label with no usable key).
- **Shape-sniffed link classification** (a key is links iff its value
  looks like a list of wikilinks). Rejected: a link-shaped property
  (`records: [{target: ...}]`) would silently convert to
  relationships; classification consults the catalog/generated type
  — a set lookup — and shape checks are validators (§4). This is the
  LABS-311 no-shape-sniffing rule applied to the frontmatter itself.
- **`serde_yaml` / `pulldown-cmark` dependencies.** Rejected: `saphyr`
  already ships in the workspace (R6 would need its own justification
  otherwise); the body needs no AST — it is opaque text; and a
  hand-rolled emitter for the tagged-JSON value vocabulary is the
  only way to guarantee the deterministic output the format needs.
- **Relationship properties inside wikilinks** (`[[Target|{"since":
  2020}]]`-style). Rejected: breaks Obsidian's display-alias syntax.
- **Relationship properties in the sidecar manifest** (the original
  draft). Rejected on review: relationship data stopped being
  vault-authoritative — a vault copied without `.cypherlite/` lost
  it, it was invisible to git diffs of the files, and the sidecar
  needed a drift-prone merge keyed by `(start, type, end,
  occurrence)`. The frontmatter map form (§4) keeps wikilinks clean
  and the data in the vault.
- **Relationship properties as body annotations** (HTML link
  attributes, Pandoc-style `[]{key=val}` blocks, Dataview inline
  fields). Rejected: moves topology out of frontmatter — breaking
  YAML tooling — and none are Obsidian-native round-trip forms.
- **Failing a vault open on unresolved wikilinks.** Rejected: dangling
  links are idiomatic Obsidian (a note planned but not yet written)
  and OKF-tolerated; failing the open would refuse real vaults.
  Materializing a stub node carries the link losslessly and matches
  Obsidian's own click-to-create semantics (§4).
- **Preserving unresolved wikilinks in the sidecar** (the original
  draft). Rejected: duplicated vault state in the engine sidecar and
  needed a merge-back at checkpoint; stub materialization makes the
  vault the single authority.
- **`## <name>` body sections mapping to named properties** (the lpgfs
  multi-text form). Rejected: ambiguous (which property does a
  heading-less body belong to?) and breaks the lossless round-trip a
  storage codec owes; one `body` property is unambiguous, headings are
  markdown text.
- **Embeddings rendered into the frontmatter** (vector properties as
  YAML float lists, or a per-embedding `_embedding` block). Rejected:
  derived values would pollute every node file — hundreds of opaque
  float lines that churn wholesale on every re-embedding, dominate the
  git diff, and bloat files for no human value. The sidecar holds them
  as re-derivable engine state (§3).
- **Auto-detecting markdown stores from `*.md` files in a directory.**
  Rejected: false positives would misroute real Parquet/JSON store
  directories that happen to contain markdown documentation. Explicit
  opt-in plus sidecar-manifest detection only.
- **`index.md` / `log.md` as load-bearing WAL/changelog storage.**
  Rejected: the WAL is the durable log and must stay engine-owned and
  fail-closed — a human-editable file on the recovery path is a
  corruption vector. They are checkpoint-regenerated derived views
  (§2); a per-commit live `log.md` append (history growing while the
  engine is open, safe because derived, at-most-once delivery) is a
  later nicety on the same rule.
- **Merging external writes into a running engine.** Rejected for v1:
  concurrent-writer merge is a feature of its own; detecting external
  writes at checkpoint and failing closed with a close-edit-reopen
  advisory (§5) is honest — silently overwriting the Obsidian edit
  would be silent data loss.

## Consequences

- An Obsidian vault becomes a CypherLite database: open, query, write,
  and the folder stays a valid vault Obsidian (and any OKF v0.2
  consumer) can read. Stores become human-readable, git-diffable, and
  per-file mergeable.
- A markdown store is schema-constrained: every node carries its
  label's non-null unique key, enforced at commit. Key-less Cypher
  writes fail loudly instead of being renamed into the vault. The
  engine's ids never surface in the human layer — the path (label +
  key value) is the single identity vocabulary, used by filenames,
  wikilinks, relationship membership, and sidecar embedding keys.
- First open of a raw vault infers the schema (per-label keys,
  rel-type vocabulary) and records it as a generated graph type;
  the user can accept it silently or pin/replace it for enforcement.
- The engine gains its first store whose durable snapshot is
  human-editable; that editability is the risk surface. The contract
  is explicit: closed-engine vault edits land when the WAL is empty;
  with a dirty WAL the digest guard fails closed; open-engine
  external writes are detected and refused; a journaled checkpoint
  recovers its own crashes; `reset` refuses; collision and key
  errors are loud.
- Checkpoint cost is O(vault) (render all, write changed) plus a
  journal copy of changed files — the JSON codec's cost shape, not
  Parquet's incremental shards. The stat-first digest keeps a
  nothing-changed open at stat cost. Real thresholds come from the
  generated-vault loadtest baseline, not guesswork.
- Relationship properties live in the vault frontmatter (map-form
  link entries), not the sidecar: the whole graph — nodes,
  relationships, and their properties — is vault-authoritative,
  git-diffable, and survives a vault copied without `.cypherlite/`
  (counters, catalog, digest, and embeddings are sidecar — the
  embeddings are derived and re-computed, never source data).
  Unresolved wikilinks materialize stub nodes, so external vaults
  with aspirational links load as data, never as engine danglers.
- `index.md`/`log.md` are regenerated derived views — useful in
  Obsidian, never load-bearing.
- `SnapshotCodecKind` grows a third arm compiled everywhere (saphyr is
  pure Rust and wasm-safe), so `--snapshot-format markdown` works on
  every build; the dispatch sites, `resolve_codec`, the docs
  (`docs/api/rust/spec.md`, `docs/api/cli/spec.md`) and AGENTS.md's
  crate table change in the same commit.
