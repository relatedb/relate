//! ADR-0468's first live operator chain: label scan, filter, and
//! projection as batch operators over [`Chunk`], engaged for scan-shaped
//! `MATCH` clauses (LABS-359 — the substrate's first consumer).
//!
//! The engagement gate ([`compile`]) is deliberately conservative: a
//! single unpatterned node over the one unbound seed row, a static label
//! conjunction, and `WHERE` conjuncts that each read one property of the
//! scan variable against a row-invariant, deterministic bound. Anything
//! it cannot prove it declines, and the dispatcher runs today's per-row
//! `match_clause` unchanged — same plan, same rows, so the openCypher TCK
//! is the net either way.
//!
//! The chain, pull-based inside the clause (ADR-0468's discipline):
//!
//! - the **scan operator** resolves candidates through the one existing
//!   funnel ([`node_candidates`], so index seeks, the PROFILE node-read
//!   counter, and the seek-phase stamp all keep working) and builds one
//!   [`Chunk`] per [`CHUNK_ROWS`] batch: the node binding plus one column
//!   per property the conjuncts read, encoded by the type layer's one
//!   encoder;
//! - the **filter operator** walks the compiled conjuncts over the chunk,
//!   one column at a time, consulting the null bitmap before every
//!   comparison (a null operand makes the comparison null — Cypher's
//!   three-valued logic, exactly what [`binary`] computes for a null
//!   operand — and a `WHERE` keeps only `true`). It keeps a selection
//!   vector, so a row an earlier conjunct dropped never reaches a later
//!   one — the per-row short-circuit `residual_ok` keeps, lazily
//!   including the bound's own evaluation;
//! - the **breaker** materializes: the surviving chunk compacts
//!   ([`Chunk::retain_rows`]), projects down to the clause's binding
//!   columns ([`Chunk::select_columns`]) — the scratch property columns a
//!   filter read stay behind — and decodes through the lossless
//!   [`Chunk::to_rows`]. `Row` stays the boundary representation; nothing
//!   but the binding columns ever leaves the clause.
//!
//! Zone maps on batches (LABS-645, ADR-0468 layer 2): every column's
//! construction derives its zone — null-presence, and for the typed
//! kinds the min/max over the non-null values — and the filter consults
//! it before walking a conjunct's rows. The metadata is DERIVED from the
//! batch's contents, never estimated: a skip decision must be a provable
//! implication of the predicate and the min/max ([`zone_proves_no_match`]
//! states each rule), because a wrong zone map is a correctness bug, not
//! a missed optimization. The skip IS the correct result — a conjunct
//! that provably holds for no value makes the survivor set empty — and
//! the skipped evaluation is counted (the `count_zone_skip` counter,
//! PROFILEd per clause as `zone_skips`). Two
//! boundaries, stated once: the zone is consulted only when a predicate
//! exists (a predicate-free path never reads it), and there are no bloom
//! filters — build+probe costs more than a plain small-batch scan (the
//! literature-anchored call, ADR-0468 layer 2). Top-K boundary pushdown
//! (an ORDER BY limit cut against a future sort's zone) is deliberately
//! NOT here: it needs the ORDER BY plumbing, and the extension point is
//! this filter's conjunct loop — a top-K bound is one more provable
//! no-match decision over the same zone facts.
//!
//! One ordering caveat, stated once here: the per-row path evaluates a
//! row's whole conjunct list before moving to the next row, while the
//! filter walks one conjunct across the batch. Both stop at the first
//! error, so a clause that errors errors either way — but a pathological
//! `WHERE` whose *first* conjunct errors on a late row and whose *second*
//! errors on an early one can surface the other conjunct's message. Every
//! other behavior — rows kept, rows dropped, nulls, error timing against
//! an empty survivor set — is the per-row path's, by construction.

use std::cmp::Ordering;

use relate_core::{NodeId, Result, Value};
use relate_ir::{var_prop_named, BinaryOp, Expr, MatchMode, NodePattern, PathPattern, UnaryOp};
use relate_names::{NamedToken, SmallName};

use crate::ctx::{ExecCtx, WalkStart};
use crate::eval::{binary_pure, eval};
use crate::overlay::ExecGraph;
use crate::pattern_walk::{label_index_seekable, node_candidates};
use crate::row::Row;

use super::agg_fold::is_pred_op;
use super::candidates::node_ok;
use super::chunk::{Chunk, Column, ColumnZone, CHUNK_ROWS};
use super::pushdown::flatten_and_owned;

/// The chunk's binding slot: column 0 carries the scan variable's node
/// binding, the predicate property columns follow. The breaker's
/// projection keeps exactly this slot — the clause's output is the seed
/// row's bindings (empty, by the gate) plus the pattern's binding.
const VAR_SLOT: usize = 0;

/// Run one scan-shaped `MATCH` clause as the batch chain, or decline it.
///
/// `Ok(None)` is the decline: the shape is not scan-shaped (or a test
/// hook forced the per-row path), and the caller runs its per-row
/// `match_clause`
/// unchanged. `Ok(Some(rows))` is the clause's output — the same rows,
/// order, and bindings the per-row path produces, materialized at the
/// breaker.
///
/// `want` is the plain cap's `SKIP+LIMIT` (LABS-1452), `Some` only when
/// the clause's suffix is a plain `RETURN`/`WITH … LIMIT` the cap gate
/// proved cannot reorder or drop rows (`match_cap::plain_limit_cap`):
/// the chain then returns the first `want` kept rows in scan order and
/// reads no node past the chunk that produced them. `None` is the
/// unbounded chain. The bound covers rows, scan work and the container's
/// charge, not the candidate id list: [`node_candidates`] still collects
/// every candidate id (an O(n) vector of ids, no rows) before the first
/// chunk, the one cost left until the id walk streams (LABS-1431).
pub(crate) fn try_scan_filter_batch(
    ctx: ExecCtx<'_>,
    rows: &[Row],
    optional: bool,
    match_mode: MatchMode,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    want: Option<usize>,
) -> Result<Option<Vec<Row>>> {
    // LABS-359 test hook: force the decline so a differential test can
    // pin the per-row path's answer on a shape the chain would take.
    #[cfg(test)]
    if batch_disabled() {
        return Ok(None);
    }
    let conjuncts = where_clause.map(flatten_and_owned).unwrap_or_default();
    let Some(plan) = compile(rows, optional, match_mode, patterns, &conjuncts) else {
        return Ok(None);
    };
    #[cfg(test)]
    batch_engaged();
    let ExecCtx { graph, params, .. } = ctx;
    // The walk-start fact (LABS-369) the per-row path records per walk:
    // this clause's only walk starts at the pattern's only element,
    // forward, the un-optimised default — recorded once, the way the
    // single driving row records it once.
    super::record_walk_start(WalkStart::seed_side_forward());
    let seed = &rows[0];
    let candidates = node_candidates(graph, plan.node, seed, params, &conjuncts)?;
    // LABS-1452: the plain cap's want bounds the output, so the chain stops
    // once that many kept rows exist; no cap is `None` and the chain runs
    // to the end, as before. The test hook below stands the pushdown down
    // so a differential can compare it against the unbounded chain.
    #[cfg(test)]
    let want = if pushdown_disabled() { None } else { want };
    // Admission is the walker's own per-candidate check, unchanged: the
    // same `node_ok` call the per-row path makes per candidate, outside
    // the seek phase like it is there. It runs one batch ahead of the
    // scan, so a bounded run admits (and reads) only the batches it needs.
    let mut candidates = candidates.into_iter();
    let mut out: Vec<Row> = Vec::new();
    let mut batch: Vec<NodeId> = Vec::new();
    while want.is_none_or(|w| out.len() < w) {
        let size = plan.batch_size(want.map(|w| w - out.len()));
        batch.clear();
        while batch.len() < size {
            let Some(id) = candidates.next() else { break };
            if node_ok(graph, id, plan.node, seed, params)? {
                batch.push(id);
            }
        }
        if batch.is_empty() {
            break;
        }
        graph.cancel_tick()?;
        #[cfg(test)]
        batch_scanned(batch.len());
        let chunk = scan_chunk(graph, &plan, &batch);
        let survivors = filter_chunk(ctx, &plan, &chunk)?;
        let kept = chunk.retain_rows(&survivors);
        let bound = kept.select_columns(&[VAR_SLOT]);
        out.extend(bound.to_rows());
    }
    if let Some(w) = want {
        out.truncate(w);
    }
    Row::share_schemas(&mut out);
    Ok(Some(out))
}

/// The compiled engagement: the scan's subject, the binding column's
/// name, the scratch property columns, and the filter's predicates —
/// everything the chain needs at run time, derived once here.
struct BatchPlan<'a> {
    /// The lone node pattern — the scan operator's subject (its label
    /// conjunction drives [`node_candidates`] and [`node_ok`]).
    node: &'a NodePattern,
    /// The variable the pattern binds — the binding column's name.
    var: String,
    /// The distinct properties the conjuncts read, first-mention order —
    /// one scratch column each, at slot `1 + index`.
    keys: Vec<NamedToken>,
    /// The compiled `WHERE` conjuncts, in evaluation order.
    preds: Vec<BatchPred>,
}

impl BatchPlan<'_> {
    /// How many admitted candidates the next chunk takes. A chunk is
    /// [`CHUNK_ROWS`] wide, narrowed to the rows still wanted when no
    /// predicate can drop one (every admitted candidate then survives, so
    /// a `LIMIT 1` reads one node, not a chunk of them).
    fn batch_size(&self, remaining_want: Option<usize>) -> usize {
        debug_assert!(
            remaining_want != Some(0),
            "the loop guard keeps a want unmet"
        );
        match remaining_want {
            Some(left) if self.preds.is_empty() => left.min(CHUNK_ROWS),
            _ => CHUNK_ROWS,
        }
    }
}

/// One `WHERE` conjunct in the filter operator's vocabulary. The slot is
/// the chunk column the predicate reads — the binding column is slot 0,
/// each key in [`BatchPlan::keys`] follows at `1 + index`.
enum BatchPred {
    /// `<var>.<key> <op> <bound>`: the column compared against a
    /// row-invariant bound. The bound is evaluated lazily, once per
    /// chunk, at this conjunct's turn — the timing the per-row
    /// short-circuit keeps (an earlier conjunct that empties the batch
    /// never pays for it).
    Cmp {
        slot: usize,
        op: BinaryOp,
        bound: Expr,
    },
    /// `<var>.<key> IS [NOT] NULL` — answered from the column's null
    /// bitmap alone; no value is decoded and nothing can error.
    Null { slot: usize, is_null: bool },
}

/// The engagement gate. Every condition is a proof, not a heuristic: the
/// driving table is exactly the one unbound seed row (the statement's
/// first clause, a `NEXT` stage's unit seed), the pattern is one node
/// with no traversal and no GPM-only feature, and every flattened `WHERE`
/// conjunct compiles. A mid-pipeline clause — bindings carried, multiple
/// driving rows, an `OPTIONAL`, a traversal — keeps today's path.
fn compile<'a>(
    rows: &[Row],
    optional: bool,
    match_mode: MatchMode,
    patterns: &'a [PathPattern],
    conjuncts: &[Expr],
) -> Option<BatchPlan<'a>> {
    if rows.len() != 1 || !rows[0].is_empty() {
        return None;
    }
    if optional || match_mode != MatchMode::DifferentRelationships {
        return None;
    }
    let [path] = patterns else {
        return None;
    };
    if path.needs_gpm()
        || path.is_shortest_path_function
        || path.has_path_name()
        || !path.rels.is_empty()
        || path.nodes.len() != 1
    {
        return None;
    }
    let node = &path.nodes[0];
    let var = node.variable.as_ref()?;
    if node.properties.is_some() || node.where_clause.is_some() || !label_index_seekable(node) {
        return None;
    }
    let mut plan = BatchPlan {
        node,
        var: var.to_string(),
        keys: Vec::new(),
        preds: Vec::new(),
    };
    for conjunct in conjuncts {
        plan.preds
            .push(compile_pred(conjunct, var, &mut plan.keys)?);
    }
    Some(plan)
}

/// Compile one conjunct, appending any new scratch key to `keys`. `None`
/// declines the whole clause: the per-row path evaluates shapes this
/// vocabulary does not cover (functions over the row, `OR`, `NOT`,
/// comparisons of two properties, …), and declining keeps those answers
/// byte-identical.
fn compile_pred(expr: &Expr, var: &SmallName, keys: &mut Vec<NamedToken>) -> Option<BatchPred> {
    /// The chunk slot a predicate's key reads: an already-seen key keeps
    /// its column, a new one claims the next scratch slot.
    fn slot_of(key: NamedToken, keys: &mut Vec<NamedToken>) -> usize {
        // Compare by written name: one token per name within a plan.
        match keys.iter().position(|(name, _)| *name == key.0) {
            Some(index) => 1 + index,
            None => {
                keys.push(key);
                keys.len()
            }
        }
    }
    match expr {
        Expr::Unary { op, expr } => {
            let is_null = match op {
                UnaryOp::IsNull => true,
                UnaryOp::IsNotNull => false,
                _ => return None,
            };
            let (v, key) = var_prop_named(expr)?;
            if v != var {
                return None;
            }
            Some(BatchPred::Null {
                slot: slot_of(key, keys),
                is_null,
            })
        }
        Expr::Binary { op, left, right } if is_pred_op(*op) => {
            // The property must be the scan variable's, on the left: a
            // mirrored comparison declines (conservative, like the fold's
            // operand-asymmetric declines).
            let (v, key) = var_prop_named(left)?;
            if v != var {
                return None;
            }
            // The bound is row-invariant — no variables, no
            // nondeterministic call, no scope-bearing variant — the one
            // canonical proof (kernels/program.rs), so the filter
            // evaluates it once per chunk instead of per row.
            if !super::kernels::program::bound_is_row_invariant(right) {
                return None;
            }
            Some(BatchPred::Cmp {
                slot: slot_of(key, keys),
                op: *op,
                bound: (**right).clone(),
            })
        }
        _ => None,
    }
}

/// The scan operator: one chunk over a batch of admitted candidates —
/// the node binding at [`VAR_SLOT`], one scratch column per key, each
/// read through the store's canonical property accessor (the same
/// `node_prop` read the per-row `eval` of `<var>.<key>` makes).
fn scan_chunk(graph: &ExecGraph<'_>, plan: &BatchPlan<'_>, batch: &[NodeId]) -> Chunk {
    let mut binding: Vec<Value> = Vec::with_capacity(batch.len());
    let mut scratch: Vec<Vec<Value>> = plan
        .keys
        .iter()
        .map(|_| Vec::with_capacity(batch.len()))
        .collect();
    for &id in batch {
        binding.push(crate::pattern_walk::bind_node_stub(graph, id));
        for (key, column) in plan.keys.iter().zip(&mut scratch) {
            column.push(graph.node_prop_resolved(id, key).unwrap_or(Value::Null));
        }
    }
    let mut names: Vec<String> = Vec::with_capacity(1 + plan.keys.len());
    names.push(plan.var.clone());
    names.extend(plan.keys.iter().map(|(name, _)| name.to_string()));
    let mut columns: Vec<Column> = Vec::with_capacity(names.len());
    columns.push(Column::from_values(&binding));
    for column in &scratch {
        columns.push(Column::from_values(column));
    }
    Chunk::from_columns(&names, columns)
}

/// The filter operator: the chunk's surviving rows, as a selection
/// vector in row order. Each conjunct walks its column once — the zone
/// check first (LABS-645), then the rows; a row the conjunct drops is
/// gone before the next conjunct runs.
fn filter_chunk(ctx: ExecCtx<'_>, plan: &BatchPlan<'_>, chunk: &Chunk) -> Result<Vec<usize>> {
    let ExecCtx { graph, params, .. } = ctx;
    // The bound evaluates against a row with no bindings — it is
    // variable-free by the gate, so the row it cannot read is inert; this
    // is the seed-row evaluation the per-row path makes against each
    // matched row, minus the row it never reads.
    let seed = Row::new();
    let mut survivors: Vec<usize> = (0..chunk.len()).collect();
    for pred in &plan.preds {
        if survivors.is_empty() {
            break;
        }
        match pred {
            BatchPred::Null { slot, is_null } => {
                survivors = apply_null(chunk.column(*slot), *is_null, survivors);
            }
            BatchPred::Cmp { slot, op, bound } => {
                let bound_value = eval(graph, &seed, bound, params)?;
                if bound_value.is_null() {
                    // A null bound makes every comparison null, and a
                    // `WHERE` drops null — every row goes, here, without
                    // evaluating anything else.
                    survivors.clear();
                    continue;
                }
                survivors = apply_cmp(chunk.column(*slot), *op, &bound_value, survivors)?;
            }
        }
    }
    Ok(survivors)
}

/// The zone skip's one decision (LABS-645): whether `op` against `bound`
/// provably cannot hold for any non-null value of a column whose zone is
/// `zone`. Every answer is an implication of the engine's own comparison
/// semantics — the zone's bounds are `Value`s of the column's kind, so
/// [`binary`] and [`Value::compare`] decide over them exactly as the row
/// loop decides over the column's values, and a typed column's single
/// kind makes comparability uniform across its slots:
///
/// - the ordering operators are monotone over the totally ordered kind,
///   so `<`/`<=` hold for some value iff they hold at the minimum,
///   `>`/`>=` at the maximum; an incomparable pair (cross-kind) is null
///   for every slot uniformly, and a null comparison drops;
/// - `=` holds for some value iff the bound lies within the bounds — the
///   kind is totally ordered, so a bound outside them equals no value,
///   and cross-kind equality is false, so it holds for no value either;
/// - `<>` holds for no value only when both bounds fail it, which pins
///   every value between them to the bound.
///
/// `Ok(false)` for the ops the zone cannot speak to (`IN`, the string
/// predicates, `=~`) and for every column without bounds (the fallback,
/// a NaN-bearing Float buffer, no non-null value) — no facts, no skip,
/// never an estimate.
fn zone_proves_no_match(op: BinaryOp, zone: &ColumnZone, bound: &Value) -> Result<bool> {
    let Some((min, max)) = zone.min_max() else {
        return Ok(false);
    };
    Ok(match op {
        BinaryOp::Lt | BinaryOp::Lte => {
            binary_pure(op, min.clone(), bound.clone())?.truthy() != Some(true)
        }
        BinaryOp::Gt | BinaryOp::Gte => {
            binary_pure(op, max.clone(), bound.clone())?.truthy() != Some(true)
        }
        BinaryOp::Eq => {
            !(min.compare(bound).is_some_and(|o| o != Ordering::Greater)
                && max.compare(bound).is_some_and(|o| o != Ordering::Less))
        }
        BinaryOp::Neq => {
            binary_pure(op, min.clone(), bound.clone())?.truthy() != Some(true)
                && binary_pure(op, max.clone(), bound.clone())?.truthy() != Some(true)
        }
        _ => false,
    })
}

/// One `Cmp` conjunct's pass over the survivor set (LABS-645: the zone
/// check first). The zone describes the WHOLE chunk's column — an
/// over-approximation once an earlier conjunct selected rows — so a skip
/// stays a proof and a non-skip falls through to the row loop.
fn apply_cmp(
    column: &Column,
    op: BinaryOp,
    bound: &Value,
    survivors: Vec<usize>,
) -> Result<Vec<usize>> {
    if zone_proves_no_match(op, column.zone(), bound)? {
        // The skip IS the correct result: the conjunct holds for no
        // non-null value in the column, so the survivor set is empty and
        // this conjunct's row walk for the chunk never runs.
        super::count_zone_skip();
        return Ok(Vec::new());
    }
    let mut kept = Vec::with_capacity(survivors.len());
    for row in survivors {
        // The null bitmap first: a null property makes the comparison
        // null (three-valued), which the `WHERE` drops — the shortcut is
        // `binary`'s own null arm.
        if column.is_null(row) {
            continue;
        }
        let value = column.value_at(row);
        if binary_pure(op, value, bound.clone())?.truthy() == Some(true) {
            kept.push(row);
        }
    }
    Ok(kept)
}

/// One `IS [NOT] NULL` conjunct's pass over the survivor set (LABS-645:
/// the zone's null-presence fact answers the `IS NULL` direction — no
/// null slot anywhere in the column means no row can satisfy it).
fn apply_null(column: &Column, is_null: bool, survivors: Vec<usize>) -> Vec<usize> {
    if is_null && !column.zone().null_present() {
        // See `apply_cmp`: the provable no-match skip is the correct
        // empty result.
        super::count_zone_skip();
        return Vec::new();
    }
    survivors
        .into_iter()
        .filter(|&row| column.is_null(row) == is_null)
        .collect()
}

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    /// LABS-359 test hook: force the decline, so a differential test can
    /// pin the per-row path's answer on a shape the chain would take.
    static BATCH_DISABLED: Cell<bool> = const { Cell::new(false) };
    /// LABS-359 test counter: how many clauses the chain took since the
    /// last take.
    static BATCH_ENGAGED: Cell<u32> = const { Cell::new(0) };
    /// LABS-1452 test hook: stand the plain cap's pushdown down, so the
    /// chain runs unbounded and a differential can pin the pushed run
    /// against it.
    static PUSHDOWN_DISABLED: Cell<bool> = const { Cell::new(false) };
    /// LABS-1452 test counter: how many admitted nodes the chain scanned
    /// into chunks since the last take.
    static BATCH_SCANNED: Cell<usize> = const { Cell::new(0) };
}

/// Whether the test hook stood the pushdown down.
#[cfg(test)]
fn pushdown_disabled() -> bool {
    PUSHDOWN_DISABLED.with(Cell::get)
}

/// Count `n` scanned nodes.
#[cfg(test)]
fn batch_scanned(n: usize) {
    BATCH_SCANNED.with(|c| c.set(c.get() + n));
}

/// LABS-1452 test helper: run `f` with the cap pushdown off (the chain
/// still engages, unbounded), restoring the flag on the way out.
#[cfg(test)]
pub(crate) fn with_pushdown_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::test_support::with_flag_disabled(&PUSHDOWN_DISABLED, f)
}

/// LABS-1452 test helper: drain the scanned-node counter.
#[cfg(test)]
pub(crate) fn take_batch_scanned() -> usize {
    BATCH_SCANNED.with(|c| c.replace(0))
}

/// Whether the test hook forced the per-row path.
#[cfg(test)]
fn batch_disabled() -> bool {
    BATCH_DISABLED.with(Cell::get)
}

/// Count one engagement.
#[cfg(test)]
fn batch_engaged() {
    BATCH_ENGAGED.with(|c| c.set(c.get() + 1));
}

/// LABS-359 test helper: run `f` with the batch chain forced off (the
/// per-row path), restoring the flag afterwards — including on unwind,
/// so a panicking assertion inside the closure cannot leave the chain
/// disabled for every later test on the thread (the shared guard).
#[cfg(test)]
pub(crate) fn with_batch_disabled<R>(f: impl FnOnce() -> R) -> R {
    super::test_support::with_flag_disabled(&BATCH_DISABLED, f)
}

/// LABS-359 test helper: drain the engagement counter (0 = the per-row
/// path took every scan-shaped clause since the last take).
#[cfg(test)]
pub(crate) fn take_batch_engagements() -> u32 {
    BATCH_ENGAGED.with(|c| c.replace(0))
}

/// LABS-645's generated-batch contracts. The filter's conjunct step is
/// exercised directly over generated columns — the private helpers the
/// `filter_chunk` orchestration reads — because the differential's
/// ground truth is the per-row evaluation itself (`binary` over every
/// slot), which no query-level fixture can state row by row.
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::*;
    use crate::test_support::config_with_floor;

    /// The comparison vocabulary the zone can decide (LABS-645): the six
    /// operators over the typed kinds.
    fn pred_op() -> BoxedStrategy<BinaryOp> {
        prop_oneof![
            Just(BinaryOp::Eq),
            Just(BinaryOp::Neq),
            Just(BinaryOp::Lt),
            Just(BinaryOp::Gt),
            Just(BinaryOp::Lte),
            Just(BinaryOp::Gte),
        ]
        .boxed()
    }

    /// A scalar bound from anywhere — matching kinds and mismatching
    /// ones (the cross-kind rules), NaN included (the bound side's own
    /// dead end: `binary` answers false or null for it, never true).
    fn bound() -> BoxedStrategy<Value> {
        prop_oneof![
            4 => any::<i64>().prop_map(Value::Int),
            3 => prop_oneof![
                7 => any::<f64>().prop_filter("finite", |f| f.is_finite()),
                1 => Just(f64::NAN),
            ]
            .prop_map(Value::Float),
            3 => "[a-zäöü]{0,6}".prop_map(|s| Value::String(Arc::from(s))),
            2 => any::<bool>().prop_map(Value::Bool),
        ]
        .boxed()
    }

    /// One generated typed column — a single kind's values with nulls
    /// weighted, so the one encoder claims a typed buffer (the zone's
    /// subject). Floats carry NaN sometimes: the bounds-voiding rule
    /// must be generated, not just pinned.
    fn typed_column() -> BoxedStrategy<Vec<Value>> {
        prop_oneof![
            4 => vec(
                prop_oneof![
                    1 => Just(Value::Null),
                    6 => any::<i64>().prop_map(Value::Int),
                ],
                0..=48,
            ),
            3 => vec(
                prop_oneof![
                    1 => Just(Value::Null),
                    6 => prop_oneof![
                        7 => any::<f64>().prop_filter("finite", |f| f.is_finite()),
                        1 => Just(f64::NAN),
                    ]
                    .prop_map(Value::Float),
                ],
                0..=48,
            ),
            3 => vec(
                prop_oneof![
                    1 => Just(Value::Null),
                    6 => "[a-zäöü]{0,6}".prop_map(|s| Value::String(Arc::from(s))),
                ],
                0..=48,
            ),
            2 => vec(
                prop_oneof![
                    1 => Just(Value::Null),
                    6 => any::<bool>().prop_map(Value::Bool),
                ],
                0..=48,
            ),
        ]
        .boxed()
    }

    /// The ground truth: the full row-by-row filter — every non-null
    /// slot through the shared `binary`, exactly what the per-row path
    /// evaluates. The comparison operators cannot error (their `binary`
    /// arms return values), so the expect states an invariant.
    fn full_filter(values: &[Value], op: BinaryOp, bound: &Value) -> Vec<usize> {
        values
            .iter()
            .enumerate()
            .filter(|(_, value)| {
                !(*value).is_null()
                    && binary_pure(op, (*value).clone(), bound.clone())
                        .expect("comparison operators cannot error")
                        .truthy()
                        == Some(true)
            })
            .map(|(row, _)| row)
            .collect()
    }

    proptest! {
        #![proptest_config(config_with_floor(1000))]

        /// The no-false-negative contract, 1000+ random batches: random
        /// data of every typed kind, random nulls, random comparison
        /// predicates over matching and mismatching bounds — the
        /// zone-skipped filter's output equals the full filter's,
        /// exactly.
        #[test]
        fn zone_skipped_filtering_equals_full_filtering(
            (values, op, bound) in (typed_column(), pred_op(), bound()),
        ) {
            let column = Column::from_values(&values);
            let zoned = apply_cmp(&column, op, &bound, (0..values.len()).collect())
                .expect("comparison operators cannot error");
            prop_assert_eq!(zoned, full_filter(&values, op, &bound));
        }
    }

    proptest! {
        #![proptest_config(config_with_floor(1000))]

        /// The null-presence fact's contract, 1000+ random batches:
        /// `IS NULL` over a column with no null slot provably empties
        /// the set, and every other case is the bitmap's own answer —
        /// equality with the full per-row filter.
        #[test]
        fn zone_null_skips_equal_full_filtering(
            (values, is_null) in (typed_column(), any::<bool>()),
        ) {
            let column = Column::from_values(&values);
            let zoned = apply_null(&column, is_null, (0..values.len()).collect());
            let full: Vec<usize> = values
                .iter()
                .enumerate()
                .filter(|(_, value)| value.is_null() == is_null)
                .map(|(row, _)| row)
                .collect();
            prop_assert_eq!(zoned, full);
        }
    }
}
