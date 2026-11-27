//! The Vortex property store (ADR-0665), behind the default-off
//! `vortex-store` gate (ADR-0703). This is the store ADR-0665 measured; the
//! measurement harness — the kernels, the A/B driver, the epoch probes —
//! stays in `testing/relate-vortex-lab` and drives this module.
//!
//! A node-major nullable-column store: the row index is the node id, one
//! column per promoted property, read through the executor's
//! [`PropStore`](super::prop_store::PropStore) seam for point reads, and
//! dispatched to by the executor for two lookup shapes:
//!
//! - the equality seek — the sorted `(key,row)` side-file that replaces the
//!   ADR-0614 lazy equality postings ([`VortexStore::seek`]);
//! - the `IN` member — one galloping pass over the same side-file
//!   ([`VortexStore::member`]).
//!
//! The label scan never routes here: the measured ordering keeps it on the
//! canonical label route (ADR-0665), so the store carries no label arm.
//!
//! The decoded cache keeps each touched column decoded whole — one flat,
//! row-indexed `OnceLock<Arc<Vec<Option<Value>>>>` already in the seam's
//! own shape (the `colstore` `ints()` shape the seam research measured);
//! a point hit is one locate and one clone, the same work the compact
//! records' `prop_by_id` pays.
//!
//! The read path's discipline: one relaxed atomic load while no publication
//! has appended (the unwritten fast path pays nothing), then one read-lock
//! acquisition plus an `Arc` clone per read once a delta exists, and the
//! commit-time write lock under the publication close. The per-column
//! decoded caches stay fill-once `OnceLock`s — the lock-free shape holds
//! there. The store is `Send + Sync`; the reader runtime is the fork's
//! [`CurrentThreadRuntime`] (`Arc<SmolExecutor>`, `Send` by construction).
//!
//! The store lived in the lab while the Vortex dependency sat outside the
//! workspace (ADR-0677); with the dependency vetted, the store is product
//! code behind the gate, and the lab is the harness that measures it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock, RwLock};

use relate_core::{Error, NodeId, Result, Value};
use rustc_hash::FxHashMap;

use vortex::array::VortexSessionExecute;
use vortex::file::VortexFile;
use vortex::io::runtime::current::CurrentThreadRuntime;
use vortex::scalar::{PValue, Scalar, ScalarValue};
use vortex::session::VortexSession;

use super::prop_store::{PropFold, PropStore, PublishedRow};
use super::Graph;

mod build;
mod columns;
mod side;

use build::{columnar_table, gather, row_space, seal_table, Gathered};
use columns::{chunks, projected};
use side::{equal_range, member_rows, side_index};

/// One property value the store carries. The derived order is the sort
/// total the side-file needs: Null first, then ints, then strings (the
/// variant list is already in that order).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Val {
    Null,
    Int(i64),
    Str(String),
}

/// The engine value for a stored one (`Val::Null` = absent).
pub(crate) fn to_value(val: Val) -> Option<Value> {
    match val {
        Val::Null => None,
        Val::Int(int) => Some(Value::Int(int)),
        Val::Str(string) => Some(Value::String(std::sync::Arc::from(string))),
    }
}

/// A lookup value in the store's `Val` shape; `None` for a type the store
/// does not promote (the lookup then falls through to the index route).
pub(crate) fn promoted_val(value: &Value) -> Option<Val> {
    match value {
        Value::Int(int) => Some(Val::Int(*int)),
        Value::String(string) => Some(Val::Str(string.to_string())),
        _ => None,
    }
}

/// A decoded scalar in the store's `Val` shape. The columns carry only
/// ints and strings (the promoted set [`promoted_val`] admits), so any
/// other scalar is a broken column — a typed error, never a panic on the
/// read path and never silently answered as null: a null here is a live
/// property disappearing from results (the no-silent-fallbacks rule).
pub(crate) fn decoded_val(scalar: &Scalar) -> Result<Val> {
    match scalar.value() {
        None => Ok(Val::Null),
        Some(ScalarValue::Primitive(PValue::I64(v))) => Ok(Val::Int(*v)),
        Some(ScalarValue::Primitive(PValue::I32(v))) => Ok(Val::Int(*v as i64)),
        Some(ScalarValue::Utf8(text)) => Ok(Val::Str(text.as_str().to_owned())),
        Some(other) => Err(Error::storage(format!(
            "a decoded {other:?} is outside the promoted column set — a broken column"
        ))),
    }
}

/// A dense row-id set — the per-label filter and the visibility set.
#[derive(Clone)]
struct BitSet {
    words: Vec<u64>,
    len: usize,
}

impl BitSet {
    fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)],
            len,
        }
    }

    /// `row < len` by construction: every caller walks ids below the
    /// `row_count` the bitset was sized to (`gather`).
    fn set(&mut self, row: usize) {
        self.words[row / 64] |= 1 << (row % 64);
    }

    fn get(&self, row: usize) -> bool {
        row < self.len && (self.words[row / 64] >> (row % 64)) & 1 == 1
    }

    fn intersect(&self, other: &BitSet) -> BitSet {
        BitSet {
            words: self
                .words
                .iter()
                .zip(&other.words)
                .map(|(a, b)| a & b)
                .collect(),
            len: self.len.min(other.len),
        }
    }
}

/// One column's decoded values, row-indexed directly, filled once by the
/// first reader to touch the column — the `colstore` `ints()` shape the
/// seam research measured. The decode memoizes the seam's own
/// `Option<Value>` shape, so a point hit is one locate plus one clone, the
/// same work the compact records' `prop_by_id` pays (ADR-0508's memo
/// lesson: convert once at fill, never per read).
type ColumnValues = Arc<Vec<Option<Value>>>;

/// One column's fill-once decoded state, shared across reader threads
/// (memory table 2). The hit path is one acquire load and a cold race
/// decodes once — no lock ever reaches a reader. The cached value is a
/// `Result`: a decode failure is cached as the column's decline (the
/// compact records keep answering) instead of retrying per read.
#[derive(Default)]
struct ColumnCache {
    values: OnceLock<Result<ColumnValues>>,
}

/// One key's gathered column before the file is sealed. The variant is the
/// column's type, so a value of the other type cannot reach a typed builder.
#[derive(Default)]
enum ColumnState {
    /// No value seen yet: the first value decides the kind.
    #[default]
    Empty,
    Int(Vec<Option<i64>>),
    Str(Vec<Option<String>>),
    /// A value the prototype cannot carry, or two promoted kinds for one
    /// key: the key is not a faithful column and stays off the store.
    Poisoned,
}

impl ColumnState {
    /// Record one property value at `row`, poisoning the key when the value
    /// does not fit the column's kind.
    fn push(&mut self, row: usize, value: &Value, row_count: usize) {
        let next = match (std::mem::replace(self, ColumnState::Poisoned), value) {
            (ColumnState::Poisoned, _) => ColumnState::Poisoned,
            (ColumnState::Empty, Value::Int(int)) => {
                let mut values = vec![None; row_count];
                values[row] = Some(*int);
                ColumnState::Int(values)
            }
            (ColumnState::Empty, Value::String(string)) => {
                let mut values = vec![None; row_count];
                values[row] = Some(string.to_string());
                ColumnState::Str(values)
            }
            (ColumnState::Int(mut values), Value::Int(int)) => {
                values[row] = Some(*int);
                ColumnState::Int(values)
            }
            (ColumnState::Str(mut values), Value::String(string)) => {
                values[row] = Some(string.to_string());
                ColumnState::Str(values)
            }
            // an unpromotable value or a second kind poisons the key
            (_, _) => ColumnState::Poisoned,
        };
        *self = next;
    }
}

/// One key's column: its gathered state and its decoded cache.
#[derive(Default)]
struct Column {
    state: ColumnState,
    cache: ColumnCache,
}

/// The node-major columnar store.
/// One committed version of one row since the base snapshot — the
/// delta-overlay tier (ADR-0665 versioning step 1): sparse, versioned,
/// appended by the publication feed, read by newest-≤-`ts`. `props` is
/// the row's FULL state for the keys the store carries (a key absent
/// here is absent at `ts` — the row state, not a patch).
#[derive(Clone)]
struct DeltaVersion {
    ts: u64,
    /// The row's labels at `ts` — carried by the feed, read by the set
    /// arms' overlay rung (step 2); step 1's point path does not read it.
    #[allow(dead_code)]
    labels: Vec<u32>,
    props: FxHashMap<u32, Option<Value>>,
    deleted: bool,
}

/// The rows changed since the base snapshot, each with its committed
/// versions in ascending `ts` — the sparse overlay the publication feed
/// fills and every read resolves before falling to the columnar base.
/// Unbounded until the seal step (the design's next rung) — a
/// prototype-scale bound, noted in ADR-0665. One `Arc` per row, so a
/// publication clones the map shallowly and deep-copies only the rows the
/// commit touched.
#[derive(Default, Clone)]
struct Delta {
    rows: FxHashMap<NodeId, Arc<Vec<DeltaVersion>>>,
}

pub struct VortexStore {
    session: VortexSession,
    /// The fork's shared-executor runtime: `Arc<SmolExecutor>`, `Send + Sync`,
    /// and the runtime the scan machinery spawns onto.
    rt: CurrentThreadRuntime,
    vfile: VortexFile,
    /// The base snapshot this store answers FROM: the columnar tier's
    /// build time. Reads at any `ts >= base` are answered (the delta
    /// overlay resolves the rows commits changed); a read at an earlier
    /// `ts` declines to the compact records.
    ts: u64,
    /// The rows commits changed since `ts` — copy-on-write per
    /// publication (the delta is small; readers load one `Arc` with a
    /// single lock acquisition and never lock again inside a read).
    delta: RwLock<Arc<Delta>>,
    /// Whether any publication has appended a delta version: the one fact
    /// the empty-path fast checks read. Clear = no version exists, so the
    /// base columns are still exact and a read pays one acquire load, no
    /// lock; set = the set/aggregate arms decline and `delta()` locks.
    delta_present: AtomicBool,
    row_count: usize,
    /// Rows that are visible live nodes at the snapshot (gaps excluded).
    visible: BitSet,
    /// Per-label dense bitsets over rows.
    label_bits: FxHashMap<u32, BitSet>,
    /// Promoted columns by interned key id, each with its fill-once cache.
    columns: FxHashMap<u32, Column>,
    /// The sorted `(key,row)` side-file, per interned key: `(value, row)` in
    /// value order.
    side: FxHashMap<u32, Vec<(Val, NodeId)>>,
    /// The lookup arms served, for the dispatch test.
    served: Served,
}

/// How many lookups each dispatch arm served — one counter per rung, so a
/// test names the arm it asserts.
#[derive(Default)]
struct Served {
    seek: AtomicU64,
    member: AtomicU64,
    folds: AtomicU64,
    grouped: AtomicU64,
}

impl std::fmt::Debug for VortexStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VortexStore")
            .field("row_count", &self.row_count)
            .field("columns", &self.columns.len())
            .finish_non_exhaustive()
    }
}

impl VortexStore {
    /// Step 1's one decline gate: once a delta version exists, the
    /// side-file and columns reflect the base tier alone — every set and
    /// aggregate arm declines (merging the overlay's contributions into
    /// them is the design's next rung, ADR-0665).
    fn declines_step_one(&self) -> bool {
        self.delta_present.load(AtomicOrdering::Acquire)
    }

    /// The lookup arms served so far — the dispatch test's assertion seam:
    /// `(seek, member, folds, grouped)`, one counter per rung.
    pub fn served(&self) -> (u64, u64, u64, u64) {
        (
            self.served.seek.load(AtomicOrdering::Relaxed),
            self.served.member.load(AtomicOrdering::Relaxed),
            self.served.folds.load(AtomicOrdering::Relaxed),
            self.served.grouped.load(AtomicOrdering::Relaxed),
        )
    }

    /// Encode the graph at `ts` node-major and open it, building the label
    /// bitsets and the sorted side-file in the same pass. Admits the
    /// graph's row space first — [`row_space`] owns that refusal.
    pub fn from_graph(graph: &Graph, ts: u64) -> Result<Self> {
        let ids = graph.all_node_ids_at(ts);
        let row_count = row_space(&ids)?;
        let Gathered {
            visible,
            label_bits,
            mut columns,
        } = gather(graph, ts, ids, row_count)?;
        // A poisoned key (an unpromotable value, or two promoted kinds) is not
        // a faithful column: reading it from the store would answer null for
        // the skipped values, so it stays off the store entirely and
        // [`PropStore::carries`] keeps the compact records answering for it.
        columns.retain(|_, column| !matches!(column.state, ColumnState::Poisoned));
        let array = columnar_table(&columns, row_count)?;
        let (session, rt, vfile) = seal_table(array)?;
        let side = side_index(&mut columns);
        Ok(Self {
            session,
            rt,
            vfile,
            ts,
            delta: RwLock::new(Arc::new(Delta::default())),
            delta_present: AtomicBool::new(false),
            row_count,
            visible,
            label_bits,
            columns,
            side,
            served: Served::default(),
        })
    }

    /// The delta overlay, if any publication has appended since the base
    /// snapshot. One relaxed-path load when nothing has, else one lock
    /// acquisition and one `Arc` clone — the read never locks twice.
    fn delta(&self) -> Option<Arc<Delta>> {
        if !self.declines_step_one() {
            return None;
        }
        Some(self.delta.read().unwrap().clone())
    }

    /// The newest delta version of `id` at or before `ts`.
    fn delta_version(delta: &Delta, id: NodeId, ts: u64) -> Option<&DeltaVersion> {
        delta
            .rows
            .get(&id)?
            .iter()
            .rev()
            .find(|version| version.ts <= ts)
    }

    /// Equality seek over the sorted `(key,row)` side-file: every row whose
    /// value equals `val`, in row order — the shared run resolution.
    fn seek(&self, kid: u32, val: &Val) -> Vec<NodeId> {
        let Some(entries) = self.side.get(&kid) else {
            return Vec::new();
        };
        let (start, len) = equal_range(entries, 0, val);
        entries[start..start + len]
            .iter()
            .map(|(_, row)| *row)
            .collect()
    }

    /// Batched key check: rows whose value is any of `sorted_vals` (sorted
    /// ascending) — the shared galloping member over the side-file, the
    /// #9551 shape.
    fn member(&self, kid: u32, sorted_vals: &[Val]) -> Vec<NodeId> {
        let Some(entries) = self.side.get(&kid) else {
            return Vec::new();
        };
        member_rows(entries, sorted_vals)
    }

    /// The rows carrying every label: the reduce's `None` (no labels) is the
    /// unconstrained scan, the visible set; an uninterned label intersects
    /// with an empty set, so it matches nothing.
    fn label_candidates(&self, labels: &[u32]) -> BitSet {
        labels
            .iter()
            .map(|lid| {
                self.label_bits
                    .get(lid)
                    .cloned()
                    .unwrap_or_else(|| BitSet::new(self.row_count))
            })
            .reduce(|acc, bits| acc.intersect(&bits))
            .unwrap_or_else(|| self.visible.clone())
    }

    /// One column's decoded values, filling the cache once on first touch —
    /// the one lazy-decode resolve every read goes through, so a key the
    /// store does not carry answers `None` in one place. A cached decode
    /// failure answers `None` here; `carries` reads the same cached
    /// `Result` non-initializing, so the key stops being carried from the
    /// next read on, and the discovering read falls through to the
    /// compact records (`Graph::point`'s miss path).
    fn column_values(&self, kid: u32) -> Option<&ColumnValues> {
        let cache = self.columns.get(&kid)?;
        cache
            .cache
            .values
            .get_or_init(|| self.decode_column(kid))
            .as_ref()
            .ok()
    }

    /// The aggregate arms' shared resolve: the decoded column and the
    /// label's candidate rows. A key the store does not carry declines
    /// the shape.
    fn fold_inputs(&self, labels: &[u32], kid: u32) -> Option<(&[Option<Value>], BitSet)> {
        let values = self.column_values(kid)?;
        Some((values.as_slice(), self.label_candidates(labels)))
    }

    /// The value at `(kid,row)` through the decoded cache: the flat
    /// column-indexed values (filled once by the first reader to touch the
    /// column), already in the seam's `Option<Value>` shape — a hit is one
    /// locate plus one clone, the row store's `prop_by_id` shape.
    fn value_at(&self, kid: u32, row: usize) -> Option<Value> {
        if row >= self.row_count {
            return None;
        }
        self.column_values(kid)?.get(row).cloned().flatten()
    }

    /// One column decoded whole into its flat row-indexed values, filled
    /// once by whichever reader arrives first. The one-time decode also
    /// pays the `Val`→`Option<Value>` conversion, so no hit ever converts
    /// (first-touch cost: the column's compressed projections materialize
    /// and every chunk runs `repeated_probe` — read-heavy columns pay it
    /// once and hit forever). A decode failure — the store's own file
    /// unreadable, or a scalar outside the promoted set — is cached as
    /// the column's decline: the read falls through to the compact
    /// records (the poisoned-key policy's read-side twin), never a
    /// panic on the user read path.
    fn decode_column(&self, kid: u32) -> Result<ColumnValues> {
        let name = column_name(kid);
        // The crate's one single-column read.
        let field = projected(&self.rt, &self.vfile, &name)?;
        let chunks = chunks(&field);
        let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
        let mut values = Vec::with_capacity(total);
        for chunk in &chunks {
            let mut ctx = self.session.create_execution_ctx();
            let mut probe = chunk.repeated_probe();
            for row in 0..chunk.len() {
                let scalar = probe.execute_scalar(row, &mut ctx).map_err(vortex_error)?;
                values.push(to_value(decoded_val(&scalar)?));
            }
        }
        Ok(Arc::new(values))
    }
}

/// Build the columnar store from `graph` at its commit timestamp and install
/// it through [`Graph::attach_prop_store`]. The graph must be quiescent: the
/// store is built from its state at attach time. First attach wins, so an
/// already-attached graph is an explicit error, never a silent drop.
#[doc(hidden)]
pub fn attach_vortex_store(graph: &Graph) -> Result<()> {
    let store: Arc<dyn PropStore> = Arc::new(VortexStore::from_graph(graph, graph.commit_ts())?);
    if !graph.attach_prop_store(store) {
        return Err(Error::storage(
            "a property store is already attached to this graph",
        ));
    }
    Ok(())
}

impl PropStore for VortexStore {
    fn point(&self, ts: u64, id: NodeId, kid: u32) -> Option<Value> {
        // The delta overlay first (ADR-0665 versioning step 1): a row a
        // commit changed resolves from its newest version at or before
        // `ts` — absent rows answer `None`, present ones read the
        // version's full state for the key. The base tier below is one
        // flat column index and one clone (the decode memoizes the
        // seam's own shape, the colstore `ints()` shape the seam
        // research measured); an unchanged row never reaches the delta.
        if let Some(delta) = self.delta() {
            if let Some(version) = Self::delta_version(&delta, id, ts) {
                return if version.deleted {
                    None
                } else {
                    version.props.get(&kid).cloned().flatten()
                };
            }
        }
        // No version at or before `ts`: the row reads as it was at the
        // base — a base row falls through to its column, a row the base
        // never held (created later) answers absent, which the column
        // read's own row bound settles.
        self.value_at(kid, id as usize)
    }

    fn carries(&self, kid: u32) -> bool {
        // Lock-free, from the one cell that holds the fact: an unpopulated
        // cell is carried (nothing has failed yet), a cached `Ok` is
        // carried, a cached `Err` — a broken column — is not, so every
        // later read of the key falls through to the compact records.
        self.columns
            .get(&kid)
            .is_some_and(|column| column.cache.values.get().is_none_or(Result::is_ok))
    }

    fn serves_at(&self, ts: u64) -> bool {
        // The base snapshot answers FROM its build time: the delta
        // overlay resolves every commit since, so any `ts >= base` is
        // served. An earlier `ts` (a reader pinned before the store was
        // built) declines to the compact records.
        ts >= self.ts
    }

    fn equality(
        &self,
        labels: &[u32],
        kid: u32,
        values: &[Value],
        cap: Option<usize>,
    ) -> Option<Vec<NodeId>> {
        // The set arms decline behind step 1's one gate.
        if self.declines_step_one() {
            return None;
        }
        // A value the store does not promote cannot be answered: fall through
        // to the index route.
        let vals: Vec<Val> = values.iter().map(promoted_val).collect::<Option<_>>()?;
        let candidates = self.label_candidates(labels);
        let mut rows = if let [val] = vals.as_slice() {
            self.served.seek.fetch_add(1, AtomicOrdering::Relaxed);
            self.seek(kid, val)
        } else {
            let mut sorted = vals;
            sorted.sort_unstable();
            sorted.dedup();
            self.served.member.fetch_add(1, AtomicOrdering::Relaxed);
            self.member(kid, &sorted)
        };
        rows.retain(|row| candidates.get(*row as usize));
        rows.sort_unstable();
        rows.truncate(cap.map_or(usize::MAX, |cap| cap + 1));
        Some(rows)
    }

    fn folds(&self, labels: &[u32], kid: u32, wants: &[PropFold]) -> Option<Vec<Value>> {
        // The aggregate arms decline behind step 1's one gate (the
        // one-pass fold would need the delta's contributions merged).
        if self.declines_step_one() {
            return None;
        }
        let (values, rows) = self.fold_inputs(labels, kid)?;
        // Bumped where the arm commits to an answer — a decline (the
        // step-one gate or a key the store does not carry) never counts
        // as served.
        self.served.folds.fetch_add(1, AtomicOrdering::Relaxed);
        // One pass: the label's rows over the flat column — count the
        // present values, track both extrema. The extrema use the ORDER BY
        // order, the engine's own min/max policy (`order_by_cmp`), never
        // the three-valued comparison-operator order — a null is the only
        // value skipped; an empty column answers null, the postings
        // extremum's own empty answer.
        let mut count = 0u64;
        let mut min: Option<&Value> = None;
        let mut max: Option<&Value> = None;
        for value in present_values(values, &rows) {
            count += 1;
            if min.is_none_or(|m| value.order_by_cmp(m) == std::cmp::Ordering::Less) {
                min = Some(value);
            }
            if max.is_none_or(|m| value.order_by_cmp(m) == std::cmp::Ordering::Greater) {
                max = Some(value);
            }
        }
        Some(
            wants
                .iter()
                .map(|want| match want {
                    PropFold::Count => Value::Int(count as i64),
                    PropFold::Min => min.cloned().unwrap_or(Value::Null),
                    PropFold::Max => max.cloned().unwrap_or(Value::Null),
                })
                .collect(),
        )
    }

    fn grouped_counts(&self, labels: &[u32], kid: u32) -> Option<Vec<(Value, u64)>> {
        // The grouped arm declines behind the same gate (see `folds`).
        if self.declines_step_one() {
            return None;
        }
        let (values, rows) = self.fold_inputs(labels, kid)?;
        // Bumped where the arm commits to an answer (see `folds`).
        self.served.grouped.fetch_add(1, AtomicOrdering::Relaxed);
        // The dict-native distinct: one pass, the counts landing per
        // distinct value (keyed by the store's own `Val`, the first
        // carried `Value` kept for the answer) — the absent-value group
        // stays the engine's.
        let mut counts: FxHashMap<Val, (Value, u64)> = FxHashMap::default();
        for value in present_values(values, &rows) {
            // The decoded column carries only the promoted kinds (the
            // gather drops poisoned keys), so the re-promote is total —
            // an invariant, not a filter that could drop rows silently.
            let key = promoted_val(value).expect("a decoded column value is always promoted");
            let entry = counts.entry(key).or_insert_with(|| (value.clone(), 0));
            entry.1 += 1;
        }
        Some(
            counts
                .into_iter()
                .map(|(_, (value, count))| (value, count))
                .collect(),
        )
    }

    fn on_publication(&self, ts: u64, rows: &[PublishedRow]) {
        // Copy-on-write per publication, the canonical `Arc::make_mut`
        // shape: a reader that cloned the `Arc` under the read lock before
        // this write lock keeps its snapshot, so the clone is shallow (one
        // `Arc` bump per changed row) and only the rows this commit touched
        // get their version vector copied — a commit pays for its own rows,
        // not the session's changed-row history. The map clone itself is
        // still O(changed-rows-total) — known debt the seal rung
        // (ADR-0665's versioning step 3) compacts. Every reader keeps
        // loading one `Arc` — no reader ever locks twice. Versions append
        // in publication order; `ts` is monotonic by the feed's contract.
        // Rows the store does not cover (ids and keys its columns never
        // carried) still land: a later promotion or the seal step reads
        // them, and an uncovered key's overlay simply holds `None`.
        //
        // ONE write guard covers clone, append, swap and the flag set: a
        // reader's empty-path check can never land between an installed
        // `Arc` and the stale flag and miss the versions already in it.
        // Observable states are only "no publication" and "publication
        // fully applied", and two publications serialize instead of one
        // clobbering the other's versions.
        let mut guard = self.delta.write().unwrap();
        let delta = Arc::make_mut(&mut *guard);
        for row in rows {
            let versions = Arc::make_mut(delta.rows.entry(row.id).or_default());
            let mut props: FxHashMap<u32, Option<Value>> = FxHashMap::default();
            if !row.deleted {
                for (kid, value) in &row.props {
                    props.insert(*kid, Some(value.clone()));
                }
            }
            versions.push(DeltaVersion {
                ts,
                labels: if row.deleted {
                    Vec::new()
                } else {
                    row.labels.clone()
                },
                props,
                deleted: row.deleted,
            });
        }
        // The one fact the flag carries is "a version exists": a publication
        // that appends none leaves the base columns exact, so the fast paths
        // stay valid.
        if !rows.is_empty() {
            self.delta_present.store(true, AtomicOrdering::Release);
        }
    }
}

/// The aggregation arms' shared walk: each candidate row's carried cell, in
/// row order. Skips rows the label bitset excludes and absent (null) cells —
/// the one home for the "present values" policy the folds and the dict-native
/// distinct both read.
fn present_values<'a>(
    values: &'a [Option<Value>],
    rows: &'a BitSet,
) -> impl Iterator<Item = &'a Value> + 'a {
    values
        .iter()
        .enumerate()
        .filter(move |(row, _)| rows.get(*row))
        .filter_map(|(_, value)| value.as_ref())
}

/// The wire name of one key's column (`k{kid}`) — the one file-schema
/// fact both phases share: the builder names the struct's children with
/// it, the decode projects the column back by the same call.
pub(crate) fn column_name(kid: u32) -> String {
    format!("k{kid}")
}

fn vortex_error(error: impl std::fmt::Display) -> Error {
    Error::storage(format!("vortex store: {error}"))
}

#[cfg(test)]
mod tests;
