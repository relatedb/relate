//! The classic walker's binding-schema ladder: the anonymous var-length
//! rel on a named path binds its `__trail{idx}` seat at EVERY stop,
//! the zero-hop stop included — an empty list, exactly the value the
//! rel-variable twin binds there — which is what makes the ladder total
//! (no shape keeps an insert path; the former `ladder_precomputable`
//! gate is deleted). The A/B differentials pin the totality: the
//! anonymous-seat spelling answers exactly as the variable-bound twin,
//! whose rel name binds at every stop including the zero-hop one. Plus
//! the arm builds' A/B: the cheapest-start arm's rungs (start bind,
//! backward prefix, completed row) match the forward arm's rows. Plus
//! the eval walker's terminal path-name bind: the same seat policy A/B
//! through a pattern comprehension, and the stamped path value pinned
//! against the classic walker's own MATCH row.

use std::collections::BTreeMap;

use relate_core::Value;

use super::test_support::exec_cypher;
use crate::graph::Graph;

/// Two `X` nodes and one rel between them: `[*0..1]` matches the
/// zero-hop (a, a) and (b, b) pairs plus the one-hop (a, b).
#[test]
fn zero_min_var_length_named_paths_match_through_both_binding_paths() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["X".into()],
            BTreeMap::from([("n".into(), Value::Int(0))]),
        )
        .unwrap();
    let b = g
        .create_node(
            vec!["X".into()],
            BTreeMap::from([("n".into(), Value::Int(1))]),
        )
        .unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let order = "ORDER BY ia, ib, hops";
    // The anonymous-seat spelling: the `[*0..1]` rel on a named path
    // binds its `__trail0` seat at every stop — the zero-hop stop binds
    // the empty list, exactly the value the twin's rel variable binds
    // there (the ladder's totality).
    let anon = exec_cypher(
        &g,
        &format!(
            "MATCH p = (a:X)-[*0..1]->(b:X) \
             RETURN id(a) AS ia, id(b) AS ib, size(relationships(p)) AS hops {order}"
        ),
    );
    // The rel-variable twin: the name binds at every stop, the zero-hop
    // one included (an empty list) — the seat policy's named arm.
    let named = exec_cypher(
        &g,
        &format!(
            "MATCH p = (a:X)-[rs*0..1]->(b:X) \
             RETURN id(a) AS ia, id(b) AS ib, size(relationships(p)) AS hops {order}"
        ),
    );
    // (a,a,0), (a,b,1), (b,b,0)
    assert_eq!(anon.rows.len(), 3);
    assert_eq!(anon.rows, named.rows);
    let hops: Vec<i64> = anon
        .rows
        .iter()
        .map(|row| row[2].as_int().expect("hops is an int"))
        .collect();
    assert_eq!(hops, vec![0, 1, 0]);
    // The zero-hop path carries its single endpoint and no rels — the
    // fill-end-node semantics bind_named_path keeps on both paths.
    let zero = exec_cypher(&g, "MATCH p = (a:X {n: 0})-[*0..1]->(b:X {n: 0}) RETURN p");
    assert_eq!(zero.rows.len(), 1);
    let Value::Path(p) = &zero.rows[0][0] else {
        panic!("p must be a path");
    };
    assert_eq!(p.nodes.len(), 1);
    assert!(p.rels.is_empty());
}

/// The cheapest-start arm's ladder matches the forward arm's rows on a
/// named path: `with_pivot_disabled` forces the index-0 start, so the
/// row equality is an A/B over the two arm builds — the cheapest start
/// bind's rung, the backward prefix's, and the completed row's
/// path-name bind. The second comma pattern keeps the clause off the
/// batch chain (the walker's own gate, per Q19's shape).
#[test]
fn cheapest_start_ladder_matches_the_index_zero_expansion_on_named_paths() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let others: Vec<_> = (0..2)
        .map(|i| {
            g.create_node(
                vec!["Other".into()],
                BTreeMap::from([("n".into(), Value::Int(i))]),
            )
            .unwrap()
        })
        .collect();
    let rares: Vec<_> = ["ax", "ay"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            g.create_node(
                vec!["Rare".into()],
                BTreeMap::from([
                    ("n".into(), Value::Int(i as i64)),
                    ("name".into(), Value::String((*name).into())),
                ]),
            )
            .unwrap()
        })
        .collect();
    let commons: Vec<_> = (0..8)
        .map(|i| {
            g.create_node(
                vec!["Common".into()],
                BTreeMap::from([("n".into(), Value::Int(i))]),
            )
            .unwrap()
        })
        .collect();
    // Only two of the eight Common nodes reach a Rare node; the other
    // six carry same-type rels among themselves, so the rel type's
    // total does not clamp element 0's scan below its own rows. The
    // WHERE's CONTAINS cuts element 1's seed (1 of the 2 Rare rows)
    // below its label count, so the mean arm prices its walk under
    // element 0's scan — the ADR-0351 displacement this test is about.
    for (c, r) in commons.iter().zip(rares.iter()) {
        g.create_rel(*c, *r, "R".into(), BTreeMap::new()).unwrap();
    }
    for c in (2..commons.len()).step_by(2) {
        g.create_rel(commons[c], commons[c + 1], "R".into(), BTreeMap::new())
            .unwrap();
    }
    for (r, o) in rares.iter().zip(others.iter()) {
        g.create_rel(*r, *o, "S".into(), BTreeMap::new()).unwrap();
    }
    g.commit_write();

    let query = "MATCH p = (c:Common)-[:R]->(r:Rare), (r)-[:S]->(o:Other) \
                 WHERE r.name CONTAINS 'x' \
                 RETURN c.n AS cn, r.n AS rn, o.n AS onn ORDER BY cn, rn, onn";
    let _ = super::pivot::take_pivot_backward_hops();
    let selected = exec_cypher(&g, query);
    // The cheapest start displaced to element 1 and the backward walker
    // ran (the arm this differential is about).
    assert!(super::pivot::take_pivot_backward_hops() > 0);
    let plain = super::pivot::with_pivot_disabled(|| exec_cypher(&g, query));
    assert_eq!(selected.rows, plain.rows);
    assert_eq!(selected.rows.len(), 1);
    // The named path still assembles in the pattern's original order.
    let paths = exec_cypher(
        &g,
        "MATCH p = (c:Common)-[:R]->(r:Rare), (r)-[:S]->(o:Other) \
         WHERE r.name CONTAINS 'x' RETURN p ORDER BY c.n",
    );
    assert_eq!(paths.rows.len(), 1);
    let Value::Path(p) = &paths.rows[0][0] else {
        panic!("p must be a path");
    };
    assert_eq!(p.nodes.len(), 2);
    assert_eq!(p.rels.len(), 1);
    assert_eq!(p.nodes[0].labels.as_ref(), &vec!["Common".to_string()][..]);
}

/// The eval walker's terminal path-name bind stamps its ladder's
/// complete rung (the eval walk's `WalkLadder` — the k=0 forward arm of
/// the one ladder build, mirroring the classic walker's):
/// through a pattern comprehension — the eval walker's own consumer —
/// the seat policy's A/B (the anonymous `[*0..1]` on a named path binds
/// its `__trail0` seat as the empty list at the zero-hop stop, exactly
/// the value the rel-variable twin binds there) answers the
/// same, and the stamped path value is the classic walker's own MATCH
/// row for the same pattern.
#[test]
fn eval_terminal_binds_path_names_through_both_binding_paths() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["X".into()],
            BTreeMap::from([("n".into(), Value::Int(0))]),
        )
        .unwrap();
    let b = g
        .create_node(
            vec!["X".into()],
            BTreeMap::from([("n".into(), Value::Int(1))]),
        )
        .unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    // The gated shape: the anonymous `[*0..1]` rel on a named path —
    // its zero-hop stop binds `__trail0` as the empty list, exactly the
    // value the rel-variable twin binds there (the seat policy's one
    // spelling, the totality the eval walk's ladder is built on). The
    // zero-hop row survives both: the end pattern accepts the start
    // node, so (a, a) then (a, b) bind b.n = 0 then 1.
    let declined = exec_cypher(
        &g,
        "MATCH (a:X {n: 0}) RETURN [p = (a)-[:R*0..1]->(b:X) | b.n] AS bs",
    );
    let served = exec_cypher(
        &g,
        "MATCH (a:X {n: 0}) RETURN [p = (a)-[rs:R*0..1]->(b:X) | b.n] AS bs",
    );
    assert_eq!(declined.rows, served.rows);
    assert_eq!(
        declined.rows[0][0],
        Value::List(vec![Value::Int(0), Value::Int(1)].into())
    );

    // The stamped terminal row is the classic walker's row: the same
    // named pattern through MATCH (WalkLadder's complete rung) binds
    // the same path value the comprehension's stamped row carries, the
    // comprehension's projection reading the stamped name and the
    // walk's own binds from that one row.
    let classic = exec_cypher(&g, "MATCH p = (a:X {n: 0})-[:R]->(b:X) RETURN p");
    let stamped = exec_cypher(
        &g,
        "MATCH (a:X {n: 0}) \
         RETURN [p = (a)-[:R]->(b:X) | [b.n, size(nodes(p))]] AS xs",
    );
    assert_eq!(stamped.rows.len(), 1);
    assert_eq!(
        stamped.rows[0][0],
        Value::List(vec![Value::List(vec![Value::Int(1), Value::Int(2)].into())].into()),
        "the stamped row answers both its names"
    );
    let comp = exec_cypher(
        &g,
        "MATCH (a:X {n: 0}) RETURN [p = (a)-[:R]->(b:X) | p] AS ps",
    );
    assert_eq!(
        comp.rows[0][0],
        Value::List(vec![classic.rows[0][0].clone()].into())
    );

    // The expression shortest path answers the same row. Both endpoints are
    // bound variables: live rejects an unbound endpoint in the expression
    // form ("requires bound nodes when not part of a MATCH clause").
    let enumerated = exec_cypher(
        &g,
        "MATCH (a:X {n: 0}), (b:X {n: 1}) \
         RETURN shortestPath((a)-[:R*1..2]->(b)) AS sp",
    );
    assert_eq!(enumerated.rows, classic.rows);
}
