//! REL8-2033 (corpus issue 05): a comparison that only asks whether a
//! counted set is empty answers `EXISTS` / `NOT EXISTS`, which stops at the
//! first match where `COUNT { }` runs its body to completion for every
//! outer row and `size([...])` builds the whole list first.
//!
//! The rewrite is lowering-time and shape-preserving, so these tests pin
//! both halves: every spelling answers what the `EXISTS` spelling answers,
//! and the PROFILE reads of the rewritten spelling stay flat in the anchor
//! degree.

mod common;

use common::{connection, first_column, for_each_engine, plan, run, tree_metric};
use relate::Value;

/// Fifty `:H` hubs, each with twenty `:T` edges to its own `:X` nodes, so a
/// degree-linear spelling reads 1,000 edges while an early-stopping one
/// reads one per hub.
fn seed(conn: &mut relate::Connection) {
    run(conn, "UNWIND range(1, 50) AS i CREATE (:H {id: i})");
    run(
        conn,
        "MATCH (h:H) UNWIND range(1, 20) AS j CREATE (h)-[:T]->(:X {id: j})",
    );
}

/// Every emptiness spelling over the same pattern answers its `EXISTS`
/// twin's answer: positive spellings 50, negative spellings 0.
#[test]
fn emptiness_spellings_answer_their_exists_twin() {
    for_each_engine(|engine, conn| {
        seed(conn);
        for (spelling, expected) in [
            ("EXISTS { (h)-[:T]->() }", 50),
            ("COUNT { (h)-[:T]->() } > 0", 50),
            ("COUNT { (h)-[:T]->() } >= 1", 50),
            ("COUNT { (h)-[:T]->() } <> 0", 50),
            ("0 < COUNT { (h)-[:T]->() }", 50),
            ("1 <= COUNT { (h)-[:T]->() }", 50),
            ("size([(h)-[:T]->() | 1]) > 0", 50),
            // Function names bind case-insensitively.
            ("SIZE([(h)-[:T]->() | 1]) > 0", 50),
            ("NOT EXISTS { (h)-[:T]->() }", 0),
            ("COUNT { (h)-[:T]->() } = 0", 0),
            ("COUNT { (h)-[:T]->() } < 1", 0),
            ("COUNT { (h)-[:T]->() } <= 0", 0),
            ("0 = COUNT { (h)-[:T]->() }", 0),
            ("0 >= COUNT { (h)-[:T]->() }", 0),
            ("size([(h)-[:T]->() | 1]) = 0", 0),
        ] {
            let query = format!("MATCH (h:H) WHERE {spelling} RETURN count(*) AS n");
            assert_eq!(
                first_column(&run(conn, &query)),
                vec![Value::Int(expected)],
                "[{engine}] {spelling}"
            );
        }
        // A comprehension predicate moves into the probe's body unchanged.
        let with_predicate = "MATCH (h:H) \
             WHERE size([(h)-[:T]->(x) WHERE x.id = 3 | 1]) > 0 \
             RETURN count(*) AS n";
        assert_eq!(
            first_column(&run(conn, with_predicate)),
            vec![Value::Int(50)],
            "[{engine}] the comprehension's WHERE filters the probe"
        );
        // A non-literal projection is not rewritten (it would drop the
        // projection's evaluation), and still answers.
        let non_literal = "MATCH (h:H) WHERE size([(h)-[:T]->(x) | x.id]) > 0 \
             RETURN count(*) AS n";
        assert_eq!(
            first_column(&run(conn, non_literal)),
            vec![Value::Int(50)],
            "[{engine}] a non-literal projection keeps its own evaluation"
        );
    });
}

/// The rewritten spelling stops at the first matching edge: its reads stay
/// near the label scan (one read per hub), not the 1,000-edge walk the
/// count spelling performs. Two hundred is a tenth of the walk, far above
/// the scan and far below the unrewritten cost.
#[test]
fn an_emptiness_comparison_stops_at_the_first_match() {
    for_each_engine(|engine, conn| {
        seed(conn);
        for (spelling, expected) in [
            ("COUNT { (h)-[:T]->() } > 0", 50),
            ("COUNT { (h)-[:T]->() } = 0", 0),
            ("size([(h)-[:T]->() | 1]) > 0", 50),
            // Function names bind case-insensitively.
            ("SIZE([(h)-[:T]->() | 1]) > 0", 50),
        ] {
            let profiled = run(
                conn,
                &format!("PROFILE MATCH (h:H) WHERE {spelling} RETURN count(*) AS n"),
            );
            assert_eq!(
                first_column(&profiled),
                vec![Value::Int(expected)],
                "[{engine}] {spelling}: the profiled statement answers"
            );
            let reads = tree_metric(plan(&profiled), |m| m.db_hits);
            assert!(
                reads < 200,
                "[{engine}] {spelling}: {reads} reads — the count spelling's \
                 1,000-edge walk runs for every outer row, the probe stops at \
                 the first match"
            );
        }
    });
}

/// The rewrite is lowering-time and shape-visible: `EXPLAIN` of the count
/// spelling reads the same clause shape as the `EXISTS` spelling.
#[test]
fn the_rewritten_spelling_explains_like_exists() {
    let mut conn = connection();
    seed(&mut conn);
    let mut explained = |stmt: &str| {
        let result = run(&mut conn, &format!("EXPLAIN {stmt}"));
        plan(&result).to_value().to_string()
    };
    let exists = explained("MATCH (h:H) WHERE EXISTS { (h)-[:T]->() } RETURN count(*) AS n");
    let counted = explained("MATCH (h:H) WHERE COUNT { (h)-[:T]->() } > 0 RETURN count(*) AS n");
    assert_eq!(counted, exists, "the count spelling plans the exists shape");
}

/// A CIP-213 `NEXT` body keeps its own count: the existence evaluator
/// answers from the first stage (corpus issue 80), so rewriting a count
/// that spans the chain would flip the answer — the second stage here
/// matches nothing, so the count is 0 where `EXISTS` would say 1.
#[test]
fn a_next_chain_body_keeps_its_count() {
    let mut conn = connection();
    run(&mut conn, "CREATE (:H)");
    let counted = "MATCH (h:H) WHERE COUNT { RETURN 1 AS x NEXT MATCH (n:Missing) RETURN n } > 0 \
         RETURN count(*) AS n";
    assert_eq!(
        first_column(&run(&mut conn, counted)),
        vec![Value::Int(0)],
        "the count spans the NEXT chain"
    );
}
