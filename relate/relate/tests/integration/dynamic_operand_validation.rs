//! LABS-709: dynamic label/property-key operands whose value is statically
//! known at admission are rejected before execution, so a bad one cannot
//! hide behind a zero-row `MATCH`. The operand's own constant text is covered
//! by the front's unit tests; these exercise the two paths that only the
//! engine's admission has — bound parameters and clause-scoped bindings —
//! plus the runtime-typed case that must keep working.

use std::collections::BTreeMap;

use relate::{Database, Value};

fn connection() -> relate::Connection {
    let db = Database::open(":memory:").unwrap();
    db.connect().unwrap()
}

fn params(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect()
}

fn error(query: &str, params: BTreeMap<String, Value>) -> String {
    let mut conn = connection();
    conn.query_with_params(query, params)
        .expect_err(query)
        .to_string()
}

fn assert_ok(query: &str, params: BTreeMap<String, Value>) -> relate::QueryResult {
    let mut conn = connection();
    conn.query_with_params(query, params)
        .unwrap_or_else(|error| panic!("`{query}`: {error}"))
}

fn map_param() -> Value {
    Value::Map(std::sync::Arc::new(BTreeMap::from([(
        "x".to_string(),
        Value::Int(1),
    )])))
}

#[test]
fn parameter_typed_dynamic_property_key_is_rejected_on_a_zero_row_match() {
    for (value, type_name) in [
        (Value::Int(1), "INTEGER"),
        (Value::Bool(true), "BOOLEAN"),
        (map_param(), "MAP"),
    ] {
        let needle = format!("Invalid input '{type_name}' for node or relationship property key");
        for query in [
            "MATCH (n) SET n[$a] = 1 RETURN n",
            "MATCH (n) REMOVE n[$a] RETURN n",
        ] {
            let rendered = error(query, params(&[("a", value.clone())]));
            assert!(
                rendered.contains(&needle),
                "`{query}`: expected {needle:?} in {rendered:?}"
            );
        }
    }
}

#[test]
fn parameter_typed_dynamic_label_is_rejected_on_a_zero_row_match() {
    for (value, type_name) in [(Value::Int(1), "INTEGER"), (map_param(), "MAP")] {
        let needle = format!("Invalid input '{type_name}' for dynamic label");
        for query in [
            "MATCH (n) SET n:$($a) RETURN labels(n)",
            "MATCH (n) REMOVE n:$($a) RETURN labels(n)",
        ] {
            let rendered = error(query, params(&[("a", value.clone())]));
            assert!(
                rendered.contains(&needle),
                "`{query}`: expected {needle:?} in {rendered:?}"
            );
        }
    }
}

#[test]
fn clause_bound_variable_is_rejected_on_a_zero_row_match() {
    for (binding, type_name) in [
        ("1 + 2", "INTEGER"),
        ("point({x: 2.3, y: 4.5})", "POINT"),
        ("localdatetime(\"2024185T19:32:24\")", "LOCAL DATETIME"),
    ] {
        let needle = format!("Invalid input '{type_name}' for node or relationship property key");
        let query = format!("WITH {binding} AS a MATCH (n) SET n[a] = 1 RETURN n");
        let rendered = error(&query, params(&[]));
        assert!(
            rendered.contains(&needle),
            "`{query}`: expected {needle:?} in {rendered:?}"
        );
    }
}

#[test]
fn constant_constructor_operands_are_rejected() {
    for query in [
        "MATCH (n) SET n[point({x:3,y:0})] = 1 RETURN n",
        "MATCH (n) REMOVE n[localdatetime(\"2024185T19:32:24\")] RETURN n",
        "MATCH (n) SET n:$(point({x:3,y:0})) RETURN labels(n)",
    ] {
        let rendered = error(query, params(&[]));
        assert!(
            rendered.contains("Invalid input 'POINT' for")
                || rendered.contains("Invalid input 'LOCAL DATETIME' for"),
            "`{query}`: unexpected error {rendered:?}"
        );
    }
}

#[test]
fn subquery_nested_dynamic_label_self_reference_is_rejected() {
    let query = "CREATE (:_1) CREATE (n:$(COLLECT { \
                 MATCH (n) WITH COUNT(n) AS cnt RETURN '_' + cnt })) \
                 RETURN substring(labels(n)[0], 1)";
    let rendered = error(query, params(&[]));
    assert!(
        rendered.contains("cannot be created and referenced in the same clause"),
        "unexpected error {rendered:?}"
    );
}

/// A string parameter and a runtime-typed variable are legitimate dynamic
/// names: the admission rule must not reject them.
#[test]
fn string_parameters_and_runtime_typed_names_keep_working() {
    let result = assert_ok(
        "CREATE (n) WITH n SET n[$a] = 1 RETURN n.k",
        params(&[("a", Value::String("k".into()))]),
    );
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);

    let result = assert_ok(
        "CREATE (n {name: 'k'}) WITH n SET n[n.name] = 2 RETURN n.k",
        params(&[]),
    );
    assert_eq!(result.rows, vec![vec![Value::Int(2)]]);
}

/// `EXPLAIN` compiles the wrapped statement through the same admission, so
/// the parameter rule applies there too (and nothing executes).
#[test]
fn explain_rejects_the_same_parameter_typed_operand() {
    let rendered = error(
        "EXPLAIN MATCH (n) SET n[$a] = 1 RETURN n",
        params(&[("a", Value::Int(1))]),
    );
    assert!(
        rendered.contains("Invalid input 'INTEGER' for node or relationship property key"),
        "unexpected error {rendered:?}"
    );
}
