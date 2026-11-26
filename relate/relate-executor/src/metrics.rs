//! The PROFILE metric vocabulary (ADR-0344): the sink the dispatch loop
//! reports each statement-level clause operator's metrics to, the record it
//! reports, and the typed facts the record carries (store-read mix, phase
//! times, walk start, kernel rung, frontier counters). The thread-local
//! clocks and counters that feed these live beside the phase they count:
//! the phase clocks and node reads in [`crate::ctx`], the hop,
//! neighbor-walk, zone-skip and frontier tallies in
//! [`crate::exec::profiling`].

use crate::frontier_decline::FrontierDeclineReason;

/// Declares a counter struct of same-typed monotonic fields together with
/// the field-wise delta `since` — a decrease is a broken invariant, so the
/// subtraction is checked and stays loud in release as in debug, never a
/// wrapped or clamped value. One field list generates both, so a new
/// counter can neither miss the delta nor the `since`. A `merge`
/// declaration additionally generates the field-wise merge
/// (`AddAssign`), for the counter whose documented semantic is
/// accumulation across runs. The family is the *monotone* counters;
/// `exec::profile_tick`'s `ExpressionBytes` stays outside it because its
/// meter falls when an arm releases memory, so its delta saturates.
macro_rules! delta_counters {
    // The accumulating counter: the declaration plus the merge.
    (merge $($rest:tt)*) => {
        delta_counters!(@declare [merge] $($rest)*);
    };
    // The one parse of the declaration grammar: the struct, its `since`,
    // and — when declared `merge` — the field-wise merge.
    (@declare [$($mode:ident)?]
        $(#[$meta:meta])*
        pub struct $name:ident: $ty:ty {
            $($(#[$fmeta:meta])* $field:ident,)+
        }
    ) => {
        $(#[$meta])*
        pub struct $name {
            $($(#[$fmeta])* pub $field: $ty,)+
        }

        impl $name {
            /// The field-wise delta from `earlier` — the one spelling every
            /// reader uses. The counters only ever increase, so a decrease
            /// is a broken invariant: the subtraction is checked, never a
            /// wrapped or clamped value.
            pub fn since(self, earlier: $name) -> $name {
                $name {
                    $(
                        $field: self.$field
                            .checked_sub(earlier.$field)
                            .expect("the counters only ever increase"),
                    )+
                }
            }
        }

        delta_counters!(@maybe_merge $($mode)? $name { $($field,)+ });
    };
    // The merge: the per-run accumulation in
    // `exec::profiling::count_frontier`, the delta's checked spelling.
    (@maybe_merge merge $name:ident { $($field:ident,)+ }) => {
        impl std::ops::AddAssign for $name {
            fn add_assign(&mut self, run: $name) {
                $(
                    self.$field = self.$field
                        .checked_add(run.$field)
                        .expect("the counters only ever accumulate");
                )+
            }
        }
    };
    (@maybe_merge $name:ident { $($field:ident,)+ }) => {};
    // The plain counter: the declaration only.
    ($($rest:tt)*) => {
        delta_counters!(@declare [] $($rest)*);
    };
}

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

delta_counters! {
    merge
    /// The store-read mix a clause's arm performed (LABS-369): the kinds the
    /// single `Db Hits` total blends. A `Rows=103` operator that spent 300k
    /// relationship hops and 0 node reads is a different animal from one that
    /// read 103 nodes — the one-glance diagnosis for the scan-turned-traversal
    /// family. Merged across one streamed stage's PROFILE windows
    /// (`profile_chain.rs`): the stage's record is the checked sum over
    /// its batches, minus the immediate downstream stage's sum.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct StoreReads: u64 {
        /// Relationship hops taken.
        hops,
        /// Neighbors the walk traversed (counted at the adjacency walk).
        neighbor_walks,
        /// Node-store reads: one per candidate a node match examined (the
        /// all-nodes scan, the label scan, the index seek).
        node_reads,
    }
}

delta_counters! {
    merge
    /// The clause's inclusive phase times (LABS-448): the elapsed time spent
    /// resolving node candidates (the seek — the store work behind the
    /// `Nodes` kind) and following relationships (the walk — the store work
    /// behind the `Rels`/`Degree` kinds); the per-candidate admission checks
    /// and the arm's binding/cloning stay unattributed. Inclusive of the nesting
    /// a compound operator performs: a walk that triggers nested seeks counts
    /// both, like the table's `Total` column. Merged across one streamed
    /// stage's PROFILE windows (`profile_chain.rs`), the store-read mix's
    /// shape.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct PhaseTimes: u128 {
        /// The seek phase's elapsed nanoseconds.
        seek_ns,
        /// The walk phase's elapsed nanoseconds.
        walk_ns,
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

delta_counters! {
    merge
    /// The frontier family's run counters (ADR-0478), one clause's sum over
    /// its per-seed BFS runs: the edge visits (the work the walk budget
    /// caps), the completed levels, the accepted endpoints, and the NFA
    /// rung's own PROFILE facts — the (node, state) cells the runs created,
    /// the re-arrival work spent, the rule memo's hits and misses, and the
    /// admission asks by verdict. Zero on every clause the frontier
    /// declined — `levels > 0` is the engagement marker (a run always
    /// completes its level 0).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct FrontierCounters: u64 {
        /// Incident-edge visits across the clause's frontier runs — the
        /// budget's own work counter.
        edge_visits,
        /// BFS levels completed (level 0 included).
        levels,
        /// Endpoints the runs accepted, in arrival order.
        endpoints,
        /// (node, state) cells the runs created — the pairs that occupied
        /// the span×states tables, the BFS's own memory shape.
        cells,
        /// Re-arrival rule work spent, summed over the runs — the numerator
        /// of [`Self::budget_fraction`].
        rearrival_work,
        /// Rule-memo hits: asks the run's verdict table answered without a
        /// search.
        memo_hits,
        /// Rule-memo misses: asks that ran the bounded search.
        memo_misses,
        /// Admission asks the representative-chain proof settled — no
        /// search.
        representative_proofs,
        /// Admission asks the bounded search proved (an in-bounds path
        /// found).
        search_proven,
        /// Admission asks the bounded search disproved within its bounds.
        search_disproven,
        /// Admission asks the shared work budget cut — the run-time decline's
        /// verdict class (ADR-0516).
        declined_exhausted,
    }
}

delta_counters! {
    merge
    /// The expression-memo counters of one operator or statement (ADR-0654,
    /// ADR-0640): the per-memo facts summed over the memos the unit ran,
    /// read once when each memo finishes — a memo's increments are the
    /// ones already on its path, so collecting them adds none. Zero on
    /// every unit that probed no memo.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct MemoCounters: u64 {
        /// Inputs looked up in a memo table (warm-up inputs are not probes).
        probes,
        /// Probes a table answered.
        hits,
        /// Answers stored.
        inserts,
        /// Answers not stored because the meter or the ceiling refused.
        refused,
        /// The tables' allocation when read, bytes — never part of
        /// `memory_bytes` (ADR-0654 §2).
        bytes,
        /// How many memos gave up at their hit-rate review.
        disabled_count,
    }
}

impl MemoCounters {
    /// The group's six counts in `plan_keys::MEMO_GROUP_KEYS`' order
    /// (probes, hits, inserts, refused, bytes, disabled count) — the
    /// values the wire emitter pairs with that one key table.
    pub fn counters(&self) -> [u64; 6] {
        [
            self.probes,
            self.hits,
            self.inserts,
            self.refused,
            self.bytes,
            self.disabled_count,
        ]
    }

    /// The memo types behind an operator's sums (ADR-0654 §3): every
    /// operator-level memo is a guarded-state node-label memo (the
    /// constructor memos report at statement level through
    /// [`StatementMemoRecord`]), so a probed operator names that one kind.
    /// The rule lives here, beside the sums it describes — the plan builder
    /// asks, never decides which memo produced a count.
    pub fn operator_kinds(&self) -> relate_model::wire::MemoKindsWire {
        use relate_model::wire::{MemoKindWire, MemoKindsWire};
        if self.probes > 0 {
            MemoKindsWire::default().with(MemoKindWire::NodeLabel)
        } else {
            MemoKindsWire::default()
        }
    }
}

/// The statement-level memo facts (ADR-0654 §3): the constructor memos
/// live in the query overlay and serve any expression, so they report once
/// for the whole statement, attached at the plan root — the counters
/// summed over them and the memo types that were probed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatementMemoRecord {
    pub counters: MemoCounters,
    pub kinds: relate_model::wire::MemoKindsWire,
}

impl FrontierCounters {
    /// The group's ten counts in `plan_keys::FRONTIER_GROUP_KEYS`' order
    /// (edges, levels, endpoints, cells, memo hits, memo misses,
    /// representative proofs, search proven, search disproven, declined
    /// exhausted) — the values the wire emitter pairs with that one key
    /// table.
    pub fn counters(&self) -> [u64; 10] {
        [
            self.edge_visits,
            self.levels,
            self.endpoints,
            self.cells,
            self.memo_hits,
            self.memo_misses,
            self.representative_proofs,
            self.search_proven,
            self.search_disproven,
            self.declined_exhausted,
        ]
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

impl StoreReads {
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
    /// args and the renderers spell, read from its single source
    /// ([`plan_keys`](relate_model::wire::plan_keys)), as the rung names are.
    pub fn as_str(self) -> &'static str {
        match self {
            StartReason::BoundPivot => relate_model::wire::plan_keys::START_REASON_BOUND_PIVOT,
            StartReason::Cheapest => relate_model::wire::plan_keys::START_REASON_CHEAPEST,
            StartReason::SeekAnchored => relate_model::wire::plan_keys::START_REASON_SEEK_ANCHORED,
            StartReason::IndexZero => relate_model::wire::plan_keys::START_REASON_INDEX_ZERO,
        }
    }
}

/// Which rung served one `MATCH` clause (LABS-1117): the fact EXPLAIN
/// could not show, because the rungs are runtime choices inside an
/// operator, not plan nodes. The ladder records it per clause; the
/// PROFILE record carries it. [`Walker`](Self::Walker) is the per-row
/// walker, the rung every decline's home — the batch scan/filter chain
/// is its own fact (`BatchFacts::ran`), never a rung here.
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

/// The batch chain's PROFILE facts for one clause, carried as one value so
/// the record never holds a loose flag beside its counter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchFacts {
    /// Whether the clause ran ADR-0468's batch scan/filter/project chain
    /// over `Chunk` (LABS-359) — the PROFILE-visible fact that a
    /// scan-shaped `MATCH` interior executed as batch operators. `false`
    /// is every other clause, and every clause the chain declined.
    pub ran: bool,
    /// The batch filter's zone skips (LABS-645): how many conjunct-batch
    /// evaluations the chunk's derived min/max zone metadata provably
    /// replaced with the correct empty survivor set. `0` on every clause
    /// the chain declined and every clause that skipped nothing.
    pub zone_skips: u64,
}

/// The `CALL { … } IN TRANSACTIONS` batch facts one `CallSubquery`
/// operator reports to PROFILE (REL8-2047): the batch counters — how many
/// batches ran, how many committed, how many rolled back under the `ON
/// ERROR` policy — and the per-batch maxima: the input batch's retained
/// row bytes, the installed output's, the scratch the batch created
/// (entities, adjacency and property state — the redo log excluded, its
/// bytes are the WAL figure), and the redo log's reserved bytes at commit,
/// the WAL payload the batch wrote. The counters are arm-local (not global
/// counter snapshots): the batch loop notes each finished batch into the
/// shared cell the clause's record reads once its consumer drains the
/// stream. All-zero on every other clause, and on an unprofiled run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InTxBatches {
    /// Batches that started a transaction.
    pub batches: u64,
    /// Batches that committed (both arms of the commit: a facade sink's
    /// durable per-batch commit, and the statement-level boundary a
    /// nested or explicit-transaction run closes with).
    pub committed: u64,
    /// Batches that rolled back — a handled failure under `ON ERROR
    /// CONTINUE`/`BREAK`, or the failing batch of `ON ERROR FAIL`.
    pub rolled_back: u64,
    /// The largest input batch's retained row bytes.
    pub peak_input_rows_bytes: u64,
    /// The largest installed output's retained row bytes.
    pub peak_output_rows_bytes: u64,
    /// The most scratch bytes one batch created (redo excluded).
    pub peak_scratch_bytes: u64,
    /// The most redo-log bytes one batch carried at its commit — the WAL
    /// payload it wrote.
    pub peak_wal_bytes: u64,
}

/// The `CALL { … } IN TRANSACTIONS` arm's PROFILE facts (REL8-2047): the
/// batch counters and per-batch maxima the batch loop notes as each batch
/// finishes, in a cell shared between the arm's iterator (the writer) and
/// the stream's parked record (the reader), so the clause's record —
/// reported once the consumer drains or stops the stream, after every
/// batch has run — carries them. One is built only when a PROFILE record
/// is attached, so an unprofiled run never pays a cell write per batch.
pub(crate) struct InTxStats {
    facts: std::cell::Cell<InTxBatches>,
}

/// How one finished batch ended, as [`InTxStats::note_batch`] reads it —
/// the counted outcome with the per-batch figures beside it, so a
/// committed batch cannot note peaks without its WAL payload and a rolled
/// back one cannot note an installed output it never handed over.
pub(crate) enum InTxBatchEnd {
    /// The batch committed: its output was installed and its redo became
    /// the WAL payload — all four peaks are live.
    Committed {
        input_bytes: u64,
        output_bytes: u64,
        scratch_bytes: u64,
        wal_bytes: u64,
    },
    /// The batch rolled back: its own rows are gone, so there is no
    /// output or WAL figure — the input it held and the scratch it created
    /// before the rollback are the peaks it still reached.
    RolledBack {
        input_bytes: u64,
        scratch_bytes: u64,
    },
}

impl InTxStats {
    pub(crate) fn new() -> Self {
        Self {
            facts: std::cell::Cell::new(InTxBatches::default()),
        }
    }

    /// Note one finished batch: the outcome counter beside the batch count,
    /// each figure keeping the running maximum — the one write path the
    /// batch loop takes per batch.
    pub(crate) fn note_batch(&self, end: InTxBatchEnd) {
        let mut facts = self.facts.get();
        facts.batches += 1;
        match end {
            InTxBatchEnd::Committed {
                input_bytes,
                output_bytes,
                scratch_bytes,
                wal_bytes,
            } => {
                facts.committed += 1;
                facts.peak_input_rows_bytes = facts.peak_input_rows_bytes.max(input_bytes);
                facts.peak_output_rows_bytes = facts.peak_output_rows_bytes.max(output_bytes);
                facts.peak_scratch_bytes = facts.peak_scratch_bytes.max(scratch_bytes);
                facts.peak_wal_bytes = facts.peak_wal_bytes.max(wal_bytes);
            }
            InTxBatchEnd::RolledBack {
                input_bytes,
                scratch_bytes,
            } => {
                facts.rolled_back += 1;
                facts.peak_input_rows_bytes = facts.peak_input_rows_bytes.max(input_bytes);
                facts.peak_scratch_bytes = facts.peak_scratch_bytes.max(scratch_bytes);
            }
        }
        self.facts.set(facts);
    }

    /// The accumulated facts — the clause record's read, once the stream
    /// reports.
    pub(crate) fn facts(&self) -> InTxBatches {
        self.facts.get()
    }
}

/// A parked stream's PROFILE record (REL8-2047): the site and clause the
/// record keys, with the `CALL { … } IN TRANSACTIONS` arm's batch-facts
/// cell riding inside — one optional value, so a cell without a record,
/// or a record whose form has no batches, is unrepresentable. Every
/// non-batched form builds one with [`Self::at`]; the arm's constructor
/// attaches a cell through [`Self::with_intx`] — the one attaching site
/// the type's API states, not a field any caller can assign.
#[derive(Clone)]
pub(crate) struct ParkedRecord<'a> {
    /// The statement's PROFILE site — the sink and the `(stage, part)`
    /// position its keys ride.
    pub(crate) site: PlanMetricsSite<'a>,
    /// The clause index the record keys.
    pub(crate) clause: usize,
    /// The arm's batch-facts cell, noted by the batch loop as each batch
    /// finishes; absent on every other parked form. Private: presence is
    /// decided by the constructor alone.
    intx: Option<std::rc::Rc<InTxStats>>,
}

impl<'a> ParkedRecord<'a> {
    /// The plain record every non-batched form parks — no cell attached.
    pub(crate) fn at(site: PlanMetricsSite<'a>, clause: usize) -> Self {
        Self {
            site,
            clause,
            intx: None,
        }
    }

    /// Attach the `IN TRANSACTIONS` arm's batch-facts cell — the one
    /// attaching site (the arm's constructor, which keeps the writer
    /// handle beside the record it hands the reader); no other form can,
    /// so the cell's presence states "this record's clause runs batches".
    pub(crate) fn with_intx(self, stats: std::rc::Rc<InTxStats>) -> Self {
        Self {
            intx: Some(stats),
            ..self
        }
    }

    /// The accumulated batch facts for the clause record's stamp — zeroed
    /// on every record with no cell attached.
    pub(crate) fn intx_facts(&self) -> InTxBatches {
        self.intx
            .as_deref()
            .map(InTxStats::facts)
            .unwrap_or_default()
    }
}

impl InTxBatches {
    /// Whether the arm ran any batch — the group's one presence rule: a
    /// clause that ran none emits no `InTx*` key at all, so its absence is
    /// the whole "no batches" fact (never a fabricated zero group).
    pub fn ran(&self) -> bool {
        self.batches > 0
    }

    /// The group's seven counts in `plan_keys::IN_TX_GROUP_KEYS`' order
    /// (batches, committed, rolled back, peak input bytes, peak output
    /// bytes, peak scratch bytes, peak WAL bytes) — the values the wire
    /// emitter pairs with that one key table.
    pub fn counters(&self) -> [u64; 7] {
        [
            self.batches,
            self.committed,
            self.rolled_back,
            self.peak_input_rows_bytes,
            self.peak_output_rows_bytes,
            self.peak_scratch_bytes,
            self.peak_wal_bytes,
        ]
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
    /// [`ClauseMetricsRecord::reads`]. Zero for a clause that builds no
    /// containers or strings.
    pub expression_built_bytes: u64,
    /// The operator's elapsed time, nanoseconds.
    pub time_ns: u128,
    /// The store-read mix (LABS-369) behind the operator's `Db Hits`
    /// total: hops vs neighbor walks vs node reads.
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
    /// What ADR-0468's batch scan/filter/project chain did for the
    /// clause: whether it ran and what its filter skipped.
    pub batch: BatchFacts,
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
    /// The expression-memo counters of the guarded-state memos the clause's
    /// stage ran (ADR-0654 §3), summed, with `disabled_count` in place of a
    /// per-memo flag. Zero on every clause that probed no memo; stamped
    /// only while a PROFILE statement's arm runs.
    pub memo: MemoCounters,
    /// The `CALL { … } IN TRANSACTIONS` batch facts (REL8-2047) — the one
    /// clause kind that runs batches. Zeroed by the shared record
    /// constructor; the arm's shared cell stamps its accumulated facts
    /// onto the record at the one site that reports for the arm.
    pub intx: InTxBatches,
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

    /// The statement's constructor-memo facts, reported once when the
    /// statement finished (ADR-0654 §3): they belong to no one operator,
    /// so the consumer attaches them at the plan root.
    fn record_statement_memos(&self, record: StatementMemoRecord);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REL8-2047: the batch-facts cell accumulates its counters and keeps
    /// the running maxima — a smaller later batch never lowers a peak, and
    /// a rolled-back batch counts without an output or WAL figure.
    #[test]
    fn intx_stats_accumulate_counters_and_keep_maxima() {
        let stats = InTxStats::new();
        stats.note_batch(InTxBatchEnd::Committed {
            input_bytes: 100,
            output_bytes: 60,
            scratch_bytes: 500,
            wal_bytes: 200,
        });
        stats.note_batch(InTxBatchEnd::RolledBack {
            input_bytes: 80,
            scratch_bytes: 300,
        });
        let facts = stats.facts();
        assert_eq!(facts.batches, 2);
        assert_eq!(facts.committed, 1);
        assert_eq!(facts.rolled_back, 1);
        assert_eq!(facts.peak_input_rows_bytes, 100, "the larger input holds");
        assert_eq!(facts.peak_output_rows_bytes, 60);
        assert_eq!(facts.peak_scratch_bytes, 500, "the larger scratch holds");
        assert_eq!(facts.peak_wal_bytes, 200);
    }
}
