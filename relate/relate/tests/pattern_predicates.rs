//! Pattern predicates and path selectors: inline element `WHERE`s inside
//! pattern comprehensions, a named subpath beside the path variable, and
//! the scoping forms a subquery expression imports.

mod common;

use relate::Value;

use common::connection;

fn int(n: i64) -> Value {
    Value::Int(n)
}

#[test]
fn pattern_comprehension_inline_node_predicate_filters() {
    let mut conn = connection();
    conn.query(
        "CREATE (:A {prop: 1})-[:R]->(:B), (:A {prop: 2})-[:R]->(:B), \
         (:A {prop: 3})-[:R]->(:B)",
    )
    .unwrap();
    let result = conn
        .query("RETURN [(a:A WHERE a.prop > 1)-[r]-(b:B) | a.prop] AS result")
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::List(vec![int(2), int(3)].into())]]
    );
}

#[test]
fn pattern_comprehension_predicate_reads_a_later_element() {
    let mut conn = connection();
    conn.query(
        "CREATE (:A {prop: 1})-[:R]->(:B {prop: 100}), (:A {prop: 2})-[:R]->(:B {prop: 200})",
    )
    .unwrap();
    let result = conn
        .query("RETURN [(a:A WHERE b.prop > 100)-[r]-(b:B) | [a.prop, b.prop]] AS result")
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::List(
            vec![Value::List(vec![int(2), int(200)].into())].into()
        )]]
    );
}

#[test]
fn pattern_comprehension_relationship_predicate_filters() {
    let mut conn = connection();
    conn.query("CREATE (:A)-[:R {prop: 1}]->(:B), (:A)-[:R {prop: 2}]->(:B)")
        .unwrap();
    let result = conn
        .query("RETURN [(a:A)-[r:R WHERE r.prop > 1]-(b:B) | r.prop] AS result")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::List(vec![int(2)].into())]]);
}

#[test]
fn path_and_named_subpath_bind_the_same_path() {
    let mut conn = connection();
    conn.query("CREATE (:A)-[:R]->()-[:R]->(:B)").unwrap();
    let result = conn
        .query("MATCH p = ANY SHORTEST (q = (:A)-->+(:B)) RETURN p = q AS result")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(true)]]);
}

#[test]
fn expression_subquery_imports_an_unmentioned_call_scope_variable() {
    // The enclosing WITH projects only `p0`; `y` reaches the COLLECT body
    // solely through the inner naked CALL, which the static variable walk
    // does not enumerate. The projection must keep the whole incoming row.
    let mut conn = connection();
    conn.query("CREATE (:A), (:B)").unwrap();
    let result = conn
        .query(
            "MATCH (y) MATCH p0 = () \
             WITH p0, COLLECT { CALL { WITH y RETURN y AS y1 } RETURN y1 } AS c \
             RETURN size(c) AS n",
        )
        .unwrap();
    // The cross product has four rows; each COLLECT body yields exactly one
    // value, so every row's list has size one (the import is not dropped).
    assert_eq!(result.rows.len(), 4);
    assert!(result.rows.iter().all(|row| row == &vec![int(1)]));
}

#[test]
fn distinct_star_order_by_named_path() {
    let mut conn = connection();
    conn.query("CREATE (:A)-[:R]->(:B), (:B)-[:R]->(:A)")
        .unwrap();
    let result = conn
        .query("MATCH p = (a)-->(b) RETURN DISTINCT * ORDER BY p")
        .unwrap();
    assert_eq!(result.rows.len(), 2);
}

/// A pattern predicate's node pattern carries a label expression — the
/// same grammar a MATCH node pattern reads — so `WHERE (a:L1|L2)<-[:R]-(b)`
/// is a pattern predicate, not a parenthesized `HasLabel` the following
/// relationship breaks. The pattern-comprehension position admits the same
/// forms through the one lookahead.
#[test]
fn pattern_predicate_and_comprehension_admit_label_expressions() {
    let mut conn = connection();
    conn.query(
        "CREATE (:L1 {prop: 1})-[:R]->(:L2 {prop: 10}), \
         (:L3 {prop: 2})-[:R]->(:L2 {prop: 20}), \
         (:L2 {prop: 30})-[:R]->(:L1 {prop: 40})",
    )
    .unwrap();
    // The disjunction matches every L1-or-L2 target of an incoming R;
    // the :L3-target row stays out.
    let result = conn
        .query("MATCH (a), (b) WHERE (a:L1|L2)<-[:R]-(b) RETURN a.prop ORDER BY a.prop")
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![int(10)], vec![int(20)], vec![int(40)]]
    );
    // The simple-label contrast: only the :L1 target reaches.
    let result = conn
        .query("MATCH (a), (b) WHERE (a:L1)<-[:R]-(b) RETURN a.prop ORDER BY a.prop")
        .unwrap();
    assert_eq!(result.rows, vec![vec![int(40)]]);
    // The conjunction matches no node (nothing carries both labels).
    let result = conn
        .query("MATCH (a), (b) WHERE (a:L1&L2)-[:R]-(b) RETURN a.prop")
        .unwrap();
    assert_eq!(result.rows, Vec::<Vec<Value>>::new());
    // The repro's original EXPLAIN shape plans.
    conn.query("EXPLAIN MATCH (a:L1), (b:L2) WHERE (a:L1|L2)<-[:R]-(b) RETURN a")
        .unwrap();
    // A pattern comprehension's leading node pattern: both L1-or-L2
    // sources reach, in pattern order.
    let result = conn
        .query("RETURN [(x:L1|L2)-[:R]->(b) | b.prop] AS l")
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::List(vec![int(10), int(40)].into())]]
    );
}

/// The `exists(<pattern>)` form as a SET value: the predicate evaluates
/// per row and the property stores its boolean, exactly as the same
/// expression projects in a RETURN (`SET n.p1 = EXISTS((n)-[:R]->(:L2))`
/// previously died at the blanket pattern-in-SET-value rejection; a bare
/// pattern value keeps it, and an unbound pattern variable in the value
/// stays undefined).
#[test]
fn set_value_reads_an_exists_pattern_predicate_per_row() {
    let mut conn = connection();
    conn.query("CREATE (:L1 {name: 'a'})-[:R]->(:L2), (:L1 {name: 'b'})")
        .unwrap();
    let result = conn
        .query(
            "MATCH (n:L1) SET n.p1 = EXISTS((n)-[:R]->(:L2)) \
             RETURN n.name AS name, n.p1 AS p1 ORDER BY n.name",
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::String("a".into()), Value::Bool(true)],
            vec![Value::String("b".into()), Value::Bool(false)],
        ]
    );
    // The write persists.
    let result = conn
        .query("MATCH (n:L1) RETURN n.p1 AS p1 ORDER BY n.name")
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Bool(true)], vec![Value::Bool(false)]]
    );
    // A bare pattern value keeps the rejection, with the SET spelling.
    let error = conn
        .query("MATCH (n:L1) SET n.p1 = (n)-[:R]->(:L2)")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("pattern expressions are not allowed in SET values"),
        "got: {error}"
    );
    // An unbound pattern variable inside the value stays undefined.
    let error = conn
        .query("MATCH (n:L1) SET n.p1 = EXISTS((m)-[:R]->(:L2))")
        .unwrap_err();
    assert!(
        error.to_string().contains("variable 'm' not defined"),
        "got: {error}"
    );
}
