use super::*;
use crate::validate::validate_statement_tree;

/// Parse `query` and run the whole nested-statement validation battery.
fn check(query: &str) -> Result<(), EngineError> {
    validate_statement_tree(&relate_parser::parse(query).expect("parse"))
}

/// Assert the validation `result` fails and its detail names every needle.
fn assert_error(query: &str, result: Result<(), EngineError>, needles: &[&str]) {
    let rendered = result.expect_err(query).to_string();
    for needle in needles {
        assert!(
            rendered.contains(needle),
            "`{query}`: expected {needle:?} in {rendered:?}"
        );
    }
}

/// Assert the validation `result` succeeds.
fn assert_ok(query: &str, result: Result<(), EngineError>) {
    result.unwrap_or_else(|error| panic!("`{query}`: {error}"));
}

#[test]
fn constant_wrong_type_property_keys_fail_at_compile_time() {
    for query in [
        "MATCH (n) SET n[1] = 2",
        "MATCH (n) SET n[true] = 2",
        "MATCH (n) SET n[1.5] = 2",
        "MATCH (n) SET n[null] = 2",
        "MATCH (n) SET n[['a']] = 2",
        "MATCH (n) REMOVE n[1]",
        "MATCH (n) SET n[1 + 1] = 2",
    ] {
        assert_error(
            query,
            check(query),
            &[
                "node or relationship property key",
                "Expected to be STRING.",
            ],
        );
    }
}

/// The detail text reads the one core type spelling, so a union keeps
/// the CIP-100 order (`INTEGER` before `FLOAT`) rather than the
/// lexicographic order a third table would produce.
#[test]
fn list_property_key_detail_uses_the_cip100_type_order() {
    assert_error(
        "MATCH (n) SET n[[1, 1.5]] = 2",
        check("MATCH (n) SET n[[1, 1.5]] = 2"),
        &["Invalid input 'LIST<INTEGER | FLOAT>' for node or relationship property key"],
    );
}

/// The family's core regression: Neo4j folds the constant at compile
/// time, so the zero-row MATCH never hides it. Relate's per-row check
/// would never run here.
#[test]
fn constant_wrong_type_property_key_fails_on_a_zero_row_match() {
    assert_error(
        "MATCH (n:NoSuchLabel) SET n[1] = 2",
        check("MATCH (n:NoSuchLabel) SET n[1] = 2"),
        &["node or relationship property key", "INTEGER"],
    );
}

#[test]
fn constant_empty_and_nul_property_keys_fail_at_compile_time() {
    for query in [
        "MATCH (n) SET n[''] = 2",
        "MATCH (n) SET n['a\u{0}b'] = 2",
        "MATCH (n) REMOVE n['']",
    ] {
        assert_error(
            query,
            check(query),
            &["invalid name. A property key name cannot be empty or contain any null-bytes"],
        );
    }
}

#[test]
fn constant_dynamic_labels_must_be_string_or_string_list() {
    for query in [
        "MATCH (n) SET n:$(1)",
        "MATCH (n) SET n:$(['A', 1])",
        "MATCH (n) REMOVE n:$(true)",
        "MATCH (n) CREATE (m:$(1))",
        "MATCH (n) MERGE (m:$(1))",
        "MATCH (n:$(1)) RETURN n",
        "MATCH (n) SET labels(n) = 1",
        "MATCH (n) SET labels(n) = ['A', 1]",
        "MATCH (n) WHERE (n)-[:$(1)]->() RETURN n",
        "MATCH (n) RETURN [(n)-[:$(1)]->(m) | m] AS ms",
        "MATCH (a), (b) RETURN shortestPath((a)-[:$(1)]->(b)) AS p",
    ] {
        assert_error(
            query,
            check(query),
            &["dynamic label", "Expected to be STRING or LIST<STRING>."],
        );
    }
}

#[test]
fn constant_dynamic_relationship_types_are_checked_too() {
    assert_error(
        "CREATE (:A)-[:$(1)]->(:B)",
        check("CREATE (:A)-[:$(1)]->(:B)"),
        &["dynamic label", "Expected to be STRING or LIST<STRING>."],
    );
}

#[test]
fn constant_empty_and_nul_labels_fail_at_compile_time() {
    for query in [
        "MATCH (n) SET n:$('')",
        "MATCH (n) SET n:$(['A', ''])",
        "MATCH (n) SET n:$('a\u{0}b')",
    ] {
        assert_error(
            query,
            check(query),
            &["invalid name. A label name cannot be empty or contain any null-bytes"],
        );
    }
}

/// A map literal is a MAP by shape, whatever its entries evaluate to;
/// the fold rejects it without reading (or fabricating) a value.
#[test]
fn constant_map_operands_are_rejected_by_shape() {
    for query in [
        "MATCH (n) SET n[{a: 1}] = 2",
        "MATCH (n) SET n[({a: $p})] = 2",
    ] {
        assert_error(
            query,
            check(query),
            &["Invalid input 'MAP' for node or relationship property key"],
        );
    }
    assert_error(
        "MATCH (n) SET n:$({a: 1})",
        check("MATCH (n) SET n:$({a: 1})"),
        &["Invalid input 'MAP' for dynamic label"],
    );
}

/// A map literal nested inside a constant list is declined by the value
/// fold (a list cannot materialize a map member) but is still
/// constant-typed: it must fail through the type half — at the operand or
/// as a list member, on a zero-row MATCH too.
#[test]
fn map_literals_nested_in_constant_lists_are_rejected_by_shape() {
    for query in [
        "MATCH (n) SET n[[{a: 1}]] = 2",
        "MATCH (n:NoSuchLabel) SET n[[{a: $p}]] = 2",
        "MATCH (n:NoSuchLabel) REMOVE n[[{a: 1}]]",
    ] {
        assert_error(
            query,
            check(query),
            &["Invalid input 'MAP' for node or relationship property key"],
        );
    }
    for query in [
        "MATCH (n) SET n:$(['A', {b: 1}])",
        "MATCH (n:NoSuchLabel) SET n:$(['A', {b: $p}])",
        "MATCH (n) SET labels(n) = [{b: 1}]",
    ] {
        assert_error(
            query,
            check(query),
            &["Invalid input 'MAP' for dynamic label"],
        );
    }
}

/// An operand whose constant expression the value fold declines for
/// content reasons still has a decided *type*, so the zero-row MATCH that
/// would never evaluate it cannot hide it.
#[test]
fn statically_typed_non_string_operands_fail_at_compile_time() {
    for (query, type_name) in [
        ("MATCH (n:NoSuchLabel) SET n[1 + 1.5] = 2", "FLOAT"),
        ("MATCH (n:NoSuchLabel) SET n[2 / 1] = 2", "INTEGER"),
        ("MATCH (n:NoSuchLabel) SET n[1 < 2] = 2", "BOOLEAN"),
        ("MATCH (n:NoSuchLabel) SET n[NOT true] = 2", "BOOLEAN"),
    ] {
        let needle = format!("Invalid input '{type_name}' for node or relationship property key");
        assert_error(query, check(query), &[needle.as_str()]);
    }
    for (query, type_name) in [
        ("MATCH (n) SET n:$(1 + 1.5)", "FLOAT"),
        ("MATCH (n) SET n:$(1 < 2)", "BOOLEAN"),
    ] {
        let needle = format!("Invalid input '{type_name}' for dynamic label");
        assert_error(query, check(query), &[needle.as_str()]);
    }
}

#[test]
fn constant_string_keys_and_labels_are_accepted() {
    for query in [
        "MATCH (n) SET n['name'] = 2",
        "MATCH (n) SET n['na' + 'me'] = 2",
        "MATCH (n) REMOVE n['name']",
        "MATCH (n) SET n:$('Person')",
        "MATCH (n) SET n:$(['Person', 'Admin'])",
        "MATCH (n) CREATE (m:$('Person'))",
    ] {
        assert_ok(query, check(query));
    }
}

/// Parameters and runtime-computed expressions are not constant-foldable
/// here, so they stay runtime-checked: the compile-time pass must not
/// reject a legitimately dynamic key/label (no bound values were
/// supplied, so neither the fold nor the parameter rule has anything to
/// decide).
#[test]
fn parameters_and_runtime_expressions_are_left_to_runtime() {
    for query in [
        "MATCH (n) SET n[$key] = 2",
        "MATCH (n) SET n[toString(n.x)] = 2",
        "MATCH (n) SET n:$(label)",
        "MATCH (n) SET n:$($labels)",
        "MATCH (n) WHERE n:$(label) RETURN n",
    ] {
        assert_ok(query, check(query));
    }
}

/// Parse, run the whole-tree battery, then the binding/parameter pass —
/// the same order lowering uses.
fn admit(query: &str, params: &[(&str, Value)]) -> Result<(), EngineError> {
    let stmt = relate_parser::parse(query).expect("parse");
    let params: BTreeMap<String, Value> = params
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect();
    validate_statement_tree(&stmt)?;
    validate_scoped_operands(&stmt, &params)
}

/// A constructor call over constant arguments is folded at admission by
/// Neo4j, so its fixed result type must be rejected even on a zero-row
/// `MATCH`: these are the shapes that used to slip through because the
/// fold decline on a function result was read as "not statically known".
#[test]
fn constant_constructor_calls_are_typed_for_property_keys() {
    for (query, type_name) in [
        (
            "MATCH (n:NoSuchLabel) SET n[localdatetime(\"2024185T19:32:24\")] = 1",
            "LOCAL DATETIME",
        ),
        ("MATCH (n:NoSuchLabel) SET n[point({x:3,y:0})] = 1", "POINT"),
        (
            "MATCH (n:NoSuchLabel) SET n[date(\"2020-01-01\")] = 1",
            "DATE",
        ),
        (
            "MATCH (n:NoSuchLabel) SET n[duration(\"P1D\")] = 1",
            "DURATION",
        ),
        (
            "MATCH (n:NoSuchLabel) SET n[toInteger(\"12\")] = 1",
            "INTEGER",
        ),
        ("MATCH (n:NoSuchLabel) SET n[size(\"ab\")] = 1", "INTEGER"),
        ("MATCH (n:NoSuchLabel) SET n[rand()] = 1", "FLOAT"),
    ] {
        let needle = format!("Invalid input '{type_name}' for node or relationship property key");
        assert_error(query, check(query), &[needle.as_str()]);
    }
    for (query, type_name) in [
        (
            "MATCH (n:NoSuchLabel) REMOVE n[localdatetime(\"2024185T19:32:24\")]",
            "LOCAL DATETIME",
        ),
        ("MATCH (n:NoSuchLabel) REMOVE n[point({x:3,y:0})]", "POINT"),
    ] {
        let needle = format!("Invalid input '{type_name}' for node or relationship property key");
        assert_error(query, check(query), &[needle.as_str()]);
    }
}

#[test]
fn constant_constructor_calls_are_typed_for_labels() {
    for (query, type_name) in [
        (
            "MATCH (n:NoSuchLabel) SET n:$(localdatetime(\"2024185T19:32:24\"))",
            "LOCAL DATETIME",
        ),
        ("MATCH (n:NoSuchLabel) SET n:$(point({x:3,y:0}))", "POINT"),
        (
            "MATCH (n:NoSuchLabel) REMOVE n:$(point({x:3,y:0}))",
            "POINT",
        ),
        (
            "MATCH (n:NoSuchLabel) REMOVE n:$(localdatetime(\"2024185T19:32:24\"))",
            "LOCAL DATETIME",
        ),
    ] {
        let needle = format!("Invalid input '{type_name}' for dynamic label");
        assert_error(query, check(query), &[needle.as_str()]);
    }
}

/// A call over runtime data keeps its per-row semantics: the type is not
/// statically decided, and Neo4j reports the call's own argument error
/// there rather than a dynamic-operand error.
#[test]
fn calls_over_runtime_data_stay_runtime() {
    for query in [
        "MATCH (n) SET n[point(n.p)] = 1",
        "MATCH (n) SET n[localdatetime(n.p)] = 1",
        "MATCH (n) SET n[toString(n.p)] = 1",
    ] {
        assert_ok(query, check(query));
    }
}

/// `WITH <expr> AS a … SET n[a] = 1`: the operand is a variable, but its
/// binding is statically known, and Neo4j rejects on a zero-row MATCH.
#[test]
fn with_bound_variables_are_typed_for_property_keys() {
    for (binding, type_name) in [
        ("1 + 2", "INTEGER"),
        ("true", "BOOLEAN"),
        ("point({x: 2.3, y: 4.5})", "POINT"),
        ("localdatetime(\"2024185T19:32:24\")", "LOCAL DATETIME"),
        ("{x : 1}", "MAP"),
    ] {
        let needle = format!("Invalid input '{type_name}' for node or relationship property key");
        for query in [
            format!("WITH {binding} AS a MATCH (n) SET n[a] = 1 RETURN n"),
            format!("WITH {binding} AS a MATCH (n) REMOVE n[a] RETURN n"),
        ] {
            assert_error(&query, admit(&query, &[]), &[needle.as_str()]);
        }
    }
}

#[test]
fn with_bound_variables_are_typed_for_labels() {
    for (binding, type_name) in [
        ("1 + 2", "INTEGER"),
        ("true", "BOOLEAN"),
        ("point({x:3,y:0})", "POINT"),
        ("localdatetime(\"2024185T19:32:24\")", "LOCAL DATETIME"),
    ] {
        let needle = format!("Invalid input '{type_name}' for dynamic label");
        let query = format!("WITH {binding} AS a MATCH (n) REMOVE n:$(a) RETURN labels(n)");
        assert_error(&query, admit(&query, &[]), &[needle.as_str()]);
    }
}

/// A bound list is a wrong *type* at a property key (a key is one
/// `STRING`), so it is a compile-time rejection naming the whole list.
/// A dynamic label accepts `LIST`, so its elements are the executor's
/// per-row check, exactly like a list arriving from the graph.
#[test]
fn with_bound_list_variables_are_split_by_position() {
    assert_error(
        "WITH [1, 2] AS a MATCH (n) SET n[a] = 1 RETURN n",
        admit("WITH [1, 2] AS a MATCH (n) SET n[a] = 1 RETURN n", &[]),
        &["Invalid input 'LIST<INTEGER>' for node or relationship property key"],
    );
    assert_ok(
        "WITH [1, 2] AS a MATCH (n) REMOVE n:$(a) RETURN labels(n)",
        admit(
            "WITH [1, 2] AS a MATCH (n) REMOVE n:$(a) RETURN labels(n)",
            &[],
        ),
    );
}

/// A bound `NULL`, and a bound string's content, are per-row facts:
/// Neo4j does not reject them before execution, so the executor's
/// `TypeError` is the one that fires (and only on an evaluated row).
#[test]
fn bound_null_and_name_content_are_left_to_runtime() {
    for query in [
        "WITH null AS a MATCH (n) SET n[a] = 1 RETURN n",
        "WITH null AS a MATCH (n) REMOVE n[a] RETURN n",
        "WITH null AS a MATCH (n) SET n:$(a) RETURN labels(n)",
        "WITH ['A', null] AS a MATCH (n) SET n:$(a) RETURN labels(n)",
        "WITH ['A', 1] AS a MATCH (n) SET n:$(a) RETURN labels(n)",
        "WITH '' AS a MATCH (n) SET n[a] = 1 RETURN n",
    ] {
        assert_ok(query, admit(query, &[]));
    }
    assert_ok(
        "MATCH (n) SET n[$a] = 1 RETURN n",
        admit("MATCH (n) SET n[$a] = 1 RETURN n", &[("a", Value::Null)]),
    );
    assert_ok(
        "MATCH (n) SET n:$($a) RETURN labels(n)",
        admit(
            "MATCH (n) SET n:$($a) RETURN labels(n)",
            &[("a", Value::Null)],
        ),
    );
}

/// An inline literal is folded, so its value — `NULL` included — and its
/// content are compile-time facts, as Neo4j's own literal fold has them.
#[test]
fn inline_literal_null_is_still_checked_at_compile_time() {
    for (query, needles) in [
        (
            "MATCH (n) SET n[null] = 1",
            ["node or relationship property key", "NULL"],
        ),
        ("MATCH (n) SET n:$(null)", ["dynamic label", "NULL"]),
        ("MATCH (n:$(null)) RETURN n", ["dynamic label", "NULL"]),
    ] {
        assert_error(query, check(query), &needles);
    }
}

/// A label predicate in WHERE / a projection has no compile-time type
/// check in Neo4j: the operand is evaluated per row, so admission must
/// leave it alone even for an inline literal (CIP-221, CIP-162).
#[test]
fn label_predicates_are_left_to_the_executor() {
    for query in [
        "MATCH (n) WHERE n:$(null) RETURN n",
        "MATCH (n) WHERE n:$(42) RETURN n",
        "MATCH (n) RETURN n:$(1) AS p",
        "MATCH ()-[r]->() RETURN r:$(1) AS p",
        "MATCH ()-[r]->() RETURN r:$([null]) AS p",
    ] {
        assert_ok(query, check(query));
    }
}

#[test]
fn bound_parameters_are_typed() {
    for (value, type_name) in [
        (Value::Int(1), "INTEGER"),
        (Value::Bool(true), "BOOLEAN"),
        (Value::Float(1.5), "FLOAT"),
        (map_param(), "MAP"),
    ] {
        let needle = format!("Invalid input '{type_name}' for node or relationship property key");
        for query in [
            "MATCH (n) SET n[$a] = 1 RETURN n",
            "MATCH (n) REMOVE n[$a] RETURN n",
        ] {
            assert_error(
                query,
                admit(query, &[("a", value.clone())]),
                &[needle.as_str()],
            );
        }
    }
    for (value, type_name) in [(Value::Int(1), "INTEGER"), (map_param(), "MAP")] {
        let needle = format!("Invalid input '{type_name}' for dynamic label");
        for query in [
            "MATCH (n) SET n:$($a) RETURN labels(n)",
            "MATCH (n) REMOVE n:$($a) RETURN labels(n)",
        ] {
            assert_error(
                query,
                admit(query, &[("a", value.clone())]),
                &[needle.as_str()],
            );
        }
    }
}

fn map_param() -> Value {
    Value::Map(std::sync::Arc::new(BTreeMap::from([(
        "x".to_string(),
        Value::Int(1),
    )])))
}

/// Only a *bad* binding is rejected: string bindings and bindings whose
/// value is only known at execution keep working, and a name rebound by
/// a later clause is typed by its new binding, not the old one.
#[test]
fn good_and_rebound_bindings_stay_accepted() {
    for query in [
        "WITH 'key' AS a MATCH (n) SET n[a] = 1 RETURN n",
        "WITH 'Label' AS a MATCH (n) REMOVE n:$(a) RETURN labels(n)",
        "MATCH (n) WITH n.name AS a SET n[a] = 1 RETURN n",
        "WITH 1 AS a WITH 'key' AS a MATCH (n) SET n[a] = 1 RETURN n",
        "WITH 1 AS a MATCH (n) WITH 'key' AS a SET n[a] = 1 RETURN n",
        "MATCH (n) UNWIND [1] AS a SET n[a] = 1 RETURN n",
    ] {
        assert_ok(query, admit(query, &[]));
    }
    assert_ok(
        "MATCH (n) SET n[$a] = 1 RETURN n",
        admit(
            "MATCH (n) SET n[$a] = 1 RETURN n",
            &[("a", Value::String("key".into()))],
        ),
    );
    assert_ok(
        "MATCH (n) SET n:$($a) RETURN labels(n)",
        admit(
            "MATCH (n) SET n:$($a) RETURN labels(n)",
            &[("a", Value::String("Label".into()))],
        ),
    );
    // A parameter that is not bound at admission has nothing to decide.
    assert_ok(
        "MATCH (n) SET n[$a] = 1 RETURN n",
        admit("MATCH (n) SET n[$a] = 1 RETURN n", &[]),
    );
}

/// The scoped pass walks `FOREACH` bodies with the enclosing bindings,
/// so the same rule reaches a nested write site.
#[test]
fn scoped_bindings_reach_foreach_bodies() {
    assert_error(
        "WITH 1 AS a MATCH (n) FOREACH (x IN [1] | SET n[a] = 1) RETURN n",
        admit(
            "WITH 1 AS a MATCH (n) FOREACH (x IN [1] | SET n[a] = 1) RETURN n",
            &[],
        ),
        &["Invalid input 'INTEGER' for node or relationship property key"],
    );
}

/// Every statement position the executor reaches is walked, not just the
/// head statement's own clauses: a write site inside a UNION arm, a
/// CIP-213 NEXT stage, or a CIP-135 WHEN/ELSE body is admitted by the
/// same rule. The scoped walk is called directly so the enclosing
/// statement battery (union-column and scope rules) does not pre-empt
/// the error under test.
#[test]
fn scoped_bindings_reach_union_next_and_conditional_positions() {
    fn scoped_error(query: &str) -> Result<(), EngineError> {
        let stmt = relate_parser::parse(query).expect("parse");
        validate_scoped_operands(&stmt, &BTreeMap::new())
    }
    for query in [
        "WITH 1 AS a MATCH (n) RETURN n AS x UNION MATCH (m) SET m[a] = 1 RETURN m AS x",
        "WITH 1 AS a MATCH (n) RETURN n AS x NEXT MATCH (m) SET m[a] = 1 RETURN m AS x",
        "WHEN true THEN WITH 1 AS a MATCH (n) SET n[a] = 1 RETURN n AS x ELSE RETURN 1 AS x",
    ] {
        assert_error(
            query,
            scoped_error(query),
            &["Invalid input 'INTEGER' for node or relationship property key"],
        );
    }
}
