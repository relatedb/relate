//! The `eval` dispatcher's unit tests, split from the dispatcher itself
//! (`exec` keeps the same `*_tests.rs` precedent): the scalar-eval class
//! backstops, the ADR-0511 boolean-family classification, the payload
//! sharing and undefined-variable pins, the map-projection store reads,
//! and the ADR-0463 retaining-container caps.

use super::*;
use crate::graph::Graph;
use relate_core::VectorCoordKind;

/// CIP-272: a map comprehension's key must be a non-null string. Live
/// Neo4j 2026.10.1 rejects a null key with 22G03/22N01; it does not drop
/// the entry from the produced map.
#[test]
fn map_comprehension_rejects_a_null_key() {
    let g = Graph::default();
    for cypher in [
        "RETURN { i: j IN {a: 1} | null : i } AS result",
        "RETURN { i IN range(0, 3) | null : i } AS result",
        "WITH [\"name\"] AS keys, [\"Pelle\", date(\"2024-05-10\")] AS values \
         RETURN { i IN range(0, size(values) - 1) | keys[i] : values[i] } AS result",
    ] {
        let err = match crate::exec::test_support::try_exec_cypher(&g, cypher) {
            Ok(_) => panic!("{cypher}: expected an error"),
            Err(e) => e,
        };
        assert!(
            matches!(err, relate_core::Error::Type(_)),
            "{cypher}: {err:?}"
        );
    }
}

/// The scalar-eval backstop for an aggregate that reached evaluation
/// outside an aggregation context is a `Syntax` error, not a semantic
/// one: Neo4j classifies it 42I24 under 42001 (`SyntaxError`), and the
/// front-end rejections that own most of these contexts use the same
/// class (W5 semantic-error corpus).
#[test]
fn aggregate_backstop_is_a_syntax_error() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let row = Row::new();
    for expr in [
        Expr::CountStar,
        Expr::Function {
            name: "count".into(),
            distinct: false,
            args: vec![Expr::Int(1)],
        },
        Expr::Function {
            name: "collect".into(),
            distinct: false,
            args: vec![Expr::Int(1)],
        },
    ] {
        let err = eval(&graph, &row, &expr, &params).unwrap_err();
        assert!(
            matches!(err, relate_core::Error::Syntax(_)),
            "{expr:?}: {err:?}"
        );
    }
}

/// ADR-0511 (LABS-400): the non-boolean arm is the typed
/// `InvalidArgumentType` variant, so the class table (ABI v2) reports
/// Neo4j's TypeError class instead of the SyntaxError class ADR-0003
/// pinned for this family, while the "requires a boolean" wording the
/// message contract carries survives.
#[test]
fn a_non_boolean_operand_is_the_typed_invalid_argument_type() {
    for ctx in ["quantifier WHERE", "WHEN predicate", "AND"] {
        let err = bool_or_null(&Value::Int(1), ctx).unwrap_err();
        assert!(
            matches!(err, Error::InvalidArgumentType(_)),
            "{ctx}: {err:?}"
        );
        assert!(
            err.to_string().contains("requires a boolean"),
            "{ctx}: {err}"
        );
        assert_eq!(
            relate_error::error_info(&err).neo4j_code,
            relate_error::TYPE_ERROR_CODE,
            "{ctx}: {err}"
        );
    }
    // The boolean and null readings are untouched.
    assert_eq!(bool_or_null(&Value::Bool(true), "AND").unwrap(), Some(true));
    assert_eq!(bool_or_null(&Value::Null, "AND").unwrap(), None);
}

/// ADR-0458 §7: the predicate positions apply the deprecated list/path
/// runtime conversion (a property read has no statically known type), and
/// a non-boolean predicate is the typed `InvalidArgumentType` rather than a
/// silent no-match.
#[test]
fn predicate_positions_convert_a_runtime_list_and_reject_other_non_booleans() {
    assert!(predicate_value_holds(&Value::Bool(true)).unwrap());
    assert!(!predicate_value_holds(&Value::Bool(false)).unwrap());
    assert!(!predicate_value_holds(&Value::Null).unwrap());
    assert!(predicate_value_holds(&Value::List(vec![Value::Int(1)].into())).unwrap());
    assert!(!predicate_value_holds(&Value::List(Vec::new().into())).unwrap());
    let err = predicate_value_holds(&Value::Int(1)).unwrap_err();
    assert!(
        matches!(err, Error::InvalidArgumentType(_))
            && err.to_string().contains("requires a boolean"),
        "{err:?}"
    );
}

/// LABS-156 (ADR-0170): reading a bound variable and carrying a row
/// share the String/List/Map/Vector payload (a refcount bump) instead of
/// deep-copying it, so the cost no longer scales with the payload size.
#[test]
fn variable_read_and_row_carry_share_payloads() {
    let mut row = Row::new();
    row.insert(
        "list".into(),
        Value::List((0..10_000).map(Value::Int).collect::<Vec<_>>().into()),
    );
    row.insert(
        "map".into(),
        Value::Map(
            BTreeMap::from([
                ("a".to_string(), Value::Int(1)),
                ("b".to_string(), Value::String("two".into())),
            ])
            .into(),
        ),
    );
    row.insert("string".into(), Value::String("x".repeat(1_000).into()));
    row.insert(
        "vector".into(),
        Value::Vector {
            coord: VectorCoordKind::Float64,
            values: vec![Value::Float(1.0), Value::Float(2.0), Value::Float(3.0)].into(),
        },
    );

    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let read = |name: &str| eval(&graph, &row, &Expr::Variable(name.into()), &params).unwrap();

    let (Value::List(bound), Value::List(read_back)) = (&row["list"], read("list")) else {
        panic!("list variable did not read back as a list");
    };
    assert_eq!(read_back.len(), 10_000);
    assert!(Arc::ptr_eq(bound, &read_back));

    let (Value::Map(bound), Value::Map(read_back)) = (&row["map"], read("map")) else {
        panic!("map variable did not read back as a map");
    };
    assert!(Arc::ptr_eq(bound, &read_back));

    let (Value::String(bound), Value::String(read_back)) = (&row["string"], read("string")) else {
        panic!("string variable did not read back as a string");
    };
    assert!(read_back.len() >= 1_000);
    assert!(Arc::ptr_eq(bound, &read_back));

    let (
        Value::Vector { values: bound, .. },
        Value::Vector {
            values: read_back, ..
        },
    ) = (&row["vector"], read("vector"))
    else {
        panic!("vector variable did not read back as a vector");
    };
    assert!(Arc::ptr_eq(bound, &read_back));

    // Operators clone the input row for every output row (expand,
    // pattern matching, ...); that carry shares the list, not copies it.
    let carried = row.clone();
    let (Value::List(original), Value::List(cloned)) = (&row["list"], &carried["list"]) else {
        panic!("cloned row lost the list binding");
    };
    assert!(Arc::ptr_eq(original, cloned));
}

/// ADR-0335: an unbound variable is the typed undefined-variable error,
/// never a fabricated null, in both the scalar and the map-projection
/// arms.
#[test]
fn unbound_variables_fail_with_undefined_variable() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let row = Row::new();

    let error = eval(&graph, &row, &Expr::Variable("missing".into()), &params).unwrap_err();
    assert_eq!(error, Error::undefined_variable("missing"));
    assert_eq!(
        error.to_string(),
        "syntax error: UndefinedVariable: variable 'missing' not defined"
    );

    let projection = Expr::MapProjection {
        expr: Box::new(Expr::Map(Vec::new())),
        items: vec![MapProjectionItem::Variable {
            name: "missing".into(),
        }],
    };
    assert_eq!(
        eval(&graph, &row, &projection, &params).unwrap_err(),
        Error::undefined_variable("missing")
    );
}

/// A `{.*}` all-properties map projection reads a bound entity's
/// properties from its store entry, the same policy `properties()`
/// follows (CIP-257): the bound value carries labels but no property
/// map, so projecting from the value alone would answer `{}`.
#[test]
fn all_properties_map_projection_reads_the_store_entry() {
    use crate::exec::test_support::{exec_cypher, Builder};
    let builder = Builder::new();
    let a = builder.anon(&["Person"]);
    let b = builder.anon(&["Other"]);
    builder.rel(a, b, "KNOWS", &[("since", Value::Int(2020))]);
    let g = builder.done();

    // The node's store entry, not the empty property map carried by the
    // bound value; the builder's anonymous nodes carry no properties.
    assert_eq!(
        exec_cypher(&g, "MATCH (n:Person) RETURN n{.*, k: 1} AS p").rows,
        vec![vec![Value::Map(
            BTreeMap::from([("k".to_string(), Value::Int(1))]).into()
        )]]
    );
    // The relationship arm follows the same policy: the stored property
    // is projected, and an explicit key overrides it.
    assert_eq!(
        exec_cypher(&g, "MATCH ()-[r:KNOWS]->() RETURN r{.*} AS p").rows,
        vec![vec![Value::Map(
            BTreeMap::from([("since".to_string(), Value::Int(2020))]).into()
        )]]
    );
    assert_eq!(
        exec_cypher(&g, "MATCH ()-[r:KNOWS]->() RETURN r{.*, since: 7} AS p").rows,
        vec![vec![Value::Map(
            BTreeMap::from([("since".to_string(), Value::Int(7))]).into()
        )]]
    );
}

/// ADR-0463 (post-#561): projection output rows form one retaining
/// container too. With a small configured cap, two rows whose payloads
/// are individually under it cross the container's total and refuse
/// typed instead of accumulating.
#[test]
fn a_retaining_projection_result_is_capped_by_its_running_total() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(1_000));
    let pad = "p".repeat(600);
    let query = format!("UNWIND range(1, 2) AS i RETURN toString(i) + '{pad}' AS s");
    let error = crate::exec::test_support::try_exec_cypher(&g, &query)
        .err()
        .expect("the retaining projection result must be refused");
    crate::exec::test_support::assert_expression_cap(&error, 1_000);
}

/// ADR-0463 round 4: a retaining container's running total is enforced,
/// even when every element and the slot buffer are individually small.
/// One million ~200-byte strings in a comprehension cross 256 MiB of
/// retained payload and must hit the typed refusal, not the allocator.
#[test]
fn a_retaining_comprehension_is_capped_by_its_running_total() {
    let pad = "p".repeat(200);
    let query = format!("RETURN [x IN range(1, 1000000) | x + '{pad}'] AS v");
    let error = crate::exec::test_support::try_exec_cypher(&Graph::default(), &query)
        .err()
        .expect("the retaining comprehension must be refused");
    crate::exec::test_support::assert_expression_cap(
        &error,
        crate::memory::EXPRESSION_BUDGET_BYTES,
    );
}

/// The same retained-total rule for `collect()`'s vals vector.
#[test]
fn a_retaining_collect_is_capped_by_its_running_total() {
    let pad = "p".repeat(200);
    let query = format!("UNWIND range(1, 1000000) AS x RETURN size(collect(x + '{pad}')) AS n");
    let error = crate::exec::test_support::try_exec_cypher(&Graph::default(), &query)
        .err()
        .expect("the retaining collect must be refused");
    crate::exec::test_support::assert_expression_cap(
        &error,
        crate::memory::EXPRESSION_BUDGET_BYTES,
    );
}

/// ADR-0463 (per-allocation form): a long streaming statement that
/// builds one small string per row must complete — the cap checks each
/// allocation, never a cumulative statement total. This is the bulk-load
/// regression for the false-positive class the cumulative meter caused.
#[test]
fn a_long_streaming_statement_stays_under_the_per_allocation_cap() {
    let g = Graph::default();
    let result = crate::exec::test_support::try_exec_cypher(
        &g,
        "UNWIND range(1, 1000000) AS x WITH toString(x) + '-' AS s RETURN count(s) AS c",
    )
    .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::Int(1_000_000));
}
