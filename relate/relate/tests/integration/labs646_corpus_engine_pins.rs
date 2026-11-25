//! LABS-646 pins: the two correctness surprises the PathDB corpus spike
//! (2026-11-01) characterized while translating the 142 RPQs. Both are
//! pinned as behavior tests so neither can change silently — and so the
//! corpus's recorded row counts stay explainable.
//!
//! 1. `count(p)` over a **fixed-length** path pattern raises
//!    `UndefinedVariable: variable 'p' not defined`, while the same
//!    aggregation over a var-length spelling of the identical language
//!    (`*1..1`) and `count(*)` both work. The bound path variable of a
//!    fixed-length pattern is not visible to the aggregate. This is a
//!    known engine defect characterized by the spike and carried by
//!    LABS-646 (the corpus queries spell their aggregations around it);
//!    fixing it must flip this pin — the assertion below deliberately
//!    fails when the variable becomes defined.
//! 2. Per-branch `LIMIT` in a `UNION` composes additively: each branch
//!    caps its own rows, the union keeps distinct rows, so two branches
//!    capped at 100 can legitimately answer 115 — past the single cap
//!    the PathDB originals had. The PathDB translation of `(A? | B)`
//!    (one RPQ, one cap) becomes two LIMIT-capped branches, and its row
//!    counts reflect that. The engine is Cypher-correct here; the pin
//!    records the semantics the corpus's UNION spellings rely on.

use crate::common;

use common::{connection, err_text, rows};

#[test]
fn labs646_count_over_a_fixed_length_path_variable_is_undefined() {
    let mut c = connection();
    common::run(
        &mut c,
        "CREATE (a:A {i: 1})-[:R]->(b:A {i: 2}), (a)-[:R]->(:A {i: 3})",
    );
    // The defect: the fixed-length pattern's bound `p` is not defined
    // where the aggregate sees it. Flip this arm (and the header note)
    // when the fix lands.
    let err = err_text(c.query("MATCH p = (a:A {i: 1})-[:R]->(b) RETURN count(p) AS c"));
    assert!(
        err.contains("UndefinedVariable: variable 'p' not defined"),
        "the fixed-length pin changed: {err}"
    );
    // The same language, var-length spelling: the variable is defined
    // and the count is the same two paths.
    assert_eq!(
        rows(
            &mut c,
            "MATCH p = (a:A {i: 1})-[:R*1..1]->(b) RETURN count(p) AS c"
        ),
        vec![vec![relate::Value::Int(2)]]
    );
    // `count(*)` over the fixed-length pattern works and agrees.
    assert_eq!(
        rows(
            &mut c,
            "MATCH p = (a:A {i: 1})-[:R]->(b) RETURN count(*) AS c"
        ),
        vec![vec![relate::Value::Int(2)]]
    );
}

#[test]
fn labs646_union_branch_limits_compose_additively_over_distinct_rows() {
    let mut c = connection();
    common::run(
        &mut c,
        "UNWIND range(1, 60) AS i \
         CREATE (s:S {g: 1})-[:R {i: i}]->(t:T {g: 1, i: i}), \
                (u:S {g: 2})-[:R {i: i + 100}]->(v:T {g: 2, i: i})",
    );
    // Identical branches, each capped at 60: the union deduplicates, so
    // the answer is 60, not 120.
    assert_eq!(
        rows(
            &mut c,
            "MATCH p = (a:S {g: 1})-[:R]->(b:T {g: 1}) RETURN p LIMIT 60 \
             UNION MATCH p = (a:S {g: 1})-[:R]->(b:T {g: 1}) RETURN p LIMIT 60"
        )
        .len(),
        60
    );
    // Disjoint branches of 60 rows each: no row is shared, so the union
    // answers 120 — past either branch's cap. This is the q99 shape the
    // spike measured (115 rows for two LIMIT-100 branches on GBL SF1):
    // correct Cypher, but more than the single 100-row cap the original
    // PathDB RPQ had, which is why the corpus records UNION-spelled row
    // counts as their own baseline.
    assert_eq!(
        rows(
            &mut c,
            "MATCH p = (a:S {g: 1})-[:R]->(b:T {g: 1}) RETURN p LIMIT 60 \
             UNION MATCH p = (a:S {g: 2})-[:R]->(b:T {g: 2}) RETURN p LIMIT 60"
        )
        .len(),
        120
    );
    // Neither branch leaks past its own LIMIT: the 120 is the additive
    // composition, not a broken cap.
    for g in [1, 2] {
        assert_eq!(
            rows(
                &mut c,
                &format!("MATCH p = (a:S {{g: {g}}})-[:R]->(b:T {{g: {g}}}) RETURN p LIMIT 60")
            )
            .len(),
            60
        );
    }
}
