//! The ORDER BY family of the projection: the parked-key model
//! ([`OrderKeys`], [`park_order_keys`], [`unpark_order_keys`]), the sort
//! entry point and its top-k selection, the paging stages, and the
//! SKIP/LIMIT bound resolution — split from `project.rs` (LABS-781's
//! stellar round: the order family is the projection's sibling concern,
//! not a section of it).

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, Projection};
use relate_names::SmallName;

use crate::eval::{eval, is_agg_expr};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::scope::is_star_item;
use super::scope::{projection_names, projects_name, result_column};

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
fn parked_prefix(graph: &ExecGraph<'_>, items: &[Projection]) -> String {
    uncolliding_parked_prefix(graph, &projection_names(items))
}

/// [`parked_prefix`] over a row scope's real column vocabulary: the form
/// for the paths that order rows without projection items (the catalog
/// `YIELD` paging, whose composed rows are their own scope), so the
/// collision guard is decided by the columns that actually exist.
fn uncolliding_parked_prefix(graph: &ExecGraph<'_>, columns: &[String]) -> String {
    graph.uncolliding_prefix("__ord", columns.iter().map(String::as_str))
}

/// One projection's `ORDER BY` keys, each with its resolved scope decision
/// and the parked column it is written under. Every projection path parks its
/// keys at projection time — through [`park_order_keys`] for the ordinary
/// path, [`parked_values`] for the sink and `park_values` for the fold — so
/// the one sort entry point reads them back instead of re-evaluating
/// anything.
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
    /// Whether a failed evaluation of this key is a null sort key: true for
    /// the deprecated implicit-grouping resolution — a key that reads a name
    /// the projection does not output. Decided once here, never per row.
    null_on_failure: bool,
    parked: String,
}

/// The one per-key plan builder both column-vocabulary constructors share
/// ([`OrderKeys::parked_only`] over projection items,
/// [`OrderKeys::over_columns`] over a row scope's real columns): a parked
/// name per key off the caller's freshened `prefix`, and the
/// null-on-failure policy the caller's vocabulary decides — the two
/// constructors differ in nothing else ([`OrderKeys::new`] layers the
/// verbatim table on top of the parked-only plan).
fn keyed_plan<'a>(
    keys: impl IntoIterator<Item = &'a relate_ir::OrderItem>,
    prefix: &str,
    null_on_failure: impl Fn(&Expr) -> bool,
) -> Vec<KeyPlan<'a>> {
    keys.into_iter()
        .enumerate()
        .map(|(index, key)| KeyPlan {
            key,
            verbatim: None,
            null_on_failure: null_on_failure(&key.expr),
            parked: format!("{prefix}{index}"),
        })
        .collect()
}

impl<'a> OrderKeys<'a> {
    /// Every key's plan, verbatim resolution included — for callers that
    /// evaluate keys against a projected row. The per-key scope depends only
    /// on the key expressions and the items, never on a row, so the decision
    /// is taken once here.
    pub(crate) fn new(
        graph: &ExecGraph<'_>,
        keys: &'a [relate_ir::OrderItem],
        items: &'a [Projection],
        explicit_grouping: bool,
    ) -> Self {
        let mut this = Self::parked_only(graph, keys, items);
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
    /// the per-row resolver. This constructor owns the per-key
    /// `null_on_failure` decision for every plan it builds; [`Self::new`]
    /// inherits it rather than recomputing the same value.
    pub(crate) fn parked_only(
        graph: &ExecGraph<'_>,
        keys: impl IntoIterator<Item = &'a relate_ir::OrderItem>,
        items: &'a [Projection],
    ) -> Self {
        let parked_prefix = parked_prefix(graph, items);
        Self {
            plan: keyed_plan(keys, &parked_prefix, |expr| {
                reads_unprojected_name(expr, items)
            }),
        }
    }

    /// Every key's plan over a row scope's real column vocabulary — the
    /// catalog ordering path (`SHOW … YIELD … ORDER BY`): the rows are
    /// their own scope (a `YIELD` key names an output column, so parking
    /// reads the row itself), and the parked prefix's collision guard and
    /// the null-on-failure policy are decided by the columns the composed
    /// rows actually carry, never by a projection item list this path
    /// does not have. No verbatim table: with no projection items to
    /// answer from, every key is evaluated against the row.
    pub(crate) fn over_columns(
        graph: &ExecGraph<'_>,
        keys: &'a [relate_ir::OrderItem],
        columns: &[String],
    ) -> Self {
        let parked_prefix = uncolliding_parked_prefix(graph, columns);
        Self {
            plan: keyed_plan(keys, &parked_prefix, |expr| {
                reads_name_outside(expr, columns)
            }),
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
    /// reason to sort on a re-evaluated value.
    ///
    /// A key in the deprecated implicit-grouping resolution — one that reads
    /// a name the projection does not output — sorts null when it cannot be
    /// evaluated, not failing the statement. A key that reads only projected
    /// columns is alias-scoped, so a failed evaluation is the statement
    /// failure live Neo4j raises (22G03, e.g. a property access on a
    /// projected integer alias under an explicit `GROUP BY`).
    fn value<F>(&self, index: usize, scope: &Row, key_value: &F) -> Result<Value>
    where
        F: Fn(&Expr, &Row) -> Result<Value>,
    {
        let plan = &self.plan[index];
        if let Some(name) = &plan.verbatim {
            return Ok(result_column(scope, name).clone());
        }
        match key_value(&plan.key.expr, scope) {
            Ok(value) => Ok(value),
            Err(_) if plan.null_on_failure => Ok(Value::Null),
            Err(e) => Err(e),
        }
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
/// `key_value` evaluates one key against the projected row; a deprecated
/// key it cannot evaluate becomes a null sort key, while an alias-scoped
/// key's failure propagates ([`OrderKeys::value`]).
pub(crate) fn park_order_keys(
    keys: &OrderKeys<'_>,
    projected: &mut Row,
    key_value: impl Fn(&Expr, &Row) -> Result<Value>,
) -> Result<()> {
    let parked = parked_values(keys, projected, key_value)?;
    park_values(keys, projected, parked);
    Ok(())
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
) -> Result<Vec<Value>> {
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

/// Whether `expr` reads a name the projection does not output — the
/// deprecated implicit-grouping resolution whose failure sorts null.
/// Comprehension- and reduce-bound identifiers are excluded
/// ([`relate_ir::expr_vars`]); a subquery body over-approximates its outer
/// reads, which only widens the null-on-failure set toward the deprecated
/// behaviour.
fn reads_unprojected_name(expr: &Expr, items: &[Projection]) -> bool {
    reads_name_outside(expr, &projection_names(items))
}

/// [`reads_unprojected_name`] over a row scope's real column vocabulary:
/// whether `expr` reads a name no column of the scope carries — the
/// null-on-failure decision for the paths that order rows without
/// projection items.
fn reads_name_outside(expr: &Expr, columns: &[String]) -> bool {
    let mut vars = HashSet::new();
    relate_ir::expr_vars(expr, &mut vars);
    vars.iter()
        .any(|name| !columns.iter().any(|column| column == name))
}

/// The resolved ORDER BY of a folded shape: each key's projection item
/// and that item's output name, the pair both consumers actually read
/// (the fold's item lookup and the column rewrite) — so the caller never
/// indexes back into the slice with an unchecked position.
pub(crate) fn folded_order_names<'a>(
    order: &'a [relate_ir::OrderItem],
    items: &'a [Projection],
) -> Option<Vec<(&'a relate_ir::OrderItem, String)>> {
    order
        .iter()
        .map(|key| {
            items
                .iter()
                .enumerate()
                .position(|(index, item)| {
                    !is_star_item(item)
                        && (item.expr == key.expr
                            || matches!(&key.expr, Expr::Variable(name)
                        if name == item.output_name(index).as_str()))
                })
                .map(|item| (key, items[item].output_name(item)))
        })
        .collect()
}

/// Rewrite the ORDER BY onto output columns: each key resolves to its
/// output column through the canonical [`folded_order_names`] (an
/// expression equal to an output item, or a bare reference to an output
/// name), and becomes a reference to that item's column. A key the
/// grouped rows cannot evaluate is `None`.
pub(crate) fn order_on_columns(
    order: &[relate_ir::OrderItem],
    items: &[Projection],
    var: &SmallName,
) -> Option<Vec<relate_ir::OrderItem>> {
    // The rewrite's own precondition, enforced at the boundary: no output
    // column may be named like the pattern's node variable, or a
    // rewritten key resolves against the scan's binding, not the grouped
    // row (the caller no longer has to remember the gate).
    if super::scope::projects_name(items, var) {
        return None;
    }
    Some(
        folded_order_names(order, items)?
            .into_iter()
            .map(|(key, name)| relate_ir::OrderItem {
                expr: Expr::Variable(SmallName::from_name(&name)),
                ascending: key.ascending,
            })
            .collect(),
    )
}

pub(crate) fn cmp_sort_keys(
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

/// Reserve the row buffer, then sample RSS while those rows are still the
/// live workspace and before the key vector is built.
fn hold_sort_rows(rows: &[Row]) -> Result<relate_core::tokens::WorkspaceCharge> {
    let charge = crate::workspace::charge_row_buffer(rows)?;
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
pub(crate) fn parked_keys(keys: &OrderKeys<'_>, row: &Row) -> Vec<Value> {
    let mut out = Vec::with_capacity(keys.len());
    fill_parked_keys(keys, row, &mut out);
    out
}

/// Refill `out` with the row's parked keys — the one spelling of the key
/// read, for the caller that owns a buffer across rows and the one that
/// builds a fresh vector. The window refills its comparison scratch here,
/// so a row it rejects costs no allocation.
pub(crate) fn fill_parked_keys(keys: &OrderKeys<'_>, row: &Row, out: &mut Vec<Value>) {
    crate::exec::scope::fill_columns(row, (0..keys.len()).map(|index| keys.name(index)), out);
}

/// The one `ORDER BY` application. Every projection path parks its keys at
/// projection time (see [`park_order_keys`]), so this reads them back and
/// never evaluates an expression. `top_k` asks for the first `top_k` rows
/// only — the `LIMIT` selection. A zero page never reaches here: the
/// caller answers it before any container is opened
/// ([`PageBounds::answers_nothing`], read by `project_rows`).
pub(crate) fn sort_rows_by_parked_keys(
    keys: &OrderKeys<'_>,
    rows: &mut Vec<Row>,
    top_k: Option<std::num::NonZeroUsize>,
) -> Result<()> {
    // A clause with no order keys has nothing to *sort*, so the sort's
    // precondition is its own and every caller can gate on its shape
    // alone — but the `LIMIT` selection is not the sort's: it applies
    // whatever the keys are, so a keyless caller asking for `top_k` still
    // gets its first `top_k` rows.
    if keys.is_empty() && top_k.is_none() {
        return Ok(());
    }
    match top_k {
        Some(keep) if keep.get() < rows.len() => {
            *rows = take_parked_top_k(keys, std::mem::take(rows), keep)?;
            Ok(())
        }
        _ => sort_all_rows(keys, rows),
    }
}

/// The ordered window's capacity — `skip + limit` entries — or `None` when
/// there is no `LIMIT` (a `SKIP`-only window is unbounded) or the sum
/// overflows (an overflowing window is unlimited). The one derivation of a
/// window's size: the clause's
/// [`super::classification::ProjectionClass::window_keep`] masks it for the shapes that take no
/// window, and every consumer reads the answer.
pub(crate) fn window_capacity(skip_n: usize, limit_n: Option<usize>) -> Option<usize> {
    limit_n.and_then(|limit_n| skip_n.checked_add(limit_n))
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

fn take_parked_top_k(
    keys: &OrderKeys<'_>,
    rows: Vec<Row>,
    keep: std::num::NonZeroUsize,
) -> Result<Vec<Row>> {
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
    keyed.select_nth_unstable_by(keep.get() - 1, |a, b| {
        cmp_sort_keys(keys, &a.0, a.1, &b.0, b.1)
    });
    keyed.truncate(keep.get());
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

/// A running page over one stream — the per-row twin of
/// [`page`]'s whole-table cut: the `SKIP`/`LIMIT` counters a landing shape
/// consumes one row at a time, so no container re-spells the bounds pair.
pub(crate) struct PageCursor {
    skip_left: usize,
    limit_left: Option<usize>,
}

impl PageCursor {
    pub(crate) fn new(bounds: PageBounds) -> Self {
        Self {
            skip_left: bounds.skip,
            limit_left: bounds.limit,
        }
    }

    /// Whether this row takes a slot: the `SKIP` counter first, then one
    /// `LIMIT` slot. The slot is spent even when a later stage (the
    /// trailing `WHERE`) drops the row — the page is cut before it. A spent
    /// page admits nothing (the funnels stop at [`Self::is_exhausted`], and
    /// the guard keeps a stray offer from underflowing the counter).
    pub(crate) fn admit(&mut self) -> bool {
        if self.skip_left > 0 {
            self.skip_left -= 1;
            return false;
        }
        match self.limit_left.as_mut() {
            Some(0) => false,
            Some(left) => {
                *left -= 1;
                true
            }
            None => true,
        }
    }

    /// Whether the `LIMIT` is spent.
    pub(crate) fn is_exhausted(&self) -> bool {
        self.limit_left == Some(0)
    }
}

/// The clause's page bounds: how many leading rows `SKIP` drops and how
/// many `LIMIT` keeps. One named pair, so the feed, the merge paths, the
/// window decision and the finish tail all read `bounds.skip` and
/// `bounds.limit` rather than passing a positional tuple between them.
#[derive(Clone, Copy)]
pub(crate) struct PageBounds {
    pub(crate) skip: usize,
    pub(crate) limit: Option<usize>,
}

impl PageBounds {
    /// Whether this page answers nothing at all — a `LIMIT 0`, whatever the
    /// `SKIP`. The one spelling of the empty page: the merge decides it from
    /// here, and the window decision derives its no-window answer from the
    /// same predicate, so the two cannot disagree about which pages are
    /// empty.
    pub(crate) fn answers_nothing(self) -> bool {
        self.limit == Some(0)
    }

    /// Whether this page cuts anything at all — a `SKIP`, a `LIMIT`, or
    /// both. The one spelling of "the clause pages", so no consumer
    /// re-spells the bounds fields.
    pub(crate) fn is_paged(self) -> bool {
        self.skip > 0 || self.limit.is_some()
    }

    /// A skip-only page: everything before `skip` drops, the rest is kept.
    /// The one spelling of the one-sided bounds, so a `SKIP`- or `LIMIT`-
    /// only stage (the catalog's YIELD paging) never re-spells the literal.
    pub(crate) fn skip(skip: usize) -> Self {
        Self { skip, limit: None }
    }

    /// A limit-only page from the start: the first `limit` rows are kept.
    pub(crate) fn limit(limit: usize) -> Self {
        Self {
            skip: 0,
            limit: Some(limit),
        }
    }

    /// No page at all — the stage that already paged elsewhere reads this
    /// third state by name, so no consumer re-spells the unpaged literal
    /// (a near-miss `limit: Some(0)` is exactly the empty page
    /// [`Self::answers_nothing`] names).
    pub(crate) fn unpaged() -> Self {
        Self {
            skip: 0,
            limit: None,
        }
    }
}

/// The paging stage: drop the first `bounds.skip` rows, keep at most
/// `bounds.limit`. The DISTINCT stage's row-buffer charge is re-measured by
/// the caller against the shrunken buffer — paging itself owns no workspace
/// state.
pub(crate) fn page(mut rows: Vec<Row>, bounds: PageBounds) -> Vec<Row> {
    drop(rows.drain(..bounds.skip.min(rows.len())));
    if let Some(n) = bounds.limit {
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
    skip: Option<&Expr>,
    limit: Option<&Expr>,
) -> Result<PageBounds> {
    let bound = |expr: Option<&Expr>| -> Result<Option<usize>> {
        expr.map(|expr| resolve_page_bound(graph, params, expr))
            .transpose()
    };
    Ok(PageBounds {
        skip: bound(skip)?.unwrap_or(0),
        limit: bound(limit)?,
    })
}

/// Resolve one `SKIP`/`LIMIT` bound expression against the empty row. The
/// one bound spelling [`resolve_page_bounds`] and the catalog stage
/// interpreter share; the caller applies it through [`page`].
pub(crate) fn resolve_page_bound(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    expr: &Expr,
) -> Result<usize> {
    skip_limit_count(&eval(graph, &Row::new(), expr, params)?)
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
        // The shared negative-bound wording; this runtime guard keeps its
        // `Execution` class.
        return Err(Error::execution(relate_core::negative_page_bound_text()));
    }
    // The width boundary the negative guard already spells for the low
    // end: the wasm32 surface's 32-bit `usize` would silently truncate a
    // SKIP/LIMIT at or above 2^32 through `as` (a `SKIP 4294967296`
    // skipping nothing), so the cast is explicit and refused instead.
    usize::try_from(n).map_err(|_| {
        Error::execution("IntegerTooLarge: SKIP/LIMIT exceeds the platform row-index width")
    })
}
