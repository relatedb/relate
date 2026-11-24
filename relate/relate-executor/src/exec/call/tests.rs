//! Engine-level integration tests for the CALL-clause executors — the
//! subquery call arms and their transaction-error text — kept in the exec
//! layer beside the code they drive. The `LOAD CSV` engine tests live in
//! `exec::load_csv::tests` (the `unwind.rs` pattern).

use relate_core::{Error, Value};

use super::super::test_support::{
    assert_expression_cap, exec_autocommit, exec_cypher_on, try_exec_cypher_on,
};
use crate::{ExecGraph, Graph};

#[test]
fn transaction_error_message_keeps_the_pinned_uniqueness_wrapper() {
    // ADR-0172 pins the REPORT STATUS text byte-identical: the typed-detail
    // hand-over must not intercept the uniqueness violation's wrapper.
    let error = Error::uniqueness_violation("p_id");
    assert_eq!(
        super::transaction_error_message(&error),
        "A value already exists and violates a uniqueness constraint: \
         semantic error: ConstraintValidationFailed: `p_id` uniqueness"
    );
    // The other typed detail variants hand over the legacy bare inner text.
    let error = Error::type_error("schema name must be STRING");
    assert_eq!(
        super::transaction_error_message(&error),
        "TypeError: schema name must be STRING"
    );
    // The run-time argument rejection hands over its bare `Kind: detail`
    // text too — no `semantic error: ` prefix (it had none before the
    // variant split).
    let error = Error::argument_value_kind(relate_core::ErrorKind::NumberOutOfRange, "percentile");
    assert_eq!(
        super::transaction_error_message(&error),
        "NumberOutOfRange: percentile"
    );
    let error = Error::ty("anything");
    assert_eq!(super::transaction_error_message(&error), "anything");
}

/// LABS-198: shared-execution fast path for import-invariant `CALL { }`
/// bodies — the detector's admit/refuse line plus row-equivalence of the
/// shared merge against per-row semantics.
mod shared_subquery {
    use relate_ir::{Clause, Query};

    use super::super::shared::{import_invariant_body, shared_execution_plan};
    use super::*;

    fn call_body(cypher: &str) -> (Query, relate_ir::CallScope) {
        let query = relate_front::parse_and_lower(cypher).unwrap();
        for part in &query.parts {
            for clause in &part.clauses {
                if let Clause::CallSubquery { query, scope, .. } = clause {
                    return ((**query).clone(), scope.clone());
                }
            }
        }
        panic!("test query must contain a CALL subquery: {cypher}");
    }

    fn invariant(cypher: &str) -> bool {
        let store = Graph::default();
        let graph = ExecGraph::read(&store);
        let (body, scope) = call_body(cypher);
        let rows = vec![crate::row::Row::from([
            ("i".to_string(), Value::Int(1)),
            ("p".to_string(), Value::Int(1)),
        ])];
        let plan = shared_execution_plan(&graph, &scope, &rows)
            .expect("the fixture's imports are all in the outer row");
        let Some(plan) = plan else {
            panic!("fixture must admit the fast path: {cypher}");
        };
        import_invariant_body(&body, &plan.check())
    }

    #[test]
    fn admits_chained_aggregation_with_pass_through_import() {
        // The LABS-198 shape: the import threads the chain as a bare
        // pass-through; nothing computes from it.
        assert!(invariant(
            "UNWIND range(1,5) AS i \
             CALL { WITH i MATCH (n) WITH i, count(*) AS c WITH sum(c) AS s RETURN s } \
             RETURN s"
        ));
        // Explicit scope form, body ignoring the import entirely.
        assert!(invariant(
            "UNWIND range(1,5) AS i CALL (i) { UNWIND [1,2] AS j RETURN j } RETURN j"
        ));
        // A constant positional window shares fine (the import-invariant
        // row stream is identical per outer row).
        assert!(invariant(
            "UNWIND range(1,5) AS i CALL { WITH i UNWIND [1,2,3] AS j RETURN j LIMIT 2 } RETURN j"
        ));
        // A fresh relationship binding over an import-free node pattern
        // stays shareable.
        assert!(invariant(
            "UNWIND range(1,5) AS i CALL (i) { MATCH (a)-[r]->(b) RETURN count(r) AS c } RETURN c"
        ));
    }

    #[test]
    fn refuses_bodies_computing_from_the_import() {
        // Correlated MATCH property map.
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i MATCH (n {v: i}) RETURN count(*) AS c } RETURN c"
        ));
        // Correlated WHERE.
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i MATCH (n) WHERE n.v = i RETURN count(*) AS c } \
             RETURN c"
        ));
        // Correlated pattern variable (CIP-71 scenario 17 shape): a
        // pattern binding that names the import is a join on it, not a
        // fresh binding.
        assert!(!invariant(
            "MATCH (p) CALL (p) { MATCH (p)-[:FRIEND]->(f) RETURN count(f) AS d } RETURN d"
        ));
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { MATCH (n)-[i]->(m) RETURN count(*) AS c } RETURN c"
        ));
        // Import inside an aggregate argument.
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i MATCH (n) RETURN sum(i) AS s } RETURN s"
        ));
        // Renamed pass-through would need per-row value patching.
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i AS j RETURN j } RETURN j"
        ));
        // Nondeterministic functions sample once under sharing — at any
        // spelling: the IR keeps the user's case and the temporal suffixes
        // verbatim while dispatch lowercases and strips them.
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i RETURN rand() AS r } RETURN r"
        ));
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i RETURN UUID() AS u } RETURN u"
        ));
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i RETURN datetime.realtime() AS t } RETURN t"
        ));
        // A scope-bearing expression (EXISTS here) is refused wholesale:
        // its interior executes per row with its own scope semantics that
        // sharing does not analyze — and the nested clock call is itself
        // seen by the walk's function scan. The body is refused either way.
        assert!(!invariant(
            "UNWIND range(1,5) AS i \
             CALL { WITH i MATCH (n) WHERE EXISTS { MATCH (m) WHERE rand() < 0.5 } RETURN count(*) AS c } \
             RETURN c"
        ));
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i RETURN COLLECT { MATCH (n) RETURN n } AS ns } \
             RETURN ns"
        ));
        // Writes keep per-row side effects.
        assert!(!invariant(
            "UNWIND range(1,5) AS i CALL { WITH i CREATE (n:N) RETURN 1 AS one } RETURN one"
        ));
    }

    fn rows_of(cypher: &str) -> Vec<Vec<Value>> {
        let graph = Graph::default();
        let view = ExecGraph::read(&graph);
        exec_cypher_on(&view, cypher).rows
    }

    #[test]
    fn shared_execution_matches_per_row_results() {
        // Chained aggregation (LABS-198 repro shape, UNWIND-based body).
        assert_eq!(
            rows_of(
                "UNWIND range(1,4) AS i \
                 CALL { WITH i UNWIND range(1,3) AS j WITH i, count(*) AS c WITH sum(c) AS s RETURN s } \
                 RETURN i, s ORDER BY i"
            ),
            vec![
                vec![Value::Int(1), Value::Int(3)],
                vec![Value::Int(2), Value::Int(3)],
                vec![Value::Int(3), Value::Int(3)],
                vec![Value::Int(4), Value::Int(3)],
            ]
        );
        // Same-name pass-through keeps the outer row's value, and the
        // outer row order (including duplicates) is preserved.
        assert_eq!(
            rows_of("UNWIND [2,1,2] AS i CALL { WITH i UNWIND [10,20] AS j RETURN j } RETURN i, j"),
            vec![
                vec![Value::Int(2), Value::Int(10)],
                vec![Value::Int(2), Value::Int(20)],
                vec![Value::Int(1), Value::Int(10)],
                vec![Value::Int(1), Value::Int(20)],
                vec![Value::Int(2), Value::Int(10)],
                vec![Value::Int(2), Value::Int(20)],
            ]
        );
        // Empty body result drops the outer row under CALL and null-pads
        // under OPTIONAL CALL.
        assert_eq!(
            rows_of(
                "UNWIND [1,2] AS i OPTIONAL CALL { WITH i UNWIND [] AS z RETURN 1 AS x } RETURN i, x"
            ),
            vec![
                vec![Value::Int(1), Value::Null],
                vec![Value::Int(2), Value::Null],
            ]
        );
        assert_eq!(
            rows_of("UNWIND [1,2] AS i CALL { WITH i UNWIND [] AS z RETURN 1 AS x } RETURN i"),
            Vec::<Vec<Value>>::new()
        );
        // Renamed pass-through stays on the per-row path and still tracks
        // the outer value.
        assert_eq!(
            rows_of("UNWIND [1,2] AS i CALL (i) { WITH i AS j RETURN j } RETURN i, j ORDER BY i"),
            vec![
                vec![Value::Int(1), Value::Int(1)],
                vec![Value::Int(2), Value::Int(2)]
            ]
        );
        // Import stickiness: an import referenced after a WITH dropped it
        // still resolves to the outer binding (this shape computes from the
        // import, so it runs per-row — the pin is the semantics, not the
        // path taken).
        assert_eq!(
            rows_of(
                "UNWIND [1,2] AS i CALL (i) { UNWIND [10,20] AS j WITH j RETURN i + j AS k } \
                 RETURN i, k ORDER BY i, k"
            ),
            vec![
                vec![Value::Int(1), Value::Int(11)],
                vec![Value::Int(1), Value::Int(21)],
                vec![Value::Int(2), Value::Int(12)],
                vec![Value::Int(2), Value::Int(22)],
            ]
        );
        // Per-row entropy: uuid() stays per-row, drawing a fresh value per
        // outer row.
        let drawn = rows_of("UNWIND range(1,8) AS i CALL { WITH i RETURN uuid() AS u } RETURN u");
        let mut as_text: Vec<String> = drawn.iter().map(|r| format!("{:?}", r[0])).collect();
        as_text.sort();
        as_text.dedup();
        assert_eq!(as_text.len(), drawn.len());
        // Correlated bodies keep per-row semantics.
        // Per-row order is outer-row order; no sort needed.
        let correlated = rows_of(
            "UNWIND [1,2,3] AS i CALL { WITH i UNWIND range(1, i) AS j RETURN count(*) AS c } \
             RETURN i, c",
        );
        assert_eq!(
            correlated,
            vec![
                vec![Value::Int(1), Value::Int(1)],
                vec![Value::Int(2), Value::Int(2)],
                vec![Value::Int(3), Value::Int(3)],
            ]
        );
    }
}

#[test]
fn transaction_error_message_keeps_the_legacy_text_of_adr_0307_variants() {
    // ADR-0307's typed variants report exactly what their stringly
    // predecessors did (CIP-198 pins `'/ by zero'` in REPORT STATUS).
    let cases = [
        (
            Error::arithmetic(relate_core::ArithmeticFailure::DivisionByZero),
            Error::ty("/ by zero"),
        ),
        (
            Error::arithmetic(relate_core::ArithmeticFailure::ModuloByZero),
            Error::ty("% by zero"),
        ),
        (
            Error::arithmetic(relate_core::ArithmeticFailure::RoundingNecessary),
            Error::execution("ArithmeticError: rounding necessary"),
        ),
        (
            Error::database_not_found("nosuchdb"),
            Error::semantic("DatabaseNotFound: Graph not found: nosuchdb"),
        ),
        (Error::cancelled(), Error::execution("query cancelled")),
    ];
    for (typed, legacy) in cases {
        assert_eq!(
            super::transaction_error_message(&typed),
            super::transaction_error_message(&legacy),
            "{typed:?}"
        );
    }
    assert_eq!(
        super::transaction_error_message(&Error::arithmetic(
            relate_core::ArithmeticFailure::DivisionByZero
        )),
        "/ by zero"
    );
}

/// An aggregating `WITH` inside a per-row `CALL (vars) { … }` body keeps
/// its empty-group identity row, and an imported variable the body reads
/// *after* the aggregation stays visible: an import is never a grouping
/// key of a body `WITH`, so a preceding clause that empties the input
/// still yields `count(*) = 0` and the concluding `RETURN` can add the
/// import to it.
#[test]
fn subquery_aggregation_over_an_emptied_input_keeps_the_identity_row() {
    let graph = Graph::default();
    let view = ExecGraph::read(&graph);
    let emptied_by_unwind = exec_cypher_on(
        &view,
        "UNWIND [0, 1] AS x CALL (x) { UNWIND [] AS i WITH count(*) AS c RETURN c } \
         RETURN x, c ORDER BY x",
    );
    assert_eq!(
        emptied_by_unwind.columns,
        vec!["x".to_string(), "c".to_string()]
    );
    assert_eq!(
        emptied_by_unwind.rows,
        vec![
            vec![Value::Int(0), Value::Int(0)],
            vec![Value::Int(1), Value::Int(0)],
        ]
    );
    // The emptying clause is the filter here (`WHERE i < 0`), and the
    // imported `x` is read only after the aggregation.
    let emptied_by_filter = exec_cypher_on(
        &view,
        "UNWIND [0, 1, 2] AS x \
         CALL (x) { UNWIND range(1, 3) AS i WITH i WHERE i < 0 WITH count(*) AS c \
                    RETURN x + c AS result } \
         RETURN x, result ORDER BY x",
    );
    assert_eq!(
        emptied_by_filter.rows,
        vec![
            vec![Value::Int(0), Value::Int(0)],
            vec![Value::Int(1), Value::Int(1)],
            vec![Value::Int(2), Value::Int(2)],
        ]
    );
}

/// LABS-712: a scoped `CALL` body's `RETURN *` arm must not leak the body's
/// imports into the UNION column set. Both arms export only `x`; the import
/// `a` passes through from the outer row.
#[test]
fn scoped_call_union_star_arm_exports_only_the_body_names() {
    let graph = Graph::default();
    let view = ExecGraph::read(&graph);
    let result = exec_cypher_on(
        &view,
        "WITH 1 AS a \
         CALL (a) { LET x = a + 1 RETURN * UNION RETURN 2 AS x } \
         RETURN a, x",
    );
    assert_eq!(result.columns, vec!["a".to_string(), "x".to_string()]);
    assert_eq!(result.rows, vec![vec![Value::Int(1), Value::Int(2)]]);
}

/// LABS-712 (stellar round 5): an all-unit scoped body's arms still seed a
/// `NEXT` stage when the body has one, so their row-derived shapes must
/// agree — the mismatch is the typed UNION error, never a non-rectangular
/// result that panics the seed projection. The arms' new bindings differ
/// (`x` vs `y`), and both arms produce a row once the driving row arrives.
#[test]
fn next_stage_over_all_unit_scoped_union_arms_rejects_mismatched_shapes() {
    let graph = Graph::default();
    let error = exec_autocommit(
        &graph,
        "UNWIND [1, 2] AS i \
         CALL (i) { CREATE (x:T1), (y:T2) UNION ALL CREATE (z:T1) NEXT RETURN 1 AS c } \
         RETURN c",
    )
    .err()
    .expect("the arms' row-derived columns differ");
    assert!(
        error.to_string().contains("UNION requires same columns"),
        "expected the typed UNION error, got: {error}"
    );
}

/// LABS-712: a UNION body whose arms all project no rows has no export shape
/// to agree on — `call_subquery_row` short-circuits it to the outer row — so
/// the fold must not compare the arms' arbitrary row-derived columns. The
/// arms' new bindings differ (`x` vs `y`), and the mismatch only surfaces
/// once a driving row makes both arms produce a row.
#[test]
fn all_unit_scoped_call_union_arms_need_no_column_agreement() {
    let graph = Graph::default();
    let result = exec_autocommit(
        &graph,
        "UNWIND [1, 2] AS i \
         CALL (i) { CREATE (x:T1) UNION ALL CREATE (y:T2) } \
         RETURN count(*) AS c",
    )
    .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(2)]]);
}

/// REL8-1845 (ADR-0463): the subquery product's materializing form —
/// a terminal `CALL { … }` with a plain projection over it — charges
/// each outer row's staged body result, so 5_000 driving rows x a
/// 200-row body refuses at the lowered cap instead of building the
/// million rows. Parametrized over the body: an import-computing body
/// pins the per-row path, an import-invariant one the shared fast path
/// (its one body execution, then the first outer row's merged view
/// crosses the cap).
fn plain_projection_charges_the_product(body: &str, expected_shared_runs: u64) {
    let mut graph = Graph::default();
    graph.configure_expression_memory_limit(Some(1 << 20));
    let view = ExecGraph::read(&graph);
    let error = try_exec_cypher_on(
        &view,
        &format!(
            "UNWIND range(1, 5_000) AS i \
             CALL (i) {{ {body} }} \
             RETURN v"
        ),
    )
    .err()
    .expect("a materialized subquery product must be refused at the lowered cap");
    assert_expression_cap(&error, 1 << 20);
    assert_eq!(
        super::feed::take_shared_body_runs(),
        expected_shared_runs,
        "the mode the refusal pins"
    );
}

#[test]
fn a_plain_projection_over_the_subquery_product_charges_the_staged_rows() {
    plain_projection_charges_the_product("UNWIND range(1, 200) AS j RETURN i * 1000 + j AS v", 0);
}

#[test]
fn a_plain_projection_over_the_shared_product_charges_the_merged_rows() {
    plain_projection_charges_the_product("UNWIND range(1, 200) AS j RETURN j AS v", 1);
}

/// REL8-1845 (ADR-0555): the same body, the same cap, the same clause —
/// only the consumer differs. A folding consumer keeps the product's
/// feed, so the million merged rows are never held and the answer is
/// one integer. Parametrized over the body the same way: the shared
/// member asserts the fast path ran the body once for all 5_000 outer
/// rows, the per-row member that it never engaged — the statement's
/// rows alone cannot tell the two modes apart.
fn folding_consumer_streams_the_product(body: &str, expected_shared_runs: u64) {
    let mut graph = Graph::default();
    graph.configure_expression_memory_limit(Some(1 << 20));
    let view = ExecGraph::read(&graph);
    let result = exec_cypher_on(
        &view,
        &format!(
            "UNWIND range(1, 5_000) AS i \
             CALL (i) {{ {body} }} \
             RETURN count(v) AS c"
        ),
    );
    assert_eq!(result.rows, vec![vec![Value::Int(1_000_000)]]);
    assert_eq!(
        super::feed::take_shared_body_runs(),
        expected_shared_runs,
        "the mode the fold pinned"
    );
}

#[test]
fn a_folding_consumer_streams_the_subquery_product_whatever_its_size() {
    folding_consumer_streams_the_product("UNWIND range(1, 200) AS j RETURN i * 1000 + j AS v", 0);
}

#[test]
fn a_folding_consumer_streams_the_shared_execution_product() {
    folding_consumer_streams_the_product("UNWIND range(1, 200) AS j RETURN j AS v", 1);
}

/// REL8-1845 (round 3): a writing body never parks, whatever its
/// consumer — `query_writes` is the by-construction guard, so a writing
/// body's semantics never depend on when the consuming projection
/// evaluates its items. The ordered window sits on the WITH — the CALL's
/// direct follower, the clause `call_product_parks` consults — so the
/// admission is reached and the guard is what holds this body. The
/// asserted answer is the
/// materializing order — every row's `COUNT { … }` sees all three
/// writes; today's window evaluates its items after the feed drains, so
/// the interleave the guard forecloses is not directly observable.
#[test]
fn a_writing_body_never_parks_whatever_its_consumer() {
    let mut graph = Graph::default();
    graph.configure_expression_memory_limit(Some(1 << 20));
    let result = exec_autocommit(
        &graph,
        "UNWIND range(1, 3) AS i \
         CALL (i) { WITH i CREATE (:N) } \
         WITH i, COUNT { MATCH (m:N) RETURN m } AS c \
         RETURN i, c ORDER BY c DESC, i LIMIT 2",
    )
    .unwrap();
    let counts: Vec<i64> = result
        .rows
        .iter()
        .map(|row| row[1].as_int().unwrap_or_default())
        .collect();
    assert_eq!(counts, vec![3, 3], "{:?}", result.rows);
}

/// ADR-0660's retain disposition, charged per batch (ADR-0463): each
/// `CALL { … } IN TRANSACTIONS` batch's output is its own charged
/// allocation, so a many-batch call is bounded by one batch's rows, not
/// the accumulated product. The cap below sits between the two figures —
/// one batch's output fits, every batch charged into one running total
/// does not — and the folding consumer keeps the projection from charging
/// rows of its own, so the accumulating form is what would refuse this
/// call.
#[test]
fn the_in_transactions_output_is_charged_per_batch() {
    const CAP: u64 = 64 * 1024;
    const BATCHES: usize = 60;
    const PER_BATCH: usize = 20;
    // A batch's rows carry the imported `i` beside the exported `v`.
    let one_batch = crate::workspace::retained_rows_bytes(PER_BATCH, 2);
    let whole_product = crate::workspace::retained_rows_bytes(BATCHES * PER_BATCH, 2);
    assert!(
        one_batch < CAP && CAP < whole_product,
        "the cap must admit one batch and refuse the accumulation: \
         {one_batch} < {CAP} < {whole_product}"
    );
    let mut graph = Graph::default();
    graph.configure_expression_memory_limit(Some(CAP));
    let result = exec_autocommit(
        &graph,
        &format!(
            "UNWIND range(1, {BATCHES}) AS i \
             CALL (i) {{ WITH i UNWIND range(1, {PER_BATCH}) AS j \
             RETURN i * 1000 + j AS v }} \
             IN TRANSACTIONS OF 1 ROWS \
             RETURN count(v) AS c"
        ),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Int((BATCHES * PER_BATCH) as i64)]]
    );
}
