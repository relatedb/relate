//! Mid-path allReduce pruning over quantified path patterns: the exactness
//! tests (differential against the post-filter evaluation on EV-shaped
//! road graphs, including cyclic ones), the PROFILE hop counter comparison
//! (with the >= 5x hop-ratio pin of the node's acceptance bar), the decline
//! rules (a predicate or initial naming a later binding stays a
//! post-filter), the null-prefix exactness (a null poisons the fold, never
//! prunes — a later raising element still errors), and the timing ratio.

use std::time::Instant;

use relate_core::Value;

use super::qpp::with_allreduce_pruning_disabled;
use super::qpp_hops;
use crate::exec::test_support::{exec_cypher, try_exec_cypher, Builder};
use crate::graph::Graph;

/// The pruned route query: the allReduce rides the MATCH WHERE, so the
/// quantified expansion advances it per hop and prunes mid-path.
fn pruned_query(start: &str, end: &str, max: u32, budget: &str) -> String {
    route_query(
        start,
        end,
        max,
        &format!("WHERE allReduce(acc = 0.0, r IN rels | acc + r.distance_km, acc < {budget})"),
    )
}

/// The same route query as a post-filter: the allReduce rides the WITH
/// WHERE, evaluated once per complete path. Semantically identical to
/// [`pruned_query`] — the differential and timing tests compare the two.
fn postfilter_query(start: &str, end: &str, max: u32, budget: &str) -> String {
    route_query(
        start,
        end,
        max,
        &format!("WITH p, rels WHERE allReduce(acc = 0.0, r IN rels | acc + r.distance_km, acc < {budget})"),
    )
}

/// The shared body of the two route-query spellings; `filter` is either the
/// MATCH WHERE (inlined, pruned) or a WITH WHERE over the matched rows
/// (never inlined, the post-filter reference).
fn route_query(start: &str, end: &str, max: u32, filter: &str) -> String {
    format!(
        "MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'{start}'}}) \
         (()-[rels:ROAD|CHARGE]-(x:Geo)){{1,{max}}} (b:Geo {{id:'{end}'}}) \
         {filter} \
         RETURN length(p) AS len, reduce(s = 0.0, r IN rels | s + r.distance_km) AS cost \
         ORDER BY len, cost"
    )
}

/// The rows of the `s`-to-`t` ROAD route query under `filter`, as the two
/// pruning-comparison tests read them: the `{1,3}` quantified walk returning
/// each route's length.
fn rows(graph: &Graph, filter: &str) -> relate_core::Result<Vec<Vec<Value>>> {
    try_exec_cypher(
        graph,
        &format!(
            "MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'s'}}) \
             (()-[rels:ROAD]-(x:Geo)){{1,3}} (b:Geo {{id:'t'}}) \
             {filter} \
             RETURN length(p) AS len",
        ),
    )
    .map(|result| result.rows)
}

/// The zero-distance road is the only way out of `s`: `s -> m -> t` with a
/// `0.0` first leg and a `100.0` second.
fn zero_leg_graph() -> Graph {
    let b = Builder::new();
    let s = b.node("s", &["Geo"]);
    let m = b.node("m", &["Geo"]);
    let t = b.node("t", &["Geo"]);
    b.rel(s, m, "ROAD", &[("distance_km", Value::Float(0.0))]);
    b.rel(m, t, "ROAD", &[("distance_km", Value::Float(100.0))]);
    b.done()
}

/// An acyclic EV-shaped road graph: a one-way line Paris → Lyon → Orange →
/// Marseille with a Valence leg, and a charge self-loop at Valence.
fn ev_line_graph() -> Graph {
    let b = Builder::new();
    let paris = b.node("Paris", &["Geo"]);
    let lyon = b.node("Lyon", &["Geo"]);
    let valence = b.node("Valence", &["Geo"]);
    let orange = b.node("Orange", &["Geo"]);
    let marseille = b.node("Marseille", &["Geo"]);
    b.rel(paris, lyon, "ROAD", &[("distance_km", Value::Float(465.0))]);
    b.rel(
        lyon,
        valence,
        "ROAD",
        &[("distance_km", Value::Float(230.0))],
    );
    b.rel(
        valence,
        orange,
        "ROAD",
        &[("distance_km", Value::Float(110.0))],
    );
    b.rel(
        orange,
        marseille,
        "ROAD",
        &[("distance_km", Value::Float(200.0))],
    );
    b.rel(
        lyon,
        orange,
        "ROAD",
        &[("distance_km", Value::Float(310.0))],
    );
    b.rel(
        valence,
        valence,
        "CHARGE",
        &[("distance_km", Value::Float(0.0))],
    );
    b.done()
}

/// A cyclic EV-shaped road graph: an 8-city ring (100 km legs) with a
/// 250 km chord across each triple, plus two charge self-loops. The cycles
/// make the unpruned walk fan out combinatorially within the hop bound.
pub(super) fn ev_ring_graph() -> Graph {
    let b = Builder::new();
    let cities: Vec<_> = (0..8).map(|i| b.node(&format!("c{i}"), &["Geo"])).collect();
    for i in 0..8 {
        b.rel(
            cities[i],
            cities[(i + 1) % 8],
            "ROAD",
            &[("distance_km", Value::Float(100.0))],
        );
        b.rel(
            cities[i],
            cities[(i + 3) % 8],
            "ROAD",
            &[("distance_km", Value::Float(250.0))],
        );
    }
    b.rel(
        cities[2],
        cities[2],
        "CHARGE",
        &[("distance_km", Value::Float(20.0))],
    );
    b.rel(
        cities[6],
        cities[6],
        "CHARGE",
        &[("distance_km", Value::Float(20.0))],
    );
    b.done()
}

/// A second cyclic shape: a 6-city ring with both directions of every leg
/// as separate roads, without charge stops.
fn ev_bidirectional_graph() -> Graph {
    let b = Builder::new();
    let cities: Vec<_> = (0..6).map(|i| b.node(&format!("n{i}"), &["Geo"])).collect();
    for i in 0..6 {
        let j = (i + 1) % 6;
        let d = Value::Float(80.0 + 40.0 * (i % 3) as f64);
        b.rel(cities[i], cities[j], "ROAD", &[("distance_km", d.clone())]);
        b.rel(cities[j], cities[i], "ROAD", &[("distance_km", d)]);
    }
    b.done()
}

/// The PROFILE hop counter shows the pruning: on the same cyclic route
/// query, the inlined (MATCH-WHERE) run expands strictly fewer quantified
/// hops than the post-filter (WITH-WHERE) run and the pruning-disabled
/// run of the very same statement — which expand identically.
#[test]
fn profile_hop_counter_shows_the_pruned_walk_expanding_fewer_hops() {
    let graph = ev_ring_graph();
    let query = pruned_query("c0", "c5", 8, "600");
    let postfilter = postfilter_query("c0", "c5", 8, "600");

    let pruned = qpp_hops(&graph, &query);
    let post = qpp_hops(&graph, &postfilter);
    let disabled = with_allreduce_pruning_disabled(|| qpp_hops(&graph, &query));

    eprintln!("HOPS pruned={pruned} post={post} disabled={disabled}");
    assert!(pruned < post, "pruned {pruned} hops vs post-filter {post}");
    assert!(pruned < disabled, "pruned {pruned} vs disabled {disabled}");
    assert_eq!(post, disabled, "the post-filter form never inlines");
    // The node's acceptance bar — the pruned route query at >= 5x the
    // post-filter form — pinned by the deterministic hop ratio (stable
    // where wall-time ratios are not), alongside the timing test's
    // looser >= 3x time assertion for CI stability.
    let hop_ratio = post as f64 / pruned as f64;
    assert!(
        hop_ratio >= 5.0,
        "post-filter {post} hops vs pruned {pruned} hops \
         (ratio {hop_ratio:.1}x, expected >= 5x)"
    );
}

/// Differential: the pruned evaluation answers exactly what the post-filter
/// evaluation answers, on three EV-shaped graphs including two cyclic ones.
/// The pruning-disabled run of the very same statement is the third leg.
#[test]
fn pruned_allreduce_answers_match_the_postfilter_on_three_graphs() {
    let cases = [
        (ev_line_graph(), "Paris", "Marseille", 6u32, "1000"),
        (ev_ring_graph(), "c0", "c5", 8, "600"),
        (ev_bidirectional_graph(), "n0", "n3", 6, "400"),
    ];
    for (graph, start, end, max, budget) in cases {
        let query = pruned_query(start, end, max, budget);
        let postfilter = postfilter_query(start, end, max, budget);
        let pruned_rows = exec_cypher(&graph, &query).rows;
        assert!(
            !pruned_rows.is_empty(),
            "the {start}->{end} case must have surviving routes"
        );
        assert_eq!(
            pruned_rows,
            exec_cypher(&graph, &postfilter).rows,
            "pruned vs post-filter on {start}->{end}"
        );
        assert_eq!(
            pruned_rows,
            with_allreduce_pruning_disabled(|| exec_cypher(&graph, &query)).rows,
            "pruned vs pruning-disabled on {start}->{end}"
        );
    }
}

/// The pruning is exact for a non-monotone predicate: `acc % 7 <> 3` fails
/// on some prefixes and passes on later ones — allReduce is false either
/// way, and the pruned run drops exactly the routes the post-filter drops.
#[test]
fn non_monotone_predicates_prune_exactly() {
    let b = Builder::new();
    let s = b.node("s", &["Geo"]);
    let m1 = b.node("m1", &["Geo"]);
    let m2 = b.node("m2", &["Geo"]);
    let t = b.node("t", &["Geo"]);
    // Every leg out of s lands the prefix sum on 3 mod 7 at hop 1; the
    // predicate then passes again at hop 2 — a failure no later prefix
    // can undo, so every route is dropped and the empty answers agree.
    b.rel(s, m1, "ROAD", &[("distance_km", Value::Int(3))]);
    b.rel(m1, t, "ROAD", &[("distance_km", Value::Int(5))]);
    b.rel(s, m2, "ROAD", &[("distance_km", Value::Int(10))]);
    b.rel(m2, t, "ROAD", &[("distance_km", Value::Int(3))]);
    let graph = b.done();
    let body = |filter: &str| {
        format!(
            "MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'s'}}) \
             (()-[rels:ROAD]-(x:Geo)){{1,4}} (b:Geo {{id:'t'}}) \
             {filter} \
             RETURN length(p) AS len ORDER BY len",
        )
    };
    let predicate = "allReduce(acc = 0, r IN rels | acc + r.distance_km, acc % 7 <> 3)";
    let pruned = exec_cypher(&graph, &body(&format!("WHERE {predicate}"))).rows;
    assert!(pruned.is_empty(), "every route fails a prefix: {pruned:?}");
    assert_eq!(
        pruned,
        with_allreduce_pruning_disabled(|| {
            exec_cypher(&graph, &body(&format!("WHERE {predicate}"))).rows
        })
    );
    assert_eq!(
        pruned,
        exec_cypher(&graph, &body(&format!("WITH p, rels WHERE {predicate}"))).rows
    );
}

/// A null distance makes every later prefix null: a null verdict is not
/// deciding (the list evaluator scans past it, and a later element can
/// still raise), so the fold is poisoned — the branch runs unpruned and
/// the post-filter drops the completed row on the null verdict. The same
/// routes survive either way.
#[test]
fn null_distances_answer_exactly() {
    let b = Builder::new();
    let s = b.node("s", &["Geo"]);
    let m = b.node("m", &["Geo"]);
    let t = b.node("t", &["Geo"]);
    b.rel(s, m, "ROAD", &[("distance_km", Value::Float(100.0))]);
    b.rel(m, t, "ROAD", &[("distance_km", Value::Null)]);
    b.rel(s, t, "ROAD", &[("distance_km", Value::Float(300.0))]);
    let graph = b.done();
    let body = |filter: &str| {
        format!(
            "MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'s'}}) \
             (()-[rels:ROAD]-(x:Geo)){{1,3}} (b:Geo {{id:'t'}}) \
             {filter} \
             RETURN length(p) AS len ORDER BY len",
        )
    };
    let predicate = "allReduce(acc = 0.0, r IN rels | acc + r.distance_km, acc < 500)";
    let pruned = exec_cypher(&graph, &body(&format!("WHERE {predicate}"))).rows;
    assert_eq!(pruned, vec![vec![Value::Int(1)]], "only the direct road");
    assert_eq!(
        pruned,
        with_allreduce_pruning_disabled(|| {
            exec_cypher(&graph, &body(&format!("WHERE {predicate}"))).rows
        })
    );
    assert_eq!(
        pruned,
        exec_cypher(&graph, &body(&format!("WITH p, rels WHERE {predicate}"))).rows
    );
}

/// A non-finite accumulator is not an error: float division by zero yields
/// Infinity, the predicate turns false, and no route survives — pruned and
/// unpruned runs answer identically. A dead-end branch with the same
/// zero-distance road surfaces no error either.
#[test]
fn non_finite_accumulator_matches_pruning_disabled() {
    let predicate = "allReduce(acc = 0.0, r IN rels | acc + 100 / r.distance_km, acc < 500)";

    // The zero-distance road is the only way out of s: `100 / 0.0` is
    // Infinity, so the predicate is false on every route and no row
    // survives — the same answer with and without pruning.
    let through = zero_leg_graph();
    let pruned = rows(&through, &format!("WHERE {predicate}")).unwrap();
    assert!(pruned.is_empty());
    assert_eq!(
        pruned,
        with_allreduce_pruning_disabled(|| {
            rows(&through, &format!("WHERE {predicate}")).unwrap()
        })
    );

    // The zero-distance road now leads to a dead end that yields no rows:
    // the branch is never post-filtered, so neither run raises.
    let b = Builder::new();
    let s = b.node("s", &["Geo"]);
    let d = b.node("d", &["Geo"]);
    let m = b.node("m", &["Geo"]);
    let t = b.node("t", &["Geo"]);
    b.rel(s, d, "ROAD", &[("distance_km", Value::Float(0.0))]);
    b.rel(s, m, "ROAD", &[("distance_km", Value::Float(100.0))]);
    b.rel(m, t, "ROAD", &[("distance_km", Value::Float(100.0))]);
    let dead_end = b.done();
    let expected = rows(&dead_end, &format!("WHERE {predicate}")).unwrap();
    assert_eq!(expected, vec![vec![Value::Int(2)]]);
    assert_eq!(
        expected,
        with_allreduce_pruning_disabled(|| {
            rows(&dead_end, &format!("WHERE {predicate}")).unwrap()
        })
    );
}

/// An update that genuinely raises (integer division by zero — only the
/// all-integer `/` raises) poisons the fold identically under pruning and
/// with the pruning stood down: the error text is the same in both runs.
#[test]
fn raising_updates_surface_identically() {
    let predicate =
        "allReduce(acc = 0, r IN rels | acc + 100 / toInteger(r.distance_km), acc < 500)";

    // The zero-distance road is the only way out of s: every surviving
    // route's evaluation raises, pruned or not.
    let through = zero_leg_graph();
    let pruned_err = rows(&through, &format!("WHERE {predicate}")).unwrap_err();
    assert!(pruned_err.to_string().contains("/ by zero"), "{pruned_err}");

    assert_eq!(
        pruned_err.to_string(),
        with_allreduce_pruning_disabled(|| {
            rows(&through, &format!("WHERE {predicate}")).unwrap_err()
        })
        .to_string()
    );
}

/// Review round 1, the CRITICAL exactness hole: when the allReduce's
/// `ident` (or accumulator) aliases a CIP-28 non-local singleton — a name
/// referenced inside the quantification and declared later in the pattern,
/// pre-bound to null on the walk's base row (ADR-0335) — the `initial`
/// reads null mid-path but the pattern's final binding on the post-filter's
/// completed row. Such a conjunct must be declined, never inlined: the
/// pruned, pruning-disabled, and WITH-WHERE post-filter runs answer
/// identically, and the hop counter shows the pruned statement expanding
/// exactly like its pruning-disabled self — the conjunct never pruned.
#[test]
fn an_initial_aliasing_a_later_binding_is_declined() {
    let b = Builder::new();
    let s = b.node("s", &["Geo"]);
    let m1 = b.node("m1", &["Geo"]);
    let m2 = b.node("m2", &["Geo"]);
    let t = b.node("t", &["Geo"]);
    b.rel(s, m1, "ROAD", &[("p", Value::Int(1))]);
    b.rel(s, m2, "ROAD", &[("p", Value::Int(1))]);
    b.rel(m1, m2, "ROAD", &[("p", Value::Int(1))]);
    b.rel(m2, t, "ROAD", &[("p", Value::Int(1))]);
    let graph = b.done();
    let body = |filter: &str| {
        format!(
            "MATCH (a:Geo {{id:'s'}}) ((y:Geo)-[r:ROAD]-(z:Geo WHERE b IS NULL)){{1,2}} \
             (b:Geo {{id:'t'}}) \
             {filter} \
             RETURN a.id AS aid, b.id AS bid",
        )
    };
    // The `initial` (`size(b.id)`) references the ident `b`, which aliases
    // the pattern's end node: pre-fix this was inlined and the mid-path
    // null `initial` silently dropped the surviving route.
    let predicate = "allReduce(total = size(b.id), b IN r | total + b.p, total > 0)";
    let pruned = body(&format!("WHERE {predicate}"));
    let postfilter = body(&format!("WITH a, b, r WHERE {predicate}"));
    let expected = vec![vec![Value::String("s".into()), Value::String("t".into())]];
    assert_eq!(exec_cypher(&graph, &pruned).rows, expected, "pruned");
    assert_eq!(
        with_allreduce_pruning_disabled(|| exec_cypher(&graph, &pruned).rows),
        expected,
        "pruning-disabled"
    );
    assert_eq!(
        exec_cypher(&graph, &postfilter).rows,
        expected,
        "post-filter"
    );
    assert_eq!(
        qpp_hops(&graph, &pruned),
        with_allreduce_pruning_disabled(|| qpp_hops(&graph, &pruned)),
        "a declined conjunct expands identically with the pruning stood down"
    );
}

/// Review round 1, the HIGH error-behavior divergence: a null prefix
/// verdict is not deciding — `eval_all_reduce` scans past a null and a
/// later element can still raise, erroring the row. The pruned run must
/// error identically to the pruning-disabled run, not silently return
/// the rows that survived ahead of the raise (pre-fix it returned Ok([])).
/// (The WITH-WHERE spelling is deliberately not a leg here: that filter
/// context drops a row on error by pre-existing design — `project.rs`'s
/// `.ok()` retain — so it answers Ok([]) with or without the inliner.)
#[test]
fn a_null_prefix_then_a_raising_element_errors_identically() {
    let b = Builder::new();
    let s = b.node("s", &["Geo"]);
    let m = b.node("m", &["Geo"]);
    let n = b.node("n", &["Geo"]);
    let t = b.node("t", &["Geo"]);
    // The one s→t route folds distances "abc" (size 3, holds), null
    // (size null, a null prefix), then 5.0 (size raises).
    b.rel(
        s,
        m,
        "ROAD",
        &[("distance_km", Value::String("abc".into()))],
    );
    b.rel(m, n, "ROAD", &[("distance_km", Value::Null)]);
    b.rel(n, t, "ROAD", &[("distance_km", Value::Float(5.0))]);
    let graph = b.done();
    let predicate = "allReduce(acc = 0, r IN rels | acc + 1, size(r.distance_km) < 5)";
    let body = |filter: &str| {
        format!(
            "MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'s'}}) \
             (()-[rels:ROAD]-(x:Geo)){{1,3}} (b:Geo {{id:'t'}}) \
             {filter} \
             RETURN length(p) AS len",
        )
    };
    let run = |filter: &str| try_exec_cypher(&graph, &body(filter)).map(|result| result.rows);
    let pruned_err = run(&format!("WHERE {predicate}")).unwrap_err();
    assert!(
        // The stable identity: size()'s legacy class token plus the
        // shared sentence head — not the full accepted-set wording the
        // list builtins own (the neighbouring leg pins only its class
        // token the same way).
        pruned_err
            .to_string()
            .contains("InvalidArgumentValue: Expected the value to be of type"),
        "the raising element surfaces: {pruned_err}"
    );
    assert_eq!(
        pruned_err.to_string(),
        with_allreduce_pruning_disabled(|| run(&format!("WHERE {predicate}")))
            .unwrap_err()
            .to_string(),
        "pruned errors identically to pruning-disabled"
    );
}

/// A predicate naming a binding that arrives after the quantification (the
/// pattern's end node) disqualifies the conjunct: it stays a post-filter,
/// expanding every hop — pinned by the hop counter — while filtering the
/// same routes as the inlinable spelling.
#[test]
fn a_predicate_naming_a_later_binding_stays_a_postfilter() {
    let graph = ev_ring_graph();
    let inlinable = pruned_query("c0", "c5", 6, "600");
    // `size(b.id) * 0` is a null-effect reference to the end node: the
    // predicate means the same as `acc < 600` but cannot be evaluated
    // before the quantification binds `b`.
    let declined = "MATCH REPEATABLE ELEMENTS p = (a:Geo {id:'c0'}) \
         (()-[rels:ROAD|CHARGE]-(x:Geo)){1,6} (b:Geo {id:'c5'}) \
         WHERE allReduce(acc = 0.0, r IN rels | acc + r.distance_km, acc < 600 + size(b.id) * 0) \
         RETURN length(p) AS len, reduce(s = 0.0, r IN rels | s + r.distance_km) AS cost \
         ORDER BY len, cost"
        .to_string();
    assert_eq!(
        exec_cypher(&graph, &inlinable).rows,
        exec_cypher(&graph, &declined).rows,
        "both spellings filter identically"
    );
    let declined_hops = qpp_hops(&graph, &declined);
    let postfilter_hops = qpp_hops(&graph, &postfilter_query("c0", "c5", 6, "600"));
    let pruned_hops = qpp_hops(&graph, &inlinable);
    assert_eq!(
        declined_hops, postfilter_hops,
        "the declined conjunct expands like the post-filter form"
    );
    assert!(pruned_hops < declined_hops);
}

/// Timing: the pruned route query's p50 against the post-filter form's on
/// the cyclic graph. Asserted at >= 3x for CI stability while the actual
/// ratio is reported; the node's >= 5x acceptance bar is pinned by the
/// deterministic hop ratio in
/// [`profile_hop_counter_shows_the_pruned_walk_expanding_fewer_hops`].
/// The `{1,7}` bound (one below the counter test's) keeps the unpruned
/// run's combinatorial fan-out — and so the test's wall time — bounded on
/// slow CI machines without touching the ratio's order of magnitude.
#[test]
fn pruned_route_query_p50_beats_the_postfilter_form() {
    let graph = ev_ring_graph();
    let pruned = pruned_query("c0", "c5", 7, "600");
    let postfilter = postfilter_query("c0", "c5", 7, "600");

    let p50 = |query: &str, runs: usize| {
        // Two warmup runs take parse/plan and first-touch graph pages out
        // of the measurement.
        for _ in 0..2 {
            exec_cypher(&graph, query);
        }
        let mut samples: Vec<_> = (0..runs)
            .map(|_| {
                let start = Instant::now();
                exec_cypher(&graph, query);
                start.elapsed().as_nanos() as f64
            })
            .collect();
        samples.sort_by(|a, b| a.partial_cmp(b).expect("durations are ordered"));
        samples[runs / 2]
    };
    let runs = 7;
    let post_ms = p50(&postfilter, runs) / 1e6;
    let pruned_ms = p50(&pruned, runs) / 1e6;
    let ratio = post_ms / pruned_ms;
    eprintln!("RATIO pruned={pruned_ms:.4}ms post={post_ms:.4}ms ratio={ratio:.1}x");
    assert!(
        ratio >= 3.0,
        "pruned p50 {pruned_ms:.3} ms vs post-filter p50 {post_ms:.3} ms \
         (ratio {ratio:.1}x, expected >= 3x)"
    );
}

/// A predicate reading a name bound before the MATCH (a carried budget —
/// the stateful-traversal parameter shape) stays inlinable: the mid-path
/// value is the entering row's, exactly what the post-filter reads. Pinned
/// by the hop counter, not just the answers — a declined conjunct would
/// expand like the post-filter form.
#[test]
fn a_carried_budget_stays_inlinable() {
    let graph = ev_ring_graph();
    let predicate = "allReduce(acc = 0.0, r IN rels | acc + r.distance_km, acc < budget)";
    let pruned = format!(
        "WITH 600 AS budget \
         MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'c0'}}) \
         (()-[rels:ROAD|CHARGE]-(x:Geo)){{1,6}} (b:Geo {{id:'c5'}}) \
         WHERE {predicate} RETURN count(*) AS routes"
    );
    let postfilter = format!(
        "WITH 600 AS budget \
         MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'c0'}}) \
         (()-[rels:ROAD|CHARGE]-(x:Geo)){{1,6}} (b:Geo {{id:'c5'}}) \
         WITH p, rels WHERE {predicate} RETURN count(*) AS routes"
    );
    assert_eq!(
        exec_cypher(&graph, &pruned).rows,
        exec_cypher(&graph, &postfilter).rows
    );
    assert!(qpp_hops(&graph, &pruned) < qpp_hops(&graph, &postfilter));
}
