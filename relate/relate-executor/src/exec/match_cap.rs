//! DISTINCT and row-limit caps for MATCH expansion.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Clause, Direction, Expr, OrderItem, PathPattern, Projection};
use relate_names::SmallName;

use crate::distinct_set::DistinctSet;
use crate::eval::{eval, is_agg_expr, var_prop};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::order::skip_limit_count;
use super::sink::SinkProjection;

/// Stop MATCH once `RETURN/WITH DISTINCT … LIMIT n` has enough distinct keys.
/// OpenCypher does not specify which rows `LIMIT` keeps without `ORDER BY`.
pub(crate) struct DistinctLimitCap<'a> {
    items: &'a [Projection],
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    seen: DistinctSet,
    want: usize,
}

impl DistinctLimitCap<'_> {
    fn full(&self) -> bool {
        self.seen.len() >= self.want
    }

    pub(crate) fn offer(&mut self, graph: &ExecGraph<'_>, row: &Row) -> Result<()> {
        if self.full() {
            return Ok(());
        }
        // The arity-1 keying (a one-column row is its value's key) lives in
        // `DistinctSet::insert_row` alone.
        let mut vals = Vec::with_capacity(self.items.len());
        for p in self.items {
            vals.push(eval(graph, row, &p.expr, self.params, self.cypher_version)?);
        }
        self.seen.insert_row(&vals)?;
        Ok(())
    }
}

/// The RETURN/WITH suffix fields every MATCH cap gate reads: the first
/// clause of the suffix, destructured once (`distinct_limit_cap`,
/// `ordered_limit_cap`, and `plain_limit_cap` each apply only their own
/// shape gates on top of this one parse — and the frontier matcher arm's
/// answer-shape decoder, exec/kernels/frontier_match.rs, reads the same parse).
pub(crate) struct ReturnWithSuffix<'a> {
    pub(crate) distinct: bool,
    /// CIP-236: an explicit GROUP BY changes what the clause projects
    /// (grouped rows, not one row per match), so a per-match cap no
    /// longer describes the output — every cap declines it.
    pub(crate) group_by: Option<&'a [Expr]>,
    pub(crate) items: &'a [Projection],
    pub(crate) order: &'a [OrderItem],
    pub(crate) skip: Option<&'a Expr>,
    pub(crate) limit: Option<&'a Expr>,
    pub(crate) where_clause: Option<&'a Expr>,
}

/// The first suffix clause as a `RETURN` or `WITH` (`None` for anything
/// else): the one parse every cap constructor reads its shape from.
pub(crate) fn return_with_suffix(clause: &Clause) -> Option<ReturnWithSuffix<'_>> {
    match clause {
        Clause::Return {
            distinct,
            group_by,
            items,
            order,
            skip,
            limit,
        } => Some(ReturnWithSuffix {
            distinct: *distinct,
            group_by: group_by.as_deref(),
            items,
            order,
            skip: skip.as_ref(),
            limit: limit.as_ref(),
            where_clause: None,
        }),
        Clause::With {
            distinct,
            group_by,
            items,
            order,
            skip,
            limit,
            where_clause,
            ..
        } => Some(ReturnWithSuffix {
            distinct: *distinct,
            group_by: group_by.as_deref(),
            items,
            order,
            skip: skip.as_ref(),
            limit: limit.as_ref(),
            where_clause: where_clause.as_ref(),
        }),
        _ => None,
    }
}

impl ReturnWithSuffix<'_> {
    /// Whether the clause carries a `*` or aggregate item — grouped
    /// output, not one row per match, which no per-match cap describes
    /// (CIP-236, the `group_by` note's rule for the inferred form).
    fn star_or_aggregate(&self) -> bool {
        self.items
            .iter()
            .any(|p| matches!(p.expr, Expr::Star) || is_agg_expr(&p.expr))
    }

    /// The row count the suffix's `SKIP s LIMIT n` truncates the clause's
    /// output to (`s + n`): the want both row-limit caps share. `None`
    /// when the suffix carries no `LIMIT` or the add overflows.
    /// `distinct_limit_cap` reaches this only after declining any `SKIP`,
    /// so its want there is the bare `LIMIT` count.
    pub(crate) fn skip_limit_want(
        &self,
        graph: &ExecGraph<'_>,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> Result<Option<usize>> {
        let Some(lim) = self.limit else {
            return Ok(None);
        };
        let empty = Row::new();
        let skip_n = match self.skip {
            Some(e) => skip_limit_count(&eval(graph, &empty, e, params, cypher_version)?)?,
            None => 0,
        };
        let limit_n = skip_limit_count(&eval(graph, &empty, lim, params, cypher_version)?)?;
        Ok(skip_n.checked_add(limit_n))
    }
}

pub(crate) fn distinct_limit_cap<'a>(
    suffix: &'a [Clause],
    graph: &ExecGraph<'_>,
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<DistinctLimitCap<'a>>> {
    let Some(clause) = suffix.first() else {
        return Ok(None);
    };
    let Some(s) = return_with_suffix(clause) else {
        return Ok(None);
    };
    if !s.distinct
        || s.group_by.is_some()
        || !s.order.is_empty()
        || s.skip.is_some()
        || s.where_clause.is_some()
        || s.star_or_aggregate()
    {
        return Ok(None);
    }
    let Some(want) = s.skip_limit_want(graph, params, cypher_version)? else {
        return Ok(None);
    };
    Ok(Some(DistinctLimitCap {
        items: s.items,
        params,
        cypher_version,
        seen: DistinctSet::default(),
        want,
    }))
}

/// Stop MATCH once `SKIP+LIMIT` visible rows have been produced: the row
/// counter behind both limit caps. `Ordered`'s gate (an `ORDER BY r.ts
/// DESC LIMIT` whose walk order is the stored typed-chunk order,
/// ADR-0035) and `Plain`'s (an un-ordered `LIMIT`, below) construct it.
/// It counts every offered row: a `WHERE`-bearing walk the plain cap
/// rides runs the clause filter in its sink ahead of the count
/// (`match_clause`'s filter-only reduce, LABS-853), so a row the filter
/// drops is never offered.
pub(crate) struct RowLimitCap {
    want: usize,
    produced: usize,
}

impl RowLimitCap {
    fn full(&self) -> bool {
        self.produced >= self.want
    }
}

pub(crate) enum MatchCap<'a> {
    Distinct(DistinctLimitCap<'a>),
    Ordered(RowLimitCap),
    Plain(RowLimitCap),
}

impl MatchCap<'_> {
    pub(crate) fn full(&self) -> bool {
        match self {
            MatchCap::Distinct(c) => c.full(),
            MatchCap::Ordered(c) | MatchCap::Plain(c) => c.full(),
        }
    }

    /// The plain cap's want — the row count the walk halts at — when this
    /// is a plain cap (the ladder's one yielding shape; ordered and
    /// distinct caps never yield, so they answer `None`). The contracted
    /// kernels read it as their rows-mode halt (LABS-990): the same
    /// count, the same prefix-contract-free semantics, spelled at the
    /// kernel's own final collection instead of the walker's sink. Its
    /// WHERE-exactness has a third end beside the two below: the
    /// kernels' compiled admissions are their WHERE (a conjunct outside
    /// the closed vocabulary declines the kernel), so only kept rows
    /// land and the count stays exact without a sink filter.
    pub(crate) fn plain_want(&self) -> Option<usize> {
        match self {
            MatchCap::Plain(c) => Some(c.want),
            MatchCap::Ordered(_) | MatchCap::Distinct(_) => None,
        }
    }

    pub(crate) fn offer(&mut self, graph: &ExecGraph<'_>, row: &Row) -> Result<()> {
        match self {
            MatchCap::Distinct(c) => c.offer(graph, row),
            MatchCap::Ordered(c) | MatchCap::Plain(c) => {
                if !c.full() {
                    c.produced += 1;
                }
                Ok(())
            }
        }
    }
}

pub(crate) fn rel_var_of_ts(expr: &Expr) -> Option<&SmallName> {
    var_prop(expr)
        .filter(|(_, key)| *key == "ts")
        .map(|(var, _)| var)
}

pub(crate) fn order_rel_ts_desc_var<'a>(
    order: &'a [relate_ir::OrderItem],
    items: &'a [Projection],
) -> Option<&'a SmallName> {
    let item = order.first()?;
    if order.len() != 1 || item.ascending {
        return None;
    }
    if let Some(v) = rel_var_of_ts(&item.expr) {
        return Some(v);
    }
    if let Expr::Variable(name) = &item.expr {
        for (i, p) in items.iter().enumerate() {
            if name == p.output_name(i).as_str() {
                return rel_var_of_ts(&p.expr);
            }
        }
    }
    None
}

pub(crate) fn ordered_expand_pattern(patterns: &[PathPattern], rel_var: &SmallName) -> bool {
    if patterns.len() != 1 {
        return false;
    }
    let p = &patterns[0];
    if p.rels.len() != 1 {
        return false;
    }
    let rp = &p.rels[0];
    !rp.var_length
        && rp.types.len() == 1
        && rp.direction != Direction::Both
        && rp.variable.as_ref() == Some(rel_var)
}

pub(crate) fn ordered_limit_cap(
    suffix: &[Clause],
    patterns: &[PathPattern],
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    input_rows: usize,
) -> Result<Option<RowLimitCap>> {
    if input_rows != 1 || graph.has_write_overlay() {
        return Ok(None);
    }
    let Some(clause) = suffix.first() else {
        return Ok(None);
    };
    let Some(s) = return_with_suffix(clause) else {
        return Ok(None);
    };
    if s.distinct || s.group_by.is_some() || s.where_clause.is_some() || s.star_or_aggregate() {
        return Ok(None);
    }
    let Some(rel_var) = order_rel_ts_desc_var(s.order, s.items) else {
        return Ok(None);
    };
    if !ordered_expand_pattern(patterns, rel_var) {
        return Ok(None);
    }
    let Some(want) = s.skip_limit_want(graph, params, cypher_version)? else {
        return Ok(None);
    };
    if want == 0 {
        return Ok(None);
    }
    Ok(Some(RowLimitCap { want, produced: 0 }))
}

/// Stop MATCH once an un-ordered `RETURN/WITH … [SKIP s] LIMIT n` has
/// `SKIP+LIMIT` rows. LIMIT without ORDER BY keeps an unspecified prefix
/// — the engine's deterministic enumeration order, exactly the prefix the
/// ordinary projection's own truncation keeps — so halting the walk at
/// `SKIP+LIMIT` reproduces the same rows without enumerating the rest
/// (the QPP/GPM LIMIT pull-in: the walk's `Flow`-style halt, `gpm::emit`;
/// the classic walker's sink already halts on a full cap). A `WHERE` on
/// the MATCH itself does not decline (LABS-853, see [`match_limit_cap`]):
/// the walk's sink filters ahead of the count, so the counter moves only
/// on rows the eager build would keep and the halt still lands on the
/// eager prefix — the walk stops building rider rows the projection
/// would drop. Declines
/// DISTINCT (the distinct cap owns that shape), any `ORDER BY` (ordered
/// truncation is `ordered_limit_cap`'s shape — the top rows of a sort are
/// not the walk's prefix), an explicit GROUP BY (grouped output is not
/// one row per match), a following WHERE, `*`, and aggregate items.
fn plain_limit_cap(
    suffix: &[Clause],
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<RowLimitCap>> {
    let Some(clause) = suffix.first() else {
        return Ok(None);
    };
    let Some(s) = return_with_suffix(clause) else {
        return Ok(None);
    };
    if s.distinct
        || s.group_by.is_some()
        || !s.order.is_empty()
        || s.where_clause.is_some()
        || s.star_or_aggregate()
    {
        return Ok(None);
    }
    let Some(want) = s.skip_limit_want(graph, params, cypher_version)? else {
        return Ok(None);
    };
    if want == 0 {
        return Ok(None);
    }
    Ok(Some(RowLimitCap { want, produced: 0 }))
}

/// The one fact the clause filter's placement rides: `true` when the
/// walk's sink must run the WHERE — once per row, ahead of any cap's
/// count. The plain cap's count is exact only because this answers the
/// same way [`match_limit_cap`] arms (a `WHERE`-bearing MATCH arms the
/// plain cap and its sink filter together — the two ends of one policy,
/// stated here so they cannot drift). The contracted kernels are the
/// policy's third end, exact without a sink filter: their compiled
/// admissions are their WHERE (a conjunct outside the closed vocabulary
/// declines the kernel), so only kept rows land — see
/// [`MatchCap::plain_want`].
pub(crate) fn sink_filters_clause(
    projection: Option<&SinkProjection<'_>>,
    cap: Option<&MatchCap<'_>>,
    where_clause: Option<&Expr>,
) -> bool {
    projection.is_some() || (cap.is_some() && where_clause.is_some())
}

/// The one cap-arming entry the dispatcher reads, and the one place the
/// MATCH clause's own `WHERE` policy is stated (LABS-853): a
/// `WHERE`-bearing MATCH arms only the plain cap. The plain cap's count
/// stays exact because `match_clause` runs the clause filter in the
/// walk's sink whenever a cap rides a `WHERE` (a filter-only reduce), so
/// only kept rows are offered. The ordered cap's
/// walk-order-equals-sort-order contract (ADR-0035) and the distinct
/// cap's key count have not been verified over a filtered walk, so a
/// `WHERE` declines both.
pub(crate) fn match_limit_cap<'a>(
    suffix: &'a [Clause],
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    graph: &ExecGraph<'_>,
    params: &'a BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    input_rows: usize,
) -> Result<Option<MatchCap<'a>>> {
    if where_clause.is_some() {
        return Ok(plain_limit_cap(suffix, graph, params, cypher_version)?.map(MatchCap::Plain));
    }
    if let Some(o) = ordered_limit_cap(suffix, patterns, graph, params, cypher_version, input_rows)?
    {
        return Ok(Some(MatchCap::Ordered(o)));
    }
    if let Some(p) = plain_limit_cap(suffix, graph, params, cypher_version)? {
        return Ok(Some(MatchCap::Plain(p)));
    }
    Ok(distinct_limit_cap(suffix, graph, params, cypher_version)?.map(MatchCap::Distinct))
}
