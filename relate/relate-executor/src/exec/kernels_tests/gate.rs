//! The engagement gate's suites: the structural declines, the cost
//! gate's dear-plan refusal, and the scratch-label admissions.

use relate_core::Value;

use super::super::test_support::{exec_autocommit, exec_write, write_scratch, Builder};
use super::{differential, differential_with, families, Engagement};

/// The engagement gate's declines: every shape the kernels cannot prove,
/// or that routes elsewhere before the `MATCH` arm, keeps the per-row
/// path — answering identically.
#[test]
fn the_gate_declines_every_unsupported_shape() {
    let g = families();
    for cypher in [
        // OPTIONAL keeps the per-row null-binding arm.
        "OPTIONAL MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) RETURN a.k AS ak LIMIT 99",
        // A var-length hop keeps the recursive walker.
        "MATCH (a:Alpha)-[:R*2]->(b:Beta)-[:S]->(g:Gamma) RETURN a.k AS ak LIMIT 99",
        // A two-entry property map is two predicates on one variable.
        "MATCH (a:Alpha {k: 1, s: 'ash'})-[:R]->(b:Beta)-[:S]->(g:Gamma) \
         RETURN a.k AS ak LIMIT 99",
        // A predicate outside the closed vocabulary.
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) WHERE size(g.name) > 1 \
         RETURN a.k AS ak LIMIT 99",
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) WHERE a.k > 1 AND a.s IS NULL \
         RETURN a.k AS ak LIMIT 99",
        // A predicate naming an outer variable (the driving row binds).
        "UNWIND [1, 2] AS min MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) \
         WHERE a.k >= min RETURN a.k AS ak LIMIT 99",
        // Three patterns is not the hub-star's pair.
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:B]->(u:Tag), (h)-[:C]->(m:Mid) \
         RETURN h.k AS hk LIMIT 99",
        // A repeated rel variable is the walker's reconciliation.
        "MATCH (h:Hub)-[r:A]->(t:Tag), (h)-[r:B]->(u:Tag) RETURN h.k AS hk LIMIT 99",
        // A sink projection runs in the match sink.
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) RETURN a.k AS ak",
        // A DISTINCT follower takes the cap.
        "MATCH (a:Alpha)-[:R]->(b:Beta)-[:S]->(g:Gamma) RETURN DISTINCT a.k AS ak LIMIT 2",
        // A one-hop pattern is below the chain's floor and not a pair.
        "MATCH (a:Alpha)-[:R]->(b:Beta) RETURN a.k AS ak LIMIT 99",
        // The counted expression reading a rel variable keeps the probe.
        "MATCH (h:Hub)-[r:A]->(t:Tag), (h)-[:B]->(u:Tag) \
         RETURN count(DISTINCT id(r)) > 0 AS has_pair",
        // An `EXISTS { MATCH }` over a carried row keeps the probe (the
        // correlated shape the row gate exists for).
        "MATCH (h:Hub) WHERE EXISTS { MATCH (h)-[:A]->(t:Tag), (h)-[:B]->(u:Tag) } \
         RETURN count(h.k) AS n",
    ] {
        differential(&g, cypher, Engagement::Declined);
    }
}

/// The cost gate's hub-star refusal: a hub population whose driven work
/// estimates worse than the terminal-driven alternative keeps the walker
/// (the selective-terminal plan the walker already runs) — both terminals
/// pinned by an indexed equality, the hub fanning out.
#[test]
fn the_cost_gate_refuses_a_dear_hub_plan() {
    let g = Builder::new().done();
    exec_autocommit(&g, "CREATE INDEX FOR (t:Tag) ON (t.name)").unwrap();
    let mut create = String::from("CREATE (h0:Hub {k: 0}), (needle:Tag {name: 'needle'})");
    for i in 1..20 {
        create.push_str(&format!(", (h{i}:Hub {{k: {i}}})"));
    }
    for i in 1..1000 {
        create.push_str(&format!(", (t{i}:Tag {{name: 'hay{i}'}})"));
    }
    exec_autocommit(&g, &create).unwrap();
    // Every hub fans out over A into the whole tag population and over B
    // into the needle; the terminals' indexed equality pins both sides.
    let mut spokes = Vec::new();
    for h in 0..20 {
        spokes.extend((1..50).map(|t| format!("(h{h})-[:A]->(t{t})")));
        spokes.push(format!("(h{h})-[:B]->(needle)"));
    }
    exec_autocommit(&g, &format!("CREATE {}", spokes.join(", "))).unwrap();
    differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h)-[:B]->(u:Tag) \
         WHERE t.name = 'hay1' AND u.name = 'needle' RETURN h.k AS hk LIMIT 99",
        Engagement::Declined,
    );
}

/// Scratch-label admission (ADR-0473 repair, reviewer finding 2, pinned
/// regression): pattern labels the base interner has never recorded still
/// admit scratch nodes inside an open transaction — write-path labels
/// stay strings in the overlay, interned only at commit, so the interned
/// admission falls back to the overlay's string arm instead of rejecting
/// the node (the empty answer the per-row path never gives).
#[test]
fn scratch_labels_admit_through_the_kernel() {
    let g = Builder::new().done();
    let create = "CREATE (:Zed {name: 'z'})-[:R]->(:Yed {name: 'y'})-[:S]->(:Xed {name: 'x'})";
    let read = "MATCH (a:Zed)-[:R]->(b:Yed)-[:S]->(c:Xed) RETURN a.name AS an LIMIT 9";
    // Statement 2 of the same open transaction: the MATCH is its
    // statement's first clause, so the kernel path engages over the
    // scratch-mounted view. Each side runs the two-statement transaction on
    // its own scratch (nothing is committed between them).
    let kernaled = differential_with(read, Engagement::Engaged, || {
        let scratch = write_scratch(&g);
        exec_write(&g, &scratch, create)?;
        exec_write(&g, &scratch, read)
    });
    assert_eq!(kernaled.rows, vec![vec![Value::String("z".into())]]);
}

/// The matcher's declines (stellar blockers 10 and 11, pair): a one-node
/// pattern is not a side — declined before engagement (the cost gate's
/// terminal estimate once read an empty side, a reachable expect-panic) —
/// and a fully anonymous pair shares no hub variable, which the hub-star is
/// defined by. The walker's rows are identical.
#[test]
fn the_matcher_declines_one_node_sides_and_anonymous_pairs() {
    let g = families();
    // Both orderings, and the both-sides-one-node shape.
    differential(
        &g,
        "MATCH (h:Hub), (h)-[:A]->(t:Tag) RETURN h.k AS hk, t.name AS tn LIMIT 99",
        Engagement::Declined,
    );
    differential(
        &g,
        "MATCH (h:Hub)-[:A]->(t:Tag), (h) RETURN h.k AS hk, t.name AS tn LIMIT 99",
        Engagement::Declined,
    );
    differential(
        &g,
        "MATCH (h:Hub {k: 5}), (h) RETURN h.k AS hk LIMIT 99",
        Engagement::Declined,
    );
    differential(
        &g,
        "MATCH ()-[:A]->(), ()-[:B]->() RETURN 1 AS one LIMIT 99",
        Engagement::Declined,
    );
}
