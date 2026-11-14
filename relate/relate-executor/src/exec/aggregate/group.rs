//! The group path: [`aggregate_ex`] builds the groups (explicit `GROUP BY`
//! keys or the implicit non-aggregating projection items), folds each one,
//! and parks its `ORDER BY` keys and HAVING predicate — plus the `*`
//! key desugaring both paths share.

use std::borrow::Cow;
use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Expr, Projection};
use relate_names::SmallName;

use crate::eval::{eval, is_agg_expr};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::order::{park_order_keys, OrderKeys};
use super::super::scope::{is_internal_column, is_star_item, visible_keys};
use super::super::RowFeed;
use super::eval::{agg_eval, AggScope};
use super::key::KeyTuple;
use super::seed::{ItemRole, SeedStore, Taken};
use super::stream::discover;
use super::table::{GroupBill, GroupTable, InputBill, Landing};

/// ADR-0453's one WHERE split, one mode: an aggregating path whose WHERE
/// carries aggregate calls takes it as HAVING — evaluated once per group
/// through [`agg_eval`] (aggregate calls fold the group, non-aggregate
/// leaves read the projected row), a group whose predicate is not true is
/// dropped; false and null drop, a non-boolean value is the typed
/// rejection live raises (22N01) rather than a silent drop, and a
/// predicate evaluation error aborts the statement — the same
/// convert-or-raise policy the ordinary WHERE positions read
/// (ADR-0458 §7, [`crate::eval::predicate_value_holds`]). Every other
/// WHERE form — any WHERE on a non-aggregating path included — stays the
/// plain post-merge filter: the projection applies it to the emitted
/// rows, not the group, and the aggregation retains only what it reads
/// in the seed, so an emitted row still carries the grouping keys the
/// predicate names (`GROUP BY a.p WHERE a.p > 0`).
#[derive(Clone, Copy)]
pub(crate) enum WhereMode<'a> {
    Having(&'a Expr),
    Plain(&'a Expr),
}

impl<'a> WhereMode<'a> {
    /// The predicate, whichever form it takes — the seed keeps what it
    /// reads in both modes.
    fn predicate(self) -> &'a Expr {
        match self {
            Self::Having(predicate) | Self::Plain(predicate) => predicate,
        }
    }

    /// The HAVING predicate, `None` for the plain form.
    pub(crate) fn having(self) -> Option<&'a Expr> {
        match self {
            Self::Having(predicate) => Some(predicate),
            Self::Plain(_) => None,
        }
    }

    /// The plain filter, `None` for the HAVING form.
    pub(crate) fn plain(self) -> Option<&'a Expr> {
        match self {
            Self::Plain(predicate) => Some(predicate),
            Self::Having(_) => None,
        }
    }
}

/// The projection's per-group subclause state, bundled to keep
/// [`aggregate_ex`] under the argument-count lint.
#[derive(Clone, Copy)]
pub(crate) struct GroupSubclauses<'a> {
    /// The projection's `ORDER BY` key list with its per-key alias-scope
    /// decision. The group is the only place a key's aggregate calls can be
    /// answered, so each output row parks them through [`park_order_keys`]
    /// for the sort to read back.
    pub order: &'a OrderKeys<'a>,
    /// The projection's one WHERE predicate, in the mode ADR-0453 decides
    /// for it — see [`WhereMode`].
    pub where_mode: Option<WhereMode<'a>>,
    /// ADR-0453: an importing subquery's scope row — the seed the phantom
    /// keyless group over an emptied input uses, so a non-aggregate leaf
    /// naming an import (`sum(y) + tax` with no rows left) reads the
    /// import's value instead of null. `None` at every other scope.
    pub import_scope: Option<&'a Row>,
}

/// One aggregating projection, fed a row at a time: [`Self::open`] does the
/// plan setup, [`Self::offer`] folds one row into its group, and
/// [`Self::finish`] emits the group rows (`ORDER BY` keys parked, HAVING
/// applied). The graph is passed per call, as the accumulators take it.
pub(crate) struct Aggregation<'a> {
    items: &'a [Projection],
    params: &'a BTreeMap<String, Value>,
    /// The projection's `ORDER BY`, WHERE mode and import scope (the
    /// finish reads them; the plain WHERE is read at planning only).
    subclauses: GroupSubclauses<'a>,
    key_exprs: Vec<Cow<'a, Expr>>,
    /// How each projection item is answered at the finish (a kept key
    /// value, the folded sites, or the seed row) — decided once at plan
    /// time.
    roles: Vec<ItemRole>,
    /// The groups: key index, seeds (the first-row columns the finish
    /// reads), and accumulator columns — see [`GroupTable`] for the
    /// per-group layout and what each group costs.
    groups: GroupTable<'a>,
    /// ADR-0463 (LABS-781): the group table is one retaining container.
    /// Each landed group grows one running total enforced at the per-value
    /// cap, so a high-cardinality key set refuses at ~the cap while the
    /// groups land. A row that joins an existing group adds nothing: the
    /// group keeps constant-size accumulators, never its rows. (A
    /// retaining aggregate — `collect`, a percentile, a DISTINCT call —
    /// charges its operands to the same total as they settle.)
    bill: GroupBill,
    /// The input vector's bill, released row by row as payloads move into
    /// the group table; `None` when workspace accounting is off.
    input: Option<InputBill>,
}

impl<'a> Aggregation<'a> {
    /// Plan setup. `group_by` (CIP-236) carries explicit grouping keys when
    /// a GROUP BY clause is present; that is the only difference from the
    /// implicit form, whose keys come from the non-aggregating projection
    /// items. `Some(&[])` is the empty grouping set (`GROUP BY ()`): one
    /// global group. `rows` supplies the first-row shape for the `*` key
    /// desugar and the held input vector to bill (`aggregate_ex` passes the
    /// feed's probe; a streamed fold passes none).
    pub(crate) fn open(
        graph: &ExecGraph<'_>,
        rows: &[Row],
        items: &'a [Projection],
        group_by: Option<&'a [Expr]>,
        params: &'a BTreeMap<String, Value>,
        subclauses: &GroupSubclauses<'a>,
    ) -> Result<Self> {
        let GroupSubclauses {
            order, where_mode, ..
        } = *subclauses;
        // The HAVING predicate the sites discover and the finish reads;
        // the plain form carries no aggregate call, so it discovers none.
        let having = where_mode.and_then(WhereMode::having);
        // The grouping strategy is loop-invariant: decide once whether the
        // key comes from the explicit `group_by` expressions or from the
        // inferred non-aggregating projection items.
        let key_exprs = desugar_star_keys(
            match group_by {
                Some(keys) => keys.iter().map(Cow::Borrowed).collect(),
                None => items
                    .iter()
                    .filter(|p| !is_agg_expr(&p.expr))
                    .map(|p| Cow::Borrowed(&p.expr))
                    .collect(),
            },
            rows,
        );
        // One row for an empty input only when there are no grouping keys
        // at all: implicit grouping over an all-aggregate projection, an
        // explicit empty grouping set (`GROUP BY ()`), or an explicit empty
        // `ALL` (which lowers to no keys). An explicit non-empty key set
        // has no group to emit for an empty input, so it yields zero rows --
        // emitting the phantom global group there would invent a row with
        // no key values. A `*` item counts as a key: it carries every
        // in-scope variable (ADR-0198).
        let keyless = key_exprs.is_empty();
        // The aggregate calls the projection evaluates — items, ORDER BY
        // keys, HAVING — found once, so every group streams its input
        // through constant-size accumulators instead of retaining its rows.
        let accs = discover(
            graph,
            items
                .iter()
                .filter(|p| !is_star_item(p) && is_agg_expr(&p.expr))
                .map(|p| &p.expr),
            order.exprs().chain(having),
            params,
        )?;
        let account = relate_core::tokens::workspace_accounting();
        // The input vector stays allocated until the fold finishes. Bill it
        // up front, then hand each row's payload to the group map as it
        // moves. A generated feed holds no input vector: nothing to bill,
        // nothing to release row by row.
        let input = account.then(|| InputBill::open(rows)).transpose()?;
        // What a group keeps of its first row (see `seed`). The plain
        // WHERE is evaluated after emission, against the emitted row — the
        // seed keeps what it reads so the row still carries the grouping
        // keys it names.
        let (seeds, roles) = SeedStore::planned(
            items,
            &key_exprs,
            order
                .exprs()
                .chain(where_mode.map(WhereMode::predicate))
                .chain(accs.seed_reads()),
        );
        let mut this = Self {
            items,
            params,
            subclauses: *subclauses,
            key_exprs,
            roles,
            groups: GroupTable::new(seeds, accs),
            bill: GroupBill::open(graph)?,
            input,
        };
        // A keyless aggregation has exactly one group whose key is the
        // empty set: seed it here, through the same charge sequence a row
        // landing pays, and every input row lands in it — the empty input
        // keeps the seed as its phantom group (the ADR-0453 import scope's
        // stand-in) and no post-loop insertion is needed.
        if keyless {
            this.groups.open_keyless(graph, &mut this.bill)?;
        }
        Ok(this)
    }

    /// The bytes the group table has billed to the running total so far
    /// (the charging seam the memory-shape tests read).
    #[cfg(test)]
    pub(crate) fn billed_bytes(&self) -> u64 {
        self.bill.retained.total()
    }

    /// Fold one input row into its group.
    pub(crate) fn offer(&mut self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        let Self {
            params,
            key_exprs,
            groups,
            bill,
            input,
            ..
        } = self;
        let params = *params;
        graph.cancel_tick()?;
        // Star-free after the desugar in `open`: one `eval` per key.
        let (key, key_values) = KeyTuple::of(key_exprs.len(), |i| {
            eval(graph, &row, &key_exprs[i], params)
        })?;
        if let Some(input) = input.as_mut() {
            input.release(&row)?;
        }
        // The landing bills what the row adds, ahead of the landing (see
        // [`GroupTable::land`]); the row's own payload is then spent into
        // the accumulators and, for the seed columns, the seed store.
        let landing = groups.land(graph, bill, key, &key_values, &row)?;
        let (group, as_seed) = match landing {
            Landing::Existing { group, as_seed } => (group, as_seed),
            Landing::New { group } => (group, true),
        };
        groups.accs.offer(group, graph, &row, params, bill)?;
        // The seed lands once per group: a new group's first row, or the
        // keyless group's. Every other row is spent once the accumulators
        // have it — dropped before the settle, so what a site kept from it
        // is solely the group's and is charged in full.
        if as_seed {
            groups.seeds.land(key_values, row)?;
            groups.phantom = false;
        } else {
            drop(row);
        }
        groups.accs.settle(group, graph, bill)
    }

    /// Emit the group rows: projected columns, parked `ORDER BY` keys, and
    /// HAVING applied.
    pub(crate) fn finish(self, graph: &ExecGraph<'_>) -> Result<Vec<Row>> {
        let Self {
            items,
            params,
            subclauses:
                GroupSubclauses {
                    order,
                    where_mode,
                    import_scope,
                },
            roles,
            groups,
            bill,
            input,
            ..
        } = self;
        drop(input);
        let mut out = Vec::new();
        // ADR-0463 (LABS-781): the aggregating projection's output
        // container — each emitted group row charges as it lands, ahead of
        // the landing, in its final shape (seed bindings, projected
        // values, aggregate results, parked order keys).
        let mut out_retained = graph.retain_bytes(0)?;
        let GroupTable {
            index,
            mut seeds,
            mut accs,
            ..
        } = groups;
        // Emission order is landing order; the keys are not read again.
        let group_count = index.len();
        drop(index);
        // ADR-0198 ruling 1: a `*` item projects the row's carried
        // variables through alongside the aggregate aliases.
        let has_star = roles.iter().any(|role| matches!(role, ItemRole::Star));
        // The HAVING predicate is loop-invariant, like `has_star`: decided
        // once, read per group.
        let having = where_mode.and_then(WhereMode::having);
        for group in 0..group_count {
            // The group's own first row — narrowed to the columns this
            // finish reads — is the seed every non-aggregate leaf reads;
            // the phantom keyless group over an emptied input has none, so
            // an importing subquery's scope row stands in (ADR-0453) — the
            // imports are per-row constants, present even when the body's
            // rows are gone.
            let Taken {
                row: seed,
                keys: key_values,
            } = seeds.take(group);
            let seed_row = seed.as_ref().or(import_scope);
            let mut n = seed_row.cloned().unwrap_or_default();
            let folded = accs.finish_group(group, graph, seed.as_ref(), params);
            if has_star {
                // The variables `*` carries are already in the first-row
                // seed; keep the internal `__*` names out of the projected
                // row, the same filter the non-aggregating `*` path applies.
                n.retain(|name, _| !is_internal_column(name));
            }
            for (i, (p, role)) in items.iter().zip(&roles).enumerate() {
                let val = match *role {
                    ItemRole::Star => continue,
                    ItemRole::Aggregate => agg_eval(
                        graph,
                        AggScope::of_group(&folded, seed_row),
                        &p.expr,
                        params,
                    )?,
                    ItemRole::Key(slot) => key_values[slot].clone(),
                    ItemRole::Leaf => match seed_row {
                        Some(row) => eval(graph, row, &p.expr, params)?,
                        None => Value::Null,
                    },
                };
                n.insert(p.output_name(i), val);
            }
            // Park the `ORDER BY` keys beside the projected columns, through
            // the same decision and writer every other projection path uses.
            // A key reads `n`, the projected row, so an aggregate call in it
            // reads this group's resolved value and the deprecated
            // implicit-grouping shapes still answer from their item; a key
            // that cannot be resolved orders as null, while an ill-typed
            // read of a present value fails — the documented `ORDER BY`
            // policy.
            park_order_keys(order, &mut n, |expr, scope| {
                agg_eval(
                    graph,
                    AggScope::of_group(&folded, Some(scope)),
                    expr,
                    params,
                )
            })?;
            // HAVING (ADR-0453): the aggregating WHERE, answered per group
            // with the same scope an ORDER BY key gets. Not-true drops the
            // group — the retain's verdict for a failing row; a non-boolean
            // is the typed rejection live raises (22N01) and a list/path
            // converts, both through the one predicate policy (ADR-0458 §7).
            if let Some(predicate) = having {
                let keeps = agg_eval(
                    graph,
                    AggScope::of_group(&folded, Some(&n)),
                    predicate,
                    params,
                )?;
                if !crate::eval::predicate_value_holds(&keeps)? {
                    continue;
                }
            }
            // The resolved aggregate values are shared with `n` until the
            // folded group lets go; drop it so the output row's landing
            // charge sees the payloads it solely owns.
            drop(folded);
            out_retained.add_row(graph, n.values())?;
            out.push(n);
        }
        drop(bill);
        Ok(out)
    }
}

/// The loop over [`Aggregation`]: open, offer every row of the feed, finish.
pub(crate) fn aggregate_ex<'a>(
    graph: &ExecGraph<'_>,
    feed: RowFeed<'_>,
    items: &'a [Projection],
    group_by: Option<&'a [Expr]>,
    params: &'a BTreeMap<String, Value>,
    subclauses: &GroupSubclauses<'a>,
) -> Result<Vec<Row>> {
    let mut aggregation =
        Aggregation::open(graph, feed.probe(), items, group_by, params, subclauses)?;
    feed.try_for_each(|row| aggregation.offer(graph, row))?;
    aggregation.finish(graph)
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
