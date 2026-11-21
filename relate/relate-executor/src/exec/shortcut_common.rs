//! The plumbing every statistics short-circuit shares (LABS-370, ADR-0589,
//! ADR-0611): the one-node pattern population both recognizers start from,
//! the one count-expression grammar, the grouped finish both grouped
//! answerers end in, and the charged first-seen group table both fold into.
//! The ladder that orders the short-circuits is
//! [`stats_short_circuit`](super::stats_short_circuit); this module holds
//! what the rungs read, never the ordering.

use std::collections::HashSet;

use indexmap::IndexMap;
use relate_core::{Result, Value};
use relate_ir::{Clause, Expr, MatchMode, OrderItem, PathPattern, Projection};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::group_key::{row_key, GroupKey};
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;

use super::order::{park_order_keys, OrderKeys};
use super::project::{project_rows, return_result, ProjectionInput};
use super::scope::ProjectionFields;
use super::QueryResult;

/// The population a pattern node's own shape can answer a whole-count
/// for — the one classification both node arms share (LABS-851's
/// structure round: one enumeration of the predicate-bearing
/// `NodePattern` fields, not two that must stay in sync — round 3,
/// F-1-3's enumeration lives here once, so a new predicate-bearing field
/// lands here and both arms follow):
///
/// * [`Pop::Label`] — exactly one label, no properties, no inline WHERE,
///   and a label expression the counts can mirror (a statically
///   mirroring expression — the raw front-lowered shape — is accepted,
///   round 4, F-4-1; a multi-label node needs label-set intersections
///   the per-label counts cannot answer).
/// * [`Pop::Any`] — no label and no label expression at all (a label
///   expression on an unlabeled node is dynamic or degenerate; the
///   whole-population count cannot mirror it) — the whole node
///   population, the label-less nodes included.
/// * [`Pop::No`] — anything else: the scan's filters own the answer.
pub(super) enum Pop<'a> {
    Label(&'a SmallName),
    Any,
    No,
}

pub(super) fn node_population(np: &relate_ir::NodePattern) -> Pop<'_> {
    // The node-level predicate fields the counts never check, one
    // spelling: no properties, no inline WHERE.
    let plain = np.properties.is_none() && np.where_clause.is_none();
    if np.labels.len() == 1 && plain && super::label_expr_ok(&np.label_expr, &np.labels) {
        Pop::Label(&np.labels[0].0)
    } else if np.labels.is_empty() && np.label_expr.is_none() && plain {
        Pop::Any
    } else {
        Pop::No
    }
}

/// The clause-level plain-`MATCH` gate: a non-optional `MATCH` in the
/// default relationship mode, no `WHERE`, no `SEARCH`. The clause-level
/// vocabulary the whole-population recognizers and the batch-tail
/// classifier share — the pattern-level gates (single pattern, no GPM, no
/// inline predicate) stay with each caller.
pub(super) fn plain_match_patterns(clause: &Clause) -> Option<&[PathPattern]> {
    let Clause::Match {
        optional: false,
        match_mode: MatchMode::DifferentRelationships,
        patterns,
        where_clause: None,
        search: None,
        using_index: _,
        using_join: _,
    } = clause
    else {
        return None;
    };
    Some(patterns)
}

/// The one pattern of a plain whole-population `MATCH` — the recognizer
/// ladder's first rung, shared by every short-circuit shape: the clause gate
/// ([`plain_match_patterns`]) plus exactly one pattern the IR's one
/// predicate-bearing-shape test admits (`needs_gpm`: no GPM-only element
/// sequence, no path WHERE, no path mode, no selector; round 3, F-1-3 — the
/// IR predicate is the one enumeration, so a new predicate-bearing field
/// lands there and both this rung and the batch-tail classifier follow).
pub(super) fn plain_match_pattern(clause: &Clause) -> Option<&PathPattern> {
    let [pattern] = plain_match_patterns(clause)? else {
        return None;
    };
    if pattern.needs_gpm() {
        return None;
    }
    Some(pattern)
}

/// The `MATCH` half of a plain `[Match, Return]` statement — every
/// short-circuit recognizer starts here, so the two-clause shape and the
/// pattern gate are spelled once: exactly two clauses, the first a plain
/// whole-population `MATCH` ([`plain_match_pattern`]). The `Return` half
/// stays with each recognizer — what it gates differs (the count-only
/// shape refuses ordering and paging, the grouped shapes carry them).
pub(super) fn match_return_pattern(clauses: &[Clause]) -> Option<&PathPattern> {
    if clauses.len() != 2 {
        return None;
    }
    plain_match_pattern(&clauses[0])
}

/// The bound variable and [`Pop`] of a one-node, relationship-free
/// pattern — the one destructuring both population filters below share.
/// `None` for an anonymous node and every multi-node or relationship shape.
fn single_node_population(pattern: &PathPattern) -> Option<(&SmallName, Pop<'_>)> {
    let ([node], true) = (&pattern.nodes[..], pattern.rels.is_empty()) else {
        return None;
    };
    Some((node.variable.as_ref()?, node_population(node)))
}

/// The variable of a one-node, relationship-free pattern whose node is
/// the whole node population ([`Pop::Any`]: no label, no property, no
/// inline WHERE). `None` for an anonymous node and every other shape.
pub(super) fn whole_population_node_var(pattern: &PathPattern) -> Option<&SmallName> {
    match single_node_population(pattern)? {
        (var, Pop::Any) => Some(var),
        (_, Pop::Label(_) | Pop::No) => None,
    }
}

/// The variable and label of a one-node, relationship-free pattern whose
/// node is exactly one plain label ([`Pop::Label`]: no property, no inline
/// WHERE). `None` for an anonymous node and every other shape.
pub(super) fn single_label_node(pattern: &PathPattern) -> Option<(&SmallName, &SmallName)> {
    match single_node_population(pattern)? {
        (var, Pop::Label(label)) => Some((var, label)),
        (_, Pop::Any | Pop::No) => None,
    }
}

/// The one count-expression grammar's three states, named (ADR-0217: the
/// aggregate vocabulary's own classifier, not a re-derived name literal):
/// [`Count::Star`] is `count(*)`, [`Count::Of`] a plain `count(var)`, and
/// `None` every other expression — the one spelling every count-shaped
/// consumer (`is_node_count`, the count-only recognizer) derives from.
#[derive(Clone, Copy, Debug)]
pub(super) enum Count<'a> {
    Star,
    Of(&'a SmallName),
}
pub(super) fn count_of(expr: &Expr) -> Option<Count<'_>> {
    match expr {
        Expr::CountStar => Some(Count::Star),
        Expr::Function {
            name,
            distinct: false,
            args,
        } if name.with_str(relate_ir::classify_aggregate)
            == Some(relate_ir::AggregateKind::Count) =>
        {
            match &args[..] {
                [Expr::Variable(v)] => Some(Count::Of(v)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `count(*)`, or `count(var)` over the pattern's own node (bound on every
/// row, so equal to `count(*)`) — the grouped recognizers' count arm,
/// derived from the one [`count_of`] grammar.
pub(super) fn is_node_count(expr: &Expr, var: &SmallName) -> bool {
    match count_of(expr) {
        Some(Count::Star) => true,
        Some(Count::Of(v)) => v == var,
        None => false,
    }
}

/// The paging half of a grouped finish: the ORDER BY already rewritten
/// onto output columns, and the statement's own SKIP and LIMIT — the one
/// carrier of the three facts every grouped shape holds (a recognizer
/// that admits ordering carries them here, never as three loose fields).
pub(super) struct GroupedPage<'a> {
    /// The ORDER BY rewritten onto output columns — owned here, the one
    /// carrier of the paging half.
    pub(super) order: Vec<OrderItem>,
    pub(super) skip: Option<&'a Expr>,
    pub(super) limit: Option<&'a Expr>,
}

/// The finish both grouped-aggregate shortcuts share: ordering and paging
/// over the grouped rows — rows that already carry every output column
/// ([`ProjectionInput::SinkProjected`]: the projector's one pre-projected
/// pass-through owns the policy, so the finish states the fact through
/// the source instead of fabricating projections it can evaluate) — with
/// an identity projection of the output columns declaring the column
/// shape the projector's class reads, the ORDER BY already rewritten
/// onto them.
pub(super) fn finish_grouped_rows(
    ctx: ExecCtx<'_>,
    clauses: &[Clause],
    items: &[Projection],
    page: GroupedPage<'_>,
    mut grouped: Vec<Row>,
    scope: Option<&Row>,
) -> Result<(Vec<Row>, QueryResult)> {
    // The identity projection is a column declaration only — one
    // `Variable` per output name, never evaluated — and the names come
    // from the one walk ([`projection_names`]), not a second
    // `enumerate`/`output_name` pass.
    let identity: Vec<Projection> = super::scope::projection_names(items)
        .into_iter()
        .map(|name| Projection {
            expr: Expr::Variable(SmallName::from_name(&name)),
            alias: Some(SmallName::from_name(&name)),
            source_text: None,
        })
        .collect();
    // The pre-projected surface's parked sort keys: the sink parks its
    // rows' keys before handing them over, so the grouped finish parks
    // its own — every key already rewritten onto an output column, so
    // parking copies the column under its `__ord*` name.
    let order_keys = OrderKeys::new(&page.order, &identity, false);
    for row in &mut grouped {
        park_order_keys(&order_keys, row, |expr, scope| {
            crate::eval::eval(ctx.graph, scope, expr, ctx.params)
        })?;
    }
    let mut rows = project_rows(
        ctx,
        ProjectionInput::SinkProjected(grouped),
        &ProjectionFields {
            items: &identity,
            distinct: false,
            group_by: None,
            order: &page.order,
            skip: page.skip,
            limit: page.limit,
            where_clause: None,
        },
        true,
        scope,
    )?;
    let result = return_result(ctx.graph, items, clauses, &mut rows)?;
    Ok((rows, result))
}

/// The charged first-seen group table both grouped shortcuts fold into —
/// the same container shape `agg_fold`'s group table is (an
/// insertion-ordered map keyed by the group identity; ADR-0463: retained,
/// charged as entries land), specialized to a bare count fold: one
/// first-seen key payload and a summed count per group. One container,
/// not a position map beside a vector.
pub(super) struct CountedGroups {
    table: IndexMap<Vec<GroupKey>, (Vec<Value>, u64)>,
}

impl CountedGroups {
    pub(super) fn new() -> Self {
        CountedGroups {
            table: IndexMap::new(),
        }
    }

    /// Add one member to the group `keys` name — the identity is derived
    /// from the keys (the one derivation) and the landing goes through
    /// [`Self::land`], the one insert-or-bump.
    pub(super) fn add(
        &mut self,
        graph: &ExecGraph<'_>,
        retained: &mut RetainedTotal,
        keys: Vec<Value>,
        count: u64,
    ) -> Result<()> {
        // The canonical row identity (the one DISTINCT/UNION read).
        let identity = row_key(&keys);
        self.land(graph, retained, &identity, || keys, count)
    }

    /// The single-key landing the index-aggregate walk takes per posted
    /// node: merges on the identity derived from the value directly, and
    /// only a new group materializes the payload — the value clones once
    /// per distinct group, never once per posting.
    pub(super) fn add_one(
        &mut self,
        graph: &ExecGraph<'_>,
        retained: &mut RetainedTotal,
        value: &Value,
        count: u64,
    ) -> Result<()> {
        let identity = GroupKey::new(value);
        self.land(
            graph,
            retained,
            std::slice::from_ref(&identity),
            || vec![value.clone()],
            count,
        )
    }

    /// The one insert-or-bump: an existing group's count is checked and
    /// bumped — the payload closure never runs on that path; a new group
    /// is charged through the canonical [`RetainedTotal::add_group_landing`]
    /// (a zero-seat table: the count is inline) and lands with its
    /// first-seen payload, materialized by the closure only now.
    fn land(
        &mut self,
        graph: &ExecGraph<'_>,
        retained: &mut RetainedTotal,
        identity: &[GroupKey],
        keys: impl FnOnce() -> Vec<Value>,
        count: u64,
    ) -> Result<()> {
        if let Some((_, n)) = self.table.get_mut(identity) {
            *n = n
                .checked_add(count)
                .expect("count overflow: a group sum past u64 is a broken store");
            return Ok(());
        }
        let keys = keys();
        // The canonical landing charges the entry and the value slots;
        // this table also *retains the payload values themselves* (a
        // group's first-seen keys are heap — a string, a list), so their
        // owned heap is charged here, ahead of the insert so a refusal
        // never leaves the group landed.
        for key in &keys {
            retained.add_owned(graph, key)?;
        }
        retained.add_group_landing(
            graph,
            self.table.len(),
            identity,
            keys.len(),
            crate::overlay::SeatShape::ZERO_SEATS,
        )?;
        self.table.insert(identity.to_vec(), (keys, count));
        Ok(())
    }

    /// The groups in first-seen order, each with its summed count — the
    /// scan's grouping order (the aggregate's own emission order), which
    /// an unordered `ORDER BY`-less statement keeps.
    pub(super) fn into_rows(
        self,
        graph: &ExecGraph<'_>,
        retained: &mut RetainedTotal,
        mut row_of: impl FnMut(Vec<Value>, u64) -> Row,
    ) -> Result<Vec<Row>> {
        let mut rows = Vec::with_capacity(self.table.len());
        for (_, (keys, count)) in self.table {
            let row = row_of(keys, count);
            retained.add_row(graph, row.values())?;
            rows.push(row);
        }
        Ok(rows)
    }

    /// The summed count across every group — the members the fold has
    /// counted so far, under the same overflow invariant a group's own
    /// sum states.
    pub(super) fn total_count(&self) -> u64 {
        self.table.values().fold(0u64, |total, (_, n)| {
            total
                .checked_add(*n)
                .expect("count overflow: a group sum past u64 is a broken store")
        })
    }
}

/// The one count emission: a group's summed count as the answer's Int —
/// the conversion and its invariant stated once for every shortcut that
/// answers a `count(*)` (the fold rung, the grouped rows, the label-set
/// groups).
pub(super) fn count_value(count: u64) -> Value {
    Value::Int(i64::try_from(count).expect("a group count past i64 is a broken store"))
}

/// One grouped row under the shared emission policy: the summed count in
/// its own column — `count_at` among `names`, resolved once by the
/// recognizer — and each group key in the next other column, in name
/// order. The two grouped shortcuts (the index aggregate, the label-set
/// groups) hand their keys in; the placement is stated here and nowhere
/// else.
pub(super) fn grouped_row(
    names: &[String],
    count_at: usize,
    mut keys: Vec<Value>,
    count: u64,
) -> Row {
    // The count's column is `count_at`; the keys fill the others in name
    // order — the placement stated as one insertion, never per-item
    // optionality.
    keys.insert(count_at, count_value(count));
    // The recognizer guarantees one count column and one per key item;
    // checked in every build — zip would silently truncate a broken
    // shape in a release build.
    assert_eq!(
        names.len(),
        keys.len(),
        "one count column and one per key item"
    );
    names.iter().cloned().zip(keys).collect()
}

/// Round 3, F-1-4 (the count-only recognizer's shape guard): repeated
/// node variables (a same-variable self-loop shape, `(a:A)-[:R]->(a:A)`)
/// would answer from the (A,R,A) pair count, which includes
/// distinct-endpoint rels the scan's same-variable shape excludes.
pub(super) fn node_vars_unique(pattern: &PathPattern) -> bool {
    let mut node_vars: HashSet<&SmallName> = HashSet::new();
    pattern
        .nodes
        .iter()
        .filter_map(|n| n.variable.as_ref())
        .all(|v| node_vars.insert(v))
}
