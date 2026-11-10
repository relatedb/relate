//! ADR-0186 result contract of the BFS route for `MATCH`-clause shortest
//! paths.

use relate_core::Value::{self, Bool, Int, Null};

use crate::exec::test_support::{build, fan_in, Builder};
use crate::graph::Graph;
use crate::test_support::ints;

use super::fixtures::{gpm, on_route, plain, run, shortcut, sorted, Counts};
use super::with_forced_gpm;

fn expect(rows: &[&[Value]]) -> Vec<Vec<Value>> {
    sorted(rows.iter().map(|r| r.to_vec()).collect())
}

#[test]
fn both_endpoints_bound() {
    let g = shortcut();
    let q = "MATCH (a {id: 0}), (b {id: 3}) \
             MATCH p = shortestPath((a)-[*]->(b)) RETURN [n IN nodes(p) | n.id]";
    assert_eq!(on_route(&g, q), expect(&[&[ints(&[0, 4, 3])]]));
}

#[test]
fn only_start_bound() {
    let g = shortcut();
    let q = "MATCH (a {id: 0}) MATCH p = shortestPath((a)-[*]->(b)) RETURN b.id, length(p)";
    assert_eq!(
        on_route(&g, q),
        expect(&[
            &[Int(1), Int(1)],
            &[Int(2), Int(2)],
            &[Int(3), Int(2)],
            &[Int(4), Int(1)],
        ])
    );
}

#[test]
fn only_end_bound_reports_start_to_end() {
    let g = shortcut();
    let q = "MATCH (b {id: 3}) MATCH p = shortestPath((a)-[*]->(b)) \
             RETURN a.id, [n IN nodes(p) | n.id]";
    assert_eq!(
        on_route(&g, q),
        expect(&[
            &[Int(0), ints(&[0, 4, 3])],
            &[Int(1), ints(&[1, 2, 3])],
            &[Int(2), ints(&[2, 3])],
            &[Int(3), ints(&[3, 1, 2, 3])],
            &[Int(4), ints(&[4, 3])],
        ])
    );
}

#[test]
fn both_unbound_with_labels() {
    let rels = [(0, 1), (1, 2), (3, 2), (2, 4), (0, 4)].map(|(a, b)| (a, b, "R"));
    let g = build(&[&["S"], &[], &["T"], &["S"], &["T"]], &rels).graph;
    let q = "MATCH p = shortestPath((a:S)-[*]->(b:T)) RETURN a.id, b.id, length(p)";
    let want = expect(&[
        &[Int(0), Int(2), Int(2)],
        &[Int(0), Int(4), Int(1)],
        &[Int(3), Int(2), Int(1)],
        &[Int(3), Int(4), Int(2)],
    ]);
    assert_eq!(on_route(&g, q), want);
    assert_eq!(gpm(&g, q), want);
}

#[test]
fn endpoint_maps_read_row_and_start_variables() {
    let g = shortcut();
    for (q, want) in [
        (
            "MATCH (x {id: 3}) MATCH p = shortestPath((a {id: 0})-[*]->(b {id: x.id})) \
             RETURN b.id, length(p)",
            expect(&[&[Int(3), Int(2)]]),
        ),
        (
            "MATCH p = shortestPath((a)-[*]->(b {id: a.id})) RETURN a.id, length(p)",
            expect(&[&[Int(1), Int(3)], &[Int(2), Int(3)], &[Int(3), Int(3)]]),
        ),
    ] {
        assert_eq!(on_route(&g, q), want, "{q}");
        assert_eq!(gpm(&g, q), want, "{q}");
    }
}

#[test]
fn same_variable_directed_two_cycle() {
    let g = plain(4, &[(0, 1), (1, 0), (0, 2), (2, 3), (3, 0)]);
    let q = "MATCH (a {id: 0}) MATCH p = shortestPath((a)-[*]->(a)) RETURN length(p)";
    assert_eq!(on_route(&g, q), expect(&[&[Int(2)]]));
    let q = "MATCH p = shortestPath((a)-[*]->(a)) RETURN a.id, length(p)";
    let want = expect(&[
        &[Int(0), Int(2)],
        &[Int(1), Int(2)],
        &[Int(2), Int(3)],
        &[Int(3), Int(3)],
    ]);
    assert_eq!(on_route(&g, q), want);
    assert_eq!(gpm(&g, q), want);
}

#[test]
fn undirected_closed_trail_on_triangle_and_single_edge() {
    let triangle = plain(3, &[(0, 1), (1, 2), (2, 0)]);
    let q = "MATCH (a {id: 0}) MATCH p = shortestPath((a)-[*]-(a)) RETURN length(p)";
    assert_eq!(on_route(&triangle, q), expect(&[&[Int(3)]]));
    let all = "MATCH (a {id: 0}) MATCH p = allShortestPaths((a)-[*]-(a)) \
               RETURN [r IN relationships(p) | r.k]";
    let want = expect(&[&[ints(&[0, 1, 2])], &[ints(&[2, 1, 0])]]);
    assert_eq!(on_route(&triangle, all), want);
    assert_eq!(gpm(&triangle, all), want);

    let edge = plain(2, &[(0, 1)]);
    assert!(
        on_route(&edge, q).is_empty(),
        "a trail cannot return over its only edge"
    );
}

#[test]
fn self_loop_closes_at_length_one() {
    let g = plain(2, &[(0, 1), (1, 0), (0, 0)]);
    for q in [
        "MATCH (a {id: 0}) MATCH p = shortestPath((a)-[*]->(a)) RETURN length(p)",
        "MATCH (a {id: 0}) MATCH p = shortestPath((a)-[*]-(a)) RETURN length(p)",
    ] {
        assert_eq!(on_route(&g, q), expect(&[&[Int(1)]]), "{q}");
    }
}

#[test]
fn zero_min_hops_on_same_node_is_length_zero() {
    let g = plain(2, &[(0, 1), (1, 0)]);
    let q = "MATCH (a {id: 0}) MATCH p = shortestPath((a)-[*0..2]->(a)) RETURN length(p)";
    let (rows, counts) = run(&g, q);
    assert_eq!(rows, vec![vec![Int(0)]]);
    assert_eq!(
        counts.route_runs, 1,
        "the zero-length path skipped the route"
    );
    assert_eq!(gpm(&g, q), expect(&[&[Int(0)]]));
}

#[test]
fn relationship_variable_binds_path_relationships_in_order() {
    let g = plain(4, &[(0, 1), (1, 2), (2, 3)]);
    for q in [
        "MATCH (a {id: 0}), (b {id: 3}) MATCH p = shortestPath((a)-[r*]->(b)) \
         RETURN [x IN r | x.k], r = relationships(p)",
        "MATCH (b {id: 3}) MATCH p = shortestPath((a {id: 0})-[r*]->(b)) \
         RETURN [x IN r | x.k], r = relationships(p)",
        "MATCH (a {id: 0}) MATCH p = allShortestPaths((a)-[r*]->(b {id: 3})) \
         RETURN [x IN r | x.k], r = relationships(p)",
    ] {
        assert_eq!(
            on_route(&g, q),
            expect(&[&[ints(&[0, 1, 2]), Bool(true)]]),
            "{q}"
        );
    }
    let q = "MATCH (b {id: 3}) MATCH p = shortestPath((b)<-[r*]-(a {id: 0})) \
             RETURN [x IN r | x.k], [n IN nodes(p) | n.id]";
    assert_eq!(
        on_route(&g, q),
        expect(&[&[ints(&[2, 1, 0]), ints(&[3, 2, 1, 0])]])
    );
}

#[test]
fn unreachable_pair_gives_no_row_and_optional_gives_null() {
    let g = plain(3, &[(0, 1), (2, 1)]);
    let q = "MATCH (a {id: 0}), (b {id: 2}) MATCH p = shortestPath((a)-[*]->(b)) RETURN p";
    assert!(on_route(&g, q).is_empty());
    let q = "MATCH (a {id: 0}), (b {id: 2}) \
             OPTIONAL MATCH p = shortestPath((a)-[*]->(b)) RETURN p";
    assert_eq!(on_route(&g, q), vec![vec![Null]]);
}

/// 0→1→3 and 0→2→3.
fn diamond() -> Graph {
    plain(4, &[(0, 1), (1, 3), (0, 2), (2, 3)])
}

#[test]
fn all_shortest_paths_on_diamond_gives_both_routes() {
    let g = diamond();
    let q = "MATCH (a {id: 0}), (b {id: 3}) MATCH p = allShortestPaths((a)-[*]->(b)) \
             RETURN [n IN nodes(p) | n.id]";
    assert_eq!(
        on_route(&g, q),
        expect(&[&[ints(&[0, 1, 3])], &[ints(&[0, 2, 3])]])
    );
}

#[test]
fn gql_selectors_match_function_forms() {
    let g = diamond();
    let count = |q: &str| on_route(&g, q).len();
    for (endpoints, prefix) in [
        ("(a {id: 0})-[*]->(b {id: 3})", ""),
        ("(a)-[*]->(b)", ""),
        ("(a)-[*]-(b)", ""),
        ("(a)-[*1..2]-(b {id: 3})", "MATCH (b {id: 3}) "),
    ] {
        let single = count(&format!(
            "{prefix}MATCH p = shortestPath({endpoints}) RETURN p"
        ));
        let all = count(&format!(
            "{prefix}MATCH p = allShortestPaths({endpoints}) RETURN p"
        ));
        assert!(all > single, "{endpoints}: all={all} single={single}");
        for gql in ["ANY SHORTEST", "SHORTEST 1"] {
            let q = format!("{prefix}MATCH p = {gql} {endpoints} RETURN p");
            assert_eq!(count(&q), single, "{q}");
        }
        for gql in ["SHORTEST 1 GROUP", "ALL SHORTEST"] {
            let q = format!("{prefix}MATCH p = {gql} {endpoints} RETURN p");
            assert_eq!(count(&q), all, "{q}");
        }
    }
}

#[test]
fn unbounded_var_length_is_not_capped() {
    let g = chain40();
    let q = "MATCH (a {id: 0}), (b {id: 40}) MATCH p = shortestPath((a)-[*]->(b)) \
             RETURN length(p)";
    assert_eq!(on_route(&g, q), expect(&[&[Int(40)]]));
    let q = "MATCH (a {id: 0}) MATCH p = shortestPath((a)-[*]->(b {id: 40})) \
             RETURN length(p)";
    assert_eq!(on_route(&g, q), expect(&[&[Int(40)]]));
}

#[test]
fn bound_end_runs_one_search_not_one_per_start_candidate() {
    // 50 :S starts feed a 10-node chain that ends at the bound :T node.
    let mut labels: Vec<&[&str]> = vec![&["S"]; 50];
    labels.extend([&[] as &[&str]; 10]);
    labels.push(&["T"]);
    let mut rels: Vec<(usize, usize, &str)> = (0..50).map(|i| (i, 50, "R")).collect();
    rels.extend((50..60).map(|i| (i, i + 1, "R")));
    let g = build(&labels, &rels).graph;
    let q = "MATCH (b:T) MATCH p = shortestPath((a:S)-[*]->(b)) RETURN length(p)";
    let (rows, counts) = run(&g, q);
    assert_eq!(rows.len(), 50);
    assert!(rows.iter().all(|r| r[0] == Int(11)));
    assert_eq!(counts.route_runs, 1);
    assert!(counts.expands > 0);
    assert!(
        counts.expands < 3 * rels.len() as u64,
        "expands={} for {} relationships",
        counts.expands,
        rels.len()
    );
}

#[test]
fn forced_gpm_switch_bypasses_the_route() {
    let g = shortcut();
    let q = "MATCH p = allShortestPaths((a)-[*]->(b)) \
             RETURN a.id, b.id, [r IN relationships(p) | r.k]";
    let route = on_route(&g, q);
    let (forced, counts) = with_forced_gpm(|| run(&g, q));
    assert_eq!(counts, Counts::default());
    assert_eq!(route, sorted(forced));
}

/// A `WHERE` on a `shortestPath` pattern selects the shortest path that
/// **satisfies** it. The 0→4→3 shortcut is the shortest path through the
/// graph, but the predicate excludes node 4, so the route must answer the
/// longer 0→1→2→3 path instead of zero rows.
#[test]
fn where_predicate_selects_the_shortest_satisfying_path() {
    let g = shortcut();
    let q = "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 3})) \
             WHERE NONE(n IN nodes(p) WHERE n.id = 4) \
             RETURN [n IN nodes(p) | n.id]";
    assert_eq!(on_route(&g, q), expect(&[&[ints(&[0, 1, 2, 3])]]));
}

/// The all-shortest selector keeps every satisfying shortest trail: both
/// 0→1→3 and 0→2→3 are length 2, and the predicate drops the second.
#[test]
fn where_predicate_filters_each_all_shortest_trail() {
    let rels = [(0, 1), (1, 3), (0, 2), (2, 3)].map(|(a, b)| (a, b, "R"));
    let g = build(&[&[], &[], &[], &[]], &rels).graph;
    let q = "MATCH p = allShortestPaths((a {id: 0})-[*]->(b {id: 3})) \
             WHERE NONE(n IN nodes(p) WHERE n.id = 2) \
             RETURN [n IN nodes(p) | n.id]";
    assert_eq!(on_route(&g, q), expect(&[&[ints(&[0, 1, 3])]]));
}

/// A predicate no path satisfies yields no rows instead of the unfiltered
/// shortest path.
#[test]
fn where_predicate_that_no_path_satisfies_yields_nothing() {
    let g = shortcut();
    let q = "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 3})) \
             WHERE length(p) > 10 \
             RETURN length(p)";
    assert!(on_route(&g, q).is_empty());
}

/// The zero-length path is a trail like any other to the predicate-filtered
/// walk: `[*0..]` from a node to itself must still answer length 0 when the
/// predicate accepts it.
#[test]
fn where_predicate_on_a_closed_path_keeps_the_zero_length_trail() {
    let g = shortcut();
    let q = "MATCH p = shortestPath((a {id: 0})-[*0..3]->(a)) \
             WHERE length(p) <= 3 RETURN length(p)";
    let (rows, counts) = run(&g, q);
    assert_eq!(rows, vec![vec![Int(0)]]);
    assert_eq!(
        counts.route_runs, 1,
        "the route declined a decidable filter"
    );
    assert_eq!(gpm(&g, q), expect(&[&[Int(0)]]));
}

/// 0→1→…→40, a forty-relationship chain — past the classic walk's
/// `DEFAULT_MAX_HOPS` (32), so a filtered walk that re-clamps its length
/// loop at that default cannot reach the far end.
fn chain40() -> Graph {
    let rels: Vec<(usize, usize)> = (0..40).map(|i| (i, i + 1)).collect();
    plain(41, &rels)
}

/// An explicitly bounded pattern keeps its written bound in the
/// predicate-filtered walk: `*1..40` must walk to the 40-hop trail its
/// predicate selects, not silently clamp at the engine's default cap —
/// and the route and the forced GPM enumeration answer the same rows.
#[test]
fn filtered_walk_keeps_an_explicit_bound() {
    let g = chain40();
    let q = "MATCH p = shortestPath((a {id: 0})-[:R*1..40]->(b {id: 40})) \
             WHERE length(p) = 40 \
             RETURN length(p)";
    assert_eq!(on_route(&g, q), expect(&[&[Int(40)]]));
    assert_eq!(gpm(&g, q), expect(&[&[Int(40)]]));
}

/// An unbounded pattern's filtered walk reads the GPM engine's one
/// unbounded ceiling — the relationship count `unbounded_dfs_cap` takes
/// under DIFFERENT RELATIONSHIPS — the same depth the enumeration
/// fallback explores to, so the route answers past the classic default
/// cap exactly what GPM answers.
#[test]
fn filtered_unbounded_walk_reads_the_gpm_unbounded_cap() {
    let g = chain40();
    let q = "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 40})) \
             WHERE length(p) = 40 \
             RETURN length(p)";
    assert_eq!(on_route(&g, q), expect(&[&[Int(40)]]));
    assert_eq!(gpm(&g, q), expect(&[&[Int(40)]]));
}

/// A path-reading `WHERE` conjunct that also names a variable a *later*
/// pattern in the same `MATCH` binds cannot be decided while the search runs:
/// the route declines it to enumeration — an explicit outcome, never a silent
/// "satisfied" — and the rows are exactly the GPM route's.
#[test]
fn where_conjunct_reading_a_later_pattern_name_declines_the_route() {
    let g = shortcut();
    let q = "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 3})), (x {id: 2}) \
             WHERE length(p) = x.id RETURN length(p)";
    let (rows, counts) = run(&g, q);
    assert_eq!(
        counts.route_runs, 0,
        "the route chose a trail its predicate could not decide"
    );
    assert_eq!(rows, vec![vec![Int(2)]], "the enumeration stopped matching");
    assert_eq!(sorted(rows), gpm(&g, q));
}

/// The same predicate over variables the route binds itself is decidable, so
/// the route keeps the search: one that rejects the shortcut must answer the
/// longer trail that satisfies it.
#[test]
fn path_where_over_own_variables_keeps_the_route() {
    let g = shortcut();
    let q = "MATCH p = ALL SHORTEST ((a {id: 0})-[*]->(b {id: 3}) \
             WHERE NONE(n IN nodes(p) WHERE n.id = 4)) \
             RETURN [n IN nodes(p) | n.id]";
    assert_eq!(
        on_route(&g, q),
        expect(&[&[ints(&[0, 1, 2, 3])]]),
        "the route must choose the shortest trail the predicate accepts"
    );
}

/// An `EXISTS` subquery names its own `z`, which the route can never find
/// bound in the row: the readiness check must not take that subquery-local
/// name for an outer variable and skip the predicate, which answered zero
/// rows (the unfiltered 0→4→3 was chosen, then rejected afterwards)
/// instead of the length-3 path the predicate selects.
#[test]
fn where_predicate_with_subquery_local_variable_still_filters() {
    let rels = [(0, 1), (1, 2), (2, 3), (0, 4), (4, 3)].map(|(a, b)| (a, b, "R"));
    let g = build(&[&["A"], &[], &[], &[], &[]], &rels).graph;
    let q = "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 3})) \
             WHERE EXISTS { MATCH (z:A) WHERE length(p) = 3 } \
             RETURN [n IN nodes(p) | n.id]";
    assert_eq!(on_route(&g, q), expect(&[&[ints(&[0, 1, 2, 3])]]));
}

/// `ALL SHORTEST` stops at the shortest SATISFYING length: when a longer
/// trail also satisfies the predicate, only the shortest one is returned
/// (the satisfied flag must accumulate across the walk even under
/// `shape.all`, else the search walks past the shortest satisfying length).
#[test]
fn all_shortest_stops_at_the_shortest_satisfying_length() {
    // 0→1→3 (length 2) and 0→2→4→3 (length 3); both satisfy the predicate.
    let rels = [(0, 1), (1, 3), (0, 2), (2, 4), (4, 3)].map(|(a, b)| (a, b, "R"));
    let g = build(&[&[], &[], &[], &[], &[]], &rels).graph;
    let q = "MATCH p = allShortestPaths((a {id: 0})-[*]->(b {id: 3})) \
             WHERE NONE(n IN nodes(p) WHERE n.id = 99) \
             RETURN [n IN nodes(p) | n.id]";
    assert_eq!(on_route(&g, q), expect(&[&[ints(&[0, 1, 3])]]));
}
/// Property-map endpoints, both sides: the pattern is not row-bound, but
/// each endpoint's map is a bounded seek — the pair search over the
/// candidate product, not the one-sided fan. Pinned two ways: the answer
/// matches the row-bound form, and the expansions match it too — the fan
/// would expand the full-depth ball from the start, the pair search meets
/// in the middle.
#[test]
fn both_endpoints_by_property_map_take_the_pair_search() {
    let g = shortcut();
    let bound = "MATCH (a {id: 0}), (b {id: 3}) \
                 MATCH p = shortestPath((a)-[*]->(b)) RETURN [n IN nodes(p) | n.id]";
    let seek = "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 3})) \
                RETURN [n IN nodes(p) | n.id]";
    let (rows, seek_counts) = run(&g, seek);
    assert_eq!(sorted(rows), expect(&[&[ints(&[0, 4, 3])]]));
    let (_, bound_counts) = run(&g, bound);
    assert!(seek_counts.route_runs > 0);
    assert_eq!(
        seek_counts.expands, bound_counts.expands,
        "the property-endpoint form must take the pair search, not the fan"
    );
}

/// Several end candidates match one map (a non-unique property): one
/// partition per candidate with a path, the product over both sides.
#[test]
fn property_map_seeking_several_end_candidates() {
    let b = Builder::new();
    let nodes: Vec<_> = (0..5)
        .map(|i| {
            let id = i.to_string();
            if i == 2 || i == 4 {
                b.prop_node(&id, &[], &[("tag", Value::String("x".into()))])
            } else {
                b.node(&id, &[])
            }
        })
        .collect();
    for (a, z) in [(0, 1), (1, 2), (3, 2), (0, 4), (4, 2)] {
        b.rel(nodes[a], nodes[z], "R", &[]);
    }
    let g = b.done();
    let q = "MATCH p = shortestPath((a {id: '0'})-[*..3]->(b {tag: 'x'})) \
             RETURN b.id, length(p)";
    assert_eq!(
        on_route(&g, q),
        expect(&[
            &[Value::String("2".into()), Int(2)],
            &[Value::String("4".into()), Int(1)],
        ])
    );
}

/// A bare start with a property-seekable end whose list is bounded: the
/// end side drives — one backward fan from the single resolved end, not a
/// forward ball per start. Pinned by the expansion count: each of the
/// three starts has twenty dead-end neighbours, so the forward fans would
/// follow over sixty relationships, while the backward fan follows only
/// the six that lead into the end.
#[test]
fn fan_from_bounded_seekable_end_when_start_is_bare() {
    let b = Builder::new();
    let t = b.prop_node("t", &[], &[("tag", Value::String("x".into()))]);
    for i in 0..3 {
        let s = b.node(&format!("s{i}"), &["S"]);
        let m = b.node(&format!("m{i}"), &[]);
        b.rel(s, m, "R", &[]);
        b.rel(m, t, "R", &[]);
        for j in 0..20 {
            let dead = b.node(&format!("d{i}_{j}"), &[]);
            b.rel(s, dead, "R", &[]);
        }
    }
    let g = b.done();
    let q = "MATCH p = shortestPath((a:S)-[*]->(b {tag: 'x'})) RETURN a.id, length(p)";
    let (rows, counts) = run(&g, q);
    assert_eq!(sorted(rows), sorted(gpm(&g, q)));
    assert!(counts.route_runs > 0);
    assert!(
        counts.expands < 20,
        "the bounded end must drive a backward fan, not a forward ball per start: {} expansions",
        counts.expands
    );
}

/// An end map whose property matches nothing: no candidates, no rows —
/// the route still ran (the pair product over an empty end list).
#[test]
fn property_map_seeking_to_no_candidate() {
    let g = shortcut();
    let q = "MATCH p = shortestPath((a {id: 0})-[*]->(b {id: 99})) RETURN length(p)";
    let (rows, counts) = run(&g, q);
    assert!(counts.route_runs > 0);
    assert_eq!(sorted(rows), Vec::<Vec<Value>>::new());
}

/// An `EXISTS` probe riding the route (the selector spelling — the
/// `shortestPath()` function form never carries a probe) answers from the
/// rows as they are bound and halts at the first accept: true when some
/// shortest path exists, false when none does, and the route ran for both.
#[test]
fn exists_probe_answers_from_the_streamed_rows() {
    let g = shortcut();
    let hit = "RETURN EXISTS { MATCH p = ANY SHORTEST (a {id: 0})-[*]->(b {id: 3}) } AS found";
    let (rows, counts) = run(&g, hit);
    assert!(counts.route_runs > 0, "the probe must ride the route");
    assert_eq!(sorted(rows), expect(&[&[Bool(true)]]));
    assert_eq!(on_route(&g, hit), gpm(&g, hit));

    let miss = "RETURN EXISTS { MATCH p = ANY SHORTEST (a {id: 0})-[*]->(b {id: 99}) } AS found";
    let (rows, counts) = run(&g, miss);
    assert!(counts.route_runs > 0, "the probe must ride the route");
    assert_eq!(sorted(rows), expect(&[&[Bool(false)]]));
}

/// A probe over a path-filtered all-shortest pattern stops inside the
/// filtered trail walk: the first satisfying trail halts the enumeration
/// of the rest of the level (the verdict is the same as the unfiltered
/// pattern's, the walk just does not keep going).
#[test]
fn exists_probe_halts_the_filtered_trail_walk() {
    let g = shortcut();
    let q = "RETURN EXISTS { MATCH p = ALL SHORTEST (a {id: 0})-[*]->(b {id: 3}) \
             WHERE length(p) > 0 } AS found";
    let (rows, counts) = run(&g, q);
    assert!(counts.route_runs > 0, "the probe must ride the route");
    assert_eq!(sorted(rows), expect(&[&[Bool(true)]]));
}

/// A halted probe stops the search itself (ADR-0555): the fan emits each
/// layer's partitions as the layer completes, so an EXISTS probe that the
/// first layer answers never expands the deeper layers. The same pattern
/// counted in full walks the whole ball, so the probe follows strictly
/// fewer relationships.
#[test]
fn an_exists_probe_stops_the_fan_after_its_first_answering_layer() {
    let b = Builder::new();
    // 0 -> 1..=5, and each of 1..=5 fans out to 20 leaves: a ball whose
    // second layer is far larger than its first.
    let root = b.node("0", &["N"]);
    for i in 1..=5 {
        let mid = b.node(&i.to_string(), &["N"]);
        b.rel(root, mid, "R", &[]);
        for j in 0..20 {
            let leaf = b.node(&format!("{i}_{j}"), &["N"]);
            b.rel(mid, leaf, "R", &[]);
        }
    }
    let g = b.done();
    // One pattern, two wrappers: the runs drive the identical search, so
    // their expansion counts are comparable.
    let pattern = "p = ANY SHORTEST (a {id: '0'})-[:R*..3]->(b)";
    let probe = format!("RETURN EXISTS {{ MATCH {pattern} }} AS found");
    let full = format!("MATCH {pattern} RETURN count(*) AS n");
    let (rows, probe_counts) = run(&g, &probe);
    assert!(probe_counts.route_runs > 0, "the probe must ride the route");
    assert_eq!(sorted(rows), vec![vec![Value::Bool(true)]]);
    let (_, full_counts) = run(&g, &full);
    assert!(
        probe_counts.expands < full_counts.expands,
        "the probe must not expand the deeper layers: {} vs {}",
        probe_counts.expands,
        full_counts.expands
    );
}

/// The routed `allShortestPaths` under a `LIMIT` charges the MATCH cap on
/// the rows it pushes, so the caller's input-row loop stops once the cap
/// is full. Before the cap was charged the route ran once per input row
/// whatever the `LIMIT` asked for, which is how a ten-row answer over a
/// 30k-pair input enumerated every pair — the shape that hit the
/// expression-memory limit (LABS-1301).
#[test]
fn all_shortest_paths_limit_stops_the_input_row_loop() {
    // Twenty `:A` starts, each one hop from the single `:B`, so every input
    // row has a shortest path and the eager run drives the route 20 times.
    let g = fan_in(20);

    let q = "MATCH (a:A) MATCH p = allShortestPaths((a)-[:R*]-(b:B)) \
             RETURN p LIMIT 1";
    let (rows, counts) = run(&g, q);
    assert_eq!(rows.len(), 1, "the LIMIT answer is one row");
    assert_eq!(
        counts.route_runs, 1,
        "the cap must stop the loop after the first row: {} runs",
        counts.route_runs
    );

    // The eager run is the oracle for the work the cap avoids.
    let (eager_rows, eager) = run(
        &g,
        "MATCH (a:A) MATCH p = allShortestPaths((a)-[:R*]-(b:B)) RETURN p",
    );
    assert_eq!(eager_rows.len(), 20);
    assert_eq!(eager.route_runs, 20, "one route run per input row");
}

/// A `DISTINCT` cap evaluates its RETURN expressions on the row it counts,
/// so the cap has to be charged at `finish_row` — the pre-projection seam
/// every other tail uses — and never on the projected rows the sink
/// stores: `b.id` is gone from a row that already carries only `id`, and
/// charging after the projection turns a valid query into
/// `UndefinedVariable: variable 'b' not defined` (LABS-1301 review).
#[test]
fn distinct_limit_over_a_routed_shortest_path_charges_the_pre_projection_row() {
    let g = fan_in(20);
    let (rows, counts) = run(
        &g,
        "MATCH (a:A) MATCH p = allShortestPaths((a)-[:R*]-(b:B)) \
         RETURN DISTINCT b.id AS id LIMIT 1",
    );
    assert_eq!(rows.len(), 1);
    assert!(counts.route_runs > 0, "the pattern must ride the route");
    assert_eq!(
        counts.route_runs, 1,
        "the distinct cap fills on the first row: {} runs",
        counts.route_runs
    );
}

/// The collecting sink's halt contract: once the cap is full the route's
/// own search stops, so a `LIMIT` that one input row already satisfies
/// does not enumerate and store that row's remaining paths (LABS-1301
/// review). The observable is the search's expansion count.
#[test]
fn a_full_cap_stops_the_route_search_mid_row() {
    // One `:A` start fanning through `k` `:M` middles to a single `:B`, so
    // the single input row has `k` equal-length shortest paths.
    let a: &[&str] = &["A"];
    let m: &[&str] = &["M"];
    let b: &[&str] = &["B"];
    let k = 12usize;
    let mut labels = vec![a];
    labels.extend(std::iter::repeat_n(m, k));
    labels.push(b);
    let hub = k + 1;
    let mut rels: Vec<(usize, usize, &str)> = Vec::new();
    for i in 1..=k {
        rels.push((0, i, "R"));
        rels.push((i, hub, "R"));
    }
    let g = build(&labels, &rels).graph;

    let q = |limit: &str| {
        format!("MATCH (a:A) MATCH p = allShortestPaths((a)-[:R*]-(b:B)) RETURN p{limit}")
    };
    let (limited, counts) = run(&g, &q(" LIMIT 1"));
    let (full, full_counts) = run(&g, &q(""));
    assert_eq!(limited.len(), 1);
    assert_eq!(full.len(), k);
    assert!(counts.route_runs > 0, "the pattern must ride the route");
    assert!(
        counts.expands < full_counts.expands,
        "a full cap must stop the search: {} expands vs eager {}",
        counts.expands,
        full_counts.expands
    );
}
