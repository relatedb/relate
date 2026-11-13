use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use relate_catalog::{Catalog, ConstraintDef, IndexDef};
use relate_core::{Namespace, Node, NodeId, RelId, Relationship, Result, Value};

use crate::adjacency::{SealedAdj, TypedAdj};
use crate::group_key::GroupKey;
use crate::memory::ChainShape;
use crate::pull_identity::PullIdentityIndex;
use crate::redo::{self, PullKind, RedoOp};
use crate::store::{Interner, LiveNode, LiveRel, RelTypes};

use adjacency::{adj_insert, adj_reposition};
use constraint_keys::ConstraintKeyIndex;
use count_state::{CountDelta, CountStats};

// The count-only reader's probe shape (LABS-370 round 3, F-1-5): re-exported
// for the recognizer in `exec` — the window itself lives on `Graph`.
pub(crate) use count_state::{CountProbe, LabelSetCount};

/// `Atomic*::fetch_update` was renamed to `try_update` in Rust 1.95 and
/// deprecated in 1.99. MSRV is 1.88, where `try_update` does not exist, so
/// the call keeps the old name. `#[expect(deprecated)]` is wrong here: on
/// 1.88 the lint does not fire and the build would fail.
macro_rules! atomic_fetch_update {
    ($atom:expr, $set:expr, $fetch:expr, $f:expr) => {{
        #[allow(
            deprecated,
            reason = "fetch_update renamed to try_update in 1.99; try_update needs 1.95, MSRV is 1.88"
        )]
        {
            $atom.fetch_update($set, $fetch, $f)
        }
    }};
}

mod accounting;
mod engine_api;

mod adjacency;
mod apply_redo;
// The research-only heap-census harness (ADR-0476 quarantine shape): the
// testing example of the same name reads it through the research feature.
// Private module with root re-exports only (one public path per item).
#[cfg(feature = "research")]
pub(crate) mod census;
mod chain;
mod conflict;
pub(crate) mod constraint_keys;
mod count_state;
mod declared;
mod degree_stats;
mod degree_stats_estimate;
mod degree_stats_incident;
mod derived_migration;
mod entity_mutation;
mod id_vec;
mod index;
mod index_lookup;
mod index_postings;
mod label_mutation;
mod label_probe;
mod pin;
mod prop_read;
mod query_stats;
pub use pin::SnapshotPin;
#[cfg(test)]
use pin::PIN_ACQUIRE_RETRIES;
#[cfg(test)]
use pin::PIN_CLAIMING;
use pin::{PIN_FREE, PIN_SLOTS};
mod rebuild;
mod redo_preflight;
mod rel_postings;
mod restore;
mod schema_api;
mod services;
mod shard;
// Research-only (speed-of-light kernels), quarantined behind the
// non-default `research` cargo feature (ADR-0476): absent from every
// default build and every bundle.
#[cfg(feature = "research")]
pub mod sol;
// Research-only: the live-read family the sol kernel reads (the
// helpers this quarantine used to inline in the module root; the
// qpp half retired with its prototype, ADR-0478).
#[cfg(feature = "research")]
mod research;
#[cfg(test)]
pub(crate) mod test_support;
mod theater;

pub(crate) use declared::{spatial_holds, trigram_count, RangeSeek, SpatialSeek};
use declared::{GramIndex, PointIndex, RangeIndex};
#[cfg(test)]
pub(crate) use index::CONTAINS_SAMPLE_BOUND;
use index::{ContainsMemo, Indexes};
// One canonical provenance type (LABS-643): defined beside the planner's
// `PatternStats` contract that consumes it, re-exported so the executor's
// construction sites and tests keep the one path.
#[cfg(test)]
pub(crate) use label_probe::take_label_probe_checks;
pub(crate) use label_probe::LabelProbe;
pub use query_stats::QueryStats;
// One public path (index stays private): the whole-graph index-rebuild
// count the facade's reopen tests read (LABS-1112's empty-replay skip).
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub use index::take_index_rebuilds;
pub use relate_planner::ContainsCount;
pub(crate) use services::StoreServices;

use theater::TheaterData;

/// The core-store map: integer keys, FxHash — SipHash + the default
/// hasher on dense engine-generated ids was profiled as a top-2 cost
/// (LABS-337/LABS-358); ADR-0355's rule: a key that is attacker-writable
/// must not use it.
pub(crate) type CoreMap<K, V> = dashmap::DashMap<K, V, crate::hash_seed::FxBuildHasher>;

/// The visible version at `ts` — the one walk every `*_at` read takes
/// over a version chain (LABS-337's extraction of the previously
/// five-times-spelled rev-find).
fn visible_version<Live>(vers: &Chain<Live>, ts: u64) -> Option<VerRef<'_, Live>> {
    vers.visible_at(ts)
}

/// The LIVE version — the last `xmax == 0` one, not a ts-probe: the
/// maintained-statistics paths run inside the write window, where a rel
/// created earlier in the same batch is invisible at `commit_ts` and one
/// deleted in the batch keeps a live-looking pre-delete version. The
/// `xmax == 0` convention is the one every live-version reader in the graph
/// module uses (`live_label_ids`, the rebuild's live-rel scan, and the
/// constraint_keys family's snapshots and backfills).
fn live_version<Live>(vers: &Chain<Live>) -> Option<VerRef<'_, Live>> {
    vers.live()
}

/// One live rel row of a wholesale adjacency build — the seam
/// `rebuild_adj` materializes once and hands to `adjacency::adj_build`
/// and both rebuild folds (`rebuild_degree_stats`,
/// `rebuild_count_stats`) consume. It lives here beside [`LabelCache`],
/// not in the adjacency-accessor sibling: the folds read it without
/// touching adjacency. Named fields rather than a positional tuple:
/// `start` and `end` share a type, so a swapped pair would compile and
/// only the adjacency tests would catch it.
pub(in crate::graph) struct RelRow {
    pub(in crate::graph) id: RelId,
    pub(in crate::graph) start: NodeId,
    pub(in crate::graph) type_id: u32,
    pub(in crate::graph) end: NodeId,
    pub(in crate::graph) ts: Option<i64>,
}

/// The wholesale rebuild folds' shared label cache: one live-label
/// read per distinct endpoint node ([`Graph::live_label_ids`]), the
/// same seam [`Graph::rebuild_count_stats`] and
/// [`Graph::rebuild_degree_stats`] both fold over — one spelling of
/// the ensure-then-expect read, never a hand-rolled map per file.
#[derive(Default)]
pub(in crate::graph) struct LabelCache(HashMap<NodeId, Vec<u32>>);

/// The one label-cache fill both label maps ride: `node`'s live labels
/// read exactly once into `map`, then the entry borrowed — the
/// cache-fill-and-borrow pattern, so no per-lookup clone of the live set
/// rides a walk. The two maps' *read* contracts differ and stay their
/// own: [`LabelCache::get`] is the apply fold's read-only view (its miss
/// is an invariant break, loudly), while the stats estimate's simulated
/// set (`BatchSim::node_labels`) is mutable — an `AddLabels` step extends
/// it and the pair phase reads the extended set back.
pub(in crate::graph) fn fill_labels<'m>(
    map: &'m mut HashMap<NodeId, Vec<u32>>,
    graph: &Graph,
    node: NodeId,
) -> &'m mut Vec<u32> {
    map.entry(node)
        .or_insert_with(|| graph.live_label_ids(node))
}

impl LabelCache {
    /// Read `node`'s live labels into the cache exactly once, through the
    /// one fill shape.
    pub(in crate::graph) fn ensure(&mut self, graph: &Graph, node: NodeId) {
        fill_labels(&mut self.0, graph, node);
    }

    /// The cached labels — the fold ensured the node's entry first, so
    /// a miss is the fold's own invariant broken, loudly.
    pub(in crate::graph) fn get(&self, node: NodeId) -> &[u32] {
        self.0
            .get(&node)
            .expect("the fold cached every endpoint's labels")
            .as_slice()
    }
}

/// The store's token resolver (ADR-0541): the facade hands the graph to the
/// planner beside its statistics, so plan-time resolution reads the same
/// interner execution does.
impl relate_names::TokenResolver for Graph {
    fn resolve(&self, name: &str) -> relate_names::Token {
        self.interner.resolve(name)
    }
}

impl Graph {
    /// A node's current live labels in the interned domain (u32 ids) —
    /// the histogram's key domain, read through [`live_version`] rather than
    /// a snapshot-visible one: the histogram is maintained at create time,
    /// inside the writing transaction, where the just-created node is not yet
    /// visible at `commit_ts` (the bug the first version of this read had —
    /// every bump looped over zero labels). The `xmax == 0` convention
    /// matches the rebuild's live-rel scan.
    pub(in crate::graph) fn live_label_ids(&self, node: NodeId) -> Vec<u32> {
        self.nodes
            .get(&node)
            .and_then(|vers| vers.live().map(|v| v.live.labels.clone()))
            .unwrap_or_default()
    }
}

pub(crate) use theater::{apply_show_overlay, DatabasesCatalog, ShowSchema};
pub use theater::{CheckpointTheater, DatabaseEntry, SchemaSnapshot};

/// An exhaustible `i64` id counter shared by the committed graph counters and
/// the overlay scratch counters. Both allocate the current value and advance;
/// both must fail rather than wrap once `i64::MAX` is reached. The two atomic
/// widths exist because the overlay seeds its counters from the graph's.
pub(crate) trait ExhaustibleIdCounter {
    fn try_alloc_id(&self) -> std::result::Result<i64, i64>;
}

impl ExhaustibleIdCounter for AtomicI64 {
    fn try_alloc_id(&self) -> std::result::Result<i64, i64> {
        atomic_fetch_update!(self, Ordering::Relaxed, Ordering::Relaxed, |id| {
            if (0..i64::MAX).contains(&id) {
                Some(id + 1)
            } else {
                None
            }
        })
    }
}

impl ExhaustibleIdCounter for AtomicU64 {
    fn try_alloc_id(&self) -> std::result::Result<i64, i64> {
        atomic_fetch_update!(self, Ordering::Relaxed, Ordering::Relaxed, |id| {
            (id < i64::MAX as u64).then_some(id + 1)
        })
        .map(|id| id as i64)
        .map_err(|id| id as i64)
    }
}

/// The `DeleteConnectedNode` execution error, shared by every node-delete
/// path so the message never drifts between them. The class token leads the
/// payload: live Neo4j 2026.10.1 reports the
/// `ConstraintVerificationFailed` class (G1001), and the bindings recover
/// it through the one nested-class scan of still-stringly payloads. The
/// token composes from the typed class vocabulary, so its spelling cannot
/// drift from the scan's.
pub(crate) fn delete_connected_node_error() -> relate_core::Error {
    relate_core::Error::execution(format!(
        "{}: DeleteConnectedNode: Cannot delete node with relationships unless DETACH",
        relate_error::NestedErrorClass::ConstraintVerificationFailed.as_str(),
    ))
}

/// The one typed reply for a relationship whose endpoint is not a live node:
/// the create path's semantic error, shared by the overlay's create, the
/// primitive's endpoint validation, and the redo preflight.
pub(crate) fn missing_rel_endpoint_error() -> relate_core::Error {
    relate_core::Error::semantic("cannot create relationship on missing node")
}

/// Allocate the next exhaustible id from `counter`, mapping exhaustion to the
/// caller's `<what> ID exhausted` error.
pub(crate) fn alloc_exhaustible_id(counter: &impl ExhaustibleIdCounter, what: &str) -> Result<i64> {
    counter
        .try_alloc_id()
        .map_err(|_| relate_core::Error::execution(format!("{what} ID exhausted")))
}

#[derive(Debug, Clone, Default)]
pub struct Procedure {
    pub name: String,
    pub arg_names: Vec<String>,
    /// Cypher type names (`STRING`, `INTEGER`, `NUMBER`, …) aligned with `arg_names`.
    pub arg_types: Vec<String>,
    pub yield_names: Vec<String>,
    pub rows: Vec<BTreeMap<String, Value>>,
}

use chain::{Chain, Ver, VerRef};
use id_vec::IdVec;

/// The one MVCC version-chain record: node and rel chains share the shape
/// (and, via [`cow`], the copy-on-write policy) through the aliases below.
type NodeVer = Ver<LiveNode>;
type RelVer = Ver<LiveRel>;

pub(crate) fn node_version_stride() -> u64 {
    std::mem::size_of::<NodeVer>() as u64
}

pub(crate) fn rel_version_stride() -> u64 {
    std::mem::size_of::<RelVer>() as u64
}

/// The in-map size of an inline node slot: the row and its birth timestamp.
/// One id slot, including the discriminant. The floor charges the vector
/// of these, not a hash-map entry (ADR-0532).
pub(crate) fn node_slot() -> u64 {
    std::mem::size_of::<Option<Chain<LiveNode>>>() as u64
}

pub(crate) fn rel_slot() -> u64 {
    std::mem::size_of::<Option<Chain<LiveRel>>>() as u64
}

pub(crate) fn adj_slot() -> u64 {
    std::mem::size_of::<Option<TypedAdj>>() as u64
}

/// One sealed side-table id slot, the stride of the sealed stores' arrays.
fn sealed_slot() -> u64 {
    std::mem::size_of::<Option<SealedAdj>>() as u64
}

fn sealed_value() -> u64 {
    std::mem::size_of::<SealedAdj>() as u64
}

pub(crate) fn node_value() -> u64 {
    std::mem::size_of::<Chain<LiveNode>>() as u64
}

pub(crate) fn rel_value() -> u64 {
    std::mem::size_of::<Chain<LiveRel>>() as u64
}

fn adj_value() -> u64 {
    std::mem::size_of::<TypedAdj>() as u64
}

/// Copy-on-write over `id`'s slot ([`Chain::cow`]) at the write timestamp.
fn cow<Live: Clone>(write_ts: &AtomicU64, map: &IdVec<Chain<Live>>, id: i64) -> Option<Live> {
    let ts = write_ts.load(Ordering::Acquire);
    map.get_mut(&id)?.cow(ts)
}

#[derive(Debug)]
pub struct Graph {
    /// Which fixed namespace of the store this graph is (ADR-0116 §1, §10).
    /// The executor needs it for namespace-qualified element ids (ADR-0116
    /// §7); `neo4j` is the default so existing construction sites are
    /// unchanged.
    namespace: Namespace,
    /// Store-level services (procedure registry, LOAD CSV policy, preview
    /// set) shared by every namespace graph of a store (ADR-0116 §10) as one
    /// `Arc` bundle. Not a field consumers can reach: the `Arc` identity is
    /// the one-bundle invariant, so access goes through
    /// [`Graph::procedure`], [`Graph::has_procedure`],
    /// [`Graph::register_procedure`], [`Graph::csv_policy`],
    /// [`Graph::preview`], [`Graph::databases`],
    /// [`Graph::configure_load_csv`], [`Graph::configure_preview`] and
    /// [`Graph::share_services_from`].
    services: Arc<StoreServices>,
    next_node_id: AtomicI64,
    next_rel_id: AtomicI64,
    /// Last published commit (ADR-0012 / ADR-0013). Stored after apply (ADR-0018).
    published_ts: AtomicU64,
    write_ts: AtomicU64,
    in_write: AtomicBool,
    record_redo: AtomicBool,
    nodes: IdVec<Chain<LiveNode>>,
    rels: IdVec<Chain<LiveRel>>,
    out_adj: IdVec<TypedAdj>,
    in_adj: IdVec<TypedAdj>,
    /// The sealed tier, one side table per direction keyed by node (ADR-0551):
    /// a node's compressed chunks and versioned tombstones. An entry exists
    /// only where a fold or a delete's tombstone landed, so a node that never
    /// folds pays nothing; the delta slot stays 24 bytes.
    sealed_out: IdVec<SealedAdj>,
    sealed_in: IdVec<SealedAdj>,
    /// LABS-851 (the structure round): the counting operator's count
    /// state — per-type rel counts, (start-label, type, end-label) pair
    /// counts, per-label live node counts, and the total live node
    /// count — as one bundle behind one mutex (the `indexes: Indexes`
    /// precedent), so the windowed-consistency invariant is structural:
    /// the count-only reader holds the one guard across its exactness
    /// check and reads, every bump takes the same guard, and a future
    /// arm's state lands inside the bundle instead of as another
    /// sibling lock. Per-field semantics, charges, and the bump/rebuild
    /// discipline live with the type in `graph/count_state.rs`.
    count_stats: Mutex<CountStats>,
    /// LABS-351: per-(label, rel-type) out-degree histogram, log2-bucketed
    /// (bucket k counts nodes of the label whose out-degree on the rel type
    /// is 2^k..2^(k+1)-1). The mean degree (`rel_type_rows / label_rows`)
    /// lies on the heavy-tailed shape where the hub join applies; the
    /// histogram's tail is the honest degree for start-selection and
    /// gate estimates. This is the HIGH-WATER degree, not the live one:
    /// the typed adjacency keeps tombstoned entries (its chunk length
    /// never shrinks), so the bucket only ever moves up on create — a
    /// floor that can only overestimate and never miss a supernode,
    /// which is exactly the conservative direction a gate needs. The
    /// adjacency rebuild recomputes it from the live rels in one pass.
    /// Estimate state the counting operator never reads — it stays
    /// beside the count bundle, not inside it.
    degree_histograms: Mutex<degree_stats::DegreeHistograms>,
    /// ADR-0439: the maintained, single-version per-constraint committed
    /// tuple map (`unique`/`key` constraints, node and relationship). Unlike
    /// the lazily built postings in [`Indexes`], this map is eagerly current;
    /// `graph/constraint_keys/mod.rs` owns its charge family and
    /// backfill/rebuild lifecycle; `simulation.rs` owns the op classification
    /// and the entity simulation; `seams.rs` the `apply_redo` maintenance;
    /// `ddl.rs` the batch DDL accumulator; `admission.rs` the commit-time
    /// admission probe; `estimate.rs` the entry-growth estimate (one-way: the
    /// admission and estimate sides import the map's vocabulary).
    constraint_keys: Mutex<ConstraintKeyIndex>,
    interner: Interner,
    catalog: Mutex<Catalog>,
    pub(crate) theater: Mutex<TheaterData>,
    /// Exact old definitions whose enforcement was absent from a legacy image/WAL.
    legacy_unenforced: Mutex<HashMap<String, ConstraintDef>>,
    stats: Mutex<QueryStats>,
    /// All lazily maintained index state: the label index, the equality and
    /// declared pair maps, the interned sentinels and the committed index
    /// memory charge — one bundle, [`Indexes`].
    indexes: Indexes,
    /// LABS-643: the committed epoch's memoized sampled `CONTAINS`
    /// estimates — the plan-stats read
    /// ([`Graph::pattern_contains_rows`](crate::Graph::pattern_contains_rows))
    /// answers without a TEXT index by sampling, and the executor's
    /// per-row start-selection estimate must not resample per row. See
    /// [`ContainsMemo`] for the epoch reset and the cap.
    contains_memo: Mutex<ContainsMemo>,
    pending_redo: Mutex<Vec<RedoOp>>,
    /// Source-scoped pull identity (ADR-0080).
    pull_identity: PullIdentityIndex,
    /// Ids mutated since the last successful checkpoint (ADR-0022).
    dirty_nodes: Mutex<HashSet<NodeId>>,
    dirty_rels: Mutex<HashSet<RelId>>,
    /// In-flight snapshot pins (ADR-0038). Each slot is free / claiming / ts+1.
    pins: Box<[AtomicU64]>,
    pin_cursor: AtomicU64,
    /// Live graph + index logical bytes (ADR-0042).
    used_bytes: AtomicU64,
}

impl Default for Graph {
    fn default() -> Self {
        let interner = Interner::default();
        let indexes = Indexes::new(&interner);
        Self {
            namespace: Namespace::Neo4j,
            services: Arc::new(StoreServices::default()),
            next_node_id: AtomicI64::new(0),
            next_rel_id: AtomicI64::new(0),
            published_ts: AtomicU64::new(0),
            write_ts: AtomicU64::new(0),
            in_write: AtomicBool::new(false),
            record_redo: AtomicBool::new(true),
            nodes: IdVec::default(),
            rels: IdVec::default(),
            out_adj: IdVec::default(),
            in_adj: IdVec::default(),
            sealed_out: IdVec::default(),
            sealed_in: IdVec::default(),
            count_stats: Mutex::new(CountStats::default()),
            degree_histograms: Mutex::new(degree_stats::DegreeHistograms::default()),
            constraint_keys: Mutex::new(ConstraintKeyIndex::default()),
            interner,
            catalog: Mutex::new(Catalog::default()),
            theater: Mutex::new(TheaterData::default()),
            indexes,
            contains_memo: Mutex::new(ContainsMemo::default()),
            legacy_unenforced: Mutex::new(HashMap::new()),
            stats: Mutex::new(QueryStats::default()),
            pending_redo: Mutex::new(Vec::new()),
            pull_identity: PullIdentityIndex::default(),
            dirty_nodes: Mutex::new(HashSet::new()),
            dirty_rels: Mutex::new(HashSet::new()),
            pins: (0..PIN_SLOTS).map(|_| AtomicU64::new(PIN_FREE)).collect(),
            pin_cursor: AtomicU64::new(0),
            used_bytes: AtomicU64::new(0),
        }
    }
}

fn visible(xmin: u64, xmax: u64, ts: u64) -> bool {
    xmin != 0 && xmin <= ts && (xmax == 0 || xmax > ts)
}

fn checked_persisted_id(id: i64, what: &str) -> Result<i64> {
    relate_core::validate_persisted_id(id, what)?;
    Ok(id + 1)
}

fn requires_enforced_label(def: &ConstraintDef) -> bool {
    def.kind
        .parse::<relate_model::ConstraintType>()
        .is_ok_and(|kind| kind.is_enforced_label_kind())
}

fn matching_enforced_label(def: &ConstraintDef, c: &relate_model::Constraint) -> bool {
    c.label == def.label
        && c.properties == def.properties
        && def.kind.parse::<relate_model::ConstraintType>() == Ok(c.constraint_type)
        && c.property_type == def.property_type
        && c.constraint_type
            .enforced_label_site()
            .is_some_and(|site| site.is_relationship() == def.relationship)
        && c.enforced_label
            .as_deref()
            .is_some_and(|label| !label.is_empty())
}

/// Whether a derived record names an enforced label at all: its kind
/// requires one and the label is present and non-empty. The per-entry core
/// both checkpoint gates share (each keeps its own error mapping).
fn has_enforced_label(c: &relate_model::Constraint) -> bool {
    c.constraint_type.is_enforced_label_kind()
        && c.enforced_label
            .as_deref()
            .is_some_and(|label| !label.is_empty())
}

impl Graph {
    /// Whether the committed current graph type could carry property
    /// default values (CIP-262): the committed-theater half of the
    /// statement boundary's scratch-aware gate
    /// ([`ExecGraph::has_property_defaults`] answers over both
    /// vintages). A false positive only costs one overlay read.
    pub(crate) fn has_property_defaults(&self) -> bool {
        let theater = self.theater.lock().expect("theater lock poisoned");
        spec_has_property_defaults(&theater.current_graph_type)
    }

    /// Which fixed namespace of the store this graph is (ADR-0116 §1).
    pub fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// Set the graph's namespace identity. Called by the store when it
    /// creates or swaps a namespace slot (ADR-0116 §10); not public API.
    #[doc(hidden)]
    pub fn set_namespace(&mut self, namespace: Namespace) {
        self.namespace = namespace;
    }

    pub fn commit_ts(&self) -> u64 {
        self.published_ts.load(Ordering::Acquire)
    }

    /// Whether a base write window is in flight (`begin_write`..
    /// `commit_write` / `apply_redo`): the maintained statistics are
    /// already bumped for it, but the commit is not published yet — a
    /// concurrent reader must not answer from them (the count-only
    /// short-circuit's exactness precondition, LABS-370 v1 follow-up).
    pub(crate) fn in_write_window(&self) -> bool {
        self.in_write.load(Ordering::Acquire)
    }

    pub fn set_commit_ts(&self, ts: u64) {
        self.published_ts.store(ts, Ordering::Release);
    }

    pub fn next_node_id(&self) -> NodeId {
        self.next_node_id.load(Ordering::Relaxed)
    }

    pub fn set_next_node_id(&self, id: NodeId) {
        self.next_node_id.store(id, Ordering::Relaxed);
    }

    pub fn next_rel_id(&self) -> RelId {
        self.next_rel_id.load(Ordering::Relaxed)
    }

    fn mark_node_dirty(&self, id: NodeId) {
        self.dirty_nodes.lock().unwrap().insert(id);
    }

    fn mark_rel_dirty(&self, id: RelId) {
        self.dirty_rels.lock().unwrap().insert(id);
    }

    pub fn set_next_rel_id(&self, id: RelId) {
        self.next_rel_id.store(id, Ordering::Relaxed);
    }

    fn read_ts(&self) -> u64 {
        if self.in_write.load(Ordering::Acquire) {
            self.write_ts.load(Ordering::Acquire)
        } else {
            self.commit_ts()
        }
    }

    /// Begin a write statement: versions get xmin = commit_ts+1 until publish.
    /// Returns `Execution` if the next timestamp is the unsupported sentinel.
    pub fn begin_write(&self) -> Result<()> {
        let ts = self
            .commit_ts()
            .checked_add(1)
            .filter(|ts| *ts < u64::MAX)
            .ok_or_else(|| relate_core::Error::execution("write timestamp exhausted"))?;
        self.begin_write_at(ts)
    }

    /// Begin a write with an explicit txn id (ADR-0015 optimistic snapshot).
    /// Returns `Execution` if `write_ts` is the unsupported sentinel.
    pub fn begin_write_at(&self, write_ts: u64) -> Result<()> {
        if write_ts == u64::MAX {
            return Err(relate_core::Error::execution("write timestamp exhausted"));
        }
        self.in_write.store(true, Ordering::Release);
        self.write_ts.store(write_ts, Ordering::Release);
        self.pending_redo.lock().unwrap().clear();
        self.record_redo.store(true, Ordering::Release);
        self.reset_stats();
        Ok(())
    }

    fn alloc_node_id(&self) -> Result<NodeId> {
        alloc_exhaustible_id(&self.next_node_id, "node")
    }

    fn alloc_rel_id(&self) -> Result<RelId> {
        alloc_exhaustible_id(&self.next_rel_id, "relationship")
    }

    pub fn pending_commit_ts(&self) -> u64 {
        self.write_ts.load(Ordering::Acquire)
    }

    pub fn take_redo(&self) -> Vec<RedoOp> {
        std::mem::take(&mut *self.pending_redo.lock().unwrap())
    }

    /// Publish after WAL fsync.
    pub fn commit_write(&self) {
        self.set_commit_ts(self.write_ts.load(Ordering::Acquire));
        self.in_write.store(false, Ordering::Release);
        self.pending_redo.lock().unwrap().clear();
    }

    pub fn abort_write(&self) {
        let ts = self.write_ts.load(Ordering::Acquire);
        self.nodes.retain(|_, vers| vers.abort(ts));
        self.rels.retain(|_, vers| vers.abort(ts));
        self.in_write.store(false, Ordering::Release);
        self.pending_redo.lock().unwrap().clear();
        self.rebuild_adj();
        self.rebuild_indexes();
    }

    pub fn reset_stats(&self) {
        *self.stats.lock().unwrap() = QueryStats::default();
    }

    pub fn node_at(&self, id: NodeId, ts: u64) -> Option<Node> {
        self.nodes
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).map(|v| v.live.to_node(id, &self.interner)))
    }

    /// Label-only view of a node — no property-map materialization (ADR-0033).
    /// Callers that only need labels avoid `node_at`, which builds the full
    /// property map per node (the profile-hot `CompactProps::to_map` path).
    pub fn node_labels_at(&self, id: NodeId, ts: u64) -> Option<Vec<String>> {
        self.nodes.get(&id).and_then(|vers| {
            visible_version(&vers, ts).map(|v| v.live.label_strings(&self.interner))
        })
    }

    pub fn rel_at(&self, id: RelId, ts: u64) -> Option<Relationship> {
        self.rels
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).map(|v| v.live.to_rel(id, &self.interner)))
    }

    /// Endpoints only — expand / BFS must not materialize property maps (ADR-0033).
    pub fn rel_ends_at(&self, id: RelId, ts: u64) -> Option<(NodeId, NodeId)> {
        self.rels
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).map(|v| (v.live.start, v.live.end)))
    }

    /// The rel's LIVE identity — its endpoints and type (round 3,
    /// LABS-370 F-1-2): the live version found by [`live_version`], not
    /// a ts-probe — the maintained-statistics paths run inside the write
    /// window, where a rel created earlier in the same batch is
    /// invisible at `commit_ts` (its xmin > commit_ts) and one deleted
    /// in the batch keeps a live-looking pre-delete version. The type
    /// rides along because a re-used rel id can come back as a different
    /// rel type: a live-incidence match must hold on all three axes,
    /// not the endpoints alone.
    fn rel_live_identity(&self, id: RelId) -> Option<(NodeId, NodeId, u32)> {
        self.rels.get(&id).and_then(|vers| {
            live_version(&vers).map(|v| (v.live.start, v.live.end, v.live.type_id))
        })
    }

    pub fn node_stub_at(&self, id: NodeId, ts: u64) -> Option<Node> {
        self.nodes
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).map(|v| v.live.stub(id, &self.interner)))
    }

    pub fn rel_stub_at(&self, id: RelId, ts: u64) -> Option<Relationship> {
        self.rels
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).map(|v| v.live.stub(id)))
    }

    pub fn node_prop_at(&self, id: NodeId, ts: u64, key: &str) -> Option<Value> {
        self.nodes.get(&id).and_then(|vers| {
            visible_version(&vers, ts).and_then(|v| v.live.prop(&self.interner, key))
        })
    }

    pub fn rel_prop_at(&self, id: RelId, ts: u64, key: &str) -> Option<Value> {
        self.rels.get(&id).and_then(|vers| {
            visible_version(&vers, ts).and_then(|v| v.live.payload.get(&self.interner, key))
        })
    }

    pub fn node_prop_keys_at(&self, id: NodeId, ts: u64) -> Vec<String> {
        self.nodes
            .get(&id)
            .and_then(|vers| {
                vers.visible_at(ts)
                    .map(|v| v.live.props.keys_as_strings(&self.interner))
            })
            .unwrap_or_default()
    }

    pub fn rel_prop_keys_at(&self, id: RelId, ts: u64) -> Vec<String> {
        self.rels
            .get(&id)
            .and_then(|vers| {
                vers.visible_at(ts)
                    .map(|v| v.live.payload.keys(&self.interner))
            })
            .unwrap_or_default()
    }

    pub(crate) fn rel_visible_at(&self, id: RelId, ts: u64) -> bool {
        self.rels
            .get(&id)
            .is_some_and(|vers| visible_version(&vers, ts).is_some())
    }

    pub fn node(&self, id: NodeId) -> Option<Node> {
        self.node_at(id, self.read_ts())
    }

    pub fn rel(&self, id: RelId) -> Option<Relationship> {
        self.rel_at(id, self.read_ts())
    }

    /// Account of the relationship version visible at the read timestamp.
    /// Uses the stored `Triple` / `Other` form, not the materialized map.
    fn visible_rel_account(&self, id: RelId) -> u64 {
        let ts = self.read_ts();
        self.rels
            .get(&id)
            .and_then(|vers| {
                vers.visible_at(ts)
                    .map(|v| crate::memory::live_rel_account_live(v.live))
            })
            .unwrap_or(0)
    }

    fn rel_form(&self, id: RelId) -> Option<crate::store::PayloadForm> {
        let ts = self.read_ts();
        self.rels.get(&id).and_then(|vers| {
            vers.visible_at(ts)
                .map(|v| v.live.payload.form(&self.interner))
        })
    }

    pub fn node_exists(&self, id: NodeId) -> bool {
        self.node_visible_at(id, self.read_ts())
    }

    pub fn rel_exists(&self, id: RelId) -> bool {
        self.rel_visible_at(id, self.read_ts())
    }

    pub fn live_node_count(&self) -> usize {
        self.all_node_ids().len()
    }

    /// True inside a write that records redo; replay (`apply_redo_inner`) does not.
    fn recording(&self) -> bool {
        self.in_write.load(Ordering::Acquire) && self.record_redo.load(Ordering::Acquire)
    }

    fn push_redo(&self, op: RedoOp) {
        if self.recording() {
            self.pending_redo.lock().unwrap().push(op);
        }
    }

    fn bump_stats(&self, f: impl FnOnce(&mut QueryStats)) {
        f(&mut self.stats.lock().unwrap());
    }

    fn xmin_for_install(&self) -> u64 {
        if self.in_write.load(Ordering::Acquire) {
            self.write_ts.load(Ordering::Acquire)
        } else {
            let ts = self.commit_ts();
            if ts == 0 {
                self.set_commit_ts(1);
                1
            } else {
                ts
            }
        }
    }

    pub fn node_value(&self, id: NodeId) -> Value {
        self.node(id).map(Value::from_node).unwrap_or(Value::Null)
    }

    pub fn rel_value(&self, id: RelId) -> Value {
        self.rel(id)
            .map(Value::from_relationship)
            .unwrap_or(Value::Null)
    }

    pub fn pull_identity_get(&self, source: &str, kind: PullKind, remote_id: &str) -> Option<i64> {
        self.pull_identity.get(source, kind, remote_id)
    }

    pub fn pull_identity_put(
        &self,
        source: String,
        kind: PullKind,
        remote_id: String,
        local_id: i64,
    ) {
        self.pull_identity.put(source, kind, remote_id, local_id);
    }

    pub fn drop_pull_identity(&self, kind: PullKind, local_id: i64) {
        self.pull_identity.remove(kind, local_id);
    }
}
/// Whether a graph-type spec could carry property default values
/// (CIP-262): any element type's property declares one, or any implied
/// label identifies another node element type (whose chain may inherit
/// one down). The one predicate both vintages of the statement
/// boundary's gate consult -- the committed theater
/// ([`Graph::has_property_defaults`]) and the scratch redo slice
/// ([`ExecGraph::has_property_defaults`]) -- so the two can never
/// answer the same yes/no differently.
pub(crate) fn spec_has_property_defaults(spec: &crate::graph_type::GraphTypeSpec) -> bool {
    let identifying: std::collections::BTreeSet<&str> = spec
        .node_types
        .iter()
        .map(|n| n.identifying_label.as_str())
        .collect();
    spec.node_types.iter().any(|n| {
        n.properties.iter().any(|p| p.default_value.is_some())
            || n.implied_labels
                .iter()
                .any(|l| identifying.contains(l.as_str()))
    }) || spec
        .edge_types
        .iter()
        .any(|e| e.properties.iter().any(|p| p.default_value.is_some()))
}

impl Graph {
    pub fn all_rel_ids(&self) -> Vec<RelId> {
        self.all_rel_ids_at(self.read_ts())
    }

    pub fn all_rel_ids_at(&self, ts: u64) -> Vec<RelId> {
        let mut ids: Vec<_> = self
            .rels
            .keys()
            .into_iter()
            .filter(|id| self.rel_visible_at(*id, ts))
            .collect();
        ids.sort();
        ids
    }

    pub fn all_node_ids(&self) -> Vec<NodeId> {
        self.all_node_ids_at(self.read_ts())
    }

    pub fn all_node_ids_at(&self, ts: u64) -> Vec<NodeId> {
        let mut ids: Vec<_> = self
            .nodes
            .keys()
            .into_iter()
            .filter(|id| self.node_visible_at(*id, ts))
            .collect();
        ids.sort();
        ids
    }

    pub(crate) fn node_visible_at(&self, id: NodeId, ts: u64) -> bool {
        self.nodes
            .get(&id)
            .is_some_and(|vers| visible_version(&vers, ts).is_some())
    }
}

#[cfg(test)]
mod tests;
