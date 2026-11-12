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
pub(crate) use count_state::CountProbe;

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
mod prop_read;
mod query_stats;
mod rebuild;
mod redo_preflight;
mod rel_postings;
mod restore;
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

impl LabelCache {
    /// Read `node`'s live labels into the cache exactly once.
    pub(in crate::graph) fn ensure(&mut self, graph: &Graph, node: NodeId) {
        self.0
            .entry(node)
            .or_insert_with(|| graph.live_label_ids(node));
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

/// 256 in-flight statements; excess pins retry a bounded number of times and
/// then fail with a typed "snapshot pin" error (ADR-0038, ADR-0111).
const PIN_SLOTS: usize = 256;
const PIN_FREE: u64 = 0;
const PIN_CLAIMING: u64 = u64::MAX;
/// Bounded retry budget for a full pin table. Each retry scans all 256 slots
/// and yields once; the budget is deliberately small so a saturated table
/// fails in microseconds to low milliseconds rather than stalling forever
/// (ADR-0111). The slot array and lock-free protocol are unchanged (ADR-0038).
const PIN_ACQUIRE_RETRIES: u32 = 1024;

/// Registered `snapshot_ts` until drop (ADR-0023 / ADR-0038).
pub struct SnapshotPin<'a> {
    graph: &'a Graph,
    ts: u64,
    slot: usize,
}

impl SnapshotPin<'_> {
    pub fn ts(&self) -> u64 {
        self.ts
    }

    /// Keep the pin after this guard is forgotten (explicit txn, ADR-0028 / ADR-0038).
    pub fn detach(self) -> usize {
        let slot = self.slot;
        std::mem::forget(self);
        slot
    }
}

impl Drop for SnapshotPin<'_> {
    fn drop(&mut self) {
        self.graph.unpin_slot(self.slot);
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

    /// Live graph + index logical bytes (ADR-0042, ADR-0202).
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
            .load(Ordering::Relaxed)
            .saturating_add(self.interner.bytes())
    }

    /// Full walk of retained versions, adjacency, and the interner
    /// (test / `rebuild_indexes` oracle).
    pub fn accounted_bytes(&self) -> u64 {
        self.structure_bytes().saturating_add(self.interner.bytes())
    }

    fn structure_bytes(&self) -> u64 {
        let charge = self.index_charge();
        let mut n = 0u64;
        n = n.saturating_add(self.nodes.array_bytes());
        self.nodes.for_each(|_id, vers| {
            n = n.saturating_add(crate::memory::node_chain_bytes(vers.shape()));
            for v in vers.iter() {
                n = n.saturating_add(crate::memory::live_node_account_live(
                    v.live,
                    &self.interner,
                    &charge,
                ));
            }
        });
        n = n.saturating_add(self.rels.array_bytes());
        self.rels.for_each(|_id, vers| {
            n = n.saturating_add(crate::memory::rel_chain_bytes(vers.shape()));
            for v in vers.iter() {
                n = n.saturating_add(crate::memory::live_rel_account_live(v.live));
            }
        });
        n = n.saturating_add(self.out_adj.array_bytes());
        n = n.saturating_add(self.in_adj.array_bytes());
        self.out_adj
            .for_each(|_id, adj| n = n.saturating_add(adj.floor_bytes()));
        self.in_adj
            .for_each(|_id, adj| n = n.saturating_add(adj.floor_bytes()));
        // The sealed side tables (ADR-0551) are retained structure: each
        // entry's compressed chunks and tombstones, the floor ADR-0551
        // prices, plus the slot arrays that hold them — the term the heap
        // census reads as `slot_bytes`.
        n = n.saturating_add(self.sealed_out.array_bytes());
        n = n.saturating_add(self.sealed_in.array_bytes());
        for sealed in [&self.sealed_out, &self.sealed_in] {
            sealed.for_each(|_id, region| n = n.saturating_add(region.floor_bytes()));
        }
        // LABS-351 / LABS-370: the degree histograms and the count
        // bundle's charged maps are retained structure — the recount
        // oracle must include them or the incremental entry charges
        // drift from it (the #398 lesson). Each family's term lives
        // with the family (graph/degree_stats.rs,
        // graph/count_state.rs) and composes here, at the oracle's
        // one site, beside the constraint-key term.
        n = n.saturating_add(self.degree_stats_structure_bytes());
        n = n.saturating_add(self.count_state_structure_bytes());
        // ADR-0439: the committed constraint-key map is retained structure
        // too; it carries the bytes each entry was charged, so the recount
        // and the incremental charge cannot drift.
        n = n.saturating_add(self.constraint_keys_structure_bytes());
        n
    }

    pub fn recompute_used(&self) {
        self.used_bytes
            .store(self.structure_bytes(), Ordering::Relaxed);
    }

    /// Live-size delta of `ops`. Nodes are accounted under the charge in
    /// force after `ops`; when that differs from the current charge (DDL in
    /// the batch), every retained version — including ones `ops` tombstone —
    /// is moved from the old charge to the new one. A delete keeps its
    /// record until GC, so it adds nothing. A copy-on-write set adds the
    /// new version.
    pub fn estimate_redo_delta(&self, ops: &[RedoOp]) -> i64 {
        self.estimate_redo_delta_with_constraint_growth(ops, None)
            .total
    }

    /// [`Graph::estimate_redo_delta`] with the constraint-key entry-growth
    /// term optionally supplied by the caller. The group-commit admission
    /// loop classifies each slot once and passes the delta its walk produced,
    /// so the slot's ops are not re-classified here (ADR-0439).
    #[doc(hidden)]
    pub fn estimate_redo_delta_with_constraint_growth(
        &self,
        ops: &[RedoOp],
        constraint_growth: Option<i64>,
    ) -> crate::memory::RedoDelta {
        let before = self.index_charge();
        let after = self.charge_after_ops(ops);
        let crate::memory::RedoDelta {
            total: mut delta,
            mut spill,
        } = crate::memory::estimate_redo_delta(
            &self.interner,
            ops,
            |id| self.node(id),
            |id| self.rel_form(id),
            |id| self.node_chain_shape(id),
            |id| self.rel_chain_shape(id),
            self.nodes.span(),
            self.rels.span(),
            &after,
        );
        let (adj_refusable, adj_spill) = self.estimate_adj_delta(ops);
        delta += adj_refusable + adj_spill;
        spill += adj_spill;
        delta += self.estimate_intern_delta(ops);
        // LABS-370 (security follow-up): the count family's entry growth
        // is committed structure too — an oversized-label write must be
        // rejected at admission, not after its entries land past the cap
        // (graph/degree_stats.rs owns the term).
        delta += self.estimate_stats_entry_growth(ops);
        // ADR-0439: a constrained write's new entries are committed
        // structure too — refused at admission, never after they land. The
        // group path supplies the delta its admission walk already computed.
        delta += match constraint_growth {
            Some(growth) => growth,
            None => self.estimate_constraint_key_entry_growth(ops),
        };
        if before.as_ref() != &after {
            self.nodes.for_each(|id, vers| {
                for v in vers.iter() {
                    let n = v.live.to_node(id, &self.interner);
                    delta += crate::memory::live_node_account(&n, &after) as i64
                        - crate::memory::live_node_account(&n, &before) as i64;
                }
            });
        }
        crate::memory::RedoDelta {
            total: delta,
            spill,
        }
    }

    /// The adjacency share of a batch's live-size delta, split into the
    /// refusable growth — a create's hop and slot growth (dense span or the
    /// side-map entry a wide id takes, as the store's own estimator charges
    /// it), plus a founding delete's dense side-table span — and the spill
    /// the checkpoint fold reclaims: a delete's region, chunk, and tombstone
    /// terms, and the side-map entry a founding delete at a wide id takes
    /// (ADR-0529).
    fn estimate_adj_delta(&self, ops: &[RedoOp]) -> (i64, i64) {
        let mut out: HashMap<NodeId, Vec<(u32, usize)>> = HashMap::new();
        let mut inn: HashMap<NodeId, Vec<(u32, usize)>> = HashMap::new();
        let mut fresh_ids = FreshIds::new();
        let mut delta = 0i64;
        let slot = adj_slot();
        let overflow = crate::memory::overflow_entry_bytes(adj_value());
        let mut out_span = self.out_adj.span();
        let mut in_span = self.in_adj.span();
        let mut out_side = HashSet::new();
        let mut in_side = HashSet::new();
        // One traversal carries both halves: the create terms, and the
        // batch's own rel identities and pending delete tombstones the
        // delete half prices. A rel created earlier in this batch is
        // invisible to the committed store, so its delete resolves from
        // `created` — only creates already walked are in it, so a delete
        // that precedes its create tombstones nothing, as in the apply path;
        // a rel the store holds comes from its live version.
        let mut created: HashMap<RelId, (NodeId, NodeId, u32)> = HashMap::new();
        let mut pending_out: HashMap<NodeId, HashMap<u32, HashSet<RelId>>> = HashMap::new();
        let mut pending_in: HashMap<NodeId, HashMap<u32, HashSet<RelId>>> = HashMap::new();
        for op in ops {
            match op {
                RedoOp::CreateRel {
                    id,
                    start,
                    end,
                    rel_type,
                    ..
                } => {
                    let type_id = fresh_ids.resolve(self, rel_type);
                    created.insert(*id, (*start, *end, type_id));
                    // One op touches both sides by the same rule, so each
                    // side's store, sealed region, span, side-map set and
                    // pending map travel together as one named value and
                    // the loop runs once per endpoint — the pair the
                    // incident and detach walks already iterate as a
                    // slice. The fields name the side's slots because the
                    // two store refs and the three mutable slots have
                    // pairwise-identical types: a tuple position could
                    // swap an out/in pair silently, a field name cannot.
                    struct SideAdmission<'a, 'm> {
                        adj: &'a IdVec<TypedAdj>,
                        sealed: &'a IdVec<SealedAdj>,
                        node: NodeId,
                        span: &'m mut usize,
                        side: &'m mut HashSet<NodeId>,
                        pending: &'m mut HashMap<NodeId, Vec<(u32, usize)>>,
                    }
                    let mut sides = [
                        SideAdmission {
                            adj: &self.out_adj,
                            sealed: &self.sealed_out,
                            node: *start,
                            span: &mut out_span,
                            side: &mut out_side,
                            pending: &mut out,
                        },
                        SideAdmission {
                            adj: &self.in_adj,
                            sealed: &self.sealed_in,
                            node: *end,
                            span: &mut in_span,
                            side: &mut in_side,
                            pending: &mut inn,
                        },
                    ];
                    for side in &mut sides {
                        if !side.adj.contains_key(&side.node) {
                            // The one placement policy, priced here: a dense
                            // growth is committed structure (refusable); a
                            // side-map entry is priced once per node per
                            // batch — this loop can revisit a node across
                            // its ops, so the dedupe is this caller's.
                            match crate::memory::place_in_span(side.span, side.node, slot) {
                                // An id the array cannot index is refused by
                                // the commit's own validation before anything
                                // applies, so it prices nothing.
                                None | Some(crate::memory::SpanPlacement::Inside) => {}
                                Some(crate::memory::SpanPlacement::Grew(bytes)) => {
                                    delta += bytes as i64;
                                }
                                Some(crate::memory::SpanPlacement::SideEntry) => {
                                    if side.side.insert(side.node) {
                                        delta += overflow as i64;
                                    }
                                }
                            }
                        }
                        delta += bump_adj(side.pending, side.node, type_id, || {
                            hop_counts(side.adj, side.sealed, side.node)
                        });
                    }
                }
                RedoOp::DeleteRel { id } => {
                    let Some((start, end, type_id)) = created
                        .get(id)
                        .copied()
                        // The store's rels answer through the one live-identity
                        // spelling (a dead version's delete tombstones nothing,
                        // as in the apply path).
                        .or_else(|| self.rel_live_identity(*id))
                    else {
                        continue;
                    };
                    pending_out
                        .entry(start)
                        .or_default()
                        .entry(type_id)
                        .or_default()
                        .insert(*id);
                    pending_in
                        .entry(end)
                        .or_default()
                        .entry(type_id)
                        .or_default()
                        .insert(*id);
                }
                _ => {}
            }
        }
        // Each delete records one tombstone per endpoint that does not hold
        // one yet (ADR-0551); the region, chunk, and tombstone terms are
        // reclaimed at the checkpoint fold, so they are spill and never
        // refuse a batch (ADR-0529: a delete at the cap commits). The
        // founding side-table slot is the exception — the dense span only
        // grows (ADR-0086) — so it stays refusable.
        let (slot_growth, delete_spill) = self.estimate_delete_spill(&pending_out, &pending_in);
        (delta + slot_growth, delete_spill)
    }

    /// The batch's delete terms, as `(refusable, spill)`: every delete
    /// records one tombstone per endpoint that does not hold one yet
    /// (ADR-0551) — a sealed chunk grows its tombstone vec, a never-sealed
    /// type adds its on-demand chunk to the endpoint's region, all reclaimed
    /// at the next checkpoint fold and so spill that never refuses a batch
    /// (ADR-0529) — while a founding delete's dense side-table span is
    /// permanent (ADR-0086) and stays refusable; its side-map entry,
    /// reclaimed with the region, is spill like the rest.
    fn estimate_delete_spill(
        &self,
        pending_out: &HashMap<NodeId, HashMap<u32, HashSet<RelId>>>,
        pending_in: &HashMap<NodeId, HashMap<u32, HashSet<RelId>>>,
    ) -> (i64, i64) {
        let sealed_slot = sealed_slot();
        let sealed_overflow = crate::memory::overflow_entry_bytes(sealed_value());
        let (out_refusable, out_spill) =
            price_side(&self.sealed_out, pending_out, sealed_slot, sealed_overflow);
        let (in_refusable, in_spill) =
            price_side(&self.sealed_in, pending_in, sealed_slot, sealed_overflow);
        (out_refusable + in_refusable, out_spill + in_spill)
    }

    fn estimate_intern_delta(&self, ops: &[RedoOp]) -> i64 {
        let mut seen = HashSet::new();
        let mut extra = 0i64;
        let mut consider = |s: &str| {
            if !seen.insert(s.to_string()) {
                return;
            }
            if self.interner.get_id(s).is_none() {
                extra += crate::memory::interned_string_bytes(s.len()) as i64;
            }
        };
        for op in ops {
            match op {
                RedoOp::CreateNode {
                    labels, properties, ..
                } => {
                    for label in labels.iter() {
                        consider(label);
                    }
                    for key in properties.keys() {
                        consider(key);
                    }
                }
                RedoOp::CreateRel {
                    rel_type,
                    properties,
                    ..
                } => {
                    consider(rel_type);
                    for key in properties.keys() {
                        consider(key);
                    }
                }
                RedoOp::SetNodeProp { key, .. }
                | RedoOp::RemoveNodeProp { key, .. }
                | RedoOp::SetRelProp { key, .. }
                | RedoOp::RemoveRelProp { key, .. } => consider(key),
                RedoOp::AddLabels { labels, .. } | RedoOp::RemoveLabels { labels, .. } => {
                    for label in labels {
                        consider(label);
                    }
                }
                _ => {}
            }
        }
        extra
    }

    fn index_charge(&self) -> Arc<crate::memory::IndexCharge> {
        Arc::clone(&self.indexes.charge_cache.lock().unwrap())
    }

    fn refresh_index_charge(&self) {
        let charge = {
            let theater = self.theater.lock().unwrap();
            Arc::new(crate::memory::IndexCharge::from_details(
                theater.index_details.values(),
            ))
        };
        *self.indexes.charge_cache.lock().unwrap() = charge;
    }

    fn charge_after_ops(&self, ops: &[RedoOp]) -> crate::memory::IndexCharge {
        let theater = self.theater.lock().unwrap();
        let mut details = theater.index_details.clone();
        drop(theater);
        crate::redo::project_index_details(&mut details, ops);
        crate::memory::IndexCharge::from_details(details.values())
    }

    fn charge(&self, delta: i64) {
        if delta > 0 {
            self.used_bytes.fetch_add(delta as u64, Ordering::Relaxed);
        } else if delta < 0 {
            let sub = (-delta) as u64;
            atomic_fetch_update!(self.used_bytes, Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(sub))
            })
            .ok();
        }
    }

    fn charge_node_swap(&self, before: Option<&Node>, after: Option<&Node>) {
        let charge = self.index_charge();
        let b = before
            .map(|n| crate::memory::live_node_account(n, &charge))
            .unwrap_or(0);
        let a = after
            .map(|n| crate::memory::live_node_account(n, &charge))
            .unwrap_or(0);
        self.charge(a as i64 - b as i64);
    }

    fn charge_rel_after_edit(&self, id: RelId, before_bytes: u64, before_shape: ChainShape) {
        let after_shape = self.rel_chain_shape(id);
        let after_bytes = self.visible_rel_account(id);
        if after_shape != before_shape {
            let growth = crate::memory::rel_chain_bytes(after_shape) as i64
                - crate::memory::rel_chain_bytes(before_shape) as i64;
            self.charge(after_bytes as i64 + growth);
        } else {
            self.charge(after_bytes as i64 - before_bytes as i64);
        }
    }

    #[cfg(test)]
    fn node_chain_len(&self, id: NodeId) -> usize {
        self.nodes.get(&id).map_or(0, |v| v.len())
    }

    fn node_chain_shape(&self, id: NodeId) -> ChainShape {
        self.nodes
            .get(&id)
            .map_or(ChainShape::Absent, |v| v.shape())
    }

    fn rel_chain_shape(&self, id: RelId) -> ChainShape {
        self.rels.get(&id).map_or(ChainShape::Absent, |v| v.shape())
    }

    /// Copy-on-write keeps the previous version. An in-place edit replaces it.
    fn charge_node_after_edit(&self, id: NodeId, before: Option<&Node>, before_shape: ChainShape) {
        let after = self.node(id);
        let after_shape = self.node_chain_shape(id);
        if after_shape != before_shape {
            let charge = self.index_charge();
            let added = after
                .as_ref()
                .map(|n| crate::memory::live_node_account(n, &charge))
                .unwrap_or(0);
            let growth = crate::memory::node_chain_bytes(after_shape) as i64
                - crate::memory::node_chain_bytes(before_shape) as i64;
            self.charge(added as i64 + growth);
        } else {
            self.charge_node_swap(before, after.as_ref());
        }
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

    /// Pin `commit_ts` so GC cannot drop versions this statement still sees.
    ///
    /// The slot protocol is lock-free (ADR-0038). When every slot is
    /// occupied the wait is bounded: the array is retried a fixed number of
    /// times and then acquisition fails with a typed
    /// `Error::Execution("snapshot pin: …")` instead of spinning forever
    /// (ADR-0111).
    pub fn pin_now(&self) -> Result<SnapshotPin<'_>> {
        self.pin_now_with_retry(std::thread::yield_now)
    }

    /// Run the callback once after each unsuccessful full-table scan.
    fn pin_now_with_retry(&self, mut retry: impl FnMut()) -> Result<SnapshotPin<'_>> {
        let start = self.pin_cursor.fetch_add(1, Ordering::Relaxed) as usize;
        for _ in 0..PIN_ACQUIRE_RETRIES {
            for i in 0..PIN_SLOTS {
                let slot = (start + i) % PIN_SLOTS;
                if self.pins[slot]
                    .compare_exchange(PIN_FREE, PIN_CLAIMING, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let ts = self.commit_ts();
                    self.pins[slot].store(ts.saturating_add(1), Ordering::Release);
                    return Ok(SnapshotPin {
                        graph: self,
                        ts,
                        slot,
                    });
                }
            }
            retry();
        }
        Err(relate_core::Error::execution(format!(
            "snapshot pin: all {PIN_SLOTS} slots are held"
        )))
    }

    pub fn unpin_slot(&self, slot: usize) {
        self.pins[slot].store(PIN_FREE, Ordering::Release);
    }

    /// True while any statement or explicit transaction holds a snapshot pin.
    pub fn snapshot_pinned(&self) -> bool {
        self.pins
            .iter()
            .any(|slot| slot.load(Ordering::Acquire) != PIN_FREE)
    }

    /// Lowest timestamp that may still be read. Dead iff `xmax != 0 && xmax <= horizon`.
    pub fn gc_horizon(&self) -> u64 {
        let commit = self.commit_ts();
        let mut min_pin: Option<u64> = None;
        for slot in self.pins.iter() {
            let v = slot.load(Ordering::Acquire);
            if v == PIN_CLAIMING {
                return 0;
            }
            if v != PIN_FREE {
                let ts = v - 1;
                min_pin = Some(min_pin.map_or(ts, |m| m.min(ts)));
            }
        }
        min_pin.unwrap_or(commit).min(commit)
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

    pub fn named_indexes(&self) -> Vec<IndexDef> {
        self.catalog
            .lock()
            .unwrap()
            .indexes
            .values()
            .cloned()
            .collect()
    }

    /// Capture named indexes, constraints and their SHOW metadata at one
    /// admission boundary. Embedded callers hold the publication read guard
    /// across this call and `pin_now`; direct callers must serialize with
    /// their own graph writers in the same way.
    pub fn schema_snapshot(&self) -> SchemaSnapshot {
        self.schema_snapshot_with_hook(|| {})
    }

    fn schema_snapshot_with_hook(&self, after_catalog: impl FnOnce()) -> SchemaSnapshot {
        let (indexes, constraints) = {
            let catalog = self.catalog.lock().unwrap();
            (
                catalog.indexes.values().cloned().collect(),
                catalog.constraints.values().cloned().collect(),
            )
        };
        after_catalog();
        // Publication serialization makes these non-overlapping captures
        // (catalog above, then databases + theater in capture_show_schema)
        // atomic to embedded readers, without ever nesting catalog/theater
        // locks.
        let show = self.capture_show_schema();
        SchemaSnapshot {
            indexes,
            constraints,
            show,
        }
    }

    /// The committed SHOW metadata at this instant: the store-level
    /// databases table plus this graph's theater, under the theater lock.
    pub(crate) fn capture_show_schema(&self) -> ShowSchema {
        ShowSchema::capture(self.databases().snapshot(), &self.theater.lock().unwrap())
    }

    pub fn named_constraints(&self) -> Vec<ConstraintDef> {
        self.catalog
            .lock()
            .unwrap()
            .constraints
            .values()
            .cloned()
            .collect()
    }

    /// Install one catalog constraint row. Restore paths only: this mutates
    /// the catalog WITHOUT the maintained constraint-key map (ADR-0439), so
    /// the caller must finish with `rebuild_indexes` / `finish_restore`,
    /// which builds the map from the catalog ∪ the theater's derived set.
    /// Calling it on a live graph would leave a uniqueness constraint the
    /// admission probe cannot see.
    pub fn install_named_constraint(&self, def: ConstraintDef) -> Result<()> {
        self.catalog.lock().unwrap().insert_constraint(def)
    }

    pub fn named_index(&self, name: &str) -> Option<IndexDef> {
        self.catalog.lock().unwrap().indexes.get(name).cloned()
    }

    /// By-name constraint probe, the constraint side of [`Self::named_index`].
    pub fn has_constraint(&self, name: &str) -> bool {
        self.catalog.lock().unwrap().constraints.contains_key(name)
    }

    pub fn named_index_by_schema(&self, label: &str, property: &str) -> Option<IndexDef> {
        self.catalog
            .lock()
            .unwrap()
            .index_by_schema(label, property)
            .cloned()
    }

    pub fn install_named_index(&self, def: IndexDef) -> relate_core::Result<()> {
        self.catalog.lock().unwrap().insert_index(def)
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

fn hop_counts(map: &IdVec<TypedAdj>, sealed: &IdVec<SealedAdj>, node: NodeId) -> Vec<(u32, usize)> {
    map.get(&node)
        .map(|adj| {
            adj.chunk_lens(sealed.get(&node).as_deref())
                .map(|(type_id, delta, sealed)| (type_id, delta + sealed))
                .collect()
        })
        .unwrap_or_default()
}

/// The admission estimates' fresh-name resolver (LABS-425, stellar round
/// 1): a name the interner does not know — a label or rel type the
/// not-yet-applied batch mints — resolves to a descending id below the
/// committed range, one id per distinct name. Both estimate terms
/// ([`Graph::estimate_adj_delta`] here,
/// [`Graph::estimate_stats_entry_growth`] in
/// graph/degree_stats_estimate.rs) resolved this ladder by hand before;
/// the descending-id collision invariant (fresh ids never collide with
/// committed ids or each other) now lives in exactly one place.
#[derive(Clone, Debug, PartialEq)]
struct FreshIds {
    fresh: HashMap<String, u32>,
    next: u32,
}

impl FreshIds {
    fn new() -> Self {
        Self {
            fresh: HashMap::new(),
            next: u32::MAX,
        }
    }

    /// The interned id, else this batch's earlier mint, else a new
    /// descending id minted and memoized.
    fn resolve(&mut self, g: &Graph, name: &str) -> u32 {
        g.interner
            .get_id(name)
            .or_else(|| self.fresh.get(name).copied())
            .unwrap_or_else(|| {
                let id = self.next;
                self.next = self.next.saturating_sub(1);
                self.fresh.insert(name.to_string(), id);
                id
            })
    }
}

/// One node's adjacency floor in one direction: the delta hop bytes plus its
/// sealed side-table entry's compressed chunks and tombstones.
fn adj_floor(map: &IdVec<TypedAdj>, sealed: &IdVec<SealedAdj>, node: NodeId) -> u64 {
    map.get(&node).map(|adj| adj.floor_bytes()).unwrap_or(0)
        + sealed.get(&node).map_or(0, |region| region.floor_bytes())
}

/// One side's share of the delete terms
/// ([`Graph::estimate_delete_spill`](Graph::estimate_delete_spill)'s phase
/// 2), as the `(refusable, spill)` pair: per node, the region-level terms —
/// the node's dense slot span (permanent, refusable) or its reclaimed
/// side-map entry (spill), and the chunks-vec entry growth across every
/// joining type, one
/// before-and-after term per region, never one per type, or the reserved
/// model's pow2 steps misprice a batch that joins several — then each key's
/// own terms (all fold-reclaimed, spill): a sealed chunk's tombstone-vec
/// growth, or a joining chunk's empty stream and tombstones.
fn price_side(
    sealed: &IdVec<SealedAdj>,
    pending: &HashMap<NodeId, HashMap<u32, HashSet<RelId>>>,
    sealed_slot: u64,
    sealed_overflow: u64,
) -> (i64, i64) {
    let mut span = sealed.span();
    let mut refusable = 0i64;
    let mut spill = 0i64;
    for (node, keys) in pending {
        // One key loop prices every key's own term: a chunk the region
        // already holds grows its tombstone vec (a delete after a
        // reposition updates the existing slot in place), a type the
        // region does not hold joins it, bringing the empty stream and
        // its tombstones — priced once, here, never again in a
        // region-level arm.
        let region = sealed.get(node);
        let mut joining = 0;
        for (type_id, ids) in keys {
            match region.as_ref().and_then(|region| region.chunk(*type_id)) {
                Some(chunk) => {
                    let fresh = chunk.fresh_tombstone_count(ids);
                    spill += chunk.tombstone_growth_bytes(fresh) as i64;
                }
                None => {
                    joining += 1;
                    spill += crate::adjacency::fresh_sealed_chunk_floor_bytes(ids.len()) as i64;
                }
            }
        }
        // The region-level term, once per node: a founding delete grows the
        // node's slot in the side table — the dense span permanently
        // (refusable), a wide id's side-map entry only until the region
        // empties (spill) — and its chunks-vec entry growth is one
        // before-and-after term over every joining type, never one per
        // type, or the reserved model's pow2 steps misprice a batch that
        // joins several. An existing region prices the same entry growth
        // (zero when nothing joined).
        match region {
            None => {
                // The one placement policy, priced here: a dense slot-span
                // growth is permanent (refusable), a wide id's side-map
                // entry is reclaimed with the region it founds (spill). Each
                // node reaches this arm once (`pending` is keyed by node), so
                // the entry prices unconditionally — the per-batch dedupe is
                // the create loop's, where a node can recur.
                match crate::memory::place_in_span(&mut span, *node, sealed_slot) {
                    None | Some(crate::memory::SpanPlacement::Inside) => {}
                    Some(crate::memory::SpanPlacement::Grew(bytes)) => refusable += bytes as i64,
                    Some(crate::memory::SpanPlacement::SideEntry) => {
                        spill += sealed_overflow as i64;
                    }
                }
                spill += crate::adjacency::fresh_sealed_chunk_entry_bytes(joining) as i64;
            }
            Some(region) => {
                spill += region.chunk_entry_growth_bytes(joining) as i64;
            }
        }
    }
    (refusable, spill)
}

fn bump_adj(
    sim: &mut HashMap<NodeId, Vec<(u32, usize)>>,
    node: NodeId,
    type_id: u32,
    init: impl FnOnce() -> Vec<(u32, usize)>,
) -> i64 {
    let chunks = sim.entry(node).or_insert_with(init);
    let before =
        crate::memory::adjacency_bytes(&chunks.iter().map(|(_, n)| *n).collect::<Vec<_>>());
    if let Some(slot) = chunks.iter_mut().find(|c| c.0 == type_id) {
        slot.1 += 1;
    } else {
        chunks.push((type_id, 1));
    }
    let after = crate::memory::adjacency_bytes(&chunks.iter().map(|(_, n)| *n).collect::<Vec<_>>());
    after as i64 - before as i64
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
