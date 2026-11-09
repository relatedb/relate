//! The group path: [`aggregate_ex`] builds the groups (explicit `GROUP BY`
//! keys or the implicit non-aggregating projection items), folds each one,
//! and parks its `ORDER BY` keys and HAVING predicate — plus the `*`
//! key desugaring both paths share.

use std::borrow::Cow;
use std::collections::BTreeMap;

use indexmap::map::Entry;
use indexmap::IndexMap;
use relate_core::{Result, Value};
use relate_ir::{Expr, Projection};
use relate_names::SmallName;

use crate::eval::{eval, is_agg_expr};
use crate::group_key::GroupKey;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::order::{park_order_keys, OrderKeys};
use super::super::scope::{has_star_item, is_internal_column, is_star_item, visible_keys};
use super::super::unwind::RowFeed;
use super::eval::{agg_eval, AggScope};
use super::stream::{AggSites, GroupAccs};

/// The projection's per-group subclause state, bundled to keep
/// [`aggregate_ex`] under the argument-count lint.
pub(crate) struct GroupSubclauses<'a> {
    /// The projection's `ORDER BY` key list with its per-key alias-scope
    /// decision. The group is the only place a key's aggregate calls can be
    /// answered, so each output row parks them through [`park_order_keys`]
    /// for the sort to read back.
    pub order: &'a OrderKeys<'a>,
    /// The projection's WHERE predicate when it contains aggregate calls
    /// (ADR-0453): HAVING semantics — evaluated once per group through
    /// [`agg_eval`] (aggregate calls fold the group, non-aggregate leaves
    /// read the projected row), a group whose predicate is not true is
    /// dropped; false and null drop, a non-boolean value is the typed
    /// rejection live raises (22N01) rather than a silent drop, and a
    /// predicate evaluation error aborts the statement — the same
    /// convert-or-raise policy the ordinary WHERE positions read
    /// (ADR-0458 §7, [`crate::eval::predicate_value_holds`]). `None` for
    /// every other shape.
    pub having: Option<&'a Expr>,
    /// ADR-0453: an importing subquery's scope row — the seed the phantom
    /// keyless group over an emptied input uses, so a non-aggregate leaf
    /// naming an import (`sum(y) + tax` with no rows left) reads the
    /// import's value instead of null. `None` at every other scope.
    pub import_scope: Option<&'a Row>,
}

/// One group's state: its first row — the seed every non-aggregate leaf
/// reads — and its aggregate accumulators. Never the group's rows.
struct GroupState<'e> {
    /// `None` only for the keyless group seeded before any row arrives (the
    /// ADR-0453 phantom group of an empty input).
    seed: Option<Row>,
    accs: GroupAccs<'e>,
}

/// CIP-236: `group_by` carries explicit grouping keys when a GROUP BY
/// clause is present. That is the *only* difference from the implicit
/// form: the keys come from those expressions instead of being inferred
/// from the non-aggregating projection items. `Some(&[])` is the empty
/// grouping set (`GROUP BY ()`): one global group.
pub(crate) fn aggregate_ex(
    graph: &ExecGraph<'_>,
    rows: RowFeed<'_>,
    items: &[Projection],
    group_by: Option<&[Expr]>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    subclauses: &GroupSubclauses<'_>,
) -> Result<Vec<Row>> {
    let GroupSubclauses {
        order,
        having,
        import_scope,
    } = *subclauses;
    // The grouping strategy is loop-invariant: decide once whether the key
    // comes from the explicit `group_by` expressions or from the inferred
    // non-aggregating projection items, then loop over rows.
    let key_exprs = desugar_star_keys(
        match group_by {
            Some(keys) => keys.iter().map(Cow::Borrowed).collect(),
            None => items
                .iter()
                .filter(|p| !is_agg_expr(&p.expr))
                .map(|p| Cow::Borrowed(&p.expr))
                .collect(),
        },
        rows.probe(),
    );
    // One row for an empty input only when there are no grouping keys at
    // all: implicit grouping over an all-aggregate projection, an explicit
    // empty grouping set (`GROUP BY ()`), or an explicit empty `ALL` (which
    // lowers to no keys). An explicit non-empty key set has no group to emit
    // for an empty input, so it yields zero rows -- emitting the phantom
    // global group there would invent a row with no key values. A `*` item
    // counts as a key: it carries every in-scope variable (ADR-0198).
    let keyless = key_exprs.is_empty();
    // ADR-0198 ruling 1: with aggregation in play, a `*` item groups by all
    // of the row's carried variables and projects them through to the
    // output row alongside the aggregate aliases.
    let star = has_star_item(items);
    // The aggregate calls the projection evaluates — items, ORDER BY keys,
    // HAVING — found once, so every group streams its input through
    // constant-size accumulators instead of retaining its rows.
    let sites = AggSites::discover(
        graph,
        items
            .iter()
            .filter(|p| !is_star_item(p) && is_agg_expr(&p.expr))
            .map(|p| &p.expr),
        order.exprs().chain(having),
        params,
        cypher_version,
    )?;
    let mut groups: IndexMap<Vec<GroupKey>, GroupState<'_>> = IndexMap::new();
    let mut groups_charge = relate_core::tokens::WorkspaceCharge::empty();
    let account = relate_core::tokens::workspace_accounting();
    let mut meter = crate::workspace::GroupMapMeter::default();
    // The input vector stays allocated until this loop finishes. Bill it up
    // front, then hand each row's payload to the group map as it moves.
    // A generated feed holds no input vector: nothing to bill, nothing to
    // release row by row.
    let mut retained = match &rows {
        RowFeed::Held(held) if account => Some(crate::workspace::RetainedRows::measure(held)),
        _ => None,
    };
    let mut input_charge = match &retained {
        Some(retained) => {
            let charge = relate_core::tokens::WorkspaceCharge::retain(retained.bytes())?;
            relate_core::tokens::peak_hold("aggregation");
            charge
        }
        None => relate_core::tokens::WorkspaceCharge::empty(),
    };
    // ADR-0463 (LABS-781): the group map is one retaining container. Each
    // landed group — its key, its seed row and its accumulators — grows a
    // running total enforced at the per-value cap, so a high-cardinality
    // key set refuses at ~the cap while the groups land. A row that joins
    // an existing group adds nothing: the group keeps constant-size
    // accumulators, never its rows, so the state is independent of how many
    // rows feed a group. (A retaining aggregate — `collect`, a percentile, a
    // DISTINCT call — charges its operands to its own container.)
    let mut group_retained = graph.retain_bytes(0)?;
    // A keyless aggregation (implicit grouping over an all-aggregate
    // projection, `GROUP BY ()`, an explicit empty `ALL`) has exactly one
    // group whose key is the empty set: seed it here, through the same
    // charge-and-meter sequence a row-loop landing pays, and every input
    // row lands in it — the empty input keeps the seed as its phantom group
    // (the ADR-0453 import scope's stand-in) and no post-loop insertion is
    // needed.
    if keyless {
        group_retained.add_group_entry(graph, 0, &[], 0)?;
        let accs = sites.open_group();
        group_retained.add_bytes(graph, accs.footprint())?;
        groups.insert(Vec::new(), GroupState { seed: None, accs });
        if account {
            meter.add_empty_group();
            charge_group_map(&mut groups_charge, &meter)?;
        }
    }
    for row in rows {
        let row = row?;
        graph.cancel_tick()?;
        // Star-free after the desugar above: one `eval` per key, and the
        // capacity is exact.
        let mut key = Vec::with_capacity(key_exprs.len());
        for k in &key_exprs {
            key.push(GroupKey::new(&eval(
                graph,
                &row,
                k,
                params,
                cypher_version,
            )?));
        }
        if let Some(retained) = retained.as_mut() {
            retained.release_row(&row);
            input_charge.resize(retained.bytes())?;
        }
        let map_slots = groups.len();
        match groups.entry(key) {
            Entry::Occupied(mut entry) => {
                let index = entry.index();
                let state = entry.get_mut();
                // Only the keyless group's first row lands as a seed; it is
                // charged while it still solely owns its payload, ahead of
                // the offer's clones. Every other row is spent once the
                // accumulators have it — dropped before the settle, so what
                // a site kept from it is solely the group's and is charged
                // in full.
                let lands_as_seed = state.seed.is_none();
                if lands_as_seed {
                    if account {
                        meter.add_row(index, &row);
                    }
                    group_retained.add_row(graph, row.values())?;
                }
                state.accs.offer(graph, &row, params, cypher_version)?;
                if lands_as_seed {
                    state.seed = Some(row);
                } else {
                    drop(row);
                }
                state.accs.settle(graph, &mut group_retained)?;
            }
            Entry::Vacant(entry) => {
                if account {
                    meter.add_group(entry.key(), &row);
                }
                // The map entry the new group lands in: the slot's buffer
                // growth plus the key's slots and owned payload — the
                // meter's own figures for the same landing. A `GROUP BY`
                // over a constructed key expression retains fresh key
                // bytes per group; the running total is what must see
                // them.
                group_retained.add_bytes(
                    graph,
                    crate::workspace::group_entry_insert_delta(map_slots, entry.key()),
                )?;
                // The seed row's landing charge. The retained key keeps its
                // `Arc` clone of the row's key payload alive, so the row's
                // own payload adds nothing here and the key charge above
                // counted it once — the exact figure the meter's
                // `add_group` computes for the same landing.
                group_retained.add_row(graph, row.values())?;
                let mut accs = sites.open_group();
                group_retained.add_bytes(graph, accs.footprint())?;
                accs.offer(graph, &row, params, cypher_version)?;
                // The row stays as the seed and holds its share of what the
                // sites kept; that payload was charged once with the seed.
                accs.settle(graph, &mut group_retained)?;
                entry.insert(GroupState {
                    seed: Some(row),
                    accs,
                });
            }
        }
        if account {
            charge_group_map(&mut groups_charge, &meter)?;
        }
    }
    drop(input_charge);
    let mut out = Vec::new();
    // ADR-0463 (LABS-781): the aggregating projection's output
    // container — each emitted group row charges as it lands, ahead of
    // the landing, in its final shape (seed bindings, projected
    // values, aggregate results, parked order keys).
    let mut out_retained = graph.retain_bytes(0)?;
    for (_, state) in groups {
        // The group's own first row is the seed every non-aggregate leaf
        // reads; the phantom keyless group over an emptied input has none,
        // so an importing subquery's scope row stands in (ADR-0453) — the
        // imports are per-row constants, present even when the body's rows
        // are gone.
        let GroupState { seed, accs } = state;
        let seed_row = seed.as_ref().or(import_scope);
        let mut n = seed_row.cloned().unwrap_or_default();
        let folded = accs.finish(graph, seed.as_ref(), params, cypher_version);
        if star {
            // The variables `*` carries are already in the first-row seed;
            // keep the internal `__*` names out of the projected row, the
            // same filter the non-aggregating `*` path applies.
            n.retain(|name, _| !is_internal_column(name));
        }
        for (i, p) in items.iter().enumerate() {
            if is_star_item(p) {
                continue;
            }
            let name = p.output_name(i);
            let val = if is_agg_expr(&p.expr) {
                agg_eval(
                    graph,
                    AggScope::of_group(&folded, seed_row),
                    &p.expr,
                    params,
                    cypher_version,
                )?
            } else if let Some(row) = seed_row {
                eval(graph, row, &p.expr, params, cypher_version)?
            } else {
                Value::Null
            };
            n.insert(name, val);
        }
        // Park the `ORDER BY` keys beside the projected columns, through the
        // same decision and writer every other projection path uses. A key
        // reads `n`, the projected row, so an aggregate call in it reads this
        // group's resolved value and the deprecated implicit-grouping shapes
        // still answer from their item; a key that cannot be evaluated
        // orders as null rather than failing the statement — the documented
        // `ORDER BY` policy.
        park_order_keys(order, &mut n, |expr, scope| {
            agg_eval(
                graph,
                AggScope::of_row(&folded, scope),
                expr,
                params,
                cypher_version,
            )
        });
        // HAVING (ADR-0453): the aggregating WHERE, answered per group with
        // the same scope an ORDER BY key gets. Not-true drops the group —
        // the retain's verdict for a failing row; a non-boolean is the
        // typed rejection live raises (22N01) and a list/path converts,
        // both through the one predicate policy (ADR-0458 §7).
        if let Some(predicate) = having {
            let keeps = agg_eval(
                graph,
                AggScope::of_row(&folded, &n),
                predicate,
                params,
                cypher_version,
            )?;
            if !crate::eval::predicate_value_holds(&keeps)? {
                continue;
            }
        }
        // The resolved aggregate values are shared with `n` until the folded
        // group lets go; drop it so the output row's landing charge sees
        // the payloads it solely owns.
        drop(folded);
        out_retained.add_row(graph, n.values())?;
        out.push(n);
    }
    drop(groups_charge);
    Ok(out)
}

/// Charge the group map's growth after an entry insert, then sample the
/// peak hold. The one spelling of that pair: the row loop and the keyless
/// empty-group seed both bill the map through it, and both derive the
/// charge from `meter.bytes` (already updated by `add_row`/`add_group`).
fn charge_group_map(
    charge: &mut relate_core::tokens::WorkspaceCharge,
    meter: &crate::workspace::GroupMapMeter,
) -> Result<()> {
    charge.resize(meter.bytes)?;
    relate_core::tokens::peak_hold("aggregation");
    Ok(())
}

/// ADR-0198 ruling 1: desugar a `*` key ONCE, before the row loop, into
/// one `Expr::Variable` key per visible column of the FIRST row, sorted
/// by name so the key does not depend on column insertion order (the
/// same expansion the non-aggregating projection path does). Pipeline
/// rows carry a uniform scope per clause, so every row in one
/// aggregation input has the same visible column set -- the positional
/// key vector already relies on that homogeneity. Expanding here keeps
/// the hot loop on the plain `eval` path: no per-key Star arm, no
/// per-row collect+sort. Empty input has no first row to expand from;
/// the unexpanded `*` then only keeps `keyless` false (a star key means
/// keys exist), and the row loop never runs.
pub(crate) fn desugar_star_keys<'a>(
    key_exprs: Vec<Cow<'a, Expr>>,
    rows: &[Row],
) -> Vec<Cow<'a, Expr>> {
    let Some(first) = rows.first() else {
        return key_exprs;
    };
    if !key_exprs.iter().any(|k| matches!(&**k, Expr::Star)) {
        return key_exprs;
    }
    let mut carried = visible_keys(first);
    carried.sort();
    // Row keys (String) enter the IR's name domain at construction.
    let star_keys: Vec<Expr> = carried
        .into_iter()
        .map(|name| Expr::Variable(SmallName::from_name(&name)))
        .collect();
    let mut expanded = Vec::with_capacity(key_exprs.len() + star_keys.len());
    for k in key_exprs {
        if matches!(&*k, Expr::Star) {
            expanded.extend(star_keys.iter().cloned().map(Cow::Owned));
        } else {
            expanded.push(k);
        }
    }
    expanded
}
