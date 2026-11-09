//! The frontier core (ADR-0478 Decision 1): the level-synchronous BFS
//! over deduplicated (node, automaton-state) pairs that a compiled
//! [`FrontierNfa`] drives — the production form of the research
//! prototype's `reach` (`graph/qpp.rs`, ADR-0476), running over the
//! executor's `ExecGraph` overlay view instead of the base `Graph` the
//! prototype reads.
//!
//! Semantics (the prototype's, per the design note §1):
//!
//! - **One expansion per (node, state) pair ever** — an ever-visited
//!   `u64` bitmask per node slot (the 64-state ceiling is the compiler's
//!   decline), so work is O(incidences × transitions) per level, never
//!   per path: the memory and time collapse ADR-0478 exists for.
//! - **Accept on arrival, before the dedup filter** (Decision 1): an
//!   accepting transition reaching `v` at a hop count in `[min, max]`
//!   marks the endpoint even when `v` was first visited at an earlier
//!   level — the measured seed-re-reached-at-hop-2 class. Dedup gates
//!   *expansion* only.
//! - **Unbounded runs terminate by pair-saturation fixpoint** (`max` =
//!   `None`): the level loop ends when a level adds no pair, not at a
//!   depth cap.
//! - **Epsilon edges** (the optional-copy skips of unrolled bounded
//!   quantifiers, which the prototype's two families never produce) are
//!   expanded at the pair's own level: they consume neither a hop nor a
//!   relationship, and the acceptance table's epsilon closure is already
//!   hoisted onto the transitions' `accepts` flags by the compiler.
//! - **The zero-length endpoint**: `min_hops == 0` with an accepting
//!   start state marks the seed itself (hop 0, no predecessor).
//!
//! Fidelity: every hop runs through [`ExecGraph::try_for_each_neighbor_resolved`]
//! and [`ExecGraph::hop`] — the one overlay-visible neighbor iteration
//! (write scratch, pinned snapshot `ts`, rel liveness) and the one
//! endpoint resolution, the same checks the chain kernel and the
//! per-row walker's `hop` apply. Direction rides per transition
//! (`Outgoing`/`Incoming`/`Both`): a `Both` transition visits each
//!   incident rel exactly once per endpoint (both adjacency sides,
//!   rel-deduplicated — a self-loop once, not twice). The cancellation
//!   window is sampled once per level (ADR-0111's statement-wide counter,
//!   so recursion depth cannot reset it), and the edge-visit work budget
//!   (Decision 6, the ADR-0395 fixed-constant family) is checked per
//!   level: a frontier that would blow up fails with the authored
//!   `ErrorKind::WalkBudgetExceeded` error in milliseconds instead of
//!   running to fixpoint.
//!
//! **The allocation bound (Decision 6's allocation half, sparse)**: the
//! tables — the run's per-touched-node records and first-arrival pair
//! ledger plus the re-arrival rule's memo and chain-proof visit set —
//! are sized by the (node, state) pairs the run touches, never by the
//! store's node-id span ([`FrontierTables`](super::frontier_tables) in
//! the table module, [`RuleBuffers`](super::rearival) in the rule's).
//! One set per engaged clause, recycled across its seeds, reset per
//! seed in O(1) (generation-stamped maps,
//! [`NodeMap`](super::node_map)). The exact bytes the set holds are
//! checked against
//! [`FRONTIER_TABLE_BUDGET`](super::frontier_tables) at every level
//! boundary, beside the work budget; a run that crosses it declines at
//! run time (`FrontierRunDecline::TableBudget`, the DFS fall-through, which
//! has its own caps). The two bounds compose: the work budget bounds
//! each run's edge visits, this ceiling the tables those visits grow —
//! a run that passes both is bounded in both, and neither is measured
//! in the other's units.
//!
//! What this core deliberately does not do (the feature's later gbuild
//! nodes): matcher and cost-gate engagement, the rider wiring and its
//! one nested-evaluation tick per evaluation. Witness reconstruction
//! and capped emission are the sibling module
//! [`super::frontier_emit`] — the reader of these tables. The
//! re-arrival
//! rule (ADR-0478 §3) is wired in through its own
//! rule's module ([`super::rearival`]) as a seam — the runner asks "is this
//! accepting arrival trail-admissible?" (the default pool) and "is
//! this below-min candidate an in-bounds endpoint?" (either pool) and
//! the rule answers, the runner's structure intact and the
//! witness-emit node building on the same tables.
//! The tables (in [`super::frontier_tables`]) are shaped for those
//! consumers: the parent table
//! records predecessor node, predecessor state, and the traversed rel
//! (or the epsilon skip) per
//! first arrival; the endpoint list carries each accepted endpoint's
//! own accepting arrival (state, hop count, predecessor pair, traversed
//! rel), which is
//! information the parent table alone cannot recover — its slot may
//! hold an *earlier* first arrival of the same pair. The capped
//! consumer's early exit is the `halt` probe of [`run_with_halt`]:
//! asked at every level boundary, never inside an expansion.

use relate_core::{Error, NodeId, Result, Value};

use crate::ctx::{ExecCtx, FrontierCounters, FrontierRunDecline};
use crate::eval::{eval, predicate_value_holds};
use crate::exec::candidates::node_ok;
use crate::overlay::ExecGraph;
use crate::row::Row;
use crate::store::RelTypes;

use super::frontier_tables::{
    within_table_budget, Arrival, Endpoint, FrontierTables, Parent, Parents, Step, TableCeiling,
};
use super::nfa::{FrontierNfa, Pool, STATE_CEILING};
use super::rearival::{RearivalRule, RuleBuffers, Verdict};

/// The frontier run's edge-visit work budget (ADR-0478 Decision 6, the
/// ADR-0395 fixed-constant family): a fixed constant, no ambient knob —
/// the remedy for legitimate work that needs more is a new ADR. Sized
/// against the prototype's measurements: the worst SF0.1 class measured
/// 247,026 edge visits (`(knows|likes)+`, degree-sum bound), SF1
/// projects to ~2.5M at the same bound, so 10M leaves ~4x headroom over
/// the largest legitimate single-seed run while a pathological shape
/// (huge diameter × states) still fails in well under a second.
pub(crate) const FRONTIER_WALK_BUDGET: u64 = 10_000_000;

/// The budget a frontier run is held to: [`WalkBudget::standard`] is the
/// only production constructor — there is no ambient knob to turn — and
/// the sole alternative, [`WalkBudget::for_tests`], is the test seam
/// that lets a budget-exceeded test fail a small graph fast instead of
/// building a shape that crosses [`FRONTIER_WALK_BUDGET`].
pub(crate) struct WalkBudget(u64);

impl WalkBudget {
    /// The production budget: [`FRONTIER_WALK_BUDGET`] edge visits per
    /// run.
    pub(crate) fn standard() -> Self {
        Self(FRONTIER_WALK_BUDGET)
    }

    /// The test seam: an explicit edge-visit ceiling.
    #[cfg(test)]
    pub(crate) fn for_tests(edge_visits: u64) -> Self {
        Self(edge_visits)
    }
}

/// The authored budget error (Decision 6): the `Kind:` prefix names the
/// `ErrorKind::WalkBudgetExceeded` row the error channel classifies, in
/// the ADR-0395 `NestedEvaluationBudgetExceeded` message's shape.
fn walk_budget_exceeded() -> Error {
    Error::execution(format!(
        "WalkBudgetExceeded: frontier walk exceeds {FRONTIER_WALK_BUDGET} edge \
         visits per run; a frontier that would blow up fails here instead of \
         running to fixpoint (work budget)."
    ))
}

/// One frontier run's tables and counters — the core's whole output and
/// the witness-emit / rider-integration nodes' input. The run carries
/// its tables (the clause's recycled set, handed back through
/// [`Self::reclaim`] when the consumers are done — the
/// allocation bound's recycling half) and its resolved [`Arm`] table so
/// the emission's deferred asks reuse the same name-resolved expansion
/// instead of rebuilding it per capped run (the chosen remedy for the
/// arms+rule rebuild: one resolution per run; the rule itself is the
/// emission's own, its memo recycled per capped run).
#[derive(Debug)]
pub(crate) struct FrontierRun {
    /// The run's tables — first-arrival parents (the [`Self::parents`]
    /// view), the pair ledger, the per-node marks — the clause's
    /// recycled set carried for the hand-back ([`Self::reclaim`]).
    tables: FrontierTables,
    /// The accepted endpoints, in arrival order, one per node.
    pub(crate) endpoints: Vec<Endpoint>,
    /// First arrivals per BFS level; level 0 is the seed's pair set (the
    /// start state plus its epsilon closure).
    pub(crate) per_level: Vec<usize>,
    /// Incident-edge visits — the work counter the budget caps. Each
    /// live rel of a transition's types is visited exactly once per
    /// endpoint node per direction the transition asks for (a `Both`
    /// transition: once per endpoint, both adjacency sides,
    /// rel-deduplicated).
    pub(crate) edge_visits: u64,
    /// The run's resolved expansion table — the same one the BFS loop
    /// and the run's re-arrival rule walked, handed to the emission.
    pub(crate) arms: Vec<Vec<Arm>>,
    /// The run's re-arrival rule facts — the memo's hits and misses and
    /// the admission asks by verdict, a [`FrontierCounters`] carrying
    /// only the rule's own fields (the runner's instance tally, read
    /// once when the run was packaged; the emission's deferred-ask rule
    /// is its own instance, and its tally merges into this field when
    /// its pass finishes — `add_assign` over zeroed walk fields).
    pub(crate) rule: FrontierCounters,
}

impl FrontierRun {
    /// The first-arrival parent table view — the reconstruction's read
    /// over the run's carried tables.
    pub(crate) fn parents(&self) -> Parents<'_> {
        self.tables.parents()
    }

    /// The run's PROFILE facts — the [`FrontierCounters`] the record
    /// pass counts, the one derivation (levels = the per-level vector's
    /// length, endpoints = the endpoint list's, cells = the pairs the
    /// levels created, the rule's facts carried whole), never re-split
    /// at a use site.
    pub(crate) fn facts(&self) -> FrontierCounters {
        FrontierCounters {
            edge_visits: self.edge_visits,
            levels: self.per_level.len() as u64,
            endpoints: self.endpoints.len() as u64,
            cells: self.per_level.iter().sum::<usize>() as u64,
            ..self.rule
        }
    }

    /// Take the finished run's tables back — the recycling hand-off
    /// (the allocation bound's recycling half): the run's consumers are
    /// done, and the stale entries are the next
    /// [`FrontierTables::begin_seed`]'s to clear, through the ledger
    /// the run carried. The consuming side lives here, beside the run
    /// type it owns — the table module never reaches up for the run.
    pub(super) fn reclaim(self) -> FrontierTables {
        self.tables
    }
}

/// One frontier run's outcome (ADR-0516): the re-arrival budget's
/// exhaustion fact is typed into the run's result, never a flag a
/// consumer could patch onto a run — and never a run an unproven
/// answer could be emitted from.
///
/// [`RunOutcome::Served`] carries the run whole: its tables for the
/// caller's recycle, its endpoints and arms for the consumers. A
/// declined run carries no run at all — only the counters (the record
/// pass keeps counting the walk) and the typed reason; the run's tables
/// drop with it, the clause's pool aborting exactly as an errored run's
/// does, and no consumer arm can reach an endpoint set the rule could
/// not prove.
#[derive(Debug)]
// The run rides unboxed, exactly as the emission's own paired answer
// (`CappedRun`) rides it: the declined arm's counters trip the lint's
// threshold, not a real size problem — every consumer matches the two
// arms once per run.
#[allow(clippy::large_enum_variant)]
pub(crate) enum RunOutcome {
    /// Every endpoint the run answered is proven (the chain proof or a
    /// search that ran to its verdict): the consumers' input.
    Served(FrontierRun),
    /// A search of the run's re-arrival rule cut on its shared work
    /// budget — the endpoint set is unproven — or the run's tables
    /// crossed the allocation ceiling. The rider wiring declines the
    /// clause to the next rung unchanged — the DFS answers these shapes
    /// exactly, and no partial answer ever escapes.
    Declined {
        /// The declined run's counters — the record pass's input.
        facts: FrontierCounters,
        /// The decline's typed reason: one of the two run-time
        /// declines — ADR-0516's `FrontierRunDecline::RearrivalBudget`,
        /// or the allocation ceiling's `FrontierRunDecline::TableBudget`
        /// (ADR-0521). Both are run-time: the rung ran, so its counters
        /// ride beside the reason.
        reason: FrontierRunDecline,
    },
}

impl RunOutcome {
    /// The outcome's PROFILE facts, either arm — the record pass's one
    /// input (a declined run still walked; its counters still count).
    pub(crate) fn facts(&self) -> FrontierCounters {
        match self {
            RunOutcome::Served(run) => run.facts(),
            RunOutcome::Declined { facts, .. } => *facts,
        }
    }

    /// The direct-run suites' served-run reader: a run pinned inside
    /// the budget (every suite that compares endpoint sets), where a
    /// declined outcome is a failure to name, not an endpoint set to
    /// compare — the decline path's own pin lives in the rider suite
    /// (`frontier_rider_tests`), never here.
    #[cfg(test)]
    pub(crate) fn unwrap_served(self) -> FrontierRun {
        match self {
            RunOutcome::Served(run) => run,
            RunOutcome::Declined { facts, reason } => {
                panic!("the run declined on {reason:?}: {facts:?}")
            }
        }
    }
}

/// One transition's expansion inputs, resolved once per run: the
/// transition's index into the NFA's table and its type filter in the
/// interned domain (LABS-801). The compiler left ids; the names beside
/// them are the write overlay's arm (`RelTypes::matches_name`) and come
/// from the interner's own `Arc<str>` units — a clone is a refcount, never
/// a copy, and the per-pair walk compares ids. One canonical resolver: the
/// re-arrival rule's search ([`super::rearival`]) walks the same table,
/// never a second copy.
#[derive(Debug, Clone)]
pub(crate) struct Arm {
    pub(crate) transition: usize,
    pub(crate) types: RelTypes,
}

/// Run `nfa` from `seed` over the overlay view to fixpoint or `max`:
/// the every-arrival shape — the DistinctEndpoints and Exists
/// consumers' runner. See [`run_with_halt`] for the BFS's own contract
/// and [`RunOutcome`] for the run's answer shape.
pub(crate) fn run(
    ctx: ExecCtx<'_>,
    seed: NodeId,
    nfa: &FrontierNfa,
    budget: WalkBudget,
    pool: Pool,
    tables: &mut FrontierTables,
    bufs: &mut RuleBuffers,
) -> Result<RunOutcome> {
    run_with_halt(
        ctx,
        seed,
        nfa,
        budget,
        pool,
        tables,
        bufs,
        None::<fn(&Parents<'_>, &[Endpoint]) -> bool>,
    )
}

/// Run `nfa` from `seed` over the overlay view: the level-synchronous
/// BFS of ADR-0478 Decision 1. The seed is the caller's admitted, live
/// node (the engagement proofs' scan segment), and the caller owns the
/// answer-shape gate — this core answers pair-level facts only. `pool`
/// selects the semantics the endpoints answer in ([`Pool::Walk`] — the
/// frontier's native walks, its below-min candidates answered by the
/// rule's walk-length check — or [`Pool::Default`] — the default pool's
/// trails, the re-arrival rule's jurisdiction, ADR-0478 §3): the
/// caller reads the contract's own mapping (`FrontierContract::pool`).
///
/// `tables` and `bufs` are the clause's recycled table set (the
/// allocation bound above): this run takes the tables whole and returns
/// them inside a served outcome's run — the caller recycles them into
/// the next seed through [`FrontierRun::reclaim`] once this run's
/// consumers are done. A declined outcome (the run's re-arrival rule
/// cut a search on its shared work budget, ADR-0516, or the run's
/// tables crossed [`FRONTIER_TABLE_BUDGET`](super::frontier_tables) at a
/// level boundary) drops the tables with the run, exactly an errored run
/// does. The tables must be shaped for this run's automaton (the
/// rider's engagement did; a mismatch is a wiring bug, asserted).
///
/// `halt` is the capped consumer's early-exit seam (ADR-0478
/// Consequences: the BFS stops once `cap` witnesses are reconstructed
/// and accepted): asked once after the level-0 setup and once after
/// every level completes, with the tables as they stand — a `true`
/// verdict stops the BFS at that level boundary. The candidate
/// pass below is then skipped whole: a capped run that halted has its
/// answer, and the below-min candidates are dropped exactly as the
/// DFS's own cap rider drops every path it never enumerated (the
/// MatchCap semantics, ADR-0475). `None` runs to fixpoint or `max` —
/// the DistinctEndpoints and Exists consumers' shape, every arrival
/// answered.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_with_halt<HALT: FnMut(&Parents<'_>, &[Endpoint]) -> bool>(
    ctx: ExecCtx<'_>,
    seed: NodeId,
    nfa: &FrontierNfa,
    budget: WalkBudget,
    pool: Pool,
    tables: &mut FrontierTables,
    bufs: &mut RuleBuffers,
    mut halt: Option<HALT>,
) -> Result<RunOutcome> {
    let view = ctx.graph;
    let states = nfa.states as usize;
    // The clause's sparse tables, shaped for the automaton at the
    // engagement — this run borrows the set wholesale and carries it
    // back out in the run.
    assert!(
        tables.shaped_for(states),
        "the clause's frontier tables are shaped for the run's automaton"
    );
    // The per-seed reset, before the set is taken: O(1), the previous
    // run's records retired by a generation bump.
    tables.begin_seed();
    let mut tables = std::mem::take(tables);
    // The endpoint marker is per node (distinct-endpoint answers), the
    // arrival record per arrival — one per node, in arrival order.
    let mut endpoints: Vec<Endpoint> = Vec::new();
    let arms = arms(view, nfa);
    let eps_from = epsilon_index(nfa);
    // The re-arrival rule (ADR-0478 §3), under the run's pool: the
    // default pool consults it at every accepting arrival it cannot
    // already prove a trail for; the walk pool's in-bounds arrivals are
    // its native semantics (never asked), and its below-min candidates
    // are — the walk-length check. The rule's span-indexed memo rides
    // the clause's recycled buffers, stamped fresh for this run.
    let mut rule = RearivalRule::new(view, nfa, &arms, seed, pool, bufs);
    // Below-min accepting arrivals, under either pool: a node whose
    // every observed accepting arrival sits below `min` may still hold
    // an in-bounds path whose final hop departs a pair the dedup filter
    // expanded only at its own, earlier level — the rule's candidate
    // set, asked once per node after the BFS (ADR-0478 §3, the
    // differential corpus's measured generalization).
    let mut candidates: Vec<Endpoint> = Vec::new();

    // The run's guard facts (the forced rung's gap classes): the NFA's
    // own guard tables read against the run's evaluation context — one
    // bundle the BFS's three check points share, never a second copy of
    // either table. An unguarded NFA pays the empty-table checks only.
    let guards = RunGuards { ctx, nfa };

    // Level 0: the seed pair, its epsilon closure, and — at
    // `min_hops == 0` — the zero-length endpoint. The seed's mark rides
    // the dedup filter's own check-and-set: the per-seed reset above
    // left the tables empty, so the arrival is new by construction (the
    // one parentless pair — every chain's terminator).
    // Whether the run's tables crossed the allocation ceiling — the
    // run-time decline's fact, read when the outcome is typed below.
    // Set at the level boundary by the byte check and per arrival by
    // the table backstop: either way the run stops before another
    // level expands, and the one tail below declines it.
    let mut over_ceiling = false;
    // A recycled table set keeps its capacity, so even the seed's
    // arrival and closure can cross the ceiling: the crossing is the
    // same run-time decline, never an expect — it rides the one tail.
    if tables.arrive(seed, nfa.start, None).is_err()
        || close_epsilons(seed, nfa.start, &guards, &mut tables, &eps_from)? == Closed::Ceiling
    {
        over_ceiling = true;
    }
    if !over_ceiling && nfa.min_hops == 0 {
        // The walk completes at hop 0 in any accepting state of the
        // seed's pair set; the first in state order is the recorded one
        // (the node sequence is empty either way).
        for state in 0..states as u32 {
            if tables.pair_seen(seed, state) && nfa.accepting[state as usize] {
                tables.mark_endpoint(seed);
                endpoints.push(Endpoint {
                    node: seed,
                    state,
                    hops: 0,
                    arrival: Arrival::Zero,
                });
                break;
            }
        }
    }

    // The level loop: expand every pair of the current level over its
    // state's transitions, accept on arrival before the dedup filter,
    // and gate expansion on the pair bitmask. The pairs of the level
    // sit in the ledger's `[level_start, level_end)` window — the level
    // grows the ledger in place, level boundaries in `per_level`. `max_hops
    // = None` runs to the pair-saturation fixpoint (an empty level),
    // never a depth cap; a bounded run stops after `max_hops`
    // expansions.
    let max_level = nfa.max_hops.unwrap_or(u32::MAX);
    let mut per_level = vec![tables.pair_count()];
    let mut edge_visits = 0u64;
    let mut hops = 0u32;
    // The early-halt seam's first ask: the zero-length endpoint can
    // fill a `LIMIT 1` cap before any expansion runs at all.
    let mut halted = halt
        .as_mut()
        .is_some_and(|probe| probe(&tables.parents(), &endpoints));
    let mut level_start = 0usize;
    let mut level_end = tables.pair_count();
    while !halted && !over_ceiling && level_start != level_end {
        if hops == max_level {
            break;
        }
        // The work budget, checked per level (Decision 6): the check
        // sees every level before it, so a run may overshoot its budget
        // by at most one level's bounded expansion, never a fixpoint.
        if edge_visits > budget.0 {
            return Err(walk_budget_exceeded());
        }
        // The allocation ceiling, checked at the same boundary over the
        // exact bytes the tables and the rule's buffers hold — the same
        // one-level overshoot bound as the work budget's. A crossing
        // declines the run (the DFS fall-through), never an error.
        if !within_table_budget(tables.heap_bytes() + rule.heap_bytes()) {
            over_ceiling = true;
            break;
        }
        // The cancellation window, sampled per level (ADR-0111: the
        // statement-wide counter, so depth cannot reset it).
        view.cancel_tick()?;
        hops += 1;
        // The loop's `max_level` break pins `hops <= max_level`; the
        // interval is spelled for the contract the arrival check reads.
        let in_bounds = hops >= nfa.min_hops && hops <= max_level;
        for index in level_start..level_end {
            let (u, s) = tables.pair_at(index);
            for arm in &arms[s as usize] {
                let t = &nfa.transitions[arm.transition];
                view.try_for_each_neighbor_resolved(u, t.direction, &arm.types, |rid| {
                    edge_visits += 1;
                    let Some(v) = view.hop(u, rid, t.direction) else {
                        return Ok::<(), Error>(());
                    };
                    // The guards (the forced rung's gap classes): an
                    // arrival failing its state's admission or the
                    // transition's group predicate is not admitted — no
                    // endpoint, no candidate, no cell — the (node, state)
                    // pair the DFS would never bind. `node_ok` is the
                    // walker's own check, so a marked cell's admission is
                    // the DFS's semantics exactly; the predicate reads the
                    // referenced variables' last bindings (the class F
                    // contract, `TransitionPredicate`).
                    if !guards.state_admits(t.to, v)?
                        || !guards.predicate_admits(
                            arm.transition,
                            (u, s),
                            v,
                            t.to,
                            &tables.parents(),
                        )?
                    {
                        return Ok(());
                    }
                    // The dedup filter's check-and-set, one record
                    // lookup: a newly-marked pair joins the ledger (the
                    // level's window grows in place) with its
                    // first-arrival parent, and the node's endpoint mark
                    // as it stood BEFORE this arrival rides the answer.
                    let arrived = tables.arrive(
                        v,
                        t.to,
                        Some(Parent {
                            node: u,
                            state: s,
                            step: Step::Rel(rid),
                        }),
                    );
                    // The ledger's index backstop: an arrival the ledger
                    // could not record sets the ceiling fact and this
                    // neighbour pass stops admitting — the level loop's
                    // condition ends the run before another level
                    // expands, and the crossing declines it.
                    let arrived = match arrived {
                        Ok(arrived) => arrived,
                        Err(TableCeiling) => {
                            over_ceiling = true;
                            return Ok(());
                        }
                    };
                    // Accept on arrival, BEFORE the dedup filter decides
                    // expansion: a node first seen earlier is still an
                    // endpoint at this hop count — and in the default
                    // pool, only when the rule can prove a trail
                    // (ADR-0478 §3: the representative-chain proof
                    // settles most arrivals without a search, first or
                    // re-; the bounded search settles the rest). The
                    // walk pool admits every in-bounds arrival outright
                    // — its native semantics.
                    if t.accepts && !arrived.endpoint {
                        if in_bounds {
                            let admitted = if pool == Pool::Default {
                                // The rule's admission answer: a
                                // disproof is the one rejection, and a
                                // budget cut neither admits nor rejects
                                // — the cut surfaces through the rule's
                                // exhaustion flag, which declines the
                                // run's outcome (ADR-0516), so no
                                // answer escapes on an unproven arrival.
                                rule.admits(&tables.parents(), (u, s), v)?
                            } else {
                                true
                            };
                            if admitted {
                                tables.mark_endpoint(v);
                                endpoints.push(Endpoint {
                                    node: v,
                                    state: t.to,
                                    hops,
                                    arrival: Arrival::Rel {
                                        pred: (u, s),
                                        rel: rid,
                                    },
                                });
                            }
                        } else if !tables.candidate_marked(v) {
                            // Below `min` — the loop's break pins
                            // `hops <= max_level`, so out of bounds can
                            // only be under the floor. A candidate under
                            // either pool: the observed arrival is out
                            // of bounds, but an in-bounds path to this
                            // node may exist unobserved (the dedup
                            // filter expanded its departure pair only
                            // at an earlier level).
                            tables.mark_candidate(v);
                            candidates.push(Endpoint {
                                node: v,
                                state: t.to,
                                hops,
                                arrival: Arrival::Rel {
                                    pred: (u, s),
                                    rel: rid,
                                },
                            });
                        }
                    }
                    // Expansion only for the newly-marked pair: its
                    // epsilon closure joins the same level.
                    if arrived.new
                        && close_epsilons(v, t.to, &guards, &mut tables, &eps_from)?
                            == Closed::Ceiling
                    {
                        over_ceiling = true;
                        return Ok(());
                    }
                    Ok(())
                })?;
            }
        }
        per_level.push(tables.pair_count() - level_end);
        level_start = level_end;
        level_end = tables.pair_count();
        // The early-halt seam's per-level ask: every arrival of the
        // level just expanded is already recorded, so the capped
        // consumer's reconstruction sees a complete level.
        halted = halt
            .as_mut()
            .is_some_and(|probe| probe(&tables.parents(), &endpoints));
    }

    // The below-min candidates, after the BFS: one rule question per
    // node under either pool (memoized, budget-shared — the default
    // pool's question is the same trail search every in-bounds unproven
    // arrival used; the walk pool's is the walk-length check). The
    // recorded arrival is the observed below-min one; the proven
    // path's own arrival is the search's, recorded nowhere (the
    // witness reconstruction's business). An early-halted run skips
    // the pass whole: its cap is full, and the candidates are dropped
    // exactly as the DFS's cap rider drops every path it never
    // enumerated (the MatchCap semantics, ADR-0475). A run over the
    // allocation ceiling skips it too: it declines below, whatever the
    // candidates would answer.
    if !halted && !over_ceiling {
        for candidate in candidates {
            // Only a proven path admits: a disproof rejects, and a
            // budget cut neither admits nor rejects — the run's
            // outcome below declines the clause (ADR-0516).
            if !tables.endpoint_marked(candidate.node)
                && matches!(rule.pool_witness(candidate.node)?, Verdict::Proven(_))
            {
                tables.mark_endpoint(candidate.node);
                endpoints.push(candidate);
            }
        }
    }

    // The rule's exhaustion and PROFILE facts, read before the arms move
    // into the run (the rule borrows them) and typed into the outcome
    // (ADR-0516): a declined run carries its counters — the record pass
    // still counts the walk — and drops its tables (the clause is
    // declining, the pool aborting, exactly an errored run's semantics),
    // so no consumer can reach an endpoint set the rule could not prove.
    // The ceiling's crossing is the second run-time decline (ADR-0521),
    // typed the same way.
    let declined = if over_ceiling {
        Some(FrontierRunDecline::TableBudget)
    } else if rule.budget_exhausted() {
        Some(FrontierRunDecline::RearrivalBudget)
    } else {
        None
    };
    let rule_facts = rule.counters();
    let run = FrontierRun {
        tables,
        endpoints,
        per_level,
        edge_visits,
        arms,
        rule: rule_facts,
    };
    Ok(match declined {
        Some(reason) => RunOutcome::Declined {
            facts: run.facts(),
            reason,
        },
        None => RunOutcome::Served(run),
    })
}

/// The per-state expansion table: every transition indexed by its from
/// state, its interned type ids carried as the resolved filter the
/// adjacency walk reads (LABS-801) — built once per run, never per pair.
pub(crate) fn arms(view: &ExecGraph<'_>, nfa: &FrontierNfa) -> Vec<Vec<Arm>> {
    let mut arms = vec![Vec::<Arm>::new(); nfa.states as usize];
    for (i, t) in nfa.transitions.iter().enumerate() {
        let types = RelTypes::from_interned(
            t.types
                .iter()
                .map(|&tid| (tid, view.base().rel_type_name(tid))),
        );
        arms[t.from as usize].push(Arm {
            transition: i,
            types,
        });
    }
    arms
}

/// The epsilon edges indexed by from state — the runner's one view of
/// the compiler's epsilon list.
fn epsilon_index(nfa: &FrontierNfa) -> Vec<Vec<u32>> {
    let mut eps_from = vec![Vec::new(); nfa.states as usize];
    for &(from, to) in &nfa.epsilons {
        eps_from[from as usize].push(to);
    }
    eps_from
}

/// The epsilon closure's outcome: every target joined the ledger, or
/// the table ceiling cut one of them — the typed fact the callers fold
/// into the run's table-budget decline (never an untyped bool on the
/// success channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closed {
    /// Every epsilon target was recorded.
    Done,
    /// A target could not be recorded: the run declines.
    Ceiling,
}

/// Expand one newly-marked pair's epsilon edges at the pair's own level:
/// every epsilon-reachable state of the same node joins the ledger's
/// current level (an epsilon consumes neither a hop nor a rel), its
/// parent recording the skip — the guard of each target state checked
/// first: a failing guard means the walk cannot stand at that position,
/// so the cell is never marked. A target already marked had its own
/// closure expanded when it was marked — the bitmask check is the whole
/// dedup.
fn close_epsilons(
    node: NodeId,
    state: u32,
    guards: &RunGuards<'_, '_>,
    tables: &mut FrontierTables,
    eps_from: &[Vec<u32>],
) -> Result<Closed> {
    let mut work = vec![(node, state)];
    while let Some((n, s)) = work.pop() {
        for &to in &eps_from[s as usize] {
            let parent = Parent {
                node: n,
                state: s,
                step: Step::Epsilon,
            };
            if !guards.state_admits(to, n)? {
                continue;
            }
            match tables.arrive(n, to, Some(parent)) {
                Ok(arrived) if arrived.new => work.push((n, to)),
                Ok(_) => {}
                // The ledger's index ceiling: the caller folds the fact
                // into the run's ceiling decline — a closure that could
                // not record its target never silently drops it.
                Err(TableCeiling) => return Ok(Closed::Ceiling),
            }
        }
    }
    Ok(Closed::Done)
}

/// One run's guard facts: the NFA's own guard tables (the forced rung's
/// compile, [`super::nfa`]) read against the run's evaluation context —
/// the one bundle the BFS's check points share. The per-state guards
/// are the walker's own `node_ok` over each state's interior node
/// patterns; the per-transition predicates are the group WHEREs, each
/// referenced variable resolved to its last binding on the walk.
struct RunGuards<'c, 'n> {
    ctx: ExecCtx<'c>,
    nfa: &'n FrontierNfa,
}

impl RunGuards<'_, '_> {
    /// Whether `node` satisfies every guard the state carries: `Ok(true)`
    /// on an unguarded state without a read, and a failing guard is a
    /// plain `false` — the pair-level answer the DFS's own per-node check
    /// would give for the same position.
    fn state_admits(&self, state: u32, node: NodeId) -> Result<bool> {
        let guards = &self.nfa.state_guards[state as usize];
        if guards.is_empty() {
            return Ok(true);
        }
        let row = Row::new();
        for guard in guards {
            if !node_ok(
                self.ctx.graph,
                node,
                guard,
                &row,
                self.ctx.params,
                self.ctx.cypher_version,
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The transition's group predicate at one arrival: each referenced
    /// variable reads its LAST binding — the arrival node at the
    /// transition's landing state, the departure node at its source
    /// state, the first-arrival chain's most recent cell at any earlier
    /// state — and the predicate evaluates over that binding row with
    /// the engine's own evaluator, the DFS's per-round semantics spelled
    /// at the pair level. An unguarded transition is a plain `true`.
    fn predicate_admits(
        &self,
        transition: usize,
        from: (NodeId, u32),
        arrival: NodeId,
        to: u32,
        parents: &Parents<'_>,
    ) -> Result<bool> {
        let Some(predicate) = &self.nfa.predicates[transition] else {
            return Ok(true);
        };
        let mut row = Row::new();
        for (name, state) in &predicate.vars {
            let node = if *state == to {
                arrival
            } else if *state == from.1 {
                from.0
            } else {
                last_bound(parents, from, *state)
            };
            let node = self
                .ctx
                .graph
                .node(node)
                .expect("a walked node is live under the view that walked it");
            row.insert(name.clone(), Value::from_node(node));
        }
        // The pattern engines' per-trial WHERE reads the one predicate
        // policy (ADR-0458 §7): a runtime list/path converts (live: a QPP
        // leg `WHERE r.ls` accepts a non-empty list), a non-boolean is the
        // typed rejection live raises (22N01).
        predicate_value_holds(&eval(
            self.ctx.graph,
            &row,
            &predicate.expr,
            self.ctx.params,
            self.ctx.cypher_version,
        )?)
    }
}

/// The variable's last binding on the first-arrival chain: walk back
/// from `from` to the most recent cell at `state`. The compile proved
/// every referenced position lies between the round's start and the
/// guard's departure state, so the walk finds the binding within the
/// round, never past its boundary — the expects below are that proof's
/// invariant, not a handled failure (there is no form of it: the
/// predicate would not have compiled).
fn last_bound(parents: &Parents<'_>, from: (NodeId, u32), state: u32) -> NodeId {
    let (mut node, mut at) = from;
    for _ in 0..STATE_CEILING {
        let parent = parents
            .parent_of(node, at)
            .expect("the compile proved the referenced position lies on the chain");
        if parent.state == state {
            return parent.node;
        }
        node = parent.node;
        at = parent.state;
    }
    unreachable!("the first-arrival chain is shorter than the state ceiling")
}
