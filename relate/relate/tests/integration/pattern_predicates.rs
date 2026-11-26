//! Pattern predicates and path selectors: inline element `WHERE`s inside
//! pattern comprehensions, a named subpath beside the path variable, and
//! the scoping forms a subquery expression imports.

use crate::common;

use relate::Value;

use common::{connection, err_text, int, params, string, strings};

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

/// A pattern predicate inside a quantifier's WHERE filter is a predicate
/// home (the LABS-1610 shape): the projection validates, and the executor
/// evaluates the pattern once per list element with the loop variable
/// bound into the pattern's node property filters — the same per-row
/// evaluation the `exists(<pattern>)` SET value reads. The pinned
/// scenario's parameterized repro returns false over `['a', 'b']`; a list
/// whose every element matches reads true, and `any` over a partially
/// matching list reads true. An unbound name inside the pattern keeps the
/// planner binder's rejection, and `size()` over the pattern keeps its
/// own — the home exemption lifts the pattern-position rejection only.
#[test]
fn quantifier_filters_read_pattern_predicates_per_element() {
    let mut conn = connection();
    // The pinned scenario [15] graph: the L1 'a' carries an R to the L2
    // 'z'; over the parameter list ['a', 'b'] the answer is false ('b'
    // has no incoming-R match at all).
    conn.query("CREATE (:L1 {p1: 'a'})-[:R]->(:L2 {p1: 'z'})")
        .unwrap();
    let repro = "RETURN all(x IN $p1 WHERE (:L1 {p1: x})<-[:R]-(:L2 {p1: $p2})) AS result";
    let p = |names: &[&str]| params(&[("p1", strings(names)), ("p2", string("z"))]);
    let result = conn.query_with_params(repro, p(&["a", "b"])).unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(false)]]);
    // The issue's literal statement, EXPLAIN-wrapped, plans.
    conn.query_with_params(&format!("EXPLAIN {repro}"), p(&["a", "b"]))
        .unwrap();
    // The reversed edge gives the L1 'a' the incoming R the pattern asks
    // for, so every element of ['a'] matches and `all` reads true, while
    // `any` over ['a', 'b'] reads true on 'a' alone.
    conn.query("CREATE (:L2 {p1: 'z'})-[:R]->(:L1 {p1: 'a'})")
        .unwrap();
    let result = conn.query_with_params(repro, p(&["a"])).unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(true)]]);
    let result = conn
        .query_with_params(
            "RETURN any(x IN $p1 WHERE (:L1 {p1: x})<-[:R]-(:L2 {p1: $p2})) AS result",
            p(&["a", "b"]),
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(true)]]);
    // The home exemption lifts the pattern-position rejection only: an
    // unbound name inside the quantifier's pattern keeps the planner
    // binder's rejection, and `size()` over the pattern keeps its own
    // (the planner's argument rule) — the exemption admits the predicate
    // slot, never a value subexpression buried inside it.
    for (statement, expected) in [
        (
            "MATCH (n) WHERE all(x IN [1] WHERE (n)-[:R]->(y)) RETURN n",
            "UndefinedVariable: variable 'y' not defined",
        ),
        (
            "MATCH (n) RETURN all(x IN [1] WHERE size((n)--()) > 0) AS y",
            "UnexpectedSyntax: size() cannot be applied to a pattern predicate",
        ),
    ] {
        let text = err_text(conn.query(statement));
        assert!(text.contains(expected), "{statement}: {text}");
    }
}

/// `allReduce`'s trailing predicate is the same scope-introducing
/// predicate slot as a quantifier's WHERE filter (the one home table's
/// row beside it): a pattern predicate inside it is legal in a
/// projection, and the executor evaluates the pattern once per element
/// with the accumulator and the loop variable bound — the fold fails at
/// the first element whose pattern does not hold.
#[test]
fn allreduce_predicates_read_pattern_predicates_per_element() {
    let mut conn = connection();
    // The L2 'z' gives the L1 'a' the incoming R the pattern asks for;
    // there is no L1 'b' at all.
    conn.query("CREATE (:L2 {p1: 'z'})-[:R]->(:L1 {p1: 'a'})")
        .unwrap();
    let result = conn
        .query(
            "MATCH (n:L2 {p1: 'z'}) \
             RETURN allReduce(acc = 0, x IN ['a'] | acc + 1, (:L1 {p1: x})<-[:R]-(n)) AS r",
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(true)]]);
    // The 'b' element's pattern does not hold, so the fold fails there.
    let result = conn
        .query(
            "MATCH (n:L2 {p1: 'z'}) \
             RETURN allReduce(acc = 0, x IN ['a', 'b'] | acc + 1, (:L1 {p1: x})<-[:R]-(n)) AS r",
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(false)]]);
}

/// The quantifier-filter home reaches SET values (the value-position
/// policy's third position, beside WITH/RETURN projections): the pattern
/// predicate evaluates per element per row and the property stores its
/// boolean, exactly as the `exists(<pattern>)` SET value does.
#[test]
fn set_value_reads_a_quantifier_filter_pattern_predicate_per_row() {
    let mut conn = connection();
    conn.query("CREATE (:L2 {p1: 'z'})-[:R]->(:L1 {p1: 'a'}), (:L1 {p1: 'b'})")
        .unwrap();
    let result = conn
        .query(
            "MATCH (n:L1) SET n.flag = all(x IN ['z'] WHERE (n)<-[:R]-(:L2 {p1: x})) \
             RETURN n.p1 AS p1, n.flag AS flag ORDER BY n.p1",
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![string("a"), Value::Bool(true)],
            vec![string("b"), Value::Bool(false)],
        ]
    );
}
