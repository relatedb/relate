use super::*;
use crate::exec::test_support::{build, column, exec_cypher, int_cell, ints, path_value, Builder};
use crate::graph::Graph;
use relate_ir::{PathCount, PathMode, PathSelector};
use std::sync::Arc;

fn selector_state(start: NodeId, end: NodeId, length: usize) -> GpmState {
    GpmState {
        row: Row::new(),
        used: UsedSet::default(),
        current: end,
        nodes: vec![start, end],
        rels: (0..length).map(|index| index as RelId).collect(),
    }
}

/// The selector's rel-type read over [`selector_state`]'s walks: every
/// rel is an `R`.
fn rel_type_r(_: RelId) -> Arc<str> {
    Arc::from("R")
}

#[test]
fn gpm_shortest_groups_are_partitioned_by_endpoints() {
    let states = vec![
        selector_state(1, 2, 1),
        selector_state(1, 2, 2),
        selector_state(1, 2, 2),
        selector_state(3, 4, 3),
        selector_state(3, 4, 4),
    ];
    let selected = apply_gpm_selector(
        states,
        Some(&PathSelector::Shortest {
            count: Some(PathCount::Literal(2)),
            group: true,
        }),
        &BTreeMap::new(),
        rel_type_r,
    )
    .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|state| state.rels.len())
            .collect::<Vec<_>>(),
        vec![1, 2, 2, 3, 4]
    );
}

/// `ANY k` keeps the `k` shortest members of a partition, sharing
/// `SHORTEST k`'s order: the reference chooses arbitrarily by walking
/// breadth-first, which the conformance suite pins as `ANY k` ==
/// `SHORTEST k` on every example row. Enumeration order here is DFS
/// (deepest first), so keeping "the first `count` enumerated" would select
/// the longest paths instead.
#[test]
fn any_keeps_the_shortest_members() {
    let states = || {
        vec![
            selector_state(1, 2, 3),
            selector_state(1, 2, 1),
            selector_state(1, 2, 2),
        ]
    };
    let one = apply_gpm_selector(
        states(),
        Some(&PathSelector::Any(None)),
        &BTreeMap::new(),
        rel_type_r,
    )
    .unwrap();
    assert_eq!(
        one.iter().map(|state| state.rels.len()).collect::<Vec<_>>(),
        vec![1]
    );
    let two = apply_gpm_selector(
        states(),
        Some(&PathSelector::Any(Some(PathCount::Literal(2)))),
        &BTreeMap::new(),
        rel_type_r,
    )
    .unwrap();
    assert_eq!(
        two.iter().map(|state| state.rels.len()).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn gpm_acyclic_and_simple_apply_distinct_node_rules() {
    let cycle: [NodeId; 3] = [1, 2, 1];
    assert!(!gpm_path_mode_allows(
        Some(PathMode::Acyclic),
        cycle.into_iter(),
        []
    ));
    assert!(gpm_path_mode_allows(
        Some(PathMode::Simple),
        cycle.into_iter(),
        []
    ));
    let repeated_interior: [NodeId; 4] = [1, 2, 2, 3];
    assert!(!gpm_path_mode_allows(
        Some(PathMode::Simple),
        repeated_interior.into_iter(),
        []
    ));
}

// ---- End-to-end selector semantics (PathSelectorAcceptance shapes) ----

/// Rows of `q` over `graph`.
fn query_rows(graph: &Graph, q: &str) -> Vec<Vec<Value>> {
    exec_cypher(graph, q).rows
}

/// One row of a `collect([...])` of integer lists.
fn list_of_ints(lists: &[&[i64]]) -> Vec<Vec<Value>> {
    vec![vec![Value::List(
        lists.iter().map(|l| ints(l)).collect::<Vec<_>>().into(),
    )]]
}

/// The conformance suite's "Predicate outside parentheses in selective path
/// pattern is a postfilter" graph: a length-2 path carrying an `X`, a
/// length-3 and a length-4 path without one.
fn postfilter_graph() -> Graph {
    build(
        &[&["A"], &[], &["B"], &[], &[], &[], &[], &[]],
        &[
            (0, 1, "R"),
            (1, 2, "X"),
            (0, 3, "R"),
            (3, 4, "R"),
            (4, 2, "R"),
            (0, 5, "R"),
            (5, 6, "R"),
            (6, 7, "R"),
            (7, 2, "R"),
        ],
    )
    .graph
}

/// A predicate written outside the selective path pattern's parentheses is a
/// postfilter: the selector picks first, the predicate then drops rows. The
/// counts pin that `ANY k` picks the `k` shortest paths (its arbitrary
/// choice in the reference's breadth-first order), not the enumerated ones —
/// `ANY` and `ANY 1` therefore see only the `X`-carrying shortest path.
#[test]
fn predicate_outside_a_selective_path_pattern_is_a_postfilter() {
    let graph = postfilter_graph();
    let count = |selector: &str| {
        query_rows(
            &graph,
            &format!(
                "MATCH p = {selector} (:A)-->*(:B) \
                 WHERE none(r IN relationships(p) WHERE r:X) \
                 RETURN count(*) AS result"
            ),
        )[0][0]
            .clone()
    };
    assert_eq!(count("ANY SHORTEST"), Value::Int(0));
    assert_eq!(count("SHORTEST 1"), Value::Int(0));
    assert_eq!(count("SHORTEST 2"), Value::Int(1));
    assert_eq!(count("SHORTEST 3"), Value::Int(2));
    assert_eq!(count("ALL SHORTEST"), Value::Int(0));
    assert_eq!(count("ANY"), Value::Int(0));
    assert_eq!(count("ANY 1"), Value::Int(0));
    assert_eq!(count("ANY 2"), Value::Int(1));
    assert_eq!(count("ANY 3"), Value::Int(2));
}

/// The conformance suite's disjoint-path graph: two length-2 paths (one
/// through `X`), a length-3 path carrying an `X`, and a clean length-3 path.
fn disjoint_graph() -> Graph {
    build(
        &[&["A"], &["X"], &["B"], &[], &[], &["X"], &[], &[]],
        &[
            (0, 1, "R"),
            (1, 2, "R"),
            (0, 3, "R"),
            (3, 2, "R"),
            (0, 4, "R"),
            (4, 5, "R"),
            (5, 2, "R"),
            (0, 6, "R"),
            (6, 7, "R"),
            (7, 2, "R"),
        ],
    )
    .graph
}

/// The two-selector disjoint-path query both disjoint-selector tests run:
/// the first selector picks a clean path, the second must return the
/// distinct path disjoint from it.
fn disjoint_query(selector: &str) -> String {
    format!(
        "MATCH {selector} (p = (:A)-->+(:B) WHERE none(n IN nodes(p) WHERE n:X)) \
         MATCH {selector} (q = (:A)-->+(:B) WHERE q <> p AND length(p) = length(q)) \
         WITH length(p) AS pathLength, \
              size([n IN nodes(p) WHERE n:X]) AS pxCount, \
              size([n IN nodes(q) WHERE n:X]) AS qxCount \
         ORDER BY pathLength \
         RETURN collect([pathLength, pxCount, qxCount]) AS result"
    )
}

/// A predicate written inside the selective path pattern's parentheses is a
/// restrictor: it narrows the candidates before the selector picks. Two
/// `SHORTEST` selectors therefore find two *different* paths — `q <> p`
/// is evaluated while `q` is still a candidate, so the second selector can
/// return the disjoint path instead of comparing against a path it already
/// chose.
#[test]
fn disjoint_shortest_selectors_return_the_distinct_second_path() {
    let graph = disjoint_graph();
    let disjoint = |selector: &str| query_rows(&graph, &disjoint_query(selector));
    assert_eq!(disjoint("ANY SHORTEST"), list_of_ints(&[&[2, 0, 1]]));
    assert_eq!(disjoint("SHORTEST 1"), list_of_ints(&[&[2, 0, 1]]));
    assert_eq!(
        disjoint("SHORTEST 2"),
        list_of_ints(&[&[2, 0, 1], &[3, 0, 1]])
    );
    assert_eq!(disjoint("ANY"), list_of_ints(&[&[2, 0, 1]]));
    assert_eq!(disjoint("ANY 1"), list_of_ints(&[&[2, 0, 1]]));
    assert_eq!(disjoint("ANY 2"), list_of_ints(&[&[2, 0, 1], &[3, 0, 1]]));
}

/// The parameterized spelling of the same two selectors resolves its counts
/// identically (`ANY $two` == `SHORTEST $two`).
#[test]
fn parameterized_any_on_disjoint_selectors_matches_shortest() {
    let graph = disjoint_graph();
    let params = BTreeMap::from([
        ("one".to_string(), Value::Int(1)),
        ("two".to_string(), Value::Int(2)),
    ]);
    let disjoint = |selector: &str, params: &BTreeMap<String, Value>| {
        crate::exec::test_support::try_exec_cypher_params(&graph, &disjoint_query(selector), params)
            .unwrap()
            .rows
    };
    assert_eq!(
        disjoint("SHORTEST $two", &params),
        list_of_ints(&[&[2, 0, 1], &[3, 0, 1]])
    );
    assert_eq!(
        disjoint("ANY $two", &params),
        list_of_ints(&[&[2, 0, 1], &[3, 0, 1]])
    );
    assert_eq!(disjoint("ANY $one", &params), list_of_ints(&[&[2, 0, 1]]));
}

/// The restrictor is what makes the first selector pick the shortest clean
#[test]
fn a_path_pattern_predicate_restricts_before_the_selector_picks() {
    let graph = disjoint_graph();
    let rows = query_rows(
        &graph,
        "MATCH SHORTEST 1 (p = (:A)-->+(:B) WHERE none(n IN nodes(p) WHERE n:X)) \
         RETURN length(p) AS len",
    );
    assert_eq!(rows, vec![vec![Value::Int(2)]]);
}

/// `ANY k` and `SHORTEST k` agree on every conformance example row; on a
/// graph whose paths differ in length the arbitrary choice is the shortest.
#[test]
fn any_selects_the_same_paths_as_shortest() {
    // The conformance suite's "Predicate not required when making subpath
    // variable declaration" graph: a 6-node ring with two L-labelled
    // spokes.
    let graph = build(
        &[&["L"], &[], &["L"], &["L"], &[], &[]],
        &[
            (0, 1, "R"),
            (1, 2, "R"),
            (2, 3, "R"),
            (3, 5, "R"),
            (5, 4, "R"),
            (4, 0, "R"),
        ],
    )
    .graph;
    let lengths = |selector: &str| {
        query_rows(
            &graph,
            &format!(
                "MATCH {selector} (p = (:L)--+(:L)) \
                 WITH length(p) AS pathLength, count(*) AS count ORDER BY pathLength \
                 RETURN collect([pathLength, count]) AS result"
            ),
        )
    };
    let shortest = lengths("ANY SHORTEST");
    assert_eq!(
        shortest,
        list_of_ints(&[&[1, 2], &[2, 2], &[3, 2], &[6, 3]])
    );
    assert_eq!(lengths("ANY"), shortest);
    assert_eq!(lengths("ANY 1"), shortest);
    let second = lengths("SHORTEST 2");
    assert_eq!(
        second,
        list_of_ints(&[&[1, 2], &[2, 2], &[3, 4], &[4, 2], &[5, 2], &[6, 6]])
    );
    assert_eq!(lengths("ANY 2"), second);
}

// ---- The pinned-or-fresh rebinding policy (LABS-668) ----

/// A rel variable pre-bound to null by a FAILED `OPTIONAL MATCH` is a
/// binding to compare against, not a fresh slot: the later `MATCH`
/// matches nothing. The two engines disagree today — this test pins the
/// disagreement so it is visible rather than silent.
///
/// The classic walker treats any bound non-relationship — null included —
/// as a `Binding::Mismatch` (LABS-711): verified against a live Neo4j 5,
/// this shape returns zero rows with the rel present in the graph (the
/// positive control without the failing `OPTIONAL MATCH` returns the row).
///
/// The GPM engine still reads a null pre-binding as `Prebound::Fresh` and
/// rebinds (ADR-0335's rule for a CIP-28 non-local singleton, which is
/// pre-bound to null and rebound at its own site). Whether that rule should
/// keep covering a null produced by a failed `OPTIONAL MATCH` is an ADR
/// decision — aligning GPM with the Neo4j-verified result needs a new ADR
/// superseding ADR-0335's null arm, so the behavior is left as-is and
/// ticketed rather than flipped here.
#[test]
fn a_null_prebound_rel_variable_is_not_rebound_by_the_classic_walker() {
    let graph = build(&[&["A"], &["B"], &["C"]], &[(0, 1, "R"), (0, 2, "R")]).graph;
    // A failed OPTIONAL MATCH pre-binds its rel variable to null; the
    // following MATCH reuses the name (a join on the existing binding).
    let targets = |pattern: &str| {
        let result = exec_cypher(
            &graph,
            &format!(
                "MATCH (x:A) OPTIONAL MATCH (x)-[r:R]->(y:B) WHERE y.id = 99 \
                 MATCH (x)-[{pattern}]->(z) RETURN z.id AS z"
            ),
        );
        let mut ids = column(&result, "z", int_cell);
        ids.sort_unstable();
        ids
    };
    // The classic walker's spelling: the null pre-bound `r` is a binding
    // to compare against, so nothing matches (Neo4j-verified).
    assert_eq!(targets("r:R"), Vec::<i64>::new());
    // The GPM engine's spelling of the same query (the rel's inline WHERE
    // routes the pattern to the GPM engine): ADR-0335's null-is-fresh rule
    // still rebinds here. Pinned so the divergence is explicit — see the
    // doc comment and the ADR ticket.
    assert_eq!(targets("r WHERE r.k >= 0"), vec![1, 2]);
}

/// An END variable pre-bound to null by a FAILED `OPTIONAL MATCH` is a
/// fresh slot to the GPM engine, not a binding to compare against — the
/// end-side twin of the rel-side divergence the test above pins. The two
/// engines disagree today; this test pins the disagreement so it is
/// visible rather than silent.
///
/// The classic family treats any bound non-node — null included — as a
/// `Binding::Mismatch` that matches nothing (LABS-711, Neo4j-verified;
/// PR #644 aligned the shortest-expression engines to the same rule).
/// The GPM engine reads a null pre-binding as `Prebound::Fresh`
/// (`gpm_bind_node`, ADR-0335's null arm) and rebinds. The BFS route
/// never answers the row: `route_shape` declines an endpoint variable
/// that is neither absent nor node-bound (pinned in shortest_route's
/// reject tests), so the GPM enumeration answers, and its target accept
/// rebinds.
///
/// Whether ADR-0335's null arm should keep covering a null pre-bound END
/// variable is the open LABS-863 decision — aligning GPM with the family
/// rule needs a new ADR superseding that arm, and the change is wider
/// than the route: the one `prebound` spelling every GPM node binding
/// site and the frontier family's seeds read. The behavior is left as-is
/// and pinned rather than flipped here.
#[test]
fn a_null_prebound_end_variable_is_rebound_by_the_gpm_engine() {
    let graph = build(&[&["A"], &["B"], &["C"]], &[(0, 1, "R"), (0, 2, "R")]).graph;
    // A failed OPTIONAL MATCH pre-binds its end variable to null; the
    // following MATCH reuses the name (a join on the existing binding).
    let targets = |second: &str| {
        let result = exec_cypher(
            &graph,
            &format!(
                "MATCH (x:A) OPTIONAL MATCH (x)-[q:R]->(y:B) WHERE y.id = 99 \
                 MATCH {second} RETURN y.id AS z"
            ),
        );
        let mut ids = column(&result, "z", int_cell);
        ids.sort_unstable();
        ids
    };
    // The classic walker's spelling (a fresh rel variable, so the null
    // end is the only pre-bound reading): the null pre-bound `y` is a
    // binding to compare against, so nothing matches — the family rule.
    assert_eq!(targets("(x)-[r2:R]->(y)"), Vec::<i64>::new());
    // The GPM engine's spelling of the same row (the selector routes the
    // pattern to the GPM engine): ADR-0335's null-is-fresh arm rebinds
    // `y` to every reachable end. Pinned so the divergence is explicit —
    // see the doc comment and the LABS-863 ADR ticket.
    assert_eq!(targets("p = shortestPath((x)-[*]->(y))"), vec![1, 2]);
}

/// The GPM walk ticks the traversed-rel counter (`neighbor_walks`, the
/// `Db Hits` read kind behind PROFILE) — the counting point's own
/// contract: "the one counting point every matcher uses: the
/// relationship it actually walked, never a candidate it filtered out".
/// The walk used to tick none: a GPM-routed pattern reported zero
/// traversals while the classic walker reported one per walked rel
/// (regression, LABS-668).
#[test]
fn the_gpm_walk_counts_its_traversed_rels() {
    let graph = build(&[&["A"], &["B"], &["C"]], &[(0, 1, "R"), (0, 2, "R")]).graph;
    let _ = super::super::take_neighbor_walks();
    exec_cypher(
        &graph,
        "MATCH (x:A)-[r WHERE r.k >= 0]->(z) RETURN z.id AS z",
    );
    let walks = super::super::take_neighbor_walks();
    assert_eq!(walks, 2, "one neighbor walk per traversed rel");
}

// ---- The quantified group's one bind order (CIP-28 [11]) ----

/// The CIP-28 [11] graph: `X → A → A:B → A:B`, the two `A:B` nodes
/// clearing the `h > 2` inline predicate, so the named quantified group
/// `(p2 = (x:A)-[:R]->(z:B WHERE z.h > 2))*` makes exactly three stops
/// after the fixed-length prefix: 0, 1, and 2 repetitions.
fn subpath_group_graph() -> Graph {
    let b = Builder::new();
    let n99 = b.node("n99", &["X"]);
    let n1 = b.node("n1", &["A"]);
    let n2 = b.prop_node("n2", &["A", "B"], &[("h", Value::Int(3))]);
    let n3 = b.prop_node("n3", &["A", "B"], &[("h", Value::Int(4))]);
    b.rel(n99, n1, "R", &[]);
    b.rel(n1, n2, "R", &[]);
    b.rel(n2, n3, "R", &[]);
    b.done()
}

/// Every stop of a quantified group that names a subpath binds the same
/// names in the same order — the zero-repetition stop included, where
/// the whole group vocabulary (`x`, `z`, `p2`) binds fresh as empty
/// lists. The completion's path-name rung is memoized per walk and
/// stamped onto every complete state (`Row::stamp`'s prefix invariant),
/// so a zero-rep stop that appended the vocabulary in any other order —
/// a sorted accumulator's alphabetical order put `p2` before `x`/`z`,
/// while every count≥1 stop's iteration walk binds `x`/`z` in pattern
/// order and appends `p2` last — desynchronized the complete states'
/// schemas: the debug assert fired (CIP-28 [11], the cip-eval ratchet's
/// 88→87) and a release build silently re-keyed every later row. The
/// pin is the bindings at every stop, so a regressed order fails on the
/// values (a re-keyed row reads another list under `p2`), not only on
/// the assert: the unfiltered enumeration returns one row per stop with
/// its group sizes, and the CIP-28 [11] filtered spelling answers
/// `[1, 1]`.
#[test]
fn quantified_subpath_group_binds_one_schema_across_its_stops() {
    let graph = subpath_group_graph();
    let stops = query_rows(
        &graph,
        "MATCH p1 = (a:X)-[b]->(c)(p2 = (x:A)-[:R]->(z:B WHERE z.h > 2))* \
         RETURN size(x) AS n, [q IN p2 | length(q)] AS lens \
         ORDER BY n",
    );
    assert_eq!(
        stops,
        vec![
            vec![Value::Int(0), Value::List(Vec::new().into())],
            vec![Value::Int(1), ints(&[1])],
            vec![Value::Int(2), ints(&[1, 1])],
        ],
        "every stop binds its group lists, the zero-rep empty ones included"
    );
    // The CIP-28 [11] spelling the ratchet pins: the size filter keeps
    // the 2-repetition stop and projects the subpath list's lengths.
    assert_eq!(
        query_rows(
            &graph,
            "MATCH p1 = (a:X)-[b]->(c)(p2 = (x:A)-[:R]->(z:B WHERE z.h > 2))* \
             WHERE size(x) = 2 \
             RETURN [q IN p2 | length(q)] AS lens"
        ),
        vec![vec![ints(&[1, 1])]],
        "the filtered spelling completes on the same one-schema rows"
    );
}

// ---- Interior hops ride ids; observed values materialize (LABS-853) ----

/// `a -R{w:1}-> b -R{w:2}-> c`, every node carrying a `name`: the walk's
/// interior hops ride as ids (`GpmState`), so every value a query
/// observes — the path, the rel list, the end node — must still come out
/// carrying its full property map.
fn lazy_chain_graph() -> Graph {
    let b = Builder::new();
    let a = b.prop_node("a", &["N"], &[("name", Value::String("A".into()))]);
    let m = b.prop_node("b", &["N"], &[("name", Value::String("B".into()))]);
    let c = b.prop_node("c", &["N"], &[("name", Value::String("C".into()))]);
    b.rel(a, m, "R", &[("w", Value::Int(1))]);
    b.rel(m, c, "R", &[("w", Value::Int(2))]);
    b.done()
}

/// The property map a node or relationship value carries.
fn carried_prop(value: &Value, key: &str) -> Option<Value> {
    match value {
        Value::Node(node) => node.properties.get(key).cloned(),
        Value::Relationship(rel) => rel.properties.get(key).cloned(),
        other => panic!("not an entity: {other:?}"),
    }
}

/// The observable values of a var-length walk — the named path, the
/// rel-list variable, and the end node — are the full records: the
/// path materializes from the walk's id slices at the path-name bind,
/// the row's stubs inflate where the value leaves the engine.
#[test]
fn var_length_observed_values_carry_full_records() {
    let graph = lazy_chain_graph();
    let rows = query_rows(
        &graph,
        "MATCH p = (x:N {id: 'a'})-[r:R*1..2]->(y) RETURN p, r, y ORDER BY length(p)",
    );
    assert_eq!(rows.len(), 2);
    let names = ["B", "C"];
    for (row, end_name) in rows.iter().zip(names) {
        let path = path_value(&row[0]).expect("p is a path");
        let path_names: Vec<Value> = path
            .nodes
            .iter()
            .map(|node| {
                node.properties
                    .get("name")
                    .cloned()
                    .expect("path node name")
            })
            .collect();
        assert_eq!(path_names.len(), path.rels.len() + 1);
        assert_eq!(path_names.last(), Some(&Value::String(end_name.into())));
        for rel in &path.rels {
            assert!(rel.properties.get("w").is_some(), "path rel carries w");
        }
        let Value::List(rels) = &row[1] else {
            panic!("r is a list: {:?}", row[1]);
        };
        assert_eq!(rels.len(), path.rels.len());
        for (index, rel) in rels.iter().enumerate() {
            assert_eq!(
                carried_prop(rel, "w"),
                Some(Value::Int(index as i64 + 1)),
                "a returned trail rel is the full record"
            );
        }
        assert_eq!(
            carried_prop(&row[2], "name"),
            Some(Value::String(end_name.into())),
            "the end node is the full record"
        );
    }
}

/// A quantified subpath variable observes each iteration's path: it
/// materializes per iteration from the iteration's id slices, carrying
/// the full records.
#[test]
fn quantified_subpath_values_carry_full_records() {
    let graph = lazy_chain_graph();
    let rows = query_rows(
        &graph,
        "MATCH (x:N {id: 'a'}) (q = (s)-[e:R]->(t)){2} (y) \
         RETURN [seg IN q | [n IN nodes(seg) | n.name]] AS hops, \
                [seg IN q | relationships(seg)[0].w] AS ws",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::List(
                vec![
                    Value::List(vec![Value::String("A".into()), Value::String("B".into())].into()),
                    Value::List(vec![Value::String("B".into()), Value::String("C".into())].into()),
                ]
                .into()
            ),
            ints(&[1, 2]),
        ]]
    );
}

/// The rel admission reads the wanted property entries key by key (no
/// full record per candidate): a var-length rel property map and an
/// inline rel `WHERE` inside a quantified group prune exactly as before,
/// and a missing key never matches.
#[test]
fn var_length_rel_admission_reads_wanted_properties() {
    let graph = lazy_chain_graph();
    assert_eq!(
        query_rows(
            &graph,
            "MATCH (x:N {id: 'a'})-[:R*1..2 {w: 1}]->(y) RETURN y.id AS id",
        ),
        vec![vec![Value::String("b".into())]]
    );
    assert_eq!(
        query_rows(
            &graph,
            "MATCH (x:N {id: 'a'})-[:R*1..2 {missing: 1}]->(y) RETURN y.id AS id",
        ),
        Vec::<Vec<Value>>::new()
    );
    assert_eq!(
        query_rows(
            &graph,
            "MATCH (x:N {id: 'a'}) ((s)-[e:R WHERE e.w >= 1]->(t)){1,2} (y) \
             RETURN y.id AS id ORDER BY id",
        ),
        vec![
            vec![Value::String("b".into())],
            vec![Value::String("c".into())]
        ]
    );
}

/// The eval walker's node admission (`pattern_node_ok`) reads labels and
/// the wanted property entries without the full record: a pattern
/// comprehension's property-constrained target admits exactly the
/// matching node.
#[test]
fn pattern_comprehension_node_admission_reads_wanted_properties() {
    let graph = lazy_chain_graph();
    assert_eq!(
        query_rows(
            &graph,
            "MATCH (x:N {id: 'a'}) \
             RETURN [(x)-[:R]->(m:N {name: 'B'}) | m.id] AS hit, \
                    [(x)-[:R]->(m:N {name: 'Z'}) | m.id] AS miss, \
                    [(x)-[:R]->(m {name: 'B'}) | m.id] AS unlabeled",
        ),
        vec![vec![
            Value::List(vec![Value::String("b".into())].into()),
            Value::List(Vec::new().into()),
            Value::List(vec![Value::String("b".into())].into()),
        ]]
    );
}

// ---- `x{.*}` reads properties through the store (LABS-853) ----

/// The `w` values of the `e {.*}` maps a query projects, sorted: the
/// all-properties projection's answer over the lazy chain, whose bound
/// rels are property-less stubs until a consumer reads them.
fn projected_ws(graph: &Graph, q: &str) -> Vec<i64> {
    let result = exec_cypher(graph, q);
    let mut ws = column(&result, "props", |value| match value {
        Value::Map(m) => match m.get("w") {
            Some(Value::Int(w)) => Some(*w),
            _ => None,
        },
        _ => None,
    });
    ws.sort_unstable();
    ws
}

/// The `{.*}` map projection over a stub-bound var-length rel reads the
/// one store-backed map, not the stub's own empty one: every unwound
/// trail rel projects its full `{w}` map, the classic `*1..2` spelling
/// and the TRAIL group spelling alike (the pre-fix arm answered `{}` for
/// every rel).
#[test]
fn var_length_all_properties_projection_reads_the_store() {
    let graph = lazy_chain_graph();
    assert_eq!(
        projected_ws(
            &graph,
            "MATCH (x:N {id: 'a'})-[r:R*1..2]->(y) UNWIND r AS e RETURN e{.*} AS props"
        ),
        vec![1, 1, 2]
    );
    assert_eq!(
        projected_ws(
            &graph,
            "MATCH TRAIL (x:N {id: 'a'})-[r:R]->{1,2}(y) UNWIND r AS e \
             RETURN e{.*} AS props"
        ),
        vec![1, 1, 2]
    );
}

/// The same projection over the shortest-route and single-hop binds: the
/// `ANY SHORTEST` rel list unwound in both spellings, and the one-hop
/// TRAIL rel returned directly — each projects the full `{w}` map.
#[test]
fn shortest_route_all_properties_projection_reads_the_store() {
    let graph = lazy_chain_graph();
    assert_eq!(
        projected_ws(
            &graph,
            "MATCH ANY SHORTEST (x:N {id: 'a'})-[r:R*1..2]->(y:N {id: 'c'}) \
             UNWIND r AS e RETURN e{.*} AS props"
        ),
        vec![1, 2]
    );
    assert_eq!(
        projected_ws(
            &graph,
            "MATCH p = ANY SHORTEST (x:N {id: 'a'})-[r:R]->+(y:N {id: 'c'}) \
             UNWIND r AS e RETURN e{.*} AS props"
        ),
        vec![1, 2]
    );
    assert_eq!(
        projected_ws(
            &graph,
            "MATCH TRAIL (x:N {id: 'a'})-[r:R]->(y) RETURN r{.*} AS props"
        ),
        vec![1]
    );
}
