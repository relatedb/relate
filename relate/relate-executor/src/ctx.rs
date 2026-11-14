//! Shared execution-context parameter objects threaded through clause
//! execution and expression evaluation — plus the PROFILE phase clocks (LABS-448) the phase wrappers stamp. The
//! pattern-walk vocabulary both walk engines (eval, exec) share —
//! bindings, hop bounds, stub binders, path-walk carriers — lives one
//! module over, [`crate::pattern_walk`] (LABS-668).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::thread::LocalKey;

use relate_core::{Result, Value};
use web_time::Instant;

use crate::metrics::PhaseTimes;
use crate::overlay::ExecGraph;
use crate::redo::RedoOp;
use crate::row::Row;

/// Statement-level context threaded through clause execution: the
/// statement's source text (which feeds `SHOW TRANSACTIONS`'
/// `currentQuery` column) and the connection's authenticated principal
/// (ADR-0273: set by a surface that verified an identity — the Bolt server
/// after a validated HELLO — else `None`, and `dbms.showCurrentUser`
/// reports the documented placeholder).
#[derive(Clone, Copy)]
pub(crate) struct QueryCtx<'a> {
    pub query_text: &'a str,
    pub principal: Option<&'a str>,
    /// ADR-0210: per-batch durable-commit sink for `CALL { … } IN
    /// TRANSACTIONS`. Set by the facade's auto-commit write path; `None`
    /// everywhere else (tests, nested execution, explicit transactions),
    /// where batching keeps the accumulate-in-scratch semantics.
    pub batch_commit: Option<&'a dyn BatchCommit>,
}

// The PROFILE phase clocks (LABS-448) — the thread-locals behind
// [`PhaseTimes`] and the wrappers every walker stamps through. They live
// here, the crate's lowest shared module, so both walk engines (eval,
// exec) read them downward (LABS-668): the eval walkers' phase spans made
// the old home beside the exec-only counters (`exec/profiling`) an upward
// import for `eval`. The clocks and their stamp body stay private to this
// module: the stamp→run→count pairing is the wrappers' implementation
// detail, never an API a walker could hand-roll beside them.
thread_local! {
    /// Whether a PROFILE statement's clause arm is running (LABS-448) —
    /// the phase stamps read this before touching the clock, so a
    /// non-PROFILEd statement pays the thread-local read and nothing
    /// else. Armed by the clause's metric tick, disarmed by its drop.
    static PHASE_PROFILING: Cell<bool> = const { Cell::new(false) };
    /// The inclusive seek-phase sum (LABS-448): the elapsed time
    /// resolving and checking node candidates — the store work behind the
    /// `Nodes` kind. Inclusive like the table's `Total` column: a walk
    /// that triggers nested seeks counts both.
    static SEEK_NS: Cell<u128> = const { Cell::new(0) };
    /// The inclusive walk-phase sum (LABS-448): the elapsed time following
    /// relationships — the store work behind the `Rels`/`Degree` kinds.
    static WALK_NS: Cell<u128> = const { Cell::new(0) };
}

/// Arm the phase stamps for a PROFILE clause arm (LABS-448) — the metric
/// tick's first act, undone by its drop, so the stamps never run outside a
/// profiled statement.
pub(crate) fn arm_phase_profiling() {
    PHASE_PROFILING.with(|c| c.set(true));
}

/// Disarm the phase stamps (LABS-448) — the metric tick's last act.
pub(crate) fn disarm_phase_profiling() {
    PHASE_PROFILING.with(|c| c.set(false));
}

/// The one phase-span body both wrappers delegate to (LABS-448; one
/// spelling since LABS-668's stellar round): take the stamp, run the
/// work, count its elapsed time into `sum`. The stamp is this helper's
/// local — taken only while a PROFILE statement's arm runs, so a
/// non-PROFILEd statement pays the one thread-local read and nothing
/// else, and a `None` stamp counts nothing. A run that errors counts
/// nothing either: the arm's `time_ns` covers what the phases do not.
fn phase_span<T>(sum: &'static LocalKey<Cell<u128>>, run: impl FnOnce() -> Result<T>) -> Result<T> {
    let stamp = PHASE_PROFILING.with(Cell::get).then(Instant::now);
    let out = run()?;
    if let Some(start) = stamp {
        sum.with(|c| c.set(c.get() + start.elapsed().as_nanos()));
    }
    Ok(out)
}

/// One seek-phase span around a pattern node's candidate resolution
/// (LABS-448). Since the PR-604 stellar round the stamp is the counted
/// funnel's, not a per-walker obligation: `node_candidates`
/// (`crate::pattern_walk`, the shared walk tier) resolves through this —
/// the one caller — so the seek stamp lives beside the read count it
/// pairs with and every path that bumps the `Nodes` counter (both walk
/// engines' candidate reads) carries seek time, never a re-stamped copy
/// per walker. `None` stamps (non-PROFILEd statements) resolve unchanged.
pub(crate) fn phase_seek<T>(resolve: impl FnOnce() -> Result<T>) -> Result<T> {
    phase_span(&SEEK_NS, resolve)
}

/// One walk-phase span around a relationship traversal (LABS-448): the
/// symmetric spelling of [`phase_seek`] over the walk clock — both
/// delegate to the one span body, so the walker sites never hand-roll
/// the stamp→run→count pairing, and the walk-phase contract (inclusive of
/// nested phases) is stated once, here. `None` stamps resolve unchanged
/// on non-PROFILEd statements.
pub(crate) fn phase_walk<T>(walk: impl FnOnce() -> Result<T>) -> Result<T> {
    phase_span(&WALK_NS, walk)
}

/// The phase-time sums right now (LABS-448), snapshotted at the clause
/// tick the way the store-read mix (`exec::profiling::store_reads`) is:
/// the delta across the arm is the clause's phase times, two thread-local
/// reads per clause.
pub(crate) fn phase_times() -> PhaseTimes {
    PhaseTimes {
        seek_ns: SEEK_NS.with(Cell::get),
        walk_ns: WALK_NS.with(Cell::get),
    }
}

// The node-store reads (LABS-375): one per candidate a node match
// examined (the all-nodes scan, the label scan, the index seek) — the
// scan work the hop/neighbor counters never see, because they count
// relationship traversal only. It lives beside the phase clocks because
// its one counting point is the node-candidate funnel
// (`crate::pattern_walk::node_candidates`), which both walk engines read:
// a counter with an engine's name on it would be an upward import from
// the walk tier that bumps it.
thread_local! {
    static NODE_READS: Cell<u64> = const { Cell::new(0) };
}

/// Count `count` node-store reads (LABS-375) — batched because the one
/// funnel yields the whole candidate list at once, and every node-match
/// path reads it.
pub(crate) fn count_node_reads(count: u64) {
    NODE_READS.with(|c| c.set(c.get() + count));
}

/// The node-read total right now — read by the store-read mix
/// ([`crate::exec::profiling::store_reads`]) at clause boundaries, so a
/// per-clause delta costs one thread-local read, never a per-row one.
pub(crate) fn node_reads() -> u64 {
    NODE_READS.with(Cell::get)
}

/// Take the node-read total, leaving zero — the test-side snapshot the
/// store-read pins assert against.
#[cfg(test)]
pub(crate) fn take_node_reads() -> u64 {
    NODE_READS.with(|c| c.replace(0))
}

/// Per-batch commit sink for `CALL { … } IN TRANSACTIONS` (ADR-0210).
/// Called by the executor after each successful batch with the batch's
/// drained redo ops and the snapshot timestamp they were built against;
/// the implementation commits them durably (WAL + apply) before the next
/// batch starts, so live scratch state stays bounded by one batch.
/// Returns the commit timestamp the batch was applied at — the caller
/// rebases its scratch onto it, so only the sink (which owns the commit)
/// ever reads the base graph's clock.
pub trait BatchCommit {
    fn commit_batch(&self, ops: Vec<RedoOp>, snapshot_ts: u64) -> Result<u64>;
}

impl<'a> QueryCtx<'a> {
    pub(crate) fn new(query_text: &'a str) -> Self {
        Self {
            query_text,
            principal: None,
            batch_commit: None,
        }
    }

    /// ADR-0273: attach the connection's authenticated principal.
    pub(crate) fn with_principal(mut self, principal: Option<&'a str>) -> Self {
        self.principal = principal;
        self
    }

    /// ADR-0210: attach the per-batch commit sink.
    pub(crate) fn with_batch_commit(mut self, sink: Option<&'a dyn BatchCommit>) -> Self {
        self.batch_commit = sink;
        self
    }
}

/// Graph view + query parameters shared by clause execution and evaluation.
/// `principal` is the connection's authenticated principal (ADR-0273), read
/// by the `dbms.showCurrentUser` procedure arm; `None` when the surface set
/// none.
#[derive(Clone, Copy)]
pub(crate) struct ExecCtx<'a> {
    pub graph: &'a ExecGraph<'a>,
    pub params: &'a BTreeMap<String, Value>,
    pub principal: Option<&'a str>,
}

impl<'a> ExecCtx<'a> {
    pub(crate) fn new(graph: &'a ExecGraph<'a>, params: &'a BTreeMap<String, Value>) -> Self {
        Self {
            graph,
            params,
            principal: None,
        }
    }

    /// ADR-0273: attach the connection's authenticated principal.
    pub(crate) fn with_principal(mut self, principal: Option<&'a str>) -> Self {
        self.principal = principal;
        self
    }
}

/// Expression-evaluation context: graph view, current row, and parameters.
#[derive(Clone, Copy)]
pub(crate) struct EvalCtx<'a> {
    pub graph: &'a ExecGraph<'a>,
    pub row: &'a Row,
    pub params: &'a BTreeMap<String, Value>,
}
