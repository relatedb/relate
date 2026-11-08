//! The rows mode's want halt (LABS-990, ADR-0524): the plain cap
//! rides the contracted kernels — halt-at-want, count-exactness
//! through SKIP, late-hop non-starvation, the hub-boundary halt, and
//! the estimate-blind fan-out's bounded walk tally.

use relate_core::Value;

use super::super::kernels::take_kernel_engagements;
use super::super::take_neighbor_walks;
use super::super::test_support::{column, exec_autocommit, int_cell, try_exec_cypher, Builder};
use super::families;

/// The plain cap rides the chain kernel as its rows-mode halt (LABS-990):
/// a `LIMIT` below the total answers exactly `want` rows — the kernel's
/// own prefix, contract-free under LIMIT without ORDER BY (ADR-0478 §4's
/// rule, the frontier's capped emission already follows) — and each is a
/// complete binding of the uncapped answer. A `SKIP` rides the same
/// want: the kernel lands `SKIP+LIMIT` bindings, the projection skips.
#[test]
fn first_n_halts_the_chain_at_the_want() {
    let g = families();
    // The uncapped reference: no LIMIT arms no cap, so the sink
    // projection owns the clause and the walker serves it — an answer
    // independent of the kernels. Five corridor trails.
    let full = try_exec_cypher(
        &g,
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) RETURN a.k AS ak",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 0);
    assert_eq!(full.rows.len(), 5);

    let capped = try_exec_cypher(
        &g,
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) RETURN a.k AS ak LIMIT 2",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 1);
    assert_eq!(capped.rows.len(), 2, "the halt lands exactly `want` rows");
    assert!(
        capped.rows.iter().all(|row| full.rows.contains(row)),
        "capped rows must be complete bindings of the uncapped answer"
    );

    // SKIP 2 LIMIT 2 arms the same plain cap with want = 4: the kernel
    // lands the first four bindings, the projection skips two, keeps two.
    let skipped = try_exec_cypher(
        &g,
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) RETURN a.k AS ak SKIP 2 LIMIT 2",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 1);
    assert_eq!(skipped.rows.len(), 2);
    assert!(skipped.rows.iter().all(|row| full.rows.contains(row)));
}

/// The halt never starves a late hop: only the chain's **final** hop is
/// capped, so the first `want` partial bindings dying downstream cannot
/// shrink the answer below `min(want, total)` — the one-row answer here
/// lives behind two dead partials an intermediate-stage cap would have
/// kept.
#[test]
fn first_n_never_starves_a_late_hop() {
    let g = Builder::new().done();
    exec_autocommit(
        &g,
        "CREATE (a1:Alpha {k: 1}), (a2:Alpha {k: 2}),
                (d1:Beta {k: 50}), (d2:Beta {k: 60}), (b3:Beta {k: 70}),
                (g1:Gamma {name: 'one'}),
                (a1)-[:R]->(d1), (a1)-[:R]->(d2), (a2)-[:R]->(b3),
                (b3)-[:S]->(g1)",
    )
    .unwrap();
    // One complete binding (a2's corridor); a1's two partials die at S.
    let capped = try_exec_cypher(
        &g,
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) RETURN a.k AS ak LIMIT 1",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 1);
    assert_eq!(capped.rows, vec![vec![Value::Int(2)]]);
}

/// The hub-star's want halt: the pairing loop stops at `want` complete
/// bindings — exactly `want` rows, each a complete binding of the
/// uncapped pairing (the A/A star's two ordered pairs, the walker-served
/// no-LIMIT reference, capped at one).
#[test]
fn first_n_halts_the_hub_star_at_the_want() {
    let g = families();
    // The uncapped, walker-served reference: no LIMIT arms no cap, so
    // the sink projection owns the clause and no kernel engages.
    let full = try_exec_cypher(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:A]->(u:Tag) \
         RETURN h.k AS hk, t.name AS tn, u.name AS un",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 0);
    assert_eq!(full.rows.len(), 2);

    let capped = try_exec_cypher(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:A]->(u:Tag) \
         RETURN h.k AS hk, t.name AS tn, u.name AS un LIMIT 1",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 1);
    assert_eq!(capped.rows.len(), 1);
    assert!(capped.rows.iter().all(|row| full.rows.contains(row)));

    // The halt across the hub boundary: two hubs with two A-spokes each
    // give four ordered pairs (two per hub). `LIMIT 2` fills exactly at
    // the first hub's boundary — the per-hub guard ends the loop before
    // the trailing hub expands — and `LIMIT 3` crosses it, one pair short
    // of the full four: both count-exact.
    let g = Builder::new().done();
    exec_autocommit(
        &g,
        "CREATE (h1:Hub {k: 1}), (h2:Hub {k: 2}),
                (t1:Tag {name: 'a'}), (t2:Tag {name: 'b'}),
                (t3:Tag {name: 'c'}), (t4:Tag {name: 'd'}),
                (h1)-[:A]->(t1), (h1)-[:A]->(t2),
                (h2)-[:A]->(t3), (h2)-[:A]->(t4)",
    )
    .unwrap();
    let at_boundary = try_exec_cypher(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:A]->(u:Tag) \
         RETURN h.k AS hk, t.name AS tn, u.name AS un LIMIT 2",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 1);
    assert_eq!(at_boundary.rows.len(), 2);
    assert!(
        at_boundary.rows.iter().all(|row| row[0] == Value::Int(1)),
        "the halt filled at the first hub: the trailing hub contributes nothing"
    );
    let past_boundary = try_exec_cypher(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:A]->(u:Tag) \
         RETURN h.k AS hk, t.name AS tn, u.name AS un LIMIT 3",
    )
    .unwrap();
    assert_eq!(take_kernel_engagements(), 1);
    assert_eq!(past_boundary.rows.len(), 3);
    assert!(past_boundary.rows.iter().any(|row| row[0] == Value::Int(2)));
}

/// LABS-990's own shape, bounded: a fan-out chain through unlabeled
/// interiors whose uncapped enumeration would walk every one of the
/// `F^hops` paths — pinned by the neighbor-walk tally, which the halt
/// keeps an order below the path count. The keyed seed (a RANGE index)
/// prices the start at one row and the unlabeled hops ride the any-label
/// tail (LABS-991), so the gate admits the shape at 16^4 stage rows; a
/// wider fan-out declines to the walker instead (`chain_gate_tests`). The
/// walk tallies land only while store profiling is armed (the test-build
/// default).
#[test]
fn first_n_bounds_the_chain_fan_out() {
    let g = Builder::new().done();
    // Five layers of 16 nodes, fan-out 16: 16^4 = 65k complete bindings
    // from the keyed seed; 80 nodes, 1.0k rels.
    exec_autocommit(
        &g,
        "UNWIND range(0, 4) AS l UNWIND range(0, 15) AS w \
           CREATE (:L {id: l * 100 + w, layer: l})",
    )
    .unwrap();
    exec_autocommit(&g, "CREATE INDEX l_id FOR (n:L) ON (n.id)").unwrap();
    exec_autocommit(
        &g,
        "MATCH (a:L) WHERE a.layer < 4 UNWIND range(0, 15) AS k \
           MATCH (b:L {id: (a.layer + 1) * 100 + ((a.id % 100) * 7 + k * 13) % 16}) \
           CREATE (a)-[:R]->(b)",
    )
    .unwrap();
    let capped = try_exec_cypher(
        &g,
        "MATCH (s:L {id: 0})-[:R]->()-[:R]->()-[:R]->()-[:R]->(n:L) \
         RETURN n.id AS nid LIMIT 1",
    )
    .unwrap();
    assert_eq!(
        take_kernel_engagements(),
        1,
        "the chain kernel takes the shape"
    );
    assert_eq!(capped.rows.len(), 1);
    // A last-layer id: 400..416, read through the canonical column reader.
    let nid = column(&capped, "nid", int_cell)[0];
    assert!(
        (400..416).contains(&nid),
        "the binding must land in the last layer"
    );
    // The halt's proof of work: the walked rels stay at the intermediate
    // stages' widths (16 + 256 + 4k, the final hop halted at one), not
    // the path product — an order below the 70k an unhalted walk tallies.
    let walks = take_neighbor_walks();
    assert!(
        walks < 10_000,
        "the halt must bound the walk: {walks} walked rels is the dropped-cap path count class, not the halted one"
    );
}
