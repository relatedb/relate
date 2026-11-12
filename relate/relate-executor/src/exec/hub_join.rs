//! The LABS-348 hub join — the two-pattern comma join through one
//! shared hub: each side enumerates once from the base row, rows group
//! by hub id, and each shared hub emits the cross product of the sides'
//! binding rows, skipping pairs whose used-rel sets overlap
//! (relationship uniqueness) and applying the statement residual to
//! every merged row. The whole family lives here — the shape gate
//! ([`hub_join_applies`]) with its minimum side-A row count
//! ([`HUB_JOIN_MIN_SIDE_ROWS`], below which the nested loop is already
//! cheap), the join itself ([`try_hub_join`], invoked by the driver at
//! its pattern-0→pattern-1 boundary over the caller's own pattern-0
//! rows; `Ok(None)` declines and the driver's nested loop continues
//! from them), the row-shape helpers ([`HubMerge`] with
//! [`merged_schema`], [`group_by_hub`], [`plain_side_rows`],
//! [`row_node_id`]), and the LABS-348 test hooks.

use crate::UsedSet;
use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, Result, Value};
use relate_ir::{path_pattern_vars, MatchMode, PathPattern};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::row::{Row, RowRung};

use super::one_path::{match_one_path, PathFilters};
use super::pattern_policies::{MatchTail, Matched};
use super::pushdown::{residual_ok, Residual};

/// The minimum side-A row count before the hub join pays for itself:
/// below it the nested loop expands side B once per surviving row anyway
/// (the measured single-left case joined as cheaply as the nested loop),
/// and the join's grouped bookkeeping is pure overhead.
const HUB_JOIN_MIN_SIDE_ROWS: usize = 128;

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
/// predicates survive the join). `Ok(Some(rows))` = the pair joined,
/// these are the final rows; `Ok(None)` = declined (the shape never
/// applied, side A fell below the minimum row count, or the test hook
/// forced the nested loop) — the caller's nested loop continues from
/// the pattern-0 rows it still holds.
pub(super) fn try_hub_join(
    ctx: ExecCtx<'_>,
    patterns: &[PathPattern],
    match_mode: MatchMode,
    row: &Row,
    a_rows: &Matched,
    residual: &Residual<'_>,
    plain_rows_only: bool,
) -> Result<Option<Vec<Row>>> {
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
    // Only side A is grouped — the join drives off side B's rows
    // directly (LABS-348). The emit order is side B's enumeration order outer and
    // the per-hub side-A binding order inner, both deterministic because
    // `match_one_path` enumerates deterministically (LABS-348).
    let a_by_hub = group_by_hub(a_rows, hub_var);
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
    let mut out = Vec::new();
    if b_rows.is_empty() {
        return Ok(Some(out));
    }
    // The merged rows' rung and B-slot split, built once per join (one
    // walk per side, so every row of a side shares one schema) — the
    // computation beside its `group_by_hub` sibling below.
    let a_names: Vec<String> = a_by_hub
        .values()
        .flatten()
        .next()
        .map(|(row, _)| row.keys().cloned().collect())
        .expect("the join gate kept at least one side-A row");
    let b_names: Vec<&str> = b_rows
        .first()
        .expect("checked non-empty above")
        .0
        .keys()
        .map(String::as_str)
        .collect();
    let merge = merged_schema(&a_names, &b_names);
    for (b_row, b_used) in &b_rows {
        let hub = row_node_id(b_row, hub_var);
        let Some(a_side) = a_by_hub.get(&hub) else {
            continue;
        };
        for (a_row, a_used) in a_side {
            ctx.graph.cancel_tick()?;
            if !a_used.is_disjoint(b_used) {
                // Relationship uniqueness across the comma pair: the
                // same rel cannot serve both patterns.
                continue;
            }
            let mut merged = a_row.clone();
            merged.stamp(
                &merge.rung,
                merge.fresh_slots.iter().map(|&s| b_row.values()[s].clone()),
            );
            for &s in &merge.bound_slots {
                merged.insert(b_names[s].to_string(), b_row.values()[s].clone());
            }
            if !residual_ok(ctx.graph, &merged, ctx.params, residual)? {
                continue;
            }
            out.push(merged);
        }
    }
    Ok(Some(out))
}

/// The hub join's merged-row schema: the rung every merged pair stamps —
/// side A's schema plus side B's fresh names, in B's own order — beside
/// the B-slot split: the slots whose name side A already binds (the hub
/// itself, an internal `__node0`/`__trail{idx}` name both sides bind)
/// replace their values per pair instead of appending. Built once per
/// join from the two sides' schemas (one walk per side, so every row of
/// a side shares one schema); lives beside [`group_by_hub`], the join's
/// other row-shape helper.
struct HubMerge {
    rung: RowRung,
    fresh_slots: Vec<usize>,
    bound_slots: Vec<usize>,
}

/// The merged-schema computation of the hub join — one spelling of the
/// name-classified slot walk, beside `group_by_hub` (LABS-348).
fn merged_schema(a_names: &[String], b_names: &[&str]) -> HubMerge {
    let mut merged_names = a_names.to_vec();
    let mut fresh_slots = Vec::new();
    let mut bound_slots = Vec::new();
    for (slot, name) in b_names.iter().enumerate() {
        if a_names.iter().any(|bound| bound.as_str() == *name) {
            bound_slots.push(slot);
        } else {
            fresh_slots.push(slot);
            merged_names.push((*name).to_string());
        }
    }
    HubMerge {
        rung: RowRung::from_names(&merged_names),
        fresh_slots,
        bound_slots,
    }
}

/// Group one side's rows by hub id (LABS-348). Keyed by hub id for the lookup;
/// the emit order comes from the driving side's enumeration, not the
/// grouped map's (its ordering is unconsumed; only the hub lookup reads it).
/// The side-A rows arrive by reference (a decline must leave them in
/// the caller's hands), so the map holds references.
fn group_by_hub<'a>(
    rows: &'a Matched,
    hub_var: &SmallName,
) -> BTreeMap<NodeId, Vec<&'a (Row, UsedSet)>> {
    let mut by_hub: BTreeMap<NodeId, Vec<&'a (Row, UsedSet)>> = BTreeMap::new();
    for pair in rows {
        let id = row_node_id(&pair.0, hub_var);
        by_hub.entry(id).or_default().push(pair);
    }
    by_hub
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
    match row.get_name(var) {
        Some(Value::Node(n)) => n.id,
        _ => unreachable!("a bound hub variable always holds a node"),
    }
}
