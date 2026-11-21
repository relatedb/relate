//! The executor's instrumentation cluster (LABS-448's extraction): the
//! relationship-traversal counters, the walk-start slot, the zone-skip and
//! frontier counters — every PROFILE counter behind the plan table's kinds
//! and the walk's chosen start, re-exported through `exec` so the walkers'
//! `super::` paths keep working. The phase clocks and the node-read
//! counter live one level down, in `crate::ctx` (LABS-668): the eval
//! walkers stamp through them and the shared candidate funnel bumps the
//! read count too, so their home is the crate's lowest shared module,
//! never this exec-only one.

use std::cell::Cell;

use web_time::Instant;

thread_local! {
    pub(crate) static HOP_EXPANDS: Cell<u64> = const { Cell::new(0) };
    pub(crate) static NEIGHBOR_WALKS: Cell<u64> = const { Cell::new(0) };
    /// Whether a PROFILE statement's clause arm is running (LABS-805) —
    /// the counters' batched flushes read this once per walk run before
    /// touching their thread-local, so a non-PROFILEd statement pays one
    /// flag read per run and nothing per edge (the per-edge TLS bump the
    /// issue removes: a thread-local access per hop, expensive where TLS
    /// goes through the platform's lazy-init stub). Armed by the
    /// clause's metric tick, disarmed by its drop — the phase stamps'
    /// arming idiom (LABS-448). Test builds default to armed: the
    /// counters' own unit tests run plain statements and assert their
    /// totals.
    static STORE_PROFILING: Cell<bool> = const { Cell::new(cfg!(test)) };
}

/// Arm the store-read counters for a PROFILE clause arm (LABS-805) —
/// the metric tick's act beside [`crate::ctx::arm_phase_profiling`],
/// returning the flag's prior value for the tick's drop to restore, so
/// the counters never move outside a profiled statement (or a test) and
/// a pre-armed thread (test builds default armed) keeps its state.
pub(crate) fn arm_store_profiling() -> bool {
    STORE_PROFILING.with(|c| c.replace(true))
}

/// Restore the arm flag to `prior` (LABS-805) — the metric tick's drop's
/// act beside [`crate::ctx::disarm_phase_profiling`]: the value
/// [`arm_store_profiling`] returned at the arm, never an unconditional
/// disarm (a pre-armed thread — the test-build default — stays armed).
pub(crate) fn restore_store_profiling(prior: bool) {
    STORE_PROFILING.with(|c| c.set(prior));
}

/// Disarm the store-read counters (LABS-805) — the tests' explicit
/// disarm; the metric tick restores instead ([`restore_store_profiling`]).
#[cfg(test)]
pub(crate) fn disarm_store_profiling() {
    STORE_PROFILING.with(|c| c.set(false));
}

/// Count `count` relationship hop expansions, batched (LABS-805: the
/// per-edge bump became a per-run flush — a walk tallies its hops in a
/// [`WalkTallies`] and lands the run's whole total as one gated
/// thread-local write; the frontier family's post-run accounting,
/// ADR-0478, generalized to every walk engine). A non-PROFILEd run's
/// flush is the one flag read and nothing else.
pub(crate) fn count_hop_expands(count: u64) {
    if STORE_PROFILING.with(Cell::get) {
        HOP_EXPANDS.with(|c| c.set(c.get() + count));
    }
}

/// Count `count` relationships the walk traversed, batched — see
/// [`count_hop_expands`] for the per-run flush idiom (LABS-805) and its
/// PROFILE gate. The counting event is per engine, stated at each tally
/// site: the walkers (classic, pivot, GPM) and the kernels tally a
/// relationship after its admission accepts and the hop resolves — the
/// relationship actually walked, never a candidate filtered out — while
/// the aggregate folds tally the enumerated (origin, rel) pair before
/// the rel's property/admission filters, the fold's own contract since
/// LABS-375 (a totals change there is its own decision, not this
/// refactor's).
pub(crate) fn count_neighbor_walks(count: u64) {
    if STORE_PROFILING.with(Cell::get) {
        NEIGHBOR_WALKS.with(|c| c.set(c.get() + count));
    }
}

/// The per-edge walk counts one run accumulates (LABS-805): the walk
/// engines tally their hop expansions and neighbor walks in this pair —
/// carried by the walk's shared context (`PathMatch`, `GpmCtx`, the
/// folds' envs) or held as a kernel's local — and flush to the
/// thread-local counters once per run, so a hop never pays a
/// thread-local access. `Cell` fields, so the shared (immutable) walk
/// contexts carry it by reference the way `GpmCtx`'s memo cells ride.
#[derive(Debug, Default)]
pub(crate) struct WalkTallies {
    hops: Cell<u64>,
    walks: Cell<u64>,
}

impl WalkTallies {
    /// Tally one relationship hop expansion — the per-edge unit
    /// [`count_hop_expands`] flushes, accumulated locally. The
    /// quantified expansion's own spelling (gpm/qpp.rs): one per
    /// accepted iteration, its repetition unit.
    pub(crate) fn hop(&self) {
        self.hops.set(self.hops.get() + 1);
    }

    /// Tally one hop expansion of a fixed-length walk — the
    /// past-the-first-rel policy, owned here so every engine (the
    /// classic walker's `admit_candidate`, the chain and hub-star
    /// kernels) spells it the same way: the first hop is the walk's
    /// start, every hop past it is an expansion. Called unconditionally
    /// with the hop's zero-based index.
    pub(crate) fn expansion_hop(&self, idx: usize) {
        if idx > 0 {
            self.hop();
        }
    }

    /// Tally one relationship the walk traversed — the per-edge unit
    /// [`count_neighbor_walks`] flushes, accumulated locally.
    pub(crate) fn walk(&self) {
        self.walks.set(self.walks.get() + 1);
    }

    /// Flush the run's totals into the thread-local counters, draining
    /// the cells: any number of flushes partition the run's total
    /// instead of multiplying it, so the flush's timing is the owner's
    /// drop, never a protocol a caller can get wrong. The batched
    /// counters self-gate on the PROFILE arm, so a non-PROFILEd
    /// statement's flush costs two flag reads and moves nothing.
    fn flush(&self) {
        count_hop_expands(self.hops.replace(0));
        count_neighbor_walks(self.walks.replace(0));
    }
}

impl Drop for WalkTallies {
    /// The flush's one trigger (LABS-805): the tallies' owner drops at
    /// the walk driver's end — inside the clause arm, so a PROFILEd
    /// statement's totals land while armed — and a forgotten flush is
    /// unrepresentable, the same move `ProfileTick` made for disarming.
    fn drop(&mut self) {
        self.flush();
    }
}

/// The store-read mix right now (LABS-369): the three counters the one
/// `Db Hits` total blends, read at clause boundaries so a per-clause delta
/// costs three thread-local reads, never a per-row one.
pub(crate) fn store_reads() -> crate::metrics::StoreReads {
    crate::metrics::StoreReads {
        hops: HOP_EXPANDS.with(Cell::get),
        neighbor_walks: NEIGHBOR_WALKS.with(Cell::get),
        node_reads: crate::ctx::node_reads(),
    }
}

thread_local! {
    /// The batch filter's zone skips (LABS-645): one per conjunct-batch
    /// evaluation the chunk's derived zone metadata provably replaced
    /// with the correct empty survivor set — the skip IS the result, the
    /// chunk's row walk for that conjunct never ran. Not a store read:
    /// counted wherever the batch chain runs, PROFILEd or not, and read
    /// per clause like the store-read mix.
    static ZONE_SKIPS: Cell<u64> = const { Cell::new(0) };
}

/// Count one zone skip (LABS-645) — the one counting point, the
/// store-access counters' idiom, never an inline spelling beside it.
pub(crate) fn count_zone_skip() {
    ZONE_SKIPS.with(|c| c.set(c.get() + 1));
}

/// The zone-skip total right now (LABS-645), snapshotted at the clause
/// tick the way [`store_reads`] is: the delta across the arm is the
/// clause's skips, one thread-local read per clause.
pub(crate) fn zone_skips() -> u64 {
    ZONE_SKIPS.with(Cell::get)
}

thread_local! {
    /// The frontier family's run counters (ADR-0478), accumulated
    /// wherever a frontier run completes — the rider wiring
    /// (`exec/kernels/frontier_run.rs`), PROFILEd or not — and read per
    /// clause like the store-read mix: the delta across the arm is the
    /// clause the frontier served.
    static FRONTIER: Cell<crate::metrics::FrontierCounters> = const {
        Cell::new(crate::metrics::FrontierCounters {
            edge_visits: 0,
            levels: 0,
            endpoints: 0,
            cells: 0,
            rearrival_work: 0,
            memo_hits: 0,
            memo_misses: 0,
            representative_proofs: 0,
            search_proven: 0,
            search_disproven: 0,
            declined_exhausted: 0,
        })
    };
}

/// Add one completed frontier run's counters (ADR-0478) — the one
/// counting point, the zone-skip idiom's: the rider wiring calls it once
/// per run, never an inline field-by-field spelling beside it.
pub(crate) fn count_frontier(run: crate::metrics::FrontierCounters) {
    FRONTIER.with(|c| {
        let mut counters = c.get();
        counters += run;
        c.set(counters);
    });
}

/// The frontier counters right now, snapshotted at the clause tick the
/// way [`zone_skips`] is: the delta across the arm is the clause the
/// frontier served.
pub(crate) fn frontier_counters() -> crate::metrics::FrontierCounters {
    FRONTIER.with(Cell::get)
}

thread_local! {
    /// The start the current clause's walk chose (LABS-369), recorded once
    /// per walk by the selection site and taken at the clause's metric
    /// tick. `None` until a walk records one.
    static WALK_START: Cell<Option<crate::metrics::WalkStart>> = const { Cell::new(None) };
}

/// Record how the walk starting now chose its element (LABS-369). The
/// clause's FIRST walk wins: a clause whose start varies per outer row
/// (the bound pivot depends on the row's binding) reports its
/// representative first start rather than the last row's.
pub(crate) fn record_walk_start(start: crate::metrics::WalkStart) {
    WALK_START.with(|slot| {
        if slot.get().is_none() {
            slot.set(Some(start));
        }
    });
}

/// Take the recorded walk start, clearing it: the metric tick reads it
/// once per clause, so a stale start never leaks into the next clause.
pub(crate) fn take_walk_start() -> Option<crate::metrics::WalkStart> {
    WALK_START.with(|slot| slot.take())
}

/// Put a walk start back unconditionally — the swap guard's restore half.
fn put_walk_start(start: Option<crate::metrics::WalkStart>) {
    WALK_START.with(|slot| slot.set(start));
}

/// Take the producer's walk start out for the duration of one streamed
/// stage's transform (`profile_chain.rs`): the first-wins slot is one per
/// clause, so a stage's own first walk records into the empty slot, and
/// the guard hands the producer's start back — on success and on every
/// error path alike (`Drop`, the `WalkTallies` idiom). First-wins
/// semantics stay per clause inside one streamed arm.
pub(crate) struct WalkStartSwap(Option<crate::metrics::WalkStart>);

impl WalkStartSwap {
    pub(crate) fn take() -> Self {
        Self(take_walk_start())
    }
}

impl Drop for WalkStartSwap {
    fn drop(&mut self) {
        put_walk_start(self.0.take());
    }
}

/// The node-read counter's test snapshot: it homes in `crate::ctx` beside
/// the counter itself (the shared funnel bumps it), re-exported here so
/// every store-read pin keeps its `super::` path.
#[cfg(test)]
pub(crate) use crate::ctx::take_node_reads;

#[cfg(test)]
pub(crate) fn take_hop_expands() -> u64 {
    HOP_EXPANDS.with(|c| c.replace(0))
}

#[cfg(test)]
pub(crate) fn take_neighbor_walks() -> u64 {
    NEIGHBOR_WALKS.with(|c| c.replace(0))
}

#[cfg(test)]
pub(crate) fn take_zone_skips() -> u64 {
    ZONE_SKIPS.with(|c| c.replace(0))
}

#[cfg(test)]
pub(crate) fn take_frontier_counters() -> crate::metrics::FrontierCounters {
    FRONTIER.with(|c| c.replace(crate::metrics::FrontierCounters::default()))
}

// The PROFILE metric family's two typed meters beside the counter kinds:
// the raw `u64` counters whose delta is not the checked subtraction the
// `delta_counters!` family states. Each carries its own `since` — the
// zone-skip counter's checked like its counter siblings, the expression
// meter's saturating because the live figure falls when an arm releases
// memory — and the merge the stage windows accumulate.

/// The zone-skip counter's typed figure (LABS-645): checked delta, the
/// monotone counters' shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ZoneSkips(u64);

impl ZoneSkips {
    pub(crate) fn now() -> Self {
        Self(zone_skips())
    }

    pub(crate) fn since(self, start: Self) -> Self {
        Self(
            self.0
                .checked_sub(start.0)
                .expect("the counters only ever increase"),
        )
    }

    /// The counter's figure, for the record.
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

impl std::ops::AddAssign for ZoneSkips {
    fn add_assign(&mut self, window: Self) {
        self.0 = self
            .0
            .checked_add(window.0)
            .expect("the counters only ever accumulate");
    }
}

/// The expression-built meter's typed figure (ADR-0463): a live
/// allocation total, not a counter — it falls when an arm releases
/// expression memory, so the delta saturates and yields a bare byte
/// count, never a counter type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExpressionBytes(u64);

impl ExpressionBytes {
    pub(crate) fn now(graph: &crate::overlay::ExecGraph<'_>) -> Self {
        Self(graph.expression_built_bytes())
    }

    pub(crate) fn since(self, start: Self) -> Self {
        Self(self.0.saturating_sub(start.0))
    }

    /// The meter's byte figure, for the record.
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

impl std::ops::AddAssign for ExpressionBytes {
    fn add_assign(&mut self, window: Self) {
        self.0 = self.0.saturating_add(window.0);
    }
}

/// The PROFILE metric family, one field list in two forms: the snapshot
/// (each counter's base plus the clock) a clause arm or a stage window
/// takes, and the delta (the elapsed time plus each counter's delta) a
/// record reports. Every field's type states its own delta (`since`) and
/// its own accumulation (`+=`), so the list is the only thing the two
/// forms share — a new metric lands in it once and both forms carry it.
/// `time_ns` is the delta's own derived field: the clock the snapshot
/// holds becomes the elapsed figure the delta reports.
macro_rules! profile_family {
    (
        $(#[$doc:meta])*
        $snapshot:ident / $delta:ident { $($field:ident : $ty:ty,)+ }
    ) => {
        $(#[$doc])*
        #[derive(Clone, Copy)]
        pub(crate) struct $snapshot {
            t0: Instant,
            $($field: $ty,)+
        }

        impl $snapshot {
            /// The delta since `start`: the wall time elapsed from the
            /// start snapshot's clock plus each field's own subtraction.
            pub(crate) fn since(self, start: Self) -> $delta {
                $delta {
                    time_ns: start.t0.elapsed().as_nanos(),
                    $($field: self.$field.since(start.$field),)+
                }
            }
        }

        /// The delta form: what one window or one clause arm moved its
        /// metrics — the record's own figures, with the elapsed time the
        /// snapshot's clock becomes.
        #[derive(Clone, Copy, Default)]
        pub(crate) struct $delta {
            pub(super) time_ns: u128,
            $(pub(super) $field: $ty,)+
        }

        impl $delta {
            /// This unit's own facts: its totals minus the **immediate**
            /// downstream unit's totals. The windows nest — each unit's
            /// totals already contain everything the units after it did —
            /// so the immediate downstream total is the whole subtrahend;
            /// summing the tail would count every deeper unit twice.
            pub(crate) fn minus(self, downstream: Self) -> Self {
                Self {
                    time_ns: self.time_ns.saturating_sub(downstream.time_ns),
                    $($field: self.$field.since(downstream.$field),)+
                }
            }

            /// Add one window's delta into the totals — the accumulation
            /// half of the same field list.
            pub(crate) fn accumulate(&mut self, window: Self) {
                self.time_ns += window.time_ns;
                $(self.$field += window.$field;)+
            }
        }
    };
}

profile_family! {
    /// The six-metric PROFILE snapshot — LABS-369's read kinds,
    /// LABS-448's phase times, LABS-645's zone skips, ADR-0478's frontier
    /// counters, ADR-0463's expression meter, and the clock — the one
    /// snapshot family the clause tick (`profile_tick.rs`) and the
    /// streamed stage windows (`profile_chain.rs`) both take.
    DeltaSnapshot / StageDelta {
        reads: crate::metrics::StoreReads,
        phases: crate::metrics::PhaseTimes,
        zone_skips: ZoneSkips,
        frontier: crate::metrics::FrontierCounters,
        expression_built: ExpressionBytes,
    }
}

impl DeltaSnapshot {
    /// Take the family's snapshot: the clock plus each counter's base.
    pub(crate) fn now(graph: &crate::overlay::ExecGraph<'_>) -> Self {
        Self {
            t0: Instant::now(),
            reads: store_reads(),
            phases: crate::ctx::phase_times(),
            zone_skips: ZoneSkips::now(),
            frontier: frontier_counters(),
            expression_built: ExpressionBytes::now(graph),
        }
    }
}
