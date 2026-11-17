//! The chain family's differential suite: corridor walks, the
//! rel-uniqueness rule, multi-chunk widths, and the anonymous and
//! degenerate shapes — every query the chain kernel takes answers
//! exactly what the per-row path answers.

use std::collections::BTreeMap;

use relate_core::Value;

use super::super::test_support::{exec_autocommit, try_exec_cypher_params, Builder};
use super::{differential, differential_with, families, Engagement};

/// The chain family: the corridor walk with predicates at both ends, a
/// mid predicate, undirected hops, a named rel, and a parameter bound.
#[test]
fn chain_matches_the_row_path() {
    let g = families();
    let both_ends = differential(
        &g,
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) \
         WHERE a.k >= 1 AND g.name CONTAINS 'o' RETURN a.k AS ak, g.name AS gn LIMIT 99",
        Engagement::Engaged,
    );
    // a1 -> b1 -> {one, two}, a2 -> b2 -> two, a3 -> b1 -> {one, two}:
    // five trails.
    assert_eq!(both_ends.rows.len(), 5);
    assert_eq!(both_ends.columns, vec!["ak".to_string(), "gn".to_string()]);

    // A mid-node predicate, undirected hops, and a named rel.
    let undirected = differential(
        &g,
        "MATCH (a:Alpha)-[r:R]-(b:Beta)-[:S]-(g:Gamma) WHERE b.k > 15 \
         RETURN a.k AS ak, g.name AS gn LIMIT 99",
        Engagement::Engaged,
    );
    assert_eq!(
        undirected.rows,
        vec![vec![Value::Int(2), Value::String("two".into())]]
    );

    // A parameter bound evaluates once per clause, not per row.
    let params = BTreeMap::from([("min".to_string(), Value::Int(2))]);
    let cypher = "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) WHERE a.k >= $min \
                  RETURN a.k AS ak LIMIT 99";
    differential_with(cypher, Engagement::Engaged, || {
        try_exec_cypher_params(&g, cypher, &params)
    });
}

/// The chain's rel-uniqueness rule: two hops over the same type cannot
/// take the same rel — parallel rels widen the trail set, and an
/// undirected pair over one rel cannot serve both hops of one trail.
#[test]
fn chain_keeps_the_different_relationships_rule() {
    let g = Builder::new().done();
    exec_autocommit(
        &g,
        "CREATE (x1:X)-[:K]->(y1:Y), (x1)-[:K]->(y1), (y1)-[:K]->(z1:Z), (y1)-[:K]->(z2:Z)",
    )
    .unwrap();
    // 2 first-hop choices x 2 second-hop rels = 4 trails.
    let trails = differential(
        &g,
        "MATCH (x:X)-[:K]->(y:Y)-[:K]->(z:Z) RETURN x.k AS xk, z.k AS zk LIMIT 99",
        Engagement::Engaged,
    );
    assert_eq!(trails.rows.len(), 4);
    // Two rels between one pair: both orderings of the undirected pair
    // are trails (each hop takes its own rel).
    exec_autocommit(&g, "CREATE (p1:P)-[:Q]->(m1:M), (m1)-[:Q]->(p1)").unwrap();
    let both_ways = differential(
        &g,
        "MATCH (p:P)-[:Q]-(m:M)-[:Q]-(w:P) RETURN p.k AS pk LIMIT 99",
        Engagement::Engaged,
    );
    assert_eq!(both_ways.rows.len(), 2);
    // One rel between the pair: no trail can take it twice — zero rows.
    exec_autocommit(&g, "CREATE (s1:S)-[:Q]->(n1:N)").unwrap();
    let one_rel = differential(
        &g,
        "MATCH (s:S)-[:Q]-(n)-[:Q]-(w) RETURN s.k AS sk LIMIT 99",
        Engagement::Engaged,
    );
    assert_eq!(one_rel.rows, Vec::<Vec<Value>>::new());
}

/// A population past `CHUNK_ROWS` (512) through the **chain kernel**: a
/// 3-hop corridor whose stages batch past one chunk boundary, engagement
/// counted — the batch chain's own chunk case (`batch_tests`) owns the
/// scan shape; this one pins the contracted kernel's width handling.
#[test]
fn the_chain_kernel_batches_past_one_chunk() {
    let g = Builder::new().done();
    let mut create = String::from("CREATE ");
    for i in 0..600 {
        if i > 0 {
            create.push_str(", ");
        }
        create.push_str(&format!("(:Bulk {{n: {i}}})"));
    }
    exec_autocommit(&g, &create).unwrap();
    let mut next = String::from("CREATE ");
    for i in 0..100 {
        if i > 0 {
            next.push_str(", ");
        }
        next.push_str(&format!(
            "(:Tip {{t: {i}}})-[:PREV]->(:Bulk {{n: {i}}})-[:TAIL]->(:End)"
        ));
    }
    exec_autocommit(&g, &next).unwrap();
    let trails = differential(
        &g,
        "MATCH (t:Tip)-[:PREV]->(b:Bulk)-[:TAIL]->(e:End) RETURN b.n AS bn LIMIT 999",
        Engagement::Engaged,
    );
    assert_eq!(trails.rows.len(), 100);
}

/// The stellar pre-push suspects (blockers 9–11), the chain's share: the
/// anonymous-binding shapes. Every verdict is a differential — the kernel
/// path's rows exactly the per-row path's, engagement counted. The
/// hub-star's anonymous-terminal shapes live in `hub_star.rs` and the
/// matcher's declines in `gate.rs`.
///
/// - **B11, rows (real bug, fixed):** a pattern that names no element
///   binds nothing, so the clause's output is one empty row per complete
///   binding — the row count comes from the walk, never a column's length
///   (a chunk with zero columns carries zero rows by construction, the
///   hole the suspect named; the labeled fully-anonymous chain reached it
///   and answered zero rows). Partially anonymous shapes project the
///   named remainder. A fully anonymous chain with no labels at all keeps
///   the walker: its start is unmeasurable, so the cost gate's rank
///   sentinel declines it.
#[test]
fn anonymous_chain_shapes_match_the_row_path() {
    let g = families();
    // The fully anonymous chain with labels: the corridor's five trails,
    // each an empty binding — the shape that answered zero rows before
    // the breaker's count moved off the columns.
    let anonymous = differential(
        &g,
        "MATCH (:Alpha)-[:R]->(:Beta)-[:S]->(:Gamma) RETURN 1 AS one LIMIT 99",
        Engagement::Engaged,
    );
    assert_eq!(anonymous.rows.len(), 5);
    assert_eq!(anonymous.rows[0], vec![Value::Int(1)]);
    // The same shape with no labels: unmeasurable start, the cost gate's
    // rank sentinel declines it — the walker's rows identical.
    differential(
        &g,
        "MATCH ()-[:R]->()-[:S]->() RETURN 1 AS one LIMIT 99",
        Engagement::Declined,
    );
    // Partially anonymous: an anonymous mid, anonymous far ends — the
    // named remainder is the projection. The far-ends shape keeps the
    // walker: the planner leaves its unmeasurable anonymous end at the
    // scan end, so the cost gate declines it — rows identical either way.
    differential(
        &g,
        "MATCH (a:Alpha)-[:R]->()-[:S]->() RETURN a.k AS ak LIMIT 99",
        Engagement::Engaged,
    );
    differential(
        &g,
        "MATCH ()-[:R]->(b:Beta)-[:S]->() RETURN b.k AS bk LIMIT 99",
        Engagement::Declined,
    );
    differential(
        &g,
        "MATCH (a:Alpha)-[:R]->(b)-[:S]->(g:Gamma) RETURN a.k AS ak LIMIT 99",
        Engagement::Engaged,
    );
    // Existence over the fully anonymous chain: every complete binding
    // counts, none of them projects.
    let exists = differential(
        &g,
        "RETURN EXISTS { MATCH (:Alpha)-[:R]->(:Beta)-[:S]->(:Gamma) } AS hit",
        Engagement::Engaged,
    );
    assert_eq!(exists.rows, vec![vec![Value::Bool(true)]]);
}
