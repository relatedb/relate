//! The hub-star family's differential suite: the open and
//! shared-terminal stars, mixed orientations, the cross-side
//! uniqueness rule, and the exists-mode counted read over every
//! pairing of a B row.

use relate_core::Value;

use super::super::test_support::{exec_autocommit, run_metered, star, Builder};
use super::{differential, families, ExpectedEngagement};

/// The hub-star family: the open star (distinct terminals), the shared
/// terminal (a bounded cycle), the 2-hop side, and mixed orientation.
#[test]
fn hub_star_matches_the_row_path() {
    let g = families();
    // The open star: h1 has A-spokes to x and y and a B-spoke to x; the
    // t predicate pins the A side to x, so the pair is (h1, x).
    let open = differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:B]->(u:Tag) WHERE t.name = 'x' \
         RETURN h.k AS hk, u.name AS un LIMIT 99",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(
        open.rows,
        vec![vec![Value::Int(5), Value::String("x".into())]]
    );

    // The shared terminal (q30's shape): both sides must land on the same
    // tag — h1 through B to x, h2 through both to y.
    let shared = differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:B]->(t) RETURN h.k AS hk, t.name AS tn LIMIT 99",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(
        shared.rows,
        vec![
            vec![Value::Int(5), Value::String("x".into())],
            vec![Value::Int(6), Value::String("y".into())],
        ]
    );

    // A 2-hop side: the C -> mid -> D corridor against the 1-hop A side,
    // sharing the terminal — h1's corridor lands on y, h2's on x (which
    // its A side does not reach).
    let two_hop = differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:C]->(m:Mid)-[:D]->(t) \
         RETURN h.k AS hk, t.name AS tn LIMIT 99",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(
        two_hop.rows,
        vec![vec![Value::Int(5), Value::String("y".into())]]
    );

    // Mixed orientation: the hub at the last end of one pattern.
    let mixed = differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (u:Tag)<-[:B]-(h) WHERE t.name = 'y' \
         RETURN h.k AS hk, u.name AS un LIMIT 99",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(
        mixed.rows,
        vec![
            vec![Value::Int(5), Value::String("x".into())],
            vec![Value::Int(6), Value::String("y".into())],
        ]
    );
}

/// The hub-star's cross-side uniqueness: the same rel cannot serve both
/// patterns — h1's two A rels make two ordered pairs; h2's single A rel
/// cannot pair with itself.
#[test]
fn hub_star_keeps_the_cross_side_uniqueness_rule() {
    let g = families();
    let pairs = differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:A]->(u:Tag) \
         RETURN h.k AS hk, t.name AS tn, u.name AS un LIMIT 99",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(pairs.rows.len(), 2);
}

/// The exists-mode counted coverage (ADR-0473 repair, reviewer finding 1,
/// pinned regression): the counted expression must be evaluated across
/// ALL pairings of each side-B row. A mid node of a 2-hop side 0 varies
/// across the pairings — only the terminal is shared, and only in the
/// shared-terminal mode — so a first-pairing read can see a null `m.k`
/// where a later pairing of the same B row carries it.
#[test]
fn exists_counted_reads_every_pairing_of_a_b_row() {
    let g = Builder::new().done();
    // One hub: side 0 is the 2-hop C -> Mid -> D corridor with one mid
    // carrying `k` (m2) and one without (m1); side 1 is 50 A spokes, so
    // every B row pairs with both corridor rows.
    let mut create = String::from(
        "CREATE (h:Hub)-[:C]->(m2:Mid {k: 5})-[:D]->(t2:Tag), \
         (h)-[:C]->(m1:Mid)-[:D]->(t1:Tag)",
    );
    for i in 0..50 {
        create.push_str(&format!(", (h)-[:A]->(u{i}:Unk)"));
    }
    exec_autocommit(&g, &create).unwrap();
    let has = differential(
        &g,
        "MATCH (h:Hub)-[:C]->(m:Mid)-[:D]->(t:Tag), (h)-[:A]->(u:Unk) \
         RETURN count(DISTINCT m.k) > 0 AS has",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(has.rows, vec![vec![Value::Bool(true)]]);
}

/// The hub-star's anonymous-terminal shapes (stellar blocker 11, pair): the
/// hub's own variable keeps the projection non-empty by construction, and
/// the zero-binding rows mode emits nothing off the binding counter rather
/// than a column's emptiness.
#[test]
fn hub_star_anonymous_terminals_match_the_row_path() {
    let g = families();
    differential(
        &g,
        "MATCH (h:Hub)-[:A]->(), (h)-[:B]->(t:Tag) RETURN h.k AS hk, t.name AS tn LIMIT 99",
        ExpectedEngagement::Engaged,
    );
    let none = differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:B]->(u:Tag) WHERE t.name = 'zz' \
         RETURN h.k AS hk LIMIT 99",
        ExpectedEngagement::Engaged,
    );
    assert_eq!(none.rows, Vec::<Vec<Value>>::new());
}

/// The kernel keeps no per-path state beyond the hub's own buffers: a
/// shared-terminal star over `hubs` hubs, each reaching the same terminals
/// by both rel types, runs at N and 4N hubs (4x the paths) and the peak
/// workspace charge at 4N stays within 1.25x of the peak at N.
#[test]
fn hub_star_footprint_is_independent_of_path_count() {
    const TERMINALS: usize = 40;
    let cypher = "MATCH (h:H)-[:A]->(t:T), (h)-[:B]->(t) RETURN count(*) AS c";
    let (small_rows, at_n) = run_metered(&star(100, TERMINALS), cypher);
    let (large_rows, at_4n) = run_metered(&star(400, TERMINALS), cypher);
    assert_eq!(small_rows, vec![vec![Value::Int(100 * TERMINALS as i64)]]);
    assert_eq!(large_rows, vec![vec![Value::Int(400 * TERMINALS as i64)]]);
    eprintln!("hub-star peak at N = {at_n}, at 4N = {at_4n}");
    assert!(
        at_4n as f64 <= at_n as f64 * 1.25,
        "peak {at_4n} at 4N vs {at_n} at N"
    );
}

/// Wide shared-terminal triangle (>32 side-A rows, exercising the sorted
/// terminal lookup path) must agree identically with the general walker,
/// including duplicate terminals reached through different mid nodes and
/// cross-side relationship uniqueness.
#[test]
fn hub_star_wide_shared_terminal_matches_walker() {
    let g = Builder::new().done();
    // Hub `a` has 12 mid nodes; each mid node connects to 5 terminals
    // (60 side-A rows > SHARED_TERMINAL_SORT_THRESHOLD=32), with multiple
    // mid nodes hitting the same terminal, and `a` also connects directly
    // to a subset of those terminals.
    let mut create = String::from("CREATE (a:User {id: 1})");
    for t in 0..8 {
        create.push_str(&format!(", (t{t}:User {{id: {}}})", 100 + t));
    }
    for m in 0..12 {
        create.push_str(&format!(", (m{m}:User {{id: {}}})", 10 + m));
        create.push_str(&format!(", (a)-[:KNOWS]->(m{m})"));
        for k in 0..5 {
            let t = (m + k) % 8;
            create.push_str(&format!(", (m{m})-[:KNOWS]->(t{t})"));
        }
    }
    for t in 0..6 {
        create.push_str(&format!(", (a)-[:KNOWS]->(t{t})"));
    }
    exec_autocommit(&g, &create).unwrap();
    let res = differential(
        &g,
        "MATCH (a:User {id: 1})-[:KNOWS]->(b:User)-[:KNOWS]->(c:User), (a)-[:KNOWS]->(c) \
         RETURN count(*) AS triangles",
        ExpectedEngagement::Engaged,
    );
    assert!(!res.rows.is_empty());
}
