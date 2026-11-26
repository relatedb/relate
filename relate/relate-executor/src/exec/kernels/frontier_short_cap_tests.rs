//! The capped emission's unfilled-cap fall-through (REL8-2076 finding 29):
//! a `LIMIT` without `ORDER BY` whose cap exceeds the reached (seed,
//! endpoint) pairs. Capped emission reconstructs one witness per pair, so
//! a cap that never fills is the pair-shaped answer to a full-enumeration
//! question — every further path to a reached pair is dropped. The rung
//! now declines it (`FrontierRunDecline::ShortCap`) and the walker's cap
//! rider emits exactly `min(want, paths)` rows (ADR-0475), which is what
//! `LIMIT n` promises.
//!
//! The corpus repro (`issues/29-frontier-capped-emission-drops-paths`) is
//! a fan: `a -> m1..m4 -> z` — four two-hop paths over one (seed,
//! endpoint) pair, so `LIMIT 100` must return four rows, not one.
//!
//! Per-shape contract, applied uniformly: every read shape runs on both
//! rungs (`auto`, the forced `nfa`) and asserts multiset equality with the
//! stood-down DFS (`same_rows`). A bare-cap shape additionally pins the
//! `min(want, paths)` row count, a `SKIP` shape pins the DFS's own page,
//! and a short cap names its typed decline. The write shape repeats on
//! both rungs, the filled-cap page asserts only its count and that the
//! frontier still serves (its row set is a contract-free prefix), and
//! `walk` is the untouched control.

use crate::exec::test_support::{
    build, column, exec_autocommit, exec_cypher, frontier_decline_of, int_cell, same_rows, Builder,
};
use crate::frontier_decline::{FrontierDecline, FrontierDeclineReason, FrontierRunDecline};
use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;

use super::super::take_frontier_counters;
use super::with_frontier_disabled;

/// The corpus repro's fan's path count: `a -> m1..m4 -> z`, four distinct
/// two-hop paths from one `:A` to one `:Z` — the endpoint-pair multiplicity
/// the capped emission dropped. Its rel count is two per path.
const FAN_PATHS: usize = 4;

/// The corpus repro's fan, under `engine`. Its cardinality is the
/// fixture's own fact and is checked here, once.
fn fan(engine: PatternEngine) -> Graph {
    let b = Builder::new();
    let a = b.node("0", &["A"]);
    let z = b.node("99", &["Z"]);
    for i in 1..=FAN_PATHS {
        let m = b.node(&i.to_string(), &["M"]);
        b.rel(a, m, "R", &[]);
        b.rel(m, z, "R", &[]);
    }
    let mut g = b.done();
    g.configure_pattern_engine(engine);
    assert_eq!(
        g.next_rel_id(),
        2 * FAN_PATHS as i64,
        "the fan's two rels per path"
    );
    g
}

/// The dense fixture's node count (the corpus's 30).
const NODES: usize = 30;
/// Its derived shape, one source of truth for the fixture and the caps
/// the pages below use: 20 sources (`x % 3 != 0`) × 29 targets = 580
/// rels, 600 reachable pairs, 11,020 two-hop paths.
const RELS: i64 = 580; // the id type `Graph::next_rel_id` reports
const PAIRS: usize = 600;
const PATHS: usize = 11_020;

/// `build`'s dense graph from the corpus evidence
/// (`evidence/dense-graph.cypher`): every `x -> y` except `x == y` and `x`
/// a multiple of 3. The corpus predicate `(x.id * 7 + y.id * 3) % 3 <> 0`
/// reduces to `x % 3 <> 0` (`y * 3` is zero mod 3, `7x ≡ x`), spelled here
/// so the shape is visible. Its cardinality is the fixture's own fact and
/// is checked here, once, never per rung.
fn dense(engine: PatternEngine) -> Graph {
    let labels: Vec<&[&str]> = vec![&["N"]; NODES];
    let mut rels = Vec::new();
    for x in 0..NODES {
        for y in 0..NODES {
            if x != y && x % 3 != 0 {
                rels.push((x, y, "R"));
            }
        }
    }
    let mut g = build(&labels, &rels).graph;
    g.configure_pattern_engine(engine);
    assert_eq!(g.next_rel_id(), RELS, "the corpus fixture's rel count");
    g
}

/// A start that exists but reaches no `:Z`: one `:A`, one `:Z`, and a
/// single `:R` hop to an `:M` — the rel type is interned (an untyped slot
/// needs it) and no two-hop path ends at the `:Z`, so the pair set is
/// empty.
fn empty_target(engine: PatternEngine) -> Graph {
    let g = Builder::new();
    let a = g.node("0", &["A"]);
    g.node("99", &["Z"]);
    let m = g.node("1", &["M"]);
    g.rel(a, m, "R", &[]);
    let mut g = g.done();
    g.configure_pattern_engine(engine);
    g
}

/// The two rungs that take the frontier family; `walk` is a separate,
/// untouched control.
const FRONTIER_RUNGS: [PatternEngine; 2] = [PatternEngine::Auto, PatternEngine::Nfa];

/// The `walk` rung's control, one spelling for every test: the same
/// statement answers `want` rows and the rung never considers the
/// frontier (no decline recorded).
fn walk_control(fixture: fn(PatternEngine) -> Graph, cypher: &str, want: usize) {
    let control = fixture(PatternEngine::Walk);
    assert_eq!(
        exec_cypher(&control, cypher).rows.len(),
        want,
        "the walk rung agrees"
    );
    assert_eq!(
        frontier_decline_of(&control, cypher),
        None,
        "the walk rung never considers the frontier"
    );
}

#[test]
fn a_short_cap_returns_every_path_not_one_witness_per_pair() {
    let cypher = "MATCH p = (a:A)-[:R*2]->(z:Z) RETURN [n IN nodes(p) | n.id] AS route LIMIT 100";
    for engine in FRONTIER_RUNGS {
        let g = fan(engine);
        let engaged = exec_cypher(&g, cypher);
        assert_eq!(
            engaged.rows.len(),
            FAN_PATHS,
            "{engine:?}: LIMIT 100 over {FAN_PATHS} paths returns {FAN_PATHS} rows"
        );
        assert!(
            take_frontier_counters().levels > 0,
            "{engine:?}: the frontier ran before it declined"
        );
        let stood_down = with_frontier_disabled(|| exec_cypher(&g, cypher));
        assert!(
            same_rows(&engaged, &stood_down),
            "{engine:?}: the four paths are the DFS's own rows"
        );
        assert_eq!(
            frontier_decline_of(&g, cypher),
            Some(FrontierRunDecline::ShortCap.into()),
            "{engine:?}: the unfilled cap's typed decline"
        );
    }
    walk_control(fan, cypher, FAN_PATHS);
}

#[test]
fn a_count_behind_a_short_cap_counts_every_path() {
    let cypher = "MATCH (a:A)-[:R*2]->(z:Z) WITH a, z LIMIT 100 RETURN count(*) AS n";
    for engine in FRONTIER_RUNGS {
        let g = fan(engine);
        let engaged = exec_cypher(&g, cypher);
        assert_eq!(
            column(&engaged, "n", int_cell),
            vec![FAN_PATHS as i64],
            "{engine:?}: the count behind the cap sees every path"
        );
        assert!(
            same_rows(
                &engaged,
                &with_frontier_disabled(|| exec_cypher(&g, cypher))
            ),
            "{engine:?}: the stood-down DFS counts the same"
        );
        assert_eq!(
            frontier_decline_of(&g, cypher),
            Some(FrontierRunDecline::ShortCap.into()),
            "{engine:?}: the count shape declines the same way"
        );
    }
}

#[test]
fn a_write_behind_a_short_cap_runs_once_per_path() {
    let write =
        "MATCH (a:A)-[:R*2]->(z:Z) WITH a, z LIMIT 100 SET z.hits = coalesce(z.hits, 0) + 1";
    for engine in FRONTIER_RUNGS {
        let g = fan(engine);
        exec_autocommit(&g, write)
            .unwrap_or_else(|e| panic!("{engine:?}: the write behind the cap must succeed: {e}"));
        assert_eq!(
            column(
                &exec_cypher(&g, "MATCH (z:Z) RETURN z.hits AS hits"),
                "hits",
                int_cell
            ),
            vec![FAN_PATHS as i64],
            "{engine:?}: the write behind the cap runs once per path"
        );
    }
}

#[test]
fn the_qpp_spelling_answers_every_path() {
    let cypher = "MATCH (a:A)-[:R]->{2}(z:Z) RETURN a.id AS a LIMIT 100";
    for engine in FRONTIER_RUNGS {
        let g = fan(engine);
        let engaged = exec_cypher(&g, cypher);
        assert_eq!(
            engaged.rows.len(),
            FAN_PATHS,
            "{engine:?}: the quantified spelling returns the same {FAN_PATHS} paths"
        );
        assert!(
            same_rows(
                &engaged,
                &with_frontier_disabled(|| exec_cypher(&g, cypher))
            ),
            "{engine:?}: the DFS agrees"
        );
        assert_eq!(
            frontier_decline_of(&g, cypher),
            Some(FrontierRunDecline::ShortCap.into()),
            "{engine:?}: the quantified spelling declines the same way"
        );
    }
}

#[test]
fn a_skip_before_a_short_cap_drops_only_the_skipped_rows() {
    let kept = FAN_PATHS - 1;
    let skipped = FAN_PATHS - kept;
    let cypher = format!("MATCH (a:A)-[:R*2]->(z:Z) RETURN a.id AS a SKIP {skipped} LIMIT 100");
    for engine in FRONTIER_RUNGS {
        let g = fan(engine);
        let engaged = exec_cypher(&g, &cypher);
        assert_eq!(
            engaged.rows.len(),
            kept,
            "{engine:?}: SKIP {skipped} of {FAN_PATHS} paths keeps {kept} (the cap counts SKIP + LIMIT)"
        );
        assert!(
            same_rows(
                &engaged,
                &with_frontier_disabled(|| exec_cypher(&g, &cypher))
            ),
            "{engine:?}: the DFS agrees"
        );
        assert_eq!(
            frontier_decline_of(&g, &cypher),
            Some(FrontierRunDecline::ShortCap.into()),
            "{engine:?}: SKIP over a short cap declines"
        );
    }
}

/// A start that reaches no target accepts no witness: the empty emission is
/// the exact empty answer, so the rung serves it (no `short-cap` decline,
/// no second walk).
#[test]
fn an_empty_pair_set_serves_its_exact_empty_answer_without_a_decline() {
    let cypher = "MATCH p = (a:A)-[:R*2]->(z:Z) RETURN p LIMIT 100";
    for engine in FRONTIER_RUNGS {
        // The routing the exemption rides, derived from the one rung list:
        // `auto`'s cost gate keeps this one-hop shape off the rung and
        // names `Cost`; the forced rung has no cost gate, runs it, and
        // serves the exact empty answer.
        let expected: Option<FrontierDeclineReason> =
            matches!(engine, PatternEngine::Auto).then_some(FrontierDecline::Cost.into());
        let g = empty_target(engine);
        let engaged = exec_cypher(&g, cypher);
        assert!(engaged.rows.is_empty(), "{engine:?}: no path, no row");
        assert!(
            same_rows(
                &engaged,
                &with_frontier_disabled(|| exec_cypher(&g, cypher))
            ),
            "{engine:?}: the DFS agrees on the empty answer"
        );
        // One exact expectation per rung — never a negative plus a branch:
        // the walked rung's plan-time `Cost`, the running rung's absent
        // decline (it served).
        assert_eq!(
            frontier_decline_of(&g, cypher),
            expected,
            "{engine:?}: the empty answer's decline"
        );
    }
}

#[test]
fn a_dense_short_cap_returns_the_whole_page() {
    // One above the pair count: the cap cannot fill, so the page is the
    // walker's exact `min(want, paths)` rows.
    let want = PAIRS + 1;
    let page = format!("MATCH p = (a)-[:R*2]->(e) RETURN a.id AS a LIMIT {want}");
    for engine in FRONTIER_RUNGS {
        let g = dense(engine);
        let engaged = exec_cypher(&g, &page);
        assert_eq!(
            engaged.rows.len(),
            want,
            "{engine:?}: LIMIT {want} over {PAIRS} pairs returns {want} of {PATHS} paths"
        );
        assert!(
            same_rows(&engaged, &with_frontier_disabled(|| exec_cypher(&g, &page))),
            "{engine:?}: the DFS page is the same multiset"
        );
        assert_eq!(
            frontier_decline_of(&g, &page),
            Some(FrontierRunDecline::ShortCap.into()),
            "{engine:?}: the dense short cap declines"
        );
    }
    walk_control(dense, &page, want);
}

#[test]
fn a_dense_short_cap_with_a_skip_returns_the_last_page() {
    // The cap cannot fill because the page runs past the pair set: SKIP
    // stops `skipped` short of PAIRS and the page is longer than that
    // shortfall, so SKIP + LIMIT > PAIRS. Both facts are independent
    // bindings, so the guard can actually fail.
    let page_rows = 20usize;
    let skipped = 10usize;
    let skip = PAIRS - skipped;
    assert!(page_rows > skipped, "the page must run past the pair set");
    let tail = format!("MATCH p = (a)-[:R*2]->(e) RETURN a.id AS a SKIP {skip} LIMIT {page_rows}");
    for engine in FRONTIER_RUNGS {
        let g = dense(engine);
        let engaged = exec_cypher(&g, &tail);
        assert_eq!(
            engaged.rows.len(),
            page_rows,
            "{engine:?}: the last page of {PATHS} paths has {page_rows} rows"
        );
        assert!(
            same_rows(&engaged, &with_frontier_disabled(|| exec_cypher(&g, &tail))),
            "{engine:?}: the DFS tail is the same multiset"
        );
        assert_eq!(
            frontier_decline_of(&g, &tail),
            Some(FrontierRunDecline::ShortCap.into()),
            "{engine:?}: the skipped short cap declines"
        );
    }
    walk_control(dense, &tail, page_rows);
}

#[test]
fn a_dense_filled_cap_still_serves_on_the_frontier() {
    // The cap equals the pair count, so the emission fills it and the
    // frontier serves a contract-free row prefix: its row set is never
    // compared — the pins are the count, the engagement, and the absent
    // decline.
    let filled = format!("MATCH p = (a)-[:R*2]->(e) RETURN a.id AS a LIMIT {PAIRS}");
    for engine in FRONTIER_RUNGS {
        let g = dense(engine);
        assert_eq!(
            exec_cypher(&g, &filled).rows.len(),
            PAIRS,
            "{engine:?}: a filled cap keeps its {PAIRS}-row prefix"
        );
        assert!(
            take_frontier_counters().levels > 0,
            "{engine:?}: the filled cap runs on the frontier"
        );
        assert_eq!(
            frontier_decline_of(&g, &filled),
            None,
            "{engine:?}: the filled cap is served, not declined"
        );
    }
}
