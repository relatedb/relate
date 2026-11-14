//! The PROFILE metric vocabulary (ADR-0344): the sink the dispatch loop
//! reports each statement-level clause operator's metrics to, the record it
//! reports, and the typed facts the record carries (store-read mix, phase
//! times, walk start, kernel rung, frontier counters). The thread-local
//! clocks and counters that feed these live in [`crate::ctx`].

use crate::frontier_decline::FrontierDeclineReason;

/// The statement's PROFILE site (ADR-0344 phase 2): the metrics sink and
/// the `(stage, part)` position its keys ride — one fact, built by the
/// statement-level walkers when they call the clause loop, so the sink
/// never travels without the position it keys.
#[derive(Clone, Copy)]
pub(crate) struct PlanMetricsSite<'a> {
    /// The sink the facade's PROFILE path attached.
    pub(crate) sink: &'a dyn PlanMetricsSink,
    /// The NEXT-chain stage (1-based, matching the tree's stage
    /// numbering): each NEXT stage restarts its part indices at 0, so the
    /// stage keeps the keys disjoint.
    pub(crate) stage: usize,
    /// The part index within the stage.
    pub(crate) part: usize,
}

impl<'a> PlanMetricsSite<'a> {
    /// The one pairing of a sink with the clause loop's `(stage, part)`
    /// key — the statement-level walkers' single construction, so a sink
    /// never travels without the position it keys.
    pub(crate) fn at(sink: &'a dyn PlanMetricsSink, stage: usize, part: usize) -> Self {
        PlanMetricsSite { sink, stage, part }
    }
}

/// The store-read mix a clause's arm performed (LABS-369): the kinds the
/// single `Db Hits` total blends. A `Rows=103` operator that spent 300k
/// relationship hops and 0 node reads is a different animal from one that
/// read 103 nodes — the one-glance diagnosis for the scan-turned-traversal
/// family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreReads {
    /// Relationship hops taken.
    pub hops: u64,
    /// Neighbors the walk traversed (counted at the adjacency walk).
    pub neighbor_walks: u64,
    /// Node-store reads: one per candidate a node match examined (the
    /// all-nodes scan, the label scan, the index seek).
    pub node_reads: u64,
}

/// The clause's inclusive phase times (LABS-448): the elapsed time spent
/// resolving node candidates (the seek — the store work behind the
/// `Nodes` kind) and following relationships (the walk — the store work
/// behind the `Rels`/`Degree` kinds); the per-candidate admission checks
/// and the arm's binding/cloning stay unattributed. Inclusive of the nesting
/// a compound operator performs: a walk that triggers nested seeks counts
/// both, like the table's `Total` column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseTimes {
    /// The seek phase's elapsed nanoseconds.
    pub seek_ns: u128,
    /// The walk phase's elapsed nanoseconds.
    pub walk_ns: u128,
}

impl PhaseTimes {
    /// The phases one clause's arm performed: `self` minus the snapshot
    /// taken when the arm started. The sums only ever increase, so a
    /// decrease is a broken invariant — the same loud subtraction
    /// [`StoreReads::since`] uses, never a clamp to a fabricated zero.
    pub(crate) fn since(self, earlier: PhaseTimes) -> PhaseTimes {
        PhaseTimes {
            seek_ns: self.seek_ns - earlier.seek_ns,
            walk_ns: self.walk_ns - earlier.walk_ns,
        }
    }
}

/// The re-arrival rule's shared expansion budget across one frontier run
/// (the ADR-0395 fixed-constant family, like the runner's
/// `FRONTIER_WALK_BUDGET`): a fixed constant, no ambient knob — the
/// remedy for legitimate work that needs more is a new ADR. Every
/// trail-existence search of the run draws from this one pool, so the
/// rule's total cost is bounded by it however many re-arrivals a run
/// sees; sized an order under the runner's own edge-visit budget. The
/// denominator of [`FrontierCounters::budget_fraction`], so it lives
/// beside the counter it calibrates.
pub const REARRIVAL_WORK_BUDGET: u64 = 1_000_000;

/// The frontier family's run counters (ADR-0478), one clause's sum over
/// its per-seed BFS runs: the edge visits (the work the walk budget
/// caps), the completed levels, the accepted endpoints, and the NFA
/// rung's own PROFILE facts — the (node, state) cells the runs created,
/// the re-arrival work spent, the rule memo's hits and misses, and the
/// admission asks by verdict. Zero on every clause the frontier
/// declined — `levels > 0` is the engagement marker (a run always
/// completes its level 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrontierCounters {
    /// Incident-edge visits across the clause's frontier runs — the
    /// budget's own work counter.
    pub edge_visits: u64,
    /// BFS levels completed (level 0 included).
    pub levels: u64,
    /// Endpoints the runs accepted, in arrival order.
    pub endpoints: u64,
    /// (node, state) cells the runs created — the pairs that occupied
    /// the span×states tables, the BFS's own memory shape.
    pub cells: u64,
    /// Re-arrival rule work spent, summed over the runs — the numerator
    /// of [`Self::budget_fraction`].
    pub rearrival_work: u64,
    /// Rule-memo hits: asks the run's verdict table answered without a
    /// search.
    pub memo_hits: u64,
    /// Rule-memo misses: asks that ran the bounded search.
    pub memo_misses: u64,
    /// Admission asks the representative-chain proof settled — no
    /// search.
    pub representative_proofs: u64,
    /// Admission asks the bounded search proved (an in-bounds path
    /// found).
    pub search_proven: u64,
    /// Admission asks the bounded search disproved within its bounds.
    pub search_disproven: u64,
    /// Admission asks the shared work budget cut — the run-time decline's
    /// verdict class (ADR-0516).
    pub declined_exhausted: u64,
}

impl FrontierCounters {
    /// The counters one clause's arm performed: `self` minus the snapshot
    /// taken when the arm started — the counters only ever increase, so
    /// a decrease is a broken invariant, the same loud subtraction
    /// [`StoreReads::since`] uses.
    pub(crate) fn since(self, earlier: FrontierCounters) -> FrontierCounters {
        self - earlier
    }

    /// The fraction of one run's re-arrival work budget the clause's
    /// runs spent: summed [`Self::rearrival_work`] over
    /// [`REARRIVAL_WORK_BUDGET`] — `1.0` on a run the budget cut
    /// mid-search (the work guard drains the budget to exactly zero
    /// before declaring the cut), `0.0` on runs that never asked the
    /// rule. A multi-seed clause sums every run's spend over the
    /// one-run budget, so the fraction reads as run-equivalents.
    pub fn budget_fraction(&self) -> f64 {
        self.rearrival_work as f64 / REARRIVAL_WORK_BUDGET as f64
    }
}

/// The field-wise delta, the one spelling [`FrontierCounters::since`]
/// reads: destructured, so a new counter cannot be silently dropped.
impl std::ops::Sub for FrontierCounters {
    type Output = FrontierCounters;

    fn sub(self, earlier: FrontierCounters) -> FrontierCounters {
        let FrontierCounters {
            edge_visits,
            levels,
            endpoints,
            cells,
            rearrival_work,
            memo_hits,
            memo_misses,
            representative_proofs,
            search_proven,
            search_disproven,
            declined_exhausted,
        } = earlier;
        FrontierCounters {
            edge_visits: self.edge_visits - edge_visits,
            levels: self.levels - levels,
            endpoints: self.endpoints - endpoints,
            cells: self.cells - cells,
            rearrival_work: self.rearrival_work - rearrival_work,
            memo_hits: self.memo_hits - memo_hits,
            memo_misses: self.memo_misses - memo_misses,
            representative_proofs: self.representative_proofs - representative_proofs,
            search_proven: self.search_proven - search_proven,
            search_disproven: self.search_disproven - search_disproven,
            declined_exhausted: self.declined_exhausted - declined_exhausted,
        }
    }
}

/// The field-wise merge, the one spelling the per-run accumulation
/// (`exec::profiling::count_frontier`) and the rule's tally merge read:
/// destructured, so a new counter cannot be silently dropped.
impl std::ops::AddAssign for FrontierCounters {
    fn add_assign(&mut self, run: FrontierCounters) {
        let FrontierCounters {
            edge_visits,
            levels,
            endpoints,
            cells,
            rearrival_work,
            memo_hits,
            memo_misses,
            representative_proofs,
            search_proven,
            search_disproven,
            declined_exhausted,
        } = run;
        self.edge_visits += edge_visits;
        self.levels += levels;
        self.endpoints += endpoints;
        self.cells += cells;
        self.rearrival_work += rearrival_work;
        self.memo_hits += memo_hits;
        self.memo_misses += memo_misses;
        self.representative_proofs += representative_proofs;
        self.search_proven += search_proven;
        self.search_disproven += search_disproven;
        self.declined_exhausted += declined_exhausted;
    }
}

impl StoreReads {
    /// The reads one clause's arm performed: `self` minus the snapshot
    /// taken when the arm started. The counters only ever increase, so a
    /// decrease is a broken invariant — the same loud subtraction the
    /// store-access total uses, never a clamp to a fabricated zero.
    pub(crate) fn since(self, earlier: StoreReads) -> StoreReads {
        StoreReads {
            hops: self.hops - earlier.hops,
            neighbor_walks: self.neighbor_walks - earlier.neighbor_walks,
            node_reads: self.node_reads - earlier.node_reads,
        }
    }

    /// The kinds' sum — the one `Db Hits` total, derived rather than
    /// snapshotted a second time.
    pub fn total(self) -> u64 {
        self.hops + self.neighbor_walks + self.node_reads
    }
}

/// How a `MATCH` walk chose its start element (LABS-369): the fact the
/// opaque `Expand` hid. A displaced start — a cheap index seek that became
/// a 135k-relationship backward traversal — is named here instead of
/// showing up only as a large `Db Hits` total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkStart {
    /// The pattern element index the walk started from.
    pub element: usize,
    /// Whether the walk ran the pivot's prefix backwards from it.
    pub backward: bool,
    /// Why that start won.
    pub reason: StartReason,
}

impl WalkStart {
    /// The un-optimised default start: the pattern's first element — the
    /// seed side — walked forward, the `index-0` reason. The one spelling
    /// every kernel and batch arm that ignores the start ladder records.
    pub(crate) fn seed_side_forward() -> WalkStart {
        WalkStart {
            element: 0,
            backward: false,
            reason: StartReason::IndexZero,
        }
    }
}

/// The start-selection reason classes (ADR-0350's bound pivot, ADR-0351's
/// cheapest-start estimate, the LABS-161 relationship seek, the default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartReason {
    /// A row-bound element anchored the walk (ADR-0350).
    BoundPivot,
    /// The cheapest-start estimate chose the element (ADR-0351).
    Cheapest,
    /// A LABS-161 relationship seek anchored the walk.
    SeekAnchored,
    /// The pattern's first element: the un-optimised default.
    IndexZero,
}

impl StartReason {
    /// The reason's stable wire name (LABS-369): the vocabulary the plan
    /// args and the renderers spell, single-sourced here.
    pub fn as_str(self) -> &'static str {
        match self {
            StartReason::BoundPivot => "bound-pivot",
            StartReason::Cheapest => "cheapest",
            StartReason::SeekAnchored => "seek-anchored",
            StartReason::IndexZero => "index-0",
        }
    }
}

/// Which rung served one `MATCH` clause (LABS-1117): the fact EXPLAIN
/// could not show, because the rungs are runtime choices inside an
/// operator, not plan nodes. The ladder records it per clause; the
/// PROFILE record carries it. [`Walker`](Self::Walker) is the per-row
/// walker, the rung every decline's home — the batch scan/filter chain
/// is its own fact (`ClauseMetricsRecord::batched`), never a rung here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelRung {
    /// The unrolled fixed-k chain kernel, `hops` hops (ADR-0473).
    Chain { hops: usize },
    /// The hub-star candidate-intersection kernel (ADR-0473).
    HubStar,
    /// The frontier-NFA rung over the BFS (ADR-0478).
    FrontierNfa,
    /// The per-row walker — every gate's decline.
    Walker,
}

impl KernelRung {
    /// The rung's stable wire name (the `StartReason::as_str` pattern).
    /// The names live in the wire vocabulary (`KernelRungWire`'s
    /// constants), so the executor and every surface spell them once; the
    /// variant↔name mapping is pinned by `relate`'s
    /// `decline_and_start_reason_names_match_the_wire_tables`.
    pub fn as_str(self) -> &'static str {
        match self {
            KernelRung::Chain { .. } => relate_model::wire::KernelRungWire::CHAIN,
            KernelRung::HubStar => relate_model::wire::KernelRungWire::HUB_STAR,
            KernelRung::FrontierNfa => relate_model::wire::KernelRungWire::FRONTIER_NFA,
            KernelRung::Walker => relate_model::wire::KernelRungWire::WALKER,
        }
    }
}

/// One clause operator's PROFILE metrics, reported to the sink (ADR-0344
/// phase 2 + the metric-fields phase): the position key `(stage, part,
/// clause)` — the same walk the plan tree construction uses — beside the
/// operator's actuals. Bundled so the sink signature stays at one
/// argument however many metrics a later phase adds.
pub struct ClauseMetricsRecord {
    /// The NEXT-chain position (1-based, matching the tree's stage
    /// numbering): each NEXT stage restarts its part indices at 0, so the
    /// stage keeps the keys disjoint — without it a chained statement's
    /// later stage overwrites the earlier stage's entries.
    pub stage: usize,
    pub part: usize,
    pub clause: usize,
    /// Rows the operator produced.
    pub rows: u64,
    /// Row-materialization bytes: the summed logical size
    /// (`Value::logical_bytes`, ADR-0042, plus each key's string bytes)
    /// of the rows the operator produced. Stateful operators'
    /// non-row state (aggregation tables, sort buffers) is a follow-up.
    pub memory_bytes: u64,
    /// Expression allocation bytes the clause's arm charged against the
    /// ADR-0463 statement budget — the delta of the view's expression
    /// counter across the arm, the same snapshot/delta shape as
    /// [`ClauseMetricsRecord::db_hits`]. Zero for a clause that builds no
    /// containers or strings.
    pub expression_built_bytes: u64,
    /// The operator's elapsed time, nanoseconds.
    pub time_ns: u128,
    /// The store-read mix behind [`ClauseMetricsRecord::db_hits`]
    /// (LABS-369): hops vs neighbor walks vs node reads.
    pub reads: StoreReads,
    /// The clause's inclusive phase times (LABS-448): the seek (candidate
    /// resolution and checks) and the walk (relationship traversal),
    /// stamped only while a PROFILE statement's arm runs — a non-PROFILEd
    /// statement's record carries zeros.
    pub phases: PhaseTimes,
    /// How the walk chose its start, when the clause ran a `MATCH` walk
    /// (LABS-369) — `None` for clauses that walk nothing.
    pub start: Option<WalkStart>,
    /// Which rung served the clause's `MATCH` walk (LABS-1117): the
    /// chain kernel, the hub-star kernel, the frontier-NFA rung, or the
    /// per-row walker — the operator the clause's `MATCH` ladder (or, for
    /// a `SEARCH` `MATCH`, its direct walker call) returned. `None`
    /// exactly when no `MATCH` operator ran: a non-`MATCH` clause; a
    /// `MATCH` a fold answered (`try_match_folds`, a boolean or walk
    /// count); a `MATCH` inside a nested evaluation (its own loop); and a
    /// `MATCH` the ADR-0468 batch chain answered — that operator is
    /// [`Self::batched`]'s own fact, never a traversal rung.
    pub kernel: Option<KernelRung>,
    /// Whether the clause ran ADR-0468's batch scan/filter/project chain
    /// over `Chunk` (LABS-359) — the PROFILE-visible fact that a
    /// scan-shaped `MATCH` interior executed as batch operators. `false`
    /// is every other clause, and every clause the chain declined.
    pub batched: bool,
    /// The batch filter's zone skips (LABS-645): how many conjunct-batch
    /// evaluations the chunk's derived min/max zone metadata provably
    /// replaced with the correct empty survivor set. `0` on every clause
    /// the chain declined and every clause that skipped nothing.
    pub zone_skips: u64,
    /// The frontier family's run counters (ADR-0478): the BFS's edge
    /// visits, completed levels, and accepted endpoints, summed over the
    /// clause's per-seed runs. Zero on every clause the frontier's
    /// compile gates declined (it never ran); a run-time decline
    /// ([`FrontierDeclineReason::Run`]) keeps them recorded, since the
    /// rung did run. The plan table's `Details` cell marks the operator
    /// (`[frontier edges=N levels=L endpoints=P]`) when it ran.
    pub frontier: FrontierCounters,
    /// Why the frontier rung did not serve the clause (ADR-0516, extended
    /// by the plan-time half): the first typed reason, `None` on every
    /// clause the rung served and on every clause the rung never
    /// considered (no single quantified pattern, the diagnostic switch's
    /// `walk` stand-down, a nested evaluation's own arm). A plan-time
    /// reason ([`FrontierDeclineReason::Plan`]) is a gate that kept the
    /// clause off the rung — the clause itself ran no frontier (its counters
    /// stay zero unless a nested EXISTS frontier run in the same clause
    /// contributed); a run-time reason
    /// ([`FrontierDeclineReason::Run`]) is the re-arrival budget's
    /// exhaustion or the table ceiling's crossing, the counters recorded
    /// beside it.
    pub frontier_decline: Option<FrontierDeclineReason>,
}

/// Per-clause PROFILE metrics sink (ADR-0344 phase 2): the dispatch loop
/// reports each statement-level clause operator's metrics once it
/// finishes, keyed `(stage, part, clause)` — the same walk the plan tree
/// construction uses, so the collected entries stamp onto the PROFILE
/// result's plan tree without a second mapping.
pub trait PlanMetricsSink {
    /// The record's `stage` is the NEXT-chain position (1-based, matching
    /// the tree's stage numbering): each NEXT stage restarts its part
    /// indices at 0, so the stage keeps the keys disjoint — without it a
    /// chained statement's later stage overwrites the earlier stage's
    /// entries.
    fn record_clause(&self, record: ClauseMetricsRecord);
}
