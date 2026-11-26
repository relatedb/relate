//! ADR-0689: a seeded count-only body answers from the maintained counts
//! once its dynamic names resolve against the seed, equal to the scan, and
//! declines every shape it cannot resolve.

use std::collections::BTreeMap;

use super::seeded_count::resolve_seed_names;
use super::stats_short_circuit::stats_short_circuit;
use super::test_support::{clauses_of, exec_cypher};
use crate::ctx::ExecCtx;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::Row;
use relate_core::Value;

fn text(s: &str) -> Value {
    Value::String(s.into())
}

fn graph() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a1 = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let a2 = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    g.create_rel(a1, b, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(a2, b, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(a1, a2, "S".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    g
}

fn seed(value: Value) -> Row {
    Row::from([("x".to_string(), value)])
}

/// The count the short-circuit serves for `cypher` under `seed`, or `None`
/// when it declines to the scan.
fn served(g: &Graph, cypher: &str, seed: Row) -> Option<Value> {
    served_on(&ExecGraph::read(g), cypher, seed)
}

/// [`served`] over an explicit view: resolve the names, then ask the one
/// short-circuit ladder.
fn served_on(view: &ExecGraph<'_>, cypher: &str, seed: Row) -> Option<Value> {
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(view, &params);
    let clauses = clauses_of(cypher);
    let resolved = resolve_seed_names(ctx, &clauses, &seed);
    stats_short_circuit(ctx, &resolved, &[seed], None)
        .unwrap()
        .map(|(_, result)| result.rows[0][0].clone())
}

#[test]
fn dynamic_label_and_type_resolve_against_the_seed() {
    let g = graph();
    let label = "MATCH (n:$(x)) RETURN count(n) AS c";
    assert_eq!(served(&g, label, seed(text("A"))), Some(Value::Int(2)));
    assert_eq!(served(&g, label, seed(text("B"))), Some(Value::Int(1)));
    assert_eq!(served(&g, label, seed(text("Nope"))), Some(Value::Int(0)));
    let ty = "MATCH ()-[r:$(x)]->() RETURN count(*) AS c";
    assert_eq!(served(&g, ty, seed(text("R"))), Some(Value::Int(2)));
    assert_eq!(served(&g, ty, seed(text("S"))), Some(Value::Int(1)));
}

#[test]
fn unresolvable_shapes_decline() {
    let g = graph();
    let label = "MATCH (n:$(x)) RETURN count(n) AS c";
    // A list of names has no single-label count; a non-string fails in the scan.
    let list = Value::List(vec![text("A"), text("B")].into());
    assert_eq!(served(&g, label, seed(list)), None);
    assert_eq!(served(&g, label, seed(Value::Int(1))), None);
    // A pattern variable that the seed already binds reads the bound entity.
    let bound = Row::from([("n".to_string(), Value::Null), ("x".to_string(), text("A"))]);
    assert_eq!(served(&g, label, bound), None);
}

#[test]
fn the_subquery_family_equals_its_static_spelling() {
    let g = graph();
    let dynamic = exec_cypher(
        &g,
        "CALL db.labels() YIELD label CALL (label) { MATCH (n:$(label)) RETURN count(n) AS c } \
         RETURN label, c ORDER BY label",
    );
    let counts: Vec<_> = dynamic
        .rows
        .iter()
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect();
    assert_eq!(
        counts,
        vec![(text("A"), Value::Int(2)), (text("B"), Value::Int(1)),]
    );
    let types = exec_cypher(
        &g,
        "CALL db.relationshipTypes() YIELD relationshipType AS t \
         CALL (t) { MATCH ()-[r:$(t)]->() RETURN count(*) AS c } RETURN t, c ORDER BY t",
    );
    let counts: Vec<_> = types
        .rows
        .iter()
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect();
    assert_eq!(
        counts,
        vec![(text("R"), Value::Int(2)), (text("S"), Value::Int(1))]
    );
}

/// A write-routed statement (every statement with a `CALL { … }` is) runs
/// on a scratch overlay: while that scratch has logged nothing the base
/// counts are exact and serve; after its first write they decline and the
/// scan reads the statement's own writes.
#[test]
fn a_pristine_scratch_serves_and_a_written_one_declines() {
    use super::test_support::{exec_cypher_on, exec_write, write_scratch};
    let g = graph();
    let scratch = write_scratch(&g);
    let label = "MATCH (n:$(x)) RETURN count(n) AS c";
    let view = ExecGraph::write(&g, &scratch);
    assert_eq!(
        served_on(&view, label, seed(text("A"))),
        Some(Value::Int(2))
    );
    exec_write(&g, &scratch, "CREATE (:A)").unwrap();
    let view = ExecGraph::write(&g, &scratch);
    assert_eq!(served_on(&view, label, seed(text("A"))), None);
    let r = exec_cypher_on(&view, "MATCH (n:A) RETURN count(n) AS c");
    assert_eq!(r.rows[0][0], Value::Int(3));
}

/// The `WHERE n:$(x)` spelling folds into the node's label and serves from
/// the counts like the pattern spelling; a label list becomes the
/// conjunction, which the counts decline and the label-index scan answers
/// equal to the static spelling.
#[test]
fn where_and_multi_label_spellings_resolve() {
    let g = graph();
    let w = "MATCH (n) WHERE n:$(x) RETURN count(n) AS c";
    assert_eq!(served(&g, w, seed(text("A"))), Some(Value::Int(2)));
    let both = Value::List(vec![text("A"), text("B")].into());
    let multi = "MATCH (n:$(x)) RETURN count(n) AS c";
    assert_eq!(served(&g, multi, seed(both.clone())), None);
    let view = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(&view, &params);
    let clauses = clauses_of(multi);
    let resolved = resolve_seed_names(ctx, &clauses, &seed(both));
    assert!(
        matches!(resolved, std::borrow::Cow::Owned(_)),
        "list resolves"
    );
    let static_rows = exec_cypher(&g, "MATCH (n:A:B) RETURN count(n) AS c").rows;
    let (_, last) = super::dispatch::run_nested_clauses(
        &view,
        &resolved,
        &params,
        vec![Row::new()],
        crate::ctx::QueryCtx::new(""),
    )
    .unwrap();
    assert_eq!(last.expect("result").rows[0][0], static_rows[0][0]);
}

/// A GPM-shaped pattern (here an inline node predicate) is outside the
/// rewrite: its dynamic operand stays dynamic and its `WHERE` is kept, so
/// the answer equals the scan's, which still applies both.
#[test]
fn a_gpm_shaped_pattern_keeps_its_where_and_its_operand() {
    use std::borrow::Cow;
    let g = graph();
    let view = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(&view, &params);
    let q = "MATCH (n WHERE n:B) WHERE n:$(x) RETURN count(n) AS c";
    let clauses = clauses_of(q);
    assert!(matches!(
        resolve_seed_names(ctx, &clauses, &seed(text("A"))),
        Cow::Borrowed(_)
    ));
    // No node is both A and B: the kept predicates give 0, where a dropped
    // WHERE would count the 1 B node.
    let r = exec_cypher(&g, "UNWIND ['A'] AS x CALL (x) { MATCH (n WHERE n:B) WHERE n:$(x) RETURN count(n) AS c } RETURN c");
    assert_eq!(r.rows[0][0], Value::Int(0));
}

/// A naked body may re-project an imported name; a `MATCH` after that
/// `WITH` reads the new value, so the seed must not resolve it.
#[test]
fn a_rebound_import_is_not_resolved_from_the_seed() {
    let g = graph();
    let r = exec_cypher(
        &g,
        "UNWIND ['A'] AS x CALL { WITH x WITH 'B' AS x MATCH (n:$(x)) RETURN count(n) AS c } RETURN c",
    );
    assert_eq!(r.rows[0][0], Value::Int(1));
}
