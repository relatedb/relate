//! End-to-end differential for the diagnostic pattern-engine switch
//! (ADR-0519): the two forced legs — `RELATE_PATTERN_ENGINE=nfa`
//! (the frontier rung on every frontier-compilable clause, its cost gate
//! bypassed) versus `=walk` (the traversal kernels stood down, the
//! per-row walker serving) — must answer identically. The shapes here
//! are the ones the rungs disagree about: the fraud f03-f06 rings (the
//! gap classes the forced rung newly compiles — per-state interior
//! guards and the guarded quantified group) and the interior-label
//! spellings of the `WHERE all(x IN nodes(path) WHERE x:Label)` idiom
//! (class K's per-state guards).
//!
//! The env switch is read once per `Database::open` (ADR-0519's
//! resolution site), so each leg opens its own fresh in-memory store
//! under its own value; the `ENV_LOCK` mutex serializes the `set_var` +
//! open pair against the sibling test — the variable is process-global
//! and a sibling thread's open must never read the other leg's value
//! (the shared `set_preview_once` note's F-13-4 hazard).

mod common;

use common::{open_with_engine, rows, run};

/// The f06 sample fixture, loaded from the fraud-qpp workload's own file
/// (one source of truth; the compile-time path follows the
/// `ONTOLOGY_DOC` precedent in `common`): the designed 4-account ring
/// (1000 -> 900 -> 810 -> 729, dates one day apart) plus the five
/// decoys, each excluded for one stated reason.
const FIXTURE_F06: &str =
    include_str!("../../../testing/relate-query-latency/workloads/fraud-qpp/fixture-f06.cypher");

/// f05's planted needle ring: three `test: true` hops with dates one day
/// apart and amounts decaying inside the 20% rule, plus one
/// `test: false` decoy transaction whose date and decay rules pass —
/// so the per-state `test` guards, not the group WHERE, are what
/// exclude it. (The workload's needle generator is the fraud lane's,
/// not a fixture file.)
const NEEDLE_RING: &str = "CREATE \
     (n0:Account {id: 'needle-0', test: true}), \
     (n1:Account {id: 'needle-1', test: true}), \
     (n2:Account {id: 'needle-2', test: true}), \
     (q1:Transaction {test: true, date: date('2026-01-01'), amount: 100.0})-[:FROM]->(n0), \
     (q1)-[:TO]->(n1), \
     (q2:Transaction {test: true, date: date('2026-01-02'), amount: 90.0})-[:FROM]->(n1), \
     (q2)-[:TO]->(n2), \
     (q3:Transaction {test: true, date: date('2026-01-03'), amount: 85.0})-[:FROM]->(n2), \
     (q3)-[:TO]->(n0), \
     (d:Transaction {test: false, date: date('2025-12-30'), amount: 1000.0})-[:FROM]->(n0), \
     (d)-[:TO]->(n2)";

/// Asserts the two legs returned the same rows, in the same order, and
/// that the differential is not vacuous — at least one row answered.
fn assert_identical(what: &str, nfa: Vec<Vec<relate::Value>>, walk: Vec<Vec<relate::Value>>) {
    assert!(
        !nfa.is_empty(),
        "{what}: no rows under forced-nfa — a vacuous differential proves nothing"
    );
    assert_eq!(
        nfa.len(),
        walk.len(),
        "{what}: the legs disagree on the row count"
    );
    assert_eq!(nfa, walk, "{what}: the legs' ordered rows differ");
}

/// The fraud f03-f06 query texts (the fraud-qpp workload's own shapes,
/// `testing/relate-query-latency/workloads/fraud-qpp/queries/` —
/// embedded, the `nfa_tests::fraud_gap_queries_compile_forced_and_
/// decline_auto` convention): every one compiles under the forced rung
/// (the gap classes' per-state guards and guarded group) and must answer
/// the walker identically, ring lists included.
#[test]
fn fraud_f03_to_f06_answer_identically_on_both_legs() {
    let f03 = "MATCH p = SIMPLE (a:Account)<-[:FROM]-(first:Transaction) \
               ((ti:Transaction)-[:TO]->(ai:Account)<-[:FROM]-(tj:Transaction) \
                 WHERE ti.date < tj.date \
                   AND ti.amount >= tj.amount >= 0.80 * ti.amount){1,6} \
               (last:Transaction)-[:TO]->(a) \
               RETURN a.id AS account, \
                      [n IN nodes(p) WHERE n:Transaction | n.amount] AS amounts";
    let f04 = "MATCH p = SIMPLE (a:Account)<-[:FROM]-(first:Transaction) \
               ((ti:Transaction)-[:TO]->(ai:Account)<-[:FROM]-(tj:Transaction) \
                 WHERE ti.date < tj.date \
                   AND ti.amount >= tj.amount >= 0.80 * ti.amount){1,} \
               (last:Transaction)-[:TO]->(a) \
               RETURN a.id AS account, \
                      [n IN nodes(p) WHERE n:Transaction | n.amount] AS amounts";
    let f05 = "MATCH p = SIMPLE (a:Account {id: 'needle-0'})\
               <-[:FROM]-(first:Transaction {test: true}) \
               ((ti:Transaction {test: true})-[:TO]->(ai:Account {test: true})\
               <-[:FROM]-(tj:Transaction {test: true}) \
                 WHERE ti.date < tj.date){1,} \
               (last:Transaction {test: true})-[:TO]->(a) \
               RETURN [n IN nodes(p) WHERE n:Account | n.id] AS ring";
    let f06 = "MATCH p = SIMPLE (a:Account {a_id: 1})<-[:FROM]-(first:Transaction) \
               ((ti:Transaction)-[:TO]->(ai:Account)<-[:FROM]-(tj:Transaction) \
                 WHERE ti.date < tj.date \
                   AND ti.amount >= tj.amount >= 0.80 * ti.amount){1,6} \
               (last:Transaction)-[:TO]->(a) \
               RETURN [n IN nodes(p) WHERE n:Account | n.a_id] AS ring, \
                      [n IN nodes(p) WHERE n:Transaction | n.amount] AS amounts";

    // The fixture's designed ring world serves f03, f04 and f06; f05's
    // needle anchor needs its own planted ring (the workload's needle
    // generator is the fraud lane's, not a fixture file), shrunk to
    // three hops with one `test: false` decoy transaction the
    // per-state guards must exclude on both legs.
    let queries = [
        ("f03", f03),
        ("f04", f04),
        ("f05 on the needle ring", f05),
        ("f06", f06),
    ];
    let mut nfa = open_with_engine("nfa");
    run(&mut nfa, FIXTURE_F06);
    run(&mut nfa, NEEDLE_RING);
    let nfa_rows: Vec<Vec<Vec<relate::Value>>> =
        queries.iter().map(|(_, q)| rows(&mut nfa, q)).collect();
    drop(nfa);

    let mut walk = open_with_engine("walk");
    run(&mut walk, FIXTURE_F06);
    run(&mut walk, NEEDLE_RING);
    for ((name, query), nfa_rows) in queries.iter().zip(&nfa_rows) {
        assert_identical(name, nfa_rows.clone(), rows(&mut walk, query));
    }

    // The oracle pin, on the forced rung's rows: the fixture's one
    // designed ring, exactly the answer oracle.json records for f06 —
    // nodes(p) lists the closed endpoint twice, so the ring column is
    // 1-2-3-4-1.
    let f06_rows = &nfa_rows[3];
    assert_eq!(
        f06_rows,
        &vec![vec![
            common::list(vec![
                relate::Value::Int(1),
                relate::Value::Int(2),
                relate::Value::Int(3),
                relate::Value::Int(4),
                relate::Value::Int(1),
            ]),
            common::list(
                [1000, 900, 810, 729]
                    .into_iter()
                    .map(relate::Value::Int)
                    .collect(),
            ),
        ]],
        "the forced rung's f06 answer must be the oracle's exact row"
    );
    // The needle ring, on both legs: the planted ring only — the
    // `test: false` decoy never rides (its date and decay rules pass, so
    // the per-state `test` guards are what exclude it).
    let needle = vec![vec![common::strings(&[
        "needle-0", "needle-1", "needle-2", "needle-0",
    ])]];
    assert_eq!(
        nfa_rows[2], needle,
        "forced-nfa: the needle ring and nothing else"
    );
    assert_eq!(
        rows(&mut walk, f05),
        needle,
        "forced-walk: the needle ring and nothing else"
    );
}

/// The interior-label shapes: the `WHERE all(x IN nodes(path) WHERE
/// x:Label)` idiom spelled as interior labels — the class-K per-state
/// guards — must answer identically on both legs, on a directed chain,
/// an undirected hop and a self-loop hop, with the idiom spelling
/// agreeing on each leg too.
#[test]
fn interior_label_shapes_answer_identically_on_both_legs() {
    /// One shape: its two spellings — the idiom, and the interior labels
    /// that must answer the same.
    struct Shape {
        what: &'static str,
        idiom: &'static str,
        interior: &'static str,
    }

    const RETURN: &str = " RETURN [n IN nodes(p) | n.k] AS ks ORDER BY ks";

    let shapes = [
        Shape {
            what: "directed chain",
            idiom: "MATCH p = (a:X)-[:R]->(b)-[:R]->(c)-[:R]->(d) \
                    WHERE all(x IN nodes(p) WHERE x:X)",
            interior: "MATCH p = (a:X)-[:R]->(:X)-[:R]->(:X)-[:R]->(d)",
        },
        Shape {
            what: "undirected hop",
            idiom: "MATCH p = (a:X)-[:R]-(b) WHERE all(x IN nodes(p) WHERE x:X)",
            interior: "MATCH p = (a:X)-[:R]-(:X)",
        },
        Shape {
            what: "self-loop hop",
            idiom: "MATCH p = (a:X)-[:R]->(b)-[:R]->(d) \
                    WHERE all(x IN nodes(p) WHERE x:X)",
            interior: "MATCH p = (a:X)-[:R]->(:X)-[:R]->(d)",
        },
    ];

    let seed = "CREATE \
                (x1:X {k: 'x1'}), (x2:X {k: 'x2'}), (x3:X {k: 'x3'}), (x4:X {k: 'x4'}), \
                (y:Y {k: 'y'}), \
                (x1)-[:R]->(x2), (x2)-[:R]->(x3), (x3)-[:R]->(x4), \
                (x3)-[:R]->(x3), \
                (y)-[:R]->(x2)";

    let mut nfa = open_with_engine("nfa");
    run(&mut nfa, seed);
    let mut walk = open_with_engine("walk");
    run(&mut walk, seed);

    for shape in shapes {
        for (spelling, body) in [("idiom", shape.idiom), ("interior", shape.interior)] {
            let query = format!("{body}{RETURN}");
            let nfa_rows = rows(&mut nfa, &query);
            let walk_rows = rows(&mut walk, &query);
            assert_identical(
                &format!("{} ({spelling} spelling)", shape.what),
                nfa_rows,
                walk_rows,
            );
        }
        // The equivalence the interior spelling exists for: on one leg,
        // the idiom and its interior-label spelling answer the same.
        let idiom = rows(&mut nfa, &format!("{}{RETURN}", shape.idiom));
        let interior = rows(&mut nfa, &format!("{}{RETURN}", shape.interior));
        assert_eq!(
            idiom, interior,
            "{}: the interior-label spelling must answer the all() idiom",
            shape.what
        );
    }
}

/// The forced rung's boundary-guard shapes, each against a world where
/// a misplaced guard changes the answer: a guarded post-group node whose
/// pattern the intermediate round ends do not satisfy (only the final
/// round's end is `V` — a guard checked at every round's end would drop
/// every multi-round walk; the unrolled spine form's variable-length
/// round is parser-unreachable, pinned on hand-built IR in `nfa_tests`
/// instead) — and an interior node whose
/// only admission fact is a `Not` or `Or` label expression (a guard
/// compiled as a plain node would admit the excluded `Bad` node).
#[test]
fn boundary_guard_shapes_answer_identically_on_both_legs() {
    let seed = "CREATE \
                (s:U {k: 's'}), (n1:U {k: 'n1'}), (n2:U:V {k: 'n2'}), (d:D {k: 'd'}), \
                (s)-[:R]->(n1), (n1)-[:R]->(n2), (n2)-[:S]->(d), \
                (g:G {k: 'g'}), (ok:A {k: 'ok'}), (bad:Bad {k: 'bad'}), \
                (t1 {k: 't1'}), (t2 {k: 't2'}), \
                (g)-[:C]->(ok), (g)-[:C]->(bad), (ok)-[:R]->(t1), (bad)-[:R]->(t2)";
    let queries = [
        (
            "post-group guard",
            "MATCH (s:U) ((a)-[:R]->(b)){2} (c:V)-[:S]->(e) RETURN DISTINCT e",
        ),
        (
            "interior Not label expression",
            "MATCH (x:G)-[:C]->(m:!Bad) ((a)-[:R]->(b)){1,} (y) RETURN DISTINCT y",
        ),
        (
            "interior Or label expression",
            "MATCH (x:G)-[:C]->(m:A|U) ((a)-[:R]->(b)){1,} (y) RETURN DISTINCT y",
        ),
    ];
    let mut nfa = open_with_engine("nfa");
    run(&mut nfa, seed);
    let nfa_rows: Vec<_> = queries.iter().map(|(_, q)| rows(&mut nfa, q)).collect();
    drop(nfa);
    let mut walk = open_with_engine("walk");
    run(&mut walk, seed);
    for ((name, query), nfa_rows) in queries.iter().zip(nfa_rows) {
        assert_identical(name, nfa_rows, rows(&mut walk, query));
    }
}

/// A class-K interior guard whose evaluation errors — a row-invariant,
/// parameter-free property-map value the forced compile admits
/// (`{k: 1/0}`) — must ERROR on both legs, never answer: the walker's
/// `node_ok` raises the arithmetic error, so the forced rung's guard
/// checks (the BFS admission and the re-arrival search alike) must
/// raise it too, never prune the branch as a failed guard. The world is
/// a parallel-rel ring under the default pool, so the run's re-arrivals
/// reach the rule's search; the same shape with an evaluable guard
/// proves the forced leg serves it on the frontier rung (the PROFILE
/// record's `frontier.levels`), so the error is the rung's own.
#[test]
fn erroring_interior_guard_errors_on_both_legs() {
    /// Whether any operator of the tree ran the frontier family — its
    /// `frontier.levels > 0` engagement mark.
    fn frontier_engaged(node: &relate::PlanNode) -> bool {
        node.metrics.as_ref().is_some_and(|m| m.frontier.levels > 0)
            || node.children.iter().any(frontier_engaged)
    }

    let seed = "CREATE \
                (n0:N {id: 0, k: 1}), (n1:N {id: 1, k: 1}), (n2:N {id: 2, k: 1}), \
                (n0)-[:R]->(n1), (n0)-[:R]->(n1), (n1)-[:R]->(n2), (n2)-[:R]->(n0)";
    let shape = |guard: &str| {
        format!("MATCH (x:N {{id: 0}}) ((a)-[:R]->(b:N {{k: {guard}}})){{1,4}} (y) RETURN DISTINCT y.id AS id")
    };

    let mut nfa = open_with_engine("nfa");
    run(&mut nfa, seed);
    let served = run(&mut nfa, &format!("PROFILE {}", shape("1")));
    assert!(!served.rows.is_empty(), "the evaluable guard answers");
    assert!(
        frontier_engaged(common::plan(&served)),
        "the forced leg serves the shape on the frontier rung"
    );
    let nfa_class = common::error_class_literal(&mut nfa, &shape("1/0"));
    drop(nfa);

    let mut walk = open_with_engine("walk");
    run(&mut walk, seed);
    let walk_class = common::error_class_literal(&mut walk, &shape("1/0"));
    assert_eq!(
        nfa_class, walk_class,
        "the legs raise the same error class for the erroring guard"
    );
}

/// The layered reachability world: `layers + 1` layers of `width`
/// `:N {id, layer}` nodes, every node below the last layer linked by
/// four `R` rels into the next layer (targets spread by a fixed stride,
/// so walks reconverge) — 4^k walks of length k from one seed onto at
/// most `width` endpoints per layer. Returns the store-building
/// statements and the edge list the expected answers are computed from.
fn layered_world(layers: i64, width: i64) -> (Vec<String>, Vec<(i64, i64)>) {
    let edges: Vec<(i64, i64)> = (0..layers * width)
        .flat_map(|a| {
            let next = (a / width + 1) * width;
            (0..4).map(move |k| (a, next + (a * 7 + k * 13) % width))
        })
        .collect();
    let statements = vec![
        format!(
            "UNWIND range(0, {}) AS i CREATE (:N {{id: i, layer: i / {width}}})",
            (layers + 1) * width - 1
        ),
        "CREATE INDEX FOR (n:N) ON (n.id)".to_string(),
        format!(
            "MATCH (a:N) WHERE a.layer < {layers} UNWIND range(0, 3) AS k \
             MATCH (b:N {{id: (a.layer + 1) * {width} + (a.id * 7 + k * 13) % {width}}}) \
             CREATE (a)-[:R]->(b)"
        ),
    ];
    (statements, edges)
}

/// The distinct nodes reachable from `seed` in `1..=max` hops over
/// `edges` — the reference the endpoint-set answers are checked against.
fn reachable(edges: &[(i64, i64)], seed: i64, max: usize) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    let mut level = vec![seed];
    for _ in 0..max {
        let next: std::collections::BTreeSet<i64> = edges
            .iter()
            .filter(|(a, _)| level.contains(a))
            .map(|&(_, b)| b)
            .collect();
        seen.extend(next.iter().copied());
        level = next.into_iter().collect();
    }
    seen.len()
}

/// The reachability aggregations over a branching recursion answer
/// identically on both legs: the endpoint-set answers (`count(DISTINCT
/// d)`, `collect(DISTINCT d)`) under the default pool, the walk count
/// (`count(d)`, `count(*)`) under REPEATABLE ELEMENTS, and the
/// end-filtered walk count the contraction must leave to the walker —
/// against the walker leg's own enumeration, every answer also pinned
/// to its independently computed value.
#[test]
fn reachability_aggregations_answer_identically_on_both_legs() {
    let (statements, edges) = layered_world(5, 6);
    let distinct = i64::try_from(reachable(&edges, 0, 5)).unwrap();
    let walks: i64 = (1..=5).map(|k| 4i64.pow(k)).sum();
    let shapes: [(&str, Option<relate::Value>); 6] = [
        (
            "MATCH (s:N {id: 0})-[:R*1..5]->(d) RETURN count(DISTINCT d) AS c",
            Some(relate::Value::Int(distinct)),
        ),
        (
            "MATCH (s:N {id: 0})-[:R*1..5]->(d) RETURN size(collect(DISTINCT d)) AS c",
            Some(relate::Value::Int(distinct)),
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..5]->(d) RETURN count(d) AS c",
            Some(relate::Value::Int(walks)),
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..5]->(d) RETURN count(*) AS c",
            Some(relate::Value::Int(walks)),
        ),
        // The end-filtered walk count: no contraction (the degree sum
        // would count the filtered arrivals) — the walker's answer.
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..5]->(d) WHERE d.layer = 5 \
             RETURN count(d) AS c",
            Some(relate::Value::Int(4i64.pow(5))),
        ),
        // The default pool's walk count stays a path count on the DFS.
        (
            "MATCH (s:N {id: 0})-[:R*1..5]->(d) RETURN count(d) AS c",
            Some(relate::Value::Int(walks)),
        ),
    ];
    let mut legs = Vec::new();
    for engine in ["nfa", "walk", "auto"] {
        let mut conn = open_with_engine(engine);
        for statement in &statements {
            run(&mut conn, statement);
        }
        legs.push(
            shapes
                .iter()
                .map(|(query, _)| rows(&mut conn, query))
                .collect::<Vec<_>>(),
        );
    }
    for (i, (query, want)) in shapes.iter().enumerate() {
        assert_identical(query, legs[0][i].clone(), legs[1][i].clone());
        assert_identical(query, legs[2][i].clone(), legs[1][i].clone());
        if let Some(want) = want {
            assert_eq!(legs[1][i], vec![vec![want.clone()]], "{query}");
        }
    }
}

/// The reachability-aggregation crash pin: twelve layers of 200,
/// branching four — ~22.4M walks from one seed. Before the cost gate
/// priced the branching recursion (and before the walk count existed),
/// both statements enumerated every path on the auto rung and died on
/// the 2 GB expression cap after ~28 s; the endpoint set and the
/// walk-count contraction answer them in milliseconds. The bound is
/// loose (a debug-ish test profile on a shared runner), the answers
/// exact.
#[test]
fn twelve_level_branching_reachability_counts_answer_without_enumeration() {
    let (statements, edges) = layered_world(12, 200);
    let mut conn = open_with_engine("auto");
    for statement in &statements {
        run(&mut conn, statement);
    }
    let distinct = i64::try_from(reachable(&edges, 0, 12)).unwrap();
    let walks: i64 = (1..=12).map(|k| 4i64.pow(k)).sum();
    for (query, want) in [
        (
            "MATCH (s:N {id: 0})-[:R*1..12]->(d) RETURN count(DISTINCT d) AS reachable",
            distinct,
        ),
        (
            "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..12]->(d) RETURN count(d) AS walks",
            walks,
        ),
    ] {
        let started = std::time::Instant::now();
        let answer = rows(&mut conn, query);
        let elapsed = started.elapsed();
        assert_eq!(answer, vec![vec![relate::Value::Int(want)]], "{query}");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "{query} took {elapsed:?} — the path enumeration is back"
        );
    }
}
