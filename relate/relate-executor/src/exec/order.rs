//! The ORDER BY family of the projection: the parked-key model
//! ([`OrderKeys`], [`park_order_keys`], [`unpark_order_keys`]), the sort
//! entry point and its top-k selection, the paging stages, and the
//! SKIP/LIMIT bound resolution — split from `project.rs` (LABS-781's
//! stellar round: the order family is the projection's sibling concern,
//! not a section of it).

use std::cmp::Ordering as CmpOrdering;
use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, Projection};

use crate::eval::{eval, is_agg_expr};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::scope::is_star_item;
use super::scope::{projection_names, projects_name, result_column, ProjectionFields};

/// Whether an `ORDER BY` key must read the projected row even when its
/// expression is verbatim a projection item.
///
/// Every key is evaluated against the projected row (see
/// [`park_order_keys`]); this decision names the shapes that may not fall
/// back to the item they match: a bare variable naming a projected column is
/// a reference to that column, a key with an aggregate call folds the group,
/// and an explicit `GROUP BY` (including `GROUP BY ALL`) establishes the
/// alias scope for every key (`GroupByAcceptance [73]`). Every other key
/// keeps the deprecated implicit-grouping resolution — one that is exactly a
/// projection item expression is answered from that item
/// (`ProjectionSubclauseDeprecation`).
fn order_key_alias_scoped(expr: &Expr, items: &[Projection], explicit_grouping: bool) -> bool {
    explicit_grouping
        || is_agg_expr(expr)
        || matches!(expr, Expr::Variable(name) if projects_name(items, name))
}

/// A `__` column-name prefix no result column starts with, so a parked sort
/// key can never overwrite — or be read back as — a user alias. The `__`
/// namespace is where internal columns live, and a user alias is allowed to
/// spell one (`AS __ord0`).
fn parked_prefix(items: &[Projection]) -> String {
    let names = projection_names(items);
    let mut prefix = String::from("__ord");
    while names.iter().any(|name| name.starts_with(&prefix)) {
        prefix.push('_');
    }
    prefix
}

/// One projection's `ORDER BY` keys, each with its resolved scope decision
/// and the parked column it is written under. Every projection path parks its
/// keys at projection time through [`park_order_keys`], so the one sort entry
/// point reads them back instead of re-evaluating anything.
pub(crate) struct OrderKeys<'a> {
    plan: Vec<KeyPlan<'a>>,
}

/// One key's row-independent plan: the key itself, the projected column it
/// may answer from, and the parked column its value is written under — one
/// entry per key, never index-aligned companions.
struct KeyPlan<'a> {
    key: &'a relate_ir::OrderItem,
    /// The projected column a verbatim, non-alias-scoped key answers from;
    /// `None` means the key is evaluated. Resolved once here, never per row.
    verbatim: Option<String>,
    parked: String,
}

impl<'a> OrderKeys<'a> {
    /// Every key's plan, verbatim resolution included — for callers that
    /// evaluate keys against a projected row. The per-key scope depends only
    /// on the key expressions and the items, never on a row, so the decision
    /// is taken once here.
    pub(crate) fn new(
        keys: &'a [relate_ir::OrderItem],
        items: &'a [Projection],
        explicit_grouping: bool,
    ) -> Self {
        let mut this = Self::parked_only(keys, items);
        for (index, key) in keys.iter().enumerate() {
            if !order_key_alias_scoped(&key.expr, items, explicit_grouping) {
                this.plan[index].verbatim = verbatim_column(items, &key.expr);
            }
        }
        this
    }

    /// Parked names only, no verbatim table: the folded-aggregate path
    /// resolves each key's value statically (an aggregate item's expression
    /// cannot be re-evaluated on the folded row), so it must never consult
    /// the per-row resolver.
    pub(crate) fn parked_only(
        keys: impl IntoIterator<Item = &'a relate_ir::OrderItem>,
        items: &'a [Projection],
    ) -> Self {
        let parked_prefix = parked_prefix(items);
        Self {
            plan: keys
                .into_iter()
                .enumerate()
                .map(|(index, key)| KeyPlan {
                    key,
                    verbatim: None,
                    parked: format!("{parked_prefix}{index}"),
                })
                .collect(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.plan.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.plan.is_empty()
    }

    /// Every key expression a [`park_order_keys`] closure can be handed —
    /// the discovery pass reads the aggregate calls out of exactly these.
    pub(crate) fn exprs(&self) -> impl Iterator<Item = &'a Expr> + '_ {
        self.plan.iter().map(|planned| &planned.key.expr)
    }

    fn iter(&self) -> impl Iterator<Item = &relate_ir::OrderItem> {
        self.plan.iter().map(|planned| planned.key)
    }

    /// The parked column of key `index`.
    pub(crate) fn name(&self, index: usize) -> &str {
        &self.plan[index].parked
    }

    /// Key `index`'s value against `scope`: the verbatim item's column, or
    /// the evaluated expression. The one key policy [`park_order_keys`] and
    /// [`parked_values`] share. A verbatim key's column is a projection
    /// output the scope row carries by contract, so a miss is a bug, not a
    /// reason to sort on a re-evaluated value; an expression that cannot be
    /// evaluated is a null sort key, not a statement failure.
    fn value<F>(&self, index: usize, scope: &Row, key_value: &F) -> Value
    where
        F: Fn(&Expr, &Row) -> Result<Value>,
    {
        if let Some(name) = &self.plan[index].verbatim {
            return result_column(scope, name).clone();
        }
        key_value(&self.plan[index].key.expr, scope).unwrap_or(Value::Null)
    }
}

/// Park every `ORDER BY` key of `keys` on `projected`, the row the
/// projection produced, so the later sort reads the values back.
///
/// The projected row is the one evaluation scope: every key resolves names
/// there, so a name the projection re-binds denotes the projected value. The
/// one short-circuit is the deprecated implicit-grouping resolution — a key
/// that is not alias-scoped and whose expression is exactly one of the items
/// verbatim is answered from that item, whose value was computed against the
/// incoming row (`ProjectionSubclauseDeprecation`: `ORDER BY a.p` beside
/// `a.p AS k` keeps the incoming `a.p`, while `ORDER BY n + 2` beside
/// `n.num AS n` reads the projected `n`, as the TCK pins).
///
/// `key_value` evaluates one key against the projected row; a key it cannot
/// evaluate becomes a null sort key ([`OrderKeys::value`]).
pub(crate) fn park_order_keys(
    keys: &OrderKeys<'_>,
    projected: &mut Row,
    key_value: impl Fn(&Expr, &Row) -> Result<Value>,
) {
    let parked = parked_values(keys, projected, key_value);
    park_values(keys, projected, parked);
}

/// Every key's parked value against `scope`, computed together and returned
/// in key order — the value half of [`park_order_keys`], for a caller that
/// builds the result row's values directly instead of parking onto a scratch
/// row. Evaluating all keys before any is written keeps a later key from
/// reading a parked column an earlier one wrote.
pub(crate) fn parked_values(
    keys: &OrderKeys<'_>,
    scope: &Row,
    key_value: impl Fn(&Expr, &Row) -> Result<Value>,
) -> Vec<Value> {
    (0..keys.len())
        .map(|index| keys.value(index, scope, &key_value))
        .collect()
}

/// Write each key's resolved value under its parked column — the one write
/// half of [`park_order_keys`], shared with the folded-aggregate path that
/// resolves its values statically.
pub(crate) fn park_values(keys: &OrderKeys<'_>, row: &mut Row, parked: Vec<Value>) {
    for (index, value) in parked.into_iter().enumerate() {
        row.insert(keys.name(index).to_string(), value);
    }
}

/// Remove the parked `ORDER BY` columns from every row once the sort has
/// consumed them — the one strip loop, shared by every projection path.
pub(crate) fn unpark_order_keys(keys: &OrderKeys<'_>, rows: &mut [Row]) {
    for row in rows {
        for index in 0..keys.len() {
            row.shift_remove(keys.name(index));
        }
    }
}

/// The projected column a key whose expression is exactly `expr` answers
/// from, if any — the documented verbatim resolution for a non-alias-scoped
/// key.
fn verbatim_column(items: &[Projection], expr: &Expr) -> Option<String> {
    items.iter().enumerate().find_map(|(index, item)| {
        (!is_star_item(item) && item.expr == *expr).then(|| item.output_name(index))
    })
}

/// The projection item each folded `ORDER BY` key answers from, or `None`
/// when a key needs more than the folded row. The fold keeps no input rows,
/// so a key is answerable only when it is one of the projection's items
/// verbatim (an aggregate item's folded value included) or a bare variable
/// naming a projected column; any other key declines the fold and the
/// grouping path answers it.
pub(crate) fn folded_order_items(
    order: &[relate_ir::OrderItem],
    items: &[Projection],
) -> Option<Vec<usize>> {
    order
        .iter()
        .map(|key| {
            items.iter().enumerate().position(|(index, item)| {
                !is_star_item(item)
                    && (item.expr == key.expr
                        || matches!(&key.expr, Expr::Variable(name)
                    if name == item.output_name(index).as_str()))
            })
        })
        .collect()
}

fn cmp_sort_keys(
    keys: &OrderKeys<'_>,
    ka: &[Value],
    ia: usize,
    kb: &[Value],
    ib: usize,
) -> CmpOrdering {
    for (i, o) in keys.iter().enumerate() {
        match ka[i].order_by_cmp(&kb[i]) {
            CmpOrdering::Equal => {}
            ord => return if o.ascending { ord } else { ord.reverse() },
        }
    }
    ia.cmp(&ib)
}

/// Apply a catalog clause's own `YIELD` paging stages (`ORDER BY`/`SKIP`/
/// `LIMIT`), in written order, to the composed rows. The stage interpreter
/// is the same one ordinary projections use for their paging sub-grammar;
/// it lives here beside those helpers so `project_catalog` stays lookup →
/// prefill → project → filter.
pub(crate) fn apply_yield_page_stages(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    stages: &[relate_parser::PageStage],
    output: &mut Vec<Row>,
) -> Result<()> {
    for stage in stages {
        match stage {
            relate_parser::PageStage::OrderBy(items) => {
                let order = items
                    .iter()
                    .map(|item| {
                        Ok(relate_ir::OrderItem {
                            expr: relate_front::lower_expr(&item.expr)?,
                            ascending: !item.descending,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                // The catalog rows are their own scope: a `YIELD` key names
                // an output column, so parking reads the row itself.
                let keys = OrderKeys::new(&order, &[], false);
                for row in output.iter_mut() {
                    park_order_keys(&keys, row, |expr, scope| {
                        eval(graph, scope, expr, params, cypher_version)
                    });
                }
                sort_rows_by_parked_keys(&keys, output, None)?;
                unpark_order_keys(&keys, output);
            }
            relate_parser::PageStage::Skip(expr) => {
                let count = skip_limit_count(&eval(
                    graph,
                    &Row::new(),
                    &relate_front::lower_expr(expr)?,
                    params,
                    cypher_version,
                )?)?;
                *output = std::mem::take(output).into_iter().skip(count).collect();
            }
            relate_parser::PageStage::Limit(expr) => {
                let count = skip_limit_count(&eval(
                    graph,
                    &Row::new(),
                    &relate_front::lower_expr(expr)?,
                    params,
                    cypher_version,
                )?)?;
                output.truncate(count);
            }
        }
    }
    Ok(())
}

/// Reserve the row buffer, then sample RSS while those rows are still the
/// live workspace and before the key vector is built.
fn hold_sort_rows(rows: &[Row]) -> Result<relate_core::tokens::WorkspaceCharge> {
    let charge = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        crate::workspace::row_buffer_bytes(rows)
    })?;
    relate_core::tokens::peak_hold("sort");
    Ok(charge)
}

fn hold_sort_keys<'a>(
    keys: impl IntoIterator<Item = &'a [Value]>,
) -> Result<relate_core::tokens::WorkspaceCharge> {
    let charge = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        crate::workspace::sort_key_columns(keys)
    })?;
    relate_core::tokens::peak_hold("sort");
    Ok(charge)
}

/// The parked sort keys of one row, in key order. Every projection path
/// parks them by contract ([`result_column`] asserts it).
fn parked_keys(keys: &OrderKeys<'_>, row: &Row) -> Vec<Value> {
    (0..keys.len())
        .map(|index| result_column(row, keys.name(index)).clone())
        .collect()
}

/// The one `ORDER BY` application. Every projection path parks its keys at
/// projection time (see [`park_order_keys`]), so this reads them back and
/// never evaluates an expression. `top_k` asks for the first `top_k` rows
/// only (the `LIMIT` selection; `0` empties the table).
pub(crate) fn sort_rows_by_parked_keys(
    keys: &OrderKeys<'_>,
    rows: &mut Vec<Row>,
    top_k: Option<usize>,
) -> Result<()> {
    match top_k {
        Some(0) => {
            rows.clear();
            Ok(())
        }
        Some(keep) if keep < rows.len() => {
            *rows = take_parked_top_k(keys, std::mem::take(rows), keep)?;
            Ok(())
        }
        _ => sort_all_rows(keys, rows),
    }
}

/// The full sort behind [`sort_rows_by_parked_keys`]'s unsorted case.
fn sort_all_rows(keys: &OrderKeys<'_>, rows: &mut [Row]) -> Result<()> {
    let _rows = hold_sort_rows(rows)?;
    let mut keyed: Vec<(Vec<Value>, usize)> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (parked_keys(keys, row), index))
        .collect();
    let _keys = hold_sort_keys(keyed.iter().map(|(keys, _)| keys.as_slice()))?;
    keyed.sort_by(|a, b| cmp_sort_keys(keys, &a.0, a.1, &b.0, b.1));
    let mut slot: Vec<Option<Row>> = rows
        .iter_mut()
        .map(|row| Some(std::mem::take(row)))
        .collect();
    for (dest, (_, src)) in keyed.into_iter().enumerate() {
        rows[dest] = slot[src].take().expect("row");
    }
    Ok(())
}

fn take_parked_top_k(keys: &OrderKeys<'_>, rows: Vec<Row>, keep: usize) -> Result<Vec<Row>> {
    let mut rows_charge = hold_sort_rows(&rows)?;
    let mut keyed: Vec<(Vec<Value>, usize, Row)> = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let row_keys = parked_keys(keys, &row);
            (row_keys, index, row)
        })
        .collect();
    let mut keys_charge = hold_sort_keys(keyed.iter().map(|(keys, _, _)| keys.as_slice()))?;
    keyed.select_nth_unstable_by(keep - 1, |a, b| cmp_sort_keys(keys, &a.0, a.1, &b.0, b.1));
    keyed.truncate(keep);
    keyed.sort_by(|a, b| cmp_sort_keys(keys, &a.0, a.1, &b.0, b.1));
    rows_charge
        .resize_with(|| crate::workspace::row_refs_bytes(keyed.iter().map(|(_, _, row)| row)))?;
    keys_charge.resize_with(|| {
        crate::workspace::sort_key_columns(keyed.iter().map(|(keys, _, _)| keys.as_slice()))
    })?;
    drop(keys_charge);
    drop(rows_charge);
    Ok(keyed.into_iter().map(|(_, _, row)| row).collect())
}

/// The paging stage: drop the first `skip_n` rows, keep at most `limit_n`.
/// The DISTINCT stage's row-buffer charge is re-measured by the caller
/// against the shrunken buffer — paging itself owns no workspace state.
pub(crate) fn page(mut rows: Vec<Row>, skip_n: usize, limit_n: Option<usize>) -> Vec<Row> {
    drop(rows.drain(..skip_n.min(rows.len())));
    if let Some(n) = limit_n {
        rows.truncate(n);
    }
    rows
}

/// Resolve the SKIP/LIMIT bounds against the empty row. Every paging site —
/// the eager pre-projection stop, the post-merge stage, and the aggregate
/// fold — resolves through this one spelling, each at its own evaluation
/// position, so CIP-27's error ordering is unchanged.
pub(crate) fn resolve_page_bounds(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    skip: Option<&Expr>,
    limit: Option<&Expr>,
) -> Result<(usize, Option<usize>)> {
    let bound = |exprs: Option<&Expr>| -> Result<Option<usize>> {
        exprs
            .map(|expr| skip_limit_count(&eval(graph, &Row::new(), expr, params, cypher_version)?))
            .transpose()
    };
    Ok((bound(skip)?.unwrap_or(0), bound(limit)?))
}

/// Whether a projection takes the eager pre-projection page: `SKIP`/`LIMIT`
/// with no `ORDER BY`, `DISTINCT`, or filter. The ordinary projection and the
/// match sink consult this one rule — the sink only after it has already
/// declined the aggregating and grouping shapes (which keep the ordinary
/// projection anyway).
pub(crate) fn pages_eagerly(fields: &ProjectionFields<'_>) -> bool {
    !fields.distinct
        && fields.order.is_empty()
        && fields.where_clause.is_none()
        && (fields.skip.is_some() || fields.limit.is_some())
}

pub(crate) fn skip_limit_count(v: &Value) -> Result<usize> {
    // ADR-0175: every FLOAT value is rejected — including 1.0, NaN and
    // ±Infinity (Neo4j's runtime gate rejects every float, 22N03); no
    // truncation.
    let n = match v {
        Value::Int(i) => *i,
        _ => {
            return Err(Error::invalid_argument_type(
                "SKIP/LIMIT must be an integer",
            ))
        }
    };
    if n < 0 {
        return Err(Error::execution(
            "NegativeIntegerArgument: SKIP/LIMIT must be non-negative",
        ));
    }
    // The width boundary the negative guard already spells for the low
    // end: the wasm32 surface's 32-bit `usize` would silently truncate a
    // SKIP/LIMIT at or above 2^32 through `as` (a `SKIP 4294967296`
    // skipping nothing), so the cast is explicit and refused instead.
    usize::try_from(n).map_err(|_| {
        Error::execution("IntegerTooLarge: SKIP/LIMIT exceeds the platform row-index width")
    })
}
