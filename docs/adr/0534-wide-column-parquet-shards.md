# ADR-0534: Wide-column Parquet shards

Status: Proposed
Claim: f54d24e89c83b16438c1adbb9ea24847f98762b9

## Context

Checkpoint shards (ADR-0022, ADR-0050) store entity properties as a single
tagged-JSON string column. Every restore pays, per row: a JSON parse, an
intermediate ordered map, a conversion to the compact property list, and a
re-interning of key strings that were already interned when the graph was
written. Measured on LDBC SF0.1 (`profiling` build): reopening a 119 MB
store holds 1.75 GB resident versus 1.01 GB to build the identical graph
from CSV — the durable format does not carry the memory format's shape,
and three representations of the same properties are live at once. At
LDBC SF1 the gap widens: a 630 MB store restores to a 21.3 GB peak
(18.7 GB steady) against a 10.7 GB build peak — the restore overhead
scales super-linearly with store size.

The live store has no static schema: property keys are open-ended runtime
data and values are dynamically typed. But in practice each key carries
one consistent type, and bulk loads allocate ids label-by-label, so a
shard part (an id-range file, ADR-0183) is label-homogeneous for the
common load shape.

## Decision

Add a **wide-column part encoding inside the Parquet codec** — not a new
`SnapshotCodecKind`, and not a replacement: an **opt-in per store**, the
legacy tagged-JSON column remaining the default. Every build reads both
encodings. The manifest's format version records which one a store uses —
3/4 legacy, 5/6 wide — and a manifest at the wide versions keeps the store
wide whatever the open flags. The opt-in is an open-time codec
configuration (the `CodecOpenConfig` slot, ADR-0460): set on a legacy
store, its first checkpoint re-encodes every part once, then resumes the
normal dirty-id incremental rewrites; set on a store whose codec is not
Parquet, the open fails instead of ignoring it.

Per part, the row schema becomes:

- `id` (`Int64`); rels add `start_id`, `end_id`, `type` as today;
- `labels`: a dictionary-encoded list column carrying the row's full label
  set — a primary label plus any marker/role labels;
- **one typed column per promoted property key**: the column *is* the key;
  its name is the key string, its type the single consistent value type
  observed on that part's rows;
- a residual `properties` tagged-JSON column holding only **unpromoted**
  keys: those below the promotion threshold, with inconsistent types, or
  with complex values a flat column cannot carry. Expected near-empty.

**Promotion policy:** a key present on at least half of a part's *newly
encoded* rows with one consistent flat type (boolean, integer, float,
string) becomes a column; anything else — below the threshold, mixed
types, or a value with no flat column shape — stays in the residual blob.
A part that copies rows from a previous part carries that part's columns
forward unchanged: copied rows are re-read, not re-derived, and shard
statistics are off, so their presence counts are not recomputable. The
threshold is one named constant in the codec. A carried column that meets
new values of another type keeps its column: those values take the
residual blob for their rows, never a coercion. The promoted columns of
one part are capped (a named constant): the page envelope of ADR-0086
admits a bounded leaf count per file, so the cap keeps the row bound
plannable; keys beyond it stay residual.

**The key table is the schema.** Column names are the keys; restore
interns each column name once per part, not once per row. No separate
key-table object; the interned catalog persists as today. A promoted
column's name carries a fixed prefix, so a property key can never collide
with a reserved column (`id`, `labels`, `type`, `start_id`, `end_id`,
`properties`) — a key named `id` or `properties` is representable.

**Layout and incrementality are unchanged.** Parts stay (kind, id-range)
files; the dirty-id coalescing of ADR-0183 rewrites only units touching
dirty ids. A legacy checkpoint keeps the fixed schema and copies exactly
as before — no wide machinery, no extra reads. A clean part is byte-stable across checkpoints and its schema
is frozen with it; a re-encoded part re-derives its schema, so key sets
adapt at every rewrite. No label partitioning of parts in this change:
for sequential loads parts are label-homogeneous anyway, and mixed parts
are correct, merely wider.

**Restore decode** reads promoted keys per column directly into the
compact property list — no JSON, no intermediate map, no per-row
re-interning. Rows with residual content take the existing JSON path for
that column only.

Predicate pushdown and row-group statistics are **not used by the engine**
(restore reads every row); they are the interop dividend — external
readers see named, typed, compressed columns.

## Alternatives considered

- **Keep the tagged-JSON column** — rejected: the measured restore cost is
  the motivation; nothing above changes then.
- **Key table plus positional values in one nested column** — rejected:
  kills the JSON parse and re-interning but forfeits per-key compression,
  typed interop and row-group statistics; strictly dominated by columns.
- **Partition parts by label (nodes) or (from-label, type, to-label)
  (rels) now** — rejected for this change: it breaks the id-range slot
  machinery of ADR-0183 to serve pruning the engine does not do yet.
  Recorded as the refinement when larger-than-memory stores need
  label-keyed part pruning; rel parts would then also carry endpoint
  labels, since a rel type alone can be underspecified.
- **A new `SnapshotCodecKind`** — rejected: the vocabulary table,
  compiled-feature matrix and CLI surface all multiply; the manifest
  version already selects encodings unambiguously.
- **Non-Parquet binary shards** — a separate exploration track, not this
  decision; this ADR stays Parquet-native for interop.

## Consequences

- Two part schemas coexist in the Parquet codec for as long as the option
  runs; both are tested by roundtrip, and a store never migrates between
  them — it opts in.
- The engine facade carries one `wide_columns` flag; the storage-level
  option is the `CodecOpenConfig` slot, so other surfaces (CLI, Bolt) can
  expose it later without new plumbing.
- The write path gains per-part schema derivation (observed keys, types,
  promotion) — more work than emitting one blob, paid only when a part
  re-encodes, so the tax concentrates on first writes, bulk loads and the
  opt-in upgrade (measured +5-20% there; incremental checkpoints derive
  only dirty rows). The follow-up direction: per-key presence/type
  statistics maintained continuously — seeded at load from each wide
  part's own schema (the footer already read) and updated by every
  transactional write and WAL replay, keyed by id range so a part's
  derivation is its buckets' union with no row walk. Stale statistics can
  only misplace a value between column and residual, never lose one.
- A part's leaf columns grow with its promoted keys, so the ADR-0086
  page-envelope row bound shrinks: at the column cap a node file holds
  ~60k rows where the fixed schema held ~700k. Total entity count is
  unaffected — a store is many parts — but large stores carry more,
  smaller parts. Planning the row bound by a part's *actual* column
  count, instead of the cap's worst case, is the refinement to make when
  stores genuinely carry tens of keys per label.
- Marker-label attributes on a minority of rows stay in the residual
  blob; their keys re-intern per row until promotion.
- Future direction, not this change: with a tiered-arena memory store
  (ADR-0206), checkpoints become stable files plus appended delta files
  with periodic compaction; the arena's append-only shape is what makes
  that natural, and the format decided here (immutable per-part files,
  manifest-selected) is compatible with it.
