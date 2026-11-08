//! The match sink's projection: a following `RETURN`/`WITH` the last pattern
//! of a `MATCH` evaluates per path, keeping only the projected row. Split
//! from `project.rs` (the order family's precedent): the sink is the
//! projection's sibling concern — it decides whether the clause can finish in
//! the walk at all — while `project.rs` owns the ordinary `RETURN`/`WITH`
//! (result shape, DISTINCT, paging, `project_rows`).

use std::collections::{BTreeMap, HashSet};

use relate_core::{Result, Value};
use relate_ir::{Clause, Projection};

use crate::eval::{eval, expr_vars, is_agg_expr};
use crate::overlay::ExecGraph;
use crate::row::{Row, RowNames};

use super::order::pages_eagerly;
use super::order::{parked_values, OrderKeys};
use super::scope::{
    is_star_item, projection_collides_with_internal_names, projection_names,
    projection_repeats_output_name, ProjectionFields,
};

/// A following `RETURN`/`WITH` the match sink can evaluate per path.
///
/// Eager `SKIP`/`LIMIT` (no `ORDER BY`, `DISTINCT`, or `WITH` filter) stays
/// on the ordinary projection so a later row's error is not raised (CIP-27).
pub(crate) struct SinkProjection<'a> {
    items: &'a [Projection],
    /// The clause's `ORDER BY` keys, parked per row by
    /// [`finish_match_row`]. The sink declines an explicit `GROUP BY` and an
    /// aggregate item, so it only ever sees the implicit-grouping decision.
    order: OrderKeys<'a>,
    names: RowNames,
}

impl<'a> SinkProjection<'a> {
    fn new(items: &'a [Projection], order: &'a [relate_ir::OrderItem]) -> Self {
        // The sink declines a `*` item and any repeated resolved name, so the
        // filtered name list aligns with `items` index for index and the
        // output schema opens with the items' columns in that order.
        let mut names = projection_names(items);
        debug_assert_eq!(names.len(), items.len());
        let order = OrderKeys::new(order, items, false);
        for index in 0..order.len() {
            names.push(order.name(index).to_string());
        }
        Self {
            items,
            order,
            names: RowNames::new(&names),
        }
    }

    /// The projected items' columns: the output schema's leading
    /// `items.len()` names, in item order — the split [`Self::new`] builds.
    fn item_names(&self) -> &[String] {
        &self.names.names()[..self.items.len()]
    }
}

/// The projection a `MATCH` may finish in its sink, when the next clause
/// is a non-aggregate `RETURN` or `WITH`.
pub(crate) fn sinkable_projection(clause: &Clause) -> Option<SinkProjection<'_>> {
    let ProjectionFields {
        distinct,
        items,
        group_by,
        order,
        skip,
        limit,
        where_clause,
    } = ProjectionFields::from_clause(clause)?;
    if group_by.is_some()
        || items
            .iter()
            .any(|item| is_star_item(item) || is_agg_expr(&item.expr))
        // The `__` namespace reservation: an alias the sink would park
        // beside cannot be told apart from engine-internal state, so the
        // sink declines and the ordinary projection keeps the column.
        || projection_collides_with_internal_names(items)
        // Two items may resolve to one output name even though the
        // front-end's conflict check (which compares verbatim names) passed
        // them — writing them over one row would collapse them. Decline.
        || projection_repeats_output_name(items)
        // ADR-0453: an aggregating subclause (ORDER BY key or WHERE
        // predicate) recomputes over a group — the sink parks keys per
        // incoming row, so it declines and the aggregation path answers.
        || order.iter().any(|o| is_agg_expr(&o.expr))
        || where_clause.is_some_and(is_agg_expr)
    {
        return None;
    }
    // A `WITH` filter runs later, on the projected row. When it names a
    // variable the projection does not produce, the ordinary path has to
    // keep that binding (pre-projection scope).
    if let Some(predicate) = where_clause {
        let mut named = HashSet::new();
        expr_vars(predicate, &mut named);
        let produced: HashSet<String> = projection_names(items).into_iter().collect();
        if named.iter().any(|name| !produced.contains(name)) {
            return None;
        }
    }
    if pages_eagerly(&ProjectionFields {
        distinct,
        items,
        group_by,
        order,
        skip,
        limit,
        where_clause,
    }) {
        return None;
    }
    Some(SinkProjection::new(items, order))
}

/// Evaluate `projection` against `row` and keep only those values, plus the
/// order keys the later sort reads. `row` itself is not retained.
pub(crate) fn finish_match_row(
    graph: &ExecGraph<'_>,
    row: &Row,
    projection: &SinkProjection<'_>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Row> {
    // Each item evaluated once against the incoming row. The result
    // columns are the projection's precomputed names, so the row's schema
    // is an `Arc` clone.
    let item_values = projection
        .items
        .iter()
        .map(|item| eval(graph, row, &item.expr, params, cypher_version))
        .collect::<Result<Vec<_>>>()?;
    // `ORDER BY` keys read the projected row: the incoming bindings with
    // each item's value written over its output name. Without a key there
    // is no scope row to build — the parked values follow the items, and
    // the output schema already carries their columns.
    let parked = if projection.order.is_empty() {
        Vec::new()
    } else {
        // shortcut: the scope schema depends on the incoming row's, so this
        // path still appends (and interns) per row; cache a per-sink scope
        // schema if ORDER BY lands on a hot expansion.
        let mut scope = row.clone();
        for (index, name) in projection.item_names().iter().enumerate() {
            scope.insert(name.clone(), item_values[index].clone());
        }
        parked_values(&projection.order, &scope, |expr, scope| {
            eval(graph, scope, expr, params, cypher_version)
        })
    };
    let mut values = item_values;
    values.extend(parked);
    Ok(Row::from_names(&projection.names, values))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use relate_front::parse_and_lower;
    use relate_ir::Query;

    use crate::exec::test_support::exec_cypher;
    use crate::graph::Graph;
    use crate::row::interned_schema_calls;

    use super::*;

    /// The sink projection a lone `RETURN` clause yields — the shape the
    /// match sink finishes each row with.
    fn projection_of(query: &Query) -> SinkProjection<'_> {
        sinkable_projection(&query.parts[0].clauses[0]).expect("the RETURN clause is sinkable")
    }

    /// A row binding `n` to a map with the given properties.
    fn row_with_n(props: &[(&str, Value)]) -> Row {
        let map = props
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        let mut row = Row::new();
        row.insert("n".to_string(), Value::Map(Arc::new(map)));
        row
    }

    fn finish(cypher: &str, row: &Row) -> Row {
        let query = parse_and_lower(cypher).unwrap();
        let projection = projection_of(&query);
        let graph = Graph::default();
        let view = ExecGraph::read(&graph);
        finish_match_row(&view, row, &projection, &BTreeMap::new(), None).unwrap()
    }

    fn text(s: &str) -> Value {
        Value::String(Arc::from(s))
    }

    #[test]
    fn every_item_evaluates_against_the_incoming_row() {
        // The output column `age` already exists on the incoming row; the
        // item must read the incoming `n.age`, never the column it writes.
        let mut row = row_with_n(&[("age", Value::Int(3))]);
        row.insert("age".to_string(), Value::Int(99));
        let row = finish("RETURN n.age + 1 AS age, n.age AS other", &row);
        assert_eq!(row.values(), [Value::Int(4), Value::Int(3)]);
    }

    #[test]
    fn an_alias_shadows_the_incoming_binding_of_that_name() {
        let row = finish("RETURN n.name AS n", &row_with_n(&[("name", text("x"))]));
        assert_eq!(row.values(), [text("x")]);
        assert_eq!(row.get("n"), Some(&text("x")));
    }

    #[test]
    fn an_order_key_reads_bindings_outside_the_projection() {
        // `n` is not a projected column, so the key needs the scope row.
        let row = finish(
            "RETURN n.name AS name ORDER BY n.age",
            &row_with_n(&[("name", text("x")), ("age", Value::Int(7))]),
        );
        assert_eq!(row.values(), [text("x"), Value::Int(7)]);
        assert_eq!(row.get("__ord0"), Some(&Value::Int(7)));
    }

    #[test]
    fn an_order_key_naming_a_projected_column_reads_that_column() {
        // `age` is a bare variable naming a projected column, so the key is
        // alias-scoped: it reads the projected value, not the item.
        let row = finish(
            "RETURN n.age AS age ORDER BY age",
            &row_with_n(&[("age", Value::Int(3))]),
        );
        assert_eq!(row.values(), [Value::Int(3), Value::Int(3)]);
        assert_eq!(row.get("__ord0"), Some(&Value::Int(3)));
    }

    #[test]
    fn a_verbatim_order_key_reads_the_projected_column() {
        // `ORDER BY n.age` is verbatim the item, so the implicit-grouping
        // resolution answers it from that item's column.
        let row = finish(
            "RETURN n.age AS k ORDER BY n.age",
            &row_with_n(&[("age", Value::Int(3))]),
        );
        assert_eq!(row.values(), [Value::Int(3), Value::Int(3)]);
    }

    /// Finish `cypher`'s sink row `times` times, asserting every result,
    /// and report how many schemas those runs interned.
    fn interning_rows(cypher: &str, times: usize) -> usize {
        let query = parse_and_lower(cypher).unwrap();
        let projection = projection_of(&query);
        let graph = Graph::default();
        let view = ExecGraph::read(&graph);
        let row = row_with_n(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let before = interned_schema_calls();
        for _ in 0..times {
            let out = finish_match_row(&view, &row, &projection, &BTreeMap::new(), None).unwrap();
            assert_eq!(out.values(), [Value::Int(1), Value::Int(2)]);
        }
        interned_schema_calls() - before
    }

    /// The projection path's output schema is the sink's, built once: a row
    /// finished a thousand times must not intern a schema even once. (The
    /// previous per-row `insert` into a cloned input row interned one per
    /// new-name item — two per row for this query.)
    #[test]
    fn a_keyless_projection_row_interns_no_schema() {
        assert_eq!(
            interning_rows("RETURN n.a AS a, n.b AS b", 1_000),
            0,
            "a keyless projection row interns a schema per row"
        );
    }

    /// The documented counterpart: the `ORDER BY` path builds a scope row
    /// whose schema extends the incoming row's, so it interns once per row
    /// per new-name item. Pinned so the shortcut is a known cost, and the
    /// test flips if the deferred per-sink scope cache lands.
    #[test]
    fn an_ordered_projection_row_interns_its_scope_schema() {
        assert_eq!(
            interning_rows("RETURN n.a AS a ORDER BY n.b", 1_000),
            1_000,
            "the ordered scope row's schema is the documented per-row cost"
        );
    }

    /// `n.name` and `` n.`name` `` differ verbatim, so the front-end's
    /// ColumnNameConflict check passes them, but they resolve to one output
    /// name. The sink must decline rather than write both over one row.
    #[test]
    fn a_projection_repeating_a_resolved_column_name_declines_the_sink() {
        let query = parse_and_lower("RETURN n.name, n.`name`").unwrap();
        assert!(
            sinkable_projection(&query.parts[0].clauses[0]).is_none(),
            "the sink must decline a projection with one resolved column name twice"
        );

        // The ordinary projection answers the query instead. Its result
        // shape is not this path's: the shared result-column dedup
        // (`return_columns`) already keeps one column for a repeated
        // resolved name, and the collapse downstream of it is the
        // pre-existing LABS-1135 bug, not the sink's. Pinned here only so
        // the decline's end-to-end answer stays visible.
        let graph = Graph::default();
        graph.begin_write().unwrap();
        graph
            .create_node(
                vec!["A".to_string()],
                BTreeMap::from([("name".to_string(), text("x"))]),
            )
            .unwrap();
        graph.commit_write();
        let result = exec_cypher(&graph, "MATCH (n:A) RETURN n.name, n.`name`");
        assert_eq!(result.columns, ["n.name"]);
        assert_eq!(result.rows.len(), 1);
    }
}
