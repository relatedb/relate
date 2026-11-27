//! The LABS-348 hub join — the two-pattern comma join through one
//! shared hub: each side enumerates once from the base row, rows group
//! by hub id, and each shared hub emits the cross product of the sides'
//! binding rows, skipping pairs whose used-rel sets overlap
//! (relationship uniqueness) and applying the statement residual to
//! every merged row. The whole family lives here — the shape gate
//! ([`hub_join_applies`]) with its minimum side-A row count
//! ([`HUB_JOIN_MIN_SIDE_ROWS`], below which the nested loop is already
//! cheap) and its cost gate ([`hub_join_pays`], ADR-0701: side B's one
//! enumeration against the nested loop's per-row expansions), the join itself ([`try_hub_join`], invoked by the driver at
//! its pattern-0→pattern-1 boundary over the caller's own pattern-0
//! rows; `Ok(None)` declines and the driver's nested loop continues
//! from them), the row-shape helpers ([`HubMerge`] with
//! [`merged_schema`], [`HubIndex`], [`plain_side_rows`],
//! [`row_node_id`]), and the LABS-348 test hooks.

use crate::UsedSet;
use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, Result};
use relate_ir::{path_pattern_vars, MatchMode, PathPattern};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::row::{Row, RowRung};

use super::feed::RowStream;
use relate_planner::Basis;

use super::graph_stats::{element_estimate, GraphStats, StartEstimate};
use super::one_path::{match_one_path, PathFilters};
use super::pattern_policies::{reduce_row, MatchReduce, MatchTail, Matched};
use super::pushdown::{residual_ok, Residual};

/// The minimum side-A row count before the hub join pays for itself:
/// below it the nested loop expands side B once per surviving row anyway
/// (the measured single-left case joined as cheaply as the nested loop),
/// and the join's grouped bookkeeping is pure overhead.
const HUB_JOIN_MIN_SIDE_ROWS: usize = 128;

/// The margin the join must beat the nested loop by when side B's full
/// enumeration is priced from a default or a rank sentinel rather than a
/// measurement: an unmeasured price is a guess, and a tie or a near-tie
/// of guesses keeps the nested loop (REL8-2374). A gate only — a
/// measured side B still ties to the join.
const HUB_JOIN_GUESS_MARGIN: u64 = 2;

#[cfg(test)]
thread_local! {
    /// LABS-348 test hook: force `try_hub_join` to decline so a test can
    /// pin the nested loop's behavior on a fixture the join would take.
    static HUB_JOIN_DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// LABS-348 test counter: how many times the hub join took the pair.
    static HUB_JOIN_ENGAGED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// LABS-348 test helper: run `f` with the hub join forced off (the nested
/// loop's plan), restoring the flag afterwards.
#[cfg(test)]
pub(crate) fn with_hub_join_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::test_support::with_flag_disabled(&HUB_JOIN_DISABLED, f)
}

/// LABS-348 test helper: drain the engagement counter (0 = the nested
/// loop took every pair since the last take).
#[cfg(test)]
pub(crate) fn take_hub_join_engagements() -> u32 {
    HUB_JOIN_ENGAGED.with(|c| c.replace(0))
}

/// LABS-348: detect the hub-join shape — exactly two patterns, each a
/// single unquantified hop between two nodes (so every variable sits at
/// an end), sharing exactly one node variable (the hub), with no side
/// variable already bound in the incoming row, no repeatable-elements
/// mode (relationship uniqueness applies), and no cap/reduce riding the
/// last pattern (the aggregation fold is its own join design; the
/// prototype scope is the plain row-returning MATCH).
fn hub_join_applies<'p>(
    patterns: &'p [PathPattern],
    match_mode: MatchMode,
    row: &Row,
    plain_rows_only: bool,
) -> Option<&'p SmallName> {
    if patterns.len() != 2 || !plain_rows_only {
        return None;
    }
    if match_mode == MatchMode::RepeatableElements {
        return None;
    }
    for p in patterns {
        if p.nodes.len() != 2
            || p.rels.len() != 1
            || p.rels[0].var_length
            || p.rels[0].quantified
            || p.is_shortest_path_function
            || p.needs_gpm()
        {
            return None;
        }
        for v in path_pattern_vars(p) {
            if row.get_name(v).is_some() {
                // A side variable (node or rel) already bound in the
                // incoming row: the nested loop's per-row binding is part
                // of the semantics this prototype does not re-derive.
                return None;
            }
        }
    }
    // The hub is the patterns' one shared variable, and it is a node
    // variable of both (a shared rel variable is not a hub end). The
    // canonical `path_pattern_vars` (relate_ir — the one extraction,
    // LABS-348) covers node AND rel variables; both sides collect
    // into sets so the intersection counts NAMES, not occurrences — a
    // repeated variable in one pattern is one name either way
    // (LABS-348: the occurrence-counting form miscounted repeated names).
    let a: HashSet<&SmallName> = path_pattern_vars(&patterns[0]).collect();
    let b: HashSet<&SmallName> = path_pattern_vars(&patterns[1]).collect();
    let shared: Vec<&SmallName> = a.intersection(&b).copied().collect();
    if shared.len() == 1 && is_shared_node_var(patterns, shared[0]) {
        Some(shared[0])
    } else {
        None
    }
}

/// The hub join's cost gate: the join pays when side B's one full
/// enumeration is no dearer than the nested loop's per-row expansions
/// from the bound hub — `rows(A) x (side B's walk from the hub)` against
/// `side B's walk from its cheapest unbound end`, both priced by the
/// ladder's one canonical estimate ([`GraphStats::estimate`], the
/// per-row start selection's own). Ties go to the join when the price is
/// measured (the fixture where every hub has one side-A row, LABS-348);
/// a default- or rank-priced side B must beat the nested loop by
/// [`HUB_JOIN_GUESS_MARGIN`] ([`join_beats_nested`]); side B's residual
/// conjuncts narrow its start like any other estimate.
fn hub_join_pays(
    ctx: ExecCtx<'_>,
    patterns: &[PathPattern],
    hub_var: &SmallName,
    side_a_rows: usize,
    residual: &Residual<'_>,
) -> bool {
    // `hub_join_applies` guarantees side B is one unquantified hop between
    // two nodes, one of them the hub: `ENDS` are its two elements.
    const ENDS: [usize; 2] = [0, 1];
    let side_b = &patterns[1];
    let stats = GraphStats::new(ctx.graph.base(), ctx.params);
    let conjuncts = residual.conjuncts;
    let hub_bound = |name: &str| hub_var == name;
    let unbound = |_: &str| false;
    let hub_end = ENDS[usize::from(side_b.nodes[ENDS[1]].variable.as_ref() == Some(hub_var))];
    let per_hub = element_estimate(&stats, side_b, &hub_bound, conjuncts.iter(), hub_end).rows;
    let nested = per_hub.saturating_mul(side_a_rows as u64);
    // The one full enumeration starts at side B's cheaper end, neither end
    // bound.
    let [first, second] =
        ENDS.map(|end| element_estimate(&stats, side_b, &unbound, conjuncts.iter(), end));
    let full = first.cheaper(second);
    join_beats_nested(full, nested)
}

/// Whether the rows are a guess (the default table or a rank sentinel)
/// rather than a count, a sample or a statistic: the trust order
/// (`Basis`'s own `Ord`) decides, so a variant's position is the one place
/// that says which side of the line it is on.
pub(super) fn is_guess(price: StartEstimate) -> bool {
    price.basis <= Basis::Default
}

/// The gate's decision on the two prices: a measured (or sampled, or
/// statistic-backed) enumeration ties to the join; a default- or
/// rank-priced one must beat the nested loop by [`HUB_JOIN_GUESS_MARGIN`].
pub(super) fn join_beats_nested(full: StartEstimate, nested: u64) -> bool {
    let margin = if is_guess(full) {
        HUB_JOIN_GUESS_MARGIN
    } else {
        1
    };
    full.rows.saturating_mul(margin) <= nested
}

/// The name is a node variable of every pattern (the hub must be a
/// shared end, not a shared rel).
fn is_shared_node_var(patterns: &[PathPattern], name: &SmallName) -> bool {
    patterns
        .iter()
        .all(|p| p.nodes.iter().any(|n| n.variable.as_ref() == Some(name)))
}

/// LABS-348's join, invoked at the driver's pattern-0→pattern-1
/// boundary: the caller hands in its own already-produced pattern-0
/// rows (`a_rows` — the plain-side policy the driver's loop itself
/// spells, so the former private side-A enumeration and the Seeded
/// handshake it fed are gone, the equivalence true by construction),
/// side B enumerates once from the base row, rows group by hub id, and
/// each shared hub emits the cross product of the sides' binding rows
/// — the per-side rows are the payloads. Relationship uniqueness is
/// preserved by skipping pairs whose used-rel sets overlap; the
/// statement residual is applied to every merged row (cross-pattern
/// predicates survive the join). The fan-out is the inter-clause stream
/// itself ([`HubFanOut`]): the rows arrive in batches as the caller
/// pulls them, never as a container proportional to the output.
/// `Ok(Some(stream))` = the pair joined; `Ok(None)` = declined (the
/// shape never applied, side A fell below the minimum row count, the
/// cost gate priced the nested loop cheaper, or the test hook forced the
/// nested loop) — the caller's nested loop
/// continues from the pattern-0 rows it still holds.
pub(super) fn try_hub_join<'a: 'r, 'r>(
    ctx: ExecCtx<'a>,
    patterns: &'r [PathPattern],
    match_mode: MatchMode,
    row: &'r Row,
    a_rows: &'r Matched,
    residual: &'r Residual<'r>,
    plain_rows_only: bool,
) -> Result<Option<RowStream<'r>>> {
    // The decline path (the overwhelmingly common one) pays a borrow, not
    // a row clone: the cheap shape checks run before the clone the join
    // itself needs (the side-B enumeration's seed) (LABS-348).
    #[cfg(test)]
    if HUB_JOIN_DISABLED.with(std::cell::Cell::get) {
        return Ok(None);
    }
    // Bind once, clone after (LABS-348): the predicate's
    // Some payload is the hub var itself, and the row clone stays
    // behind the gate — the decline path (the common one) pays one
    // predicate call and a borrow, nothing else.
    let Some(hub_var) = hub_join_applies(patterns, match_mode, row, plain_rows_only) else {
        return Ok(None);
    };
    // The caller's pattern-0 rows are side A; the row-count gate reads
    // them without a re-enumeration (the driver already paid it).
    if a_rows.len() < HUB_JOIN_MIN_SIDE_ROWS {
        // The nested loop is already cheap at this side-A size; the join
        // bookkeeping would be pure overhead (the measured single-left
        // case joined as cheaply as the nested loop) — the driver's
        // loop continues from its own rows.
        return Ok(None);
    }
    // The cost gate: the join enumerates side B once from the base row,
    // however few hubs side A holds; the nested loop expands side B per
    // side-A row from the bound hub. A selective side A (the planner's
    // order already filtered it to a few rows) makes the full enumeration
    // the expensive plan.
    if !hub_join_pays(ctx, patterns, hub_var, a_rows.len(), residual) {
        return Ok(None);
    }
    // Only side A is indexed — the join drives off side B's rows
    // directly (LABS-348). The emit order is side B's enumeration order outer and
    // the per-hub side-A binding order inner, both deterministic because
    // `match_one_path` enumerates deterministically (LABS-348). The build
    // side consumes the side-A rows as the pattern walk produced them and
    // holds only its charged hub index (ADR-0660's retain disposition).
    let a_index = HubIndex::build(a_rows.iter(), hub_var)?;
    // LABS-801: side B's filters resolve once per join — the enumeration
    // below is one walk over the side's pattern.
    let b_filters = PathFilters::resolve(ctx.graph, &patterns[1], match_mode);
    let b_rows = plain_side_rows(
        ctx,
        &patterns[1],
        &b_filters,
        row.clone(),
        match_mode,
        residual,
    )?;
    #[cfg(test)]
    HUB_JOIN_ENGAGED.with(|c| c.set(c.get() + 1));
    let Some((first, _)) = b_rows.first() else {
        return Ok(Some(RowStream::emitting(std::iter::empty())));
    };
    // The merged rows' rung and B-slot split, built once per join (one
    // walk per side, so every row of a side shares one schema) — the
    // computation beside its `HubIndex` sibling below. The names are
    // owned so the fan-out can carry them past the side-B rows it moves
    // into itself.
    let a_names: Vec<String> = a_index
        .by_hub
        .values()
        .flatten()
        .next()
        .map(|(row, _)| row.keys().cloned().collect())
        .expect("the join gate kept at least one side-A row");
    let b_names: Vec<String> = first.keys().cloned().collect();
    let merge = merged_schema(&a_names, &b_names);
    Ok(Some(RowStream::emitting(HubFanOut {
        ctx,
        index: a_index,
        b_rows: b_rows.into_iter(),
        hub: hub_var,
        b_names,
        merge,
        residual,
        current: None,
    })))
}

/// The join's one entry from the pattern loop: the admission call
/// ([`try_hub_join`], with the driver's fold-only decision), the batch
/// drain and the reduce — so the loop body states only the boundary
/// check and the early return, and the join's strategy lives beside the
/// join itself. `Ok(None)` = declined; the caller's nested loop
/// continues from the pattern-0 rows it still holds.
pub(super) fn hub_join_answer<'a: 'r, 'r>(
    ctx: ExecCtx<'a>,
    patterns: &'r [PathPattern],
    match_mode: MatchMode,
    base: &'r Row,
    current: &'r Matched,
    residual: &'r Residual<'r>,
    reduce: Option<&MatchReduce<'_>>,
) -> Result<Option<Vec<Row>>> {
    // A fold-only reduce rides the join: its rows take the same reduce
    // the nested loop's rows take in the sink.
    let plain_rows_only = reduce.is_none_or(|r| r.sink.irrevocable());
    let Some(mut stream) = try_hub_join(
        ctx,
        patterns,
        match_mode,
        base,
        current,
        residual,
        plain_rows_only,
    )?
    else {
        return Ok(None);
    };
    // ADR-0660: the join's fan-out arrives a bounded batch at a time and
    // the reduce runs per row as it arrives, so the pull holds no fan-out
    // container — only the reduced answer rows accumulate.
    let mut out = Vec::new();
    while let Some(batch) = stream.next_batch()? {
        for row in batch {
            out.extend(reduce_row(ctx.graph, row, reduce)?);
        }
    }
    Ok(Some(out))
}

/// The hub join's fan-out as one lazy row sequence (ADR-0660): side B's
/// enumeration order outer, each hub's side-A binding order inner — the
/// order the collected form emitted — one merged row built per pull. Two
/// containers are retained: the side-B probe rows (`b_rows`) are the walk's
/// own output, held for the fan-out's lifetime, and the [`HubIndex`] — the
/// only one charged (ADR-0463). Every merged row is handed to the
/// consumer's batch as it is built, never collected here.
struct HubFanOut<'a, 'r> {
    ctx: ExecCtx<'a>,
    index: HubIndex<'r>,
    b_rows: std::vec::IntoIter<(Row, UsedSet)>,
    hub: &'r SmallName,
    b_names: Vec<String>,
    merge: HubMerge,
    residual: &'r Residual<'r>,
    /// The side-B row being fanned out: its used set, its hub, and the
    /// next side-A slot of that hub's group.
    current: Option<(Row, UsedSet, NodeId, usize)>,
}

impl Iterator for HubFanOut<'_, '_> {
    type Item = Result<Row>;

    fn next(&mut self) -> Option<Result<Row>> {
        loop {
            let Some((hub, slot)) = self.current.as_ref().map(|(_, _, hub, slot)| (*hub, *slot))
            else {
                let (b_row, b_used) = self.b_rows.next()?;
                let hub = row_node_id(&b_row, self.hub);
                if self.index.by_hub.contains_key(&hub) {
                    self.current = Some((b_row, b_used, hub, 0));
                }
                continue;
            };
            // The side-A pair is `'r` — copied out of the charged index so
            // the pair's emission holds no borrow of it.
            let Some((a_row, a_used)) = self
                .index
                .by_hub
                .get(&hub)
                .and_then(|side| side.get(slot))
                .copied()
            else {
                self.current = None;
                continue;
            };
            let (b_row, b_used) = {
                let (b_row, b_used, _, slot) = self.current.as_mut().expect("checked above");
                *slot += 1;
                (b_row, b_used)
            };
            if let Err(error) = self.ctx.graph.cancel_tick() {
                return Some(Err(error));
            }
            if !a_used.is_disjoint(b_used) {
                // Relationship uniqueness across the comma pair: the
                // same rel cannot serve both patterns.
                continue;
            }
            let mut merged = a_row.clone();
            merged.stamp(
                &self.merge.rung,
                self.merge
                    .fresh_slots
                    .iter()
                    .map(|&s| b_row.values()[s].clone()),
            );
            for &s in &self.merge.bound_slots {
                merged.insert(self.b_names[s].clone(), b_row.values()[s].clone());
            }
            match residual_ok(self.ctx.graph, &merged, self.ctx.params, self.residual) {
                Ok(true) => return Some(Ok(merged)),
                Ok(false) => continue,
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// The join's build side: the side-A rows indexed by hub id — held as
/// references into the caller's own rows, so the index is its only
/// container. Its slots are charged as the rows land (ADR-0463), the
/// declared state of ADR-0660's retain disposition.
struct HubIndex<'a> {
    by_hub: BTreeMap<NodeId, Vec<&'a (Row, UsedSet)>>,
    /// The index's one retaining container: one map entry per hub plus one
    /// slot per indexed side-A row.
    charge: relate_core::tokens::WorkspaceCharge,
    slots: usize,
}

impl<'a> HubIndex<'a> {
    /// Index the side-A rows one at a time, charging each landing's slot
    /// ahead of the push (ADR-0463: a refusal leaves nothing retained).
    fn build(rows: impl Iterator<Item = &'a (Row, UsedSet)>, hub_var: &SmallName) -> Result<Self> {
        const SLOT: u64 = size_of::<&(Row, UsedSet)>() as u64;
        let mut index = Self {
            by_hub: BTreeMap::new(),
            charge: relate_core::tokens::WorkspaceCharge::empty(),
            slots: 0,
        };
        for pair in rows {
            let hub = row_node_id(&pair.0, hub_var);
            let fresh = usize::from(!index.by_hub.contains_key(&hub));
            let (hubs, slots) = (index.by_hub.len() + fresh, index.slots + 1);
            index.charge.resize_with(|| {
                crate::workspace::map_entries_bytes(hubs)
                    + crate::workspace::buffer_bytes(slots, SLOT)
            })?;
            index.by_hub.entry(hub).or_default().push(pair);
            index.slots += 1;
        }
        Ok(index)
    }
}

/// The hub join's merged-row schema: the rung every merged pair stamps —
/// side A's schema plus side B's fresh names, in B's own order — beside
/// the B-slot split: the slots whose name side A already binds (the hub
/// itself, an internal `__node0`/`__trail{idx}` name both sides bind)
/// replace their values per pair instead of appending. Built once per
/// join from the two sides' schemas (one walk per side, so every row of
/// a side shares one schema); lives beside [`HubIndex`], the join's
/// other row-shape helper.
struct HubMerge {
    rung: RowRung,
    fresh_slots: Vec<usize>,
    bound_slots: Vec<usize>,
}

/// The merged-schema computation of the hub join — one spelling of the
/// name-classified slot walk, beside `HubIndex` (LABS-348).
fn merged_schema(a_names: &[String], b_names: &[String]) -> HubMerge {
    let mut merged_names = a_names.to_vec();
    let mut fresh_slots = Vec::new();
    let mut bound_slots = Vec::new();
    for (slot, name) in b_names.iter().enumerate() {
        if a_names.iter().any(|bound| bound == name) {
            bound_slots.push(slot);
        } else {
            fresh_slots.push(slot);
            merged_names.push(name.clone());
        }
    }
    HubMerge {
        rung: RowRung::from_names(&merged_names),
        fresh_slots,
        bound_slots,
    }
}

/// One plain-side enumeration for the join's side B: the pattern from a
/// base row, a fresh used-set, no cap, no reduce, keep the set. Side A
/// is the caller's own pattern-0 output — the driver's loop spells the
/// same plain-side policy itself — so this helper enumerates side B
/// only (LABS-348).
fn plain_side_rows(
    ctx: ExecCtx<'_>,
    pattern: &PathPattern,
    filters: &PathFilters,
    row: Row,
    match_mode: MatchMode,
    residual: &Residual<'_>,
) -> Result<Matched> {
    match_one_path(
        ctx,
        pattern,
        filters,
        row,
        UsedSet::default(),
        match_mode,
        residual,
        None,
        MatchTail::Carry,
    )
}

fn row_node_id(row: &Row, var: &SmallName) -> NodeId {
    crate::pattern_walk::classify(Some(var), row, crate::pattern_walk::node_id)
        .bound_id()
        .expect("a bound hub variable always holds a node")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_core::tokens::{workspace_meter, LedgerGuard};
    use relate_ir::Clause;

    use crate::overlay::ExecGraph;
    use crate::row::RowNames;

    use super::super::batch_tail::BATCH_ROWS;
    use super::super::test_support::{build, clauses_of};
    use super::*;

    /// Side A's row total and side B's node total, both held constant
    /// across the two fixtures: only their distribution over the hubs
    /// moves, so the fan-out per hub grows while every charged input
    /// figure stays put.
    const SIDE_A_ROWS: usize = 128;
    const SIDE_B_ROWS: usize = 32;

    /// The comma pair the join takes: side A is the caller's own pattern-0
    /// output, side B enumerates from the base row through the hub.
    fn pair_patterns() -> Vec<PathPattern> {
        let clauses = clauses_of("MATCH (a:A)-[:RA]->(h:H), (b:B)-[:RB]->(h) RETURN count(*) AS c");
        match clauses.into_iter().next() {
            Some(Clause::Match { patterns, .. }) => patterns,
            _ => panic!("the fixture opens with the comma-pair MATCH"),
        }
    }

    /// Run the join over `hubs` hubs, the side-A rows and side-B nodes
    /// spread evenly over them, and report the emitted row count and the
    /// peak workspace charge while the fan-out is pulled — the stream is
    /// drained one batch at a time, so only the join's own state is held.
    fn measure(hubs: usize) -> (usize, u64) {
        assert_eq!(SIDE_A_ROWS % hubs, 0);
        // Nodes 0..hubs are the hubs; the side-B nodes follow, each wired
        // to its hub by one `RB` rel.
        let mut labels: Vec<&[&str]> = vec![&["H"][..]; hubs];
        labels.resize(hubs + SIDE_B_ROWS, &["B"][..]);
        let rels: Vec<(usize, usize, &str)> = (0..SIDE_B_ROWS)
            .map(|b| (hubs + b, b % hubs, "RB"))
            .collect();
        let built = build(&labels, &rels);
        let view = ExecGraph::read(&built.graph);
        let params = BTreeMap::new();
        let ctx = ExecCtx::new(&view, &params);
        let patterns = pair_patterns();
        let names = RowNames::new(&["h".to_string()]);
        let a_rows: Matched = (0..SIDE_A_ROWS)
            .map(|i| {
                let hub = built.nodes[i % hubs];
                (
                    Row::from_names(&names, vec![view.node_value(hub)]),
                    UsedSet::default(),
                )
            })
            .collect();
        let base = Row::new();
        let residual = Residual::empty();
        let mut ledger = crate::test_support::roomy_ledger();
        let guard = LedgerGuard::install(&mut ledger);
        let _ = take_hub_join_engagements();
        let mut stream = try_hub_join(
            ctx,
            &patterns,
            MatchMode::DifferentRelationships,
            &base,
            &a_rows,
            &residual,
            true,
        )
        .expect("the join runs infallibly")
        .expect("the join takes the pair");
        assert_eq!(take_hub_join_engagements(), 1, "the join engaged");
        // The fan-out is produced on pull, never a container in hand: this
        // is the property the peak below depends on (a held stream would
        // report every merged row in `probe`).
        assert!(!stream.is_held(), "the join hands back a produced stream");
        assert!(stream.probe().is_empty(), "no merged row is in hand");
        let mut rows = 0usize;
        while let Some(batch) = stream.next_batch().expect("the join is infallible") {
            let count = batch.into_iter().count();
            assert!(count <= BATCH_ROWS, "a batch is bounded");
            rows += count;
        }
        let (_, peak, _) = workspace_meter().expect("a ledger is installed");
        drop(guard);
        (rows, peak)
    }

    /// The join's retained total is the charged side-A index, independent
    /// of the fan-out: the same side-A and side-B row totals over one hub
    /// fan out four times as wide as over four hubs, and the peak does not
    /// rise. A join that built its output would grow with the emitted rows
    /// instead (and would hand back a held stream, per `measure`).
    #[test]
    fn the_join_streams_a_fan_out_independent_of_the_hub_index() {
        let (four_hubs, at_four_hubs) = measure(4);
        let (one_hub, at_one_hub) = measure(1);
        assert_eq!(four_hubs, SIDE_A_ROWS * SIDE_B_ROWS / 4);
        assert_eq!(one_hub, SIDE_A_ROWS * SIDE_B_ROWS);
        assert!(at_four_hubs > 0, "the side-A index is charged");
        assert!(
            at_one_hub <= at_four_hubs,
            "the fan-out per hub grew 4x ({four_hubs} -> {one_hub} rows) \
             but the retained total rose: {at_one_hub} vs {at_four_hubs}"
        );
    }
}
