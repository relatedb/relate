//! Pins for the misc-tail node's newly accepted forms (gbuild feature
//! `fix-parser-syntax-gaps`): the nested list predicate, `DISJOINT BY`
//! batching, list-comprehension SET/REMOVE targets, map-projection CALL
//! exports, name-matched UNION branches, DISTINCT/aggregation ORDER BY
//! projection scope, list-to-boolean conversion, `db.schema.visualization`'s
//! boolean argument, `db.index.fulltext.queryRelationships`, and MERGE's
//! self-referencing property map.

mod common;

use common::{connection, first_column_strings, params, run, scalar_one};
use relate::Value;

#[test]
fn list_predicate_stays_a_quantifier_inside_a_function_argument() {
    // The enclosing call's `f(ALL x)` set-quantifier spelling used to eat
    // the leading ALL, so `toBoolean(all(...))` parsed as a plain call and
    // died on WHERE.
    let mut conn = connection();
    assert_eq!(
        scalar_one(
            &mut conn,
            "RETURN toBoolean(all(n0 IN [1, 2] WHERE n0 > 0)) AS r"
        ),
        Value::Bool(true)
    );
    let mut conn = connection();
    assert_eq!(
        scalar_one(
            &mut conn,
            "RETURN toBoolean(all(n0 IN [1, 2] WHERE n0 < 0)) AS r"
        ),
        Value::Bool(false)
    );
}

#[test]
fn disjoint_by_is_accepted_on_concurrent_transactions() {
    for trailer in [
        "DISJOINT BY (i)",
        "DISJOINT BY (i, i + 1)",
        "DISJOINT BY AUTO",
        "DISJOINT BY NONE",
    ] {
        let mut conn = connection();
        let query = format!(
            "UNWIND range(0, 4) AS i \
             CALL (i) {{ CREATE (:A {{i: i}}) }} \
             IN CONCURRENT TRANSACTIONS OF 2 ROWS {trailer}"
        );
        run(&mut conn, &query);
        let created = run(&mut conn, "MATCH (n:A) RETURN count(*) AS c");
        assert_eq!(created.rows, vec![vec![Value::Int(5)]], "{trailer}");
    }
    // The hint only exists on the CONCURRENT form.
    let mut conn = connection();
    let err = conn
        .query(
            "UNWIND range(0, 4) AS i CALL (i) { CREATE (:A {i: i}) } \
             IN TRANSACTIONS OF 2 ROWS DISJOINT BY (i)",
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("DISJOINT BY"), "{err}");
}

#[test]
fn set_and_remove_accept_a_list_comprehension_target() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE (:N {num: 1}), (:N {num: 2}), (:N {num: 3})",
    );
    run(
        &mut conn,
        "MATCH (n:N) WITH collect(n) AS ns \
         SET [x IN ns WHERE x.num = 1][0].num = -1 \
         UNWIND ns AS n RETURN n.num AS num",
    );
    let mut nums: Vec<i64> = run(&mut conn, "MATCH (n:N) RETURN n.num AS num")
        .rows
        .into_iter()
        .map(|row| match row[0] {
            Value::Int(i) => i,
            ref other => panic!("expected an integer, got {other:?}"),
        })
        .collect();
    nums.sort_unstable();
    assert_eq!(nums, [-1, 2, 3]);

    run(
        &mut conn,
        "MATCH (n:N) WITH collect(n) AS ns \
         REMOVE [x IN ns WHERE x.num = 2][0].num",
    );
    let removed = run(
        &mut conn,
        "MATCH (n:N) WHERE n.num IS NULL RETURN count(*) AS c",
    );
    assert_eq!(removed.rows, vec![vec![Value::Int(1)]]);
}

#[test]
fn map_projection_exports_under_its_variable_name() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:Person {prop: 1})");
    let result = run(
        &mut conn,
        "CALL () { MATCH (n) RETURN n {.prop, .foo} } RETURN n",
    );
    assert_eq!(result.header.columns, ["n"]);
    assert_eq!(result.rows.len(), 1);
}

#[test]
fn union_branches_match_by_column_name_not_position() {
    let mut conn = connection();
    let result = run(
        &mut conn,
        "CALL () { RETURN 1 AS x, 2 AS y UNION RETURN 3 AS y, 2 AS x } RETURN x, y",
    );
    assert_eq!(result.header.columns, ["x", "y"]);
    let mut rows: Vec<(i64, i64)> = result
        .rows
        .into_iter()
        .map(|row| match (&row[0], &row[1]) {
            (Value::Int(x), Value::Int(y)) => (*x, *y),
            other => panic!("expected integers, got {other:?}"),
        })
        .collect();
    rows.sort_unstable();
    assert_eq!(rows, [(1, 2), (2, 3)]);
}

#[test]
fn distinct_order_by_reads_projected_expressions() {
    let mut conn = connection();
    let result = run(
        &mut conn,
        "WITH {p: -0.5} AS pa0, 1 AS x \
         RETURN DISTINCT {p: 1} AS pa0, pa0.p AS pa1, x AS y \
         ORDER BY -1 = pa0.p + x",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0][1],
        Value::Float(-0.5),
        "pa1 keeps the pre-projection pa0"
    );
}

#[test]
fn order_by_may_decompose_a_projected_expression_with_aggregation() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:Person {age: 1})");
    run(&mut conn, "CREATE (:Person {age: 2})");
    run(
        &mut conn,
        "MATCH (me:Person)--(you:Person) \
         RETURN me.age + you.age, count(*) AS cnt \
         ORDER BY me.age + you.age + count(*)",
    );
}

#[test]
fn with_star_may_relist_an_imported_name() {
    let mut conn = connection();
    let result = run(
        &mut conn,
        "WITH 1 AS x CALL (x) { WITH 2 AS y WITH *, x RETURN * } RETURN *",
    );
    assert_eq!(result.header.columns, ["x", "y"]);
    assert_eq!(result.rows, vec![vec![Value::Int(1), Value::Int(2)]]);
}

#[test]
fn merge_property_map_rejects_the_merged_entity_reference_under_every_spelling() {
    // The same-clause reference is the 42I58 error under every spelling —
    // the deprecated Cypher-5 leniency is gone, and the one admitted
    // `CYPHER 25` preamble (ADR-0584) rejects it exactly like the
    // unprefixed form.
    let mut conn = connection();
    run(&mut conn, "CREATE ({prop: 'x'})");
    for query in [
        "MATCH (a) WITH a AS shadow MERGE (a {prop: a.prop}) RETURN a.prop AS prop",
        "CYPHER 25 MATCH (a) WITH a AS shadow MERGE (a {prop: a.prop}) RETURN a.prop AS prop",
    ] {
        let error = conn.query(query).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot be created and referenced"),
            "{query}: {error}"
        );
    }
    // The graph still holds exactly the seeded node.
    let nodes = run(&mut conn, "MATCH (n) RETURN count(*) AS c");
    assert_eq!(nodes.rows, vec![vec![Value::Int(1)]]);
}

#[test]
fn merge_property_map_reference_for_relationships_is_rejected_under_every_spelling() {
    let mut conn = connection();
    run(&mut conn, "CREATE ()-[:R {prop: 'x'}]->()");
    for query in [
        "MATCH ()-[r]->() WITH r AS shadow MERGE ()-[r:R {prop: r.prop}]->() \
         RETURN r.prop AS prop",
        "CYPHER 25 MATCH ()-[r]->() WITH r AS shadow MERGE ()-[r:R {prop: r.prop}]->() \
         RETURN r.prop AS prop",
    ] {
        let error = conn.query(query).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot be created and referenced"),
            "{query}: {error}"
        );
    }
}

#[test]
fn runtime_lists_convert_to_booleans_but_list_literals_do_not() {
    // A *runtime* list (a parameter's value) takes the deprecated
    // list-to-boolean conversion: empty is false, non-empty is true.
    let mut conn = connection();
    let empty = conn
        .query_with_params(
            "RETURN true AND $list AS r",
            params(&[("list", Value::List(Vec::new().into()))]),
        )
        .unwrap();
    assert_eq!(empty.rows, vec![vec![Value::Bool(false)]]);
    let non_empty = conn
        .query_with_params(
            "RETURN false OR $list AS r",
            params(&[("list", Value::List(vec![Value::Bool(true)].into()))]),
        )
        .unwrap();
    assert_eq!(non_empty.rows, vec![vec![Value::Bool(true)]]);

    // A list *literal* operand stays the openCypher compile-time error
    // (TCK Boolean1-3 [8]), and so does a non-convertible literal.
    for query in ["RETURN true AND [] AS r", "RETURN 1 AND true AS r"] {
        let err = conn.query(query).unwrap_err().to_string();
        assert!(
            err.contains("expected Boolean") || err.contains("requires a boolean"),
            "{query}: {err}"
        );
    }
}

#[test]
fn list_to_boolean_conversion_does_not_reach_a_quantifier_predicate() {
    // ADR-0458 §7 scopes the deprecated conversion to `AND`/`OR`/`XOR`/`NOT`
    // operands. A list-valued quantifier WHERE predicate arrives as a plain
    // variable, so validation sees no literal to reject — the executor's
    // boolean-or-null reading is what keeps the scope.
    let mut conn = connection();
    for query in [
        "WITH [1, 2] AS xs RETURN all(x IN [1, 2] WHERE xs) AS r",
        "WITH [1, 2] AS xs RETURN any(x IN [1, 2] WHERE xs) AS r",
        "WITH [1, 2] AS xs RETURN none(x IN [1, 2] WHERE xs) AS r",
        "WITH [1, 2] AS xs RETURN single(x IN [1, 2] WHERE xs) AS r",
    ] {
        let err = conn.query(query).unwrap_err().to_string();
        assert!(
            err.contains("quantifier WHERE requires a boolean"),
            "{query}: {err}"
        );
    }
}

#[test]
fn schema_visualization_boolean_options_choose_ghost_relationships() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:P)-[:W]->(:D), (:D)-[:W]->(:C)");
    // The paren-less spelling is not legal in-query (live 2026.10), so the
    // default is written with its empty argument list.
    for (options, expected) in [("()", 4), ("(true)", 4), ("(false)", 2), ("({})", 4)] {
        let result = run(
            &mut conn,
            &format!(
                "CALL db.schema.visualization{options} YIELD relationships \
                 RETURN size(relationships) AS n"
            ),
        );
        assert_eq!(
            result.rows,
            vec![vec![Value::Int(expected)]],
            "visualization{options}"
        );
    }
    // A null argument selects nothing: the procedure yields zero rows.
    let null = run(
        &mut conn,
        "CALL db.schema.visualization(null) YIELD relationships RETURN size(relationships) AS n",
    );
    assert!(null.rows.is_empty(), "{:?}", null.rows);
    let err = conn
        .query("CALL db.schema.visualization('nope')")
        .unwrap_err()
        .to_string();
    assert!(err.contains("BOOLEAN|MAP"), "{err}");
}

#[test]
fn fulltext_query_relationships_returns_relationship_matches() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE (:A {title: 'Aladdin'})-[:RELATED_TO {note: 'magic lamp'}]->(:B {title: 'Hook'}), \
         (:B)-[:SIMILAR_TO {note: 'lamp of wishes'}]->(:C {title: 'Other'})",
    );
    run(
        &mut conn,
        "CREATE FULLTEXT INDEX relNotes FOR ()-[r:RELATED_TO|SIMILAR_TO]-() ON EACH [r.note]",
    );
    let mut notes = first_column_strings(&run(
        &mut conn,
        "CALL db.index.fulltext.queryRelationships('relNotes', 'lamp') \
         YIELD relationship RETURN relationship.note AS note",
    ));
    notes.sort();
    assert_eq!(notes, ["lamp of wishes", "magic lamp"]);
}

#[test]
fn parameter_null_keeps_a_boolean_operator_working() {
    let mut conn = connection();
    let result = conn
        .query_with_params(
            "RETURN true AND $list AS r",
            params(&[("list", Value::List(Vec::new().into()))]),
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(false)]]);
}

/// `x OR true` / `x AND false` fold to the constant before variable
/// resolution (Neo4j's rewriter), so the dead operand's variables are never
/// resolved — MiscAcceptance "IN predicate with undefined variable is
/// accepted when it can be rewritten away".
#[test]
fn absorbing_boolean_constants_skip_the_dead_operand() {
    let mut connection = connection();
    let result = connection
        .query("WITH 1 AS x WHERE undefinedVar IN [anotherUndefinedVar] OR true RETURN x")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
    let result = connection
        .query("WITH 1 AS x WHERE true OR undefinedVar RETURN x")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
    let result = connection
        .query("WITH 1 AS x WHERE undefinedVar IN [1] AND false RETURN x")
        .unwrap();
    assert!(result.rows.is_empty());
    // A non-absorbing boolean use still resolves the variable.
    let error = connection
        .query("WITH 1 AS x WHERE undefinedVar IN [1] RETURN x")
        .unwrap_err();
    assert!(error.to_string().contains("UndefinedVariable"), "{error}");
}
