//! The statement-level A/B differential (the differential-suite node,
//! a-2 over the design note `frontier-nfa-design-2026-11-01.md` §8):
//! the same whole statements — pattern, match mode, and the
//! RETURN/WHERE/aggregate suffix — run engaged and stood down
//! (`with_frontier_disabled`), over the rearival suite's generated
//! multigraphs (one proptest), a deterministic fixture matrix, and
//! hand-built cyclic edge cases. Answer-set equality (multiset) for
//! distinct-endpoint and EXISTS answers; per-row validity for capped
//! path answers (ADR-0478 Decision 4 — the row set of an un-ordered cap
//! is a contract-free prefix, never compared); identical rows and zero
//! engagements for every decline. The rider wiring's own contract tests
//! and the shared A/B helpers live in `frontier_rider_tests`; this
//! module is the corpus, not a second framework.

use std::collections::HashSet;

use proptest::prelude::*;
use relate_core::{NodeId, Value};

use super::super::take_frontier_counters;
use crate::exec::test_support::{build, exec_cypher, patterns, same_rows, Builder, Built};
use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;
use crate::test_support::config_with_floor;
use crate::QueryResult;

use super::frontier_rider_tests::{differential, differential_declined, ring_graph, seed_id};
use super::nfa::compile;
use super::rearival_tests::{multigraph, multigraph_case, pattern_body, VOCAB};
use super::{take_kernel_engagements, with_frontier_disabled};

/// What a statement shape's suffix proves about its consumer — which
/// A/B assertion the differential runs on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A distinct-endpoint answer (`DISTINCT y`, `count(DISTINCT y)`,
    /// `min`/`max` folds, a riding boundary WHERE): answer-set
    /// equality, as a multiset.
    DistinctEndpoints,
    /// Un-ordered capped emission (`LIMIT`, `SKIP + LIMIT`): per-row
    /// validity — Decision 4's row-order exclusion. The row set of a
    /// contract-free prefix is never compared. Carries the uncapped
    /// twin whose stood-down DFS enumeration is the per-row validity
    /// oracle (every capped row must be one of its rows) — every capped
    /// shape of the vocabulary has one, the invariant the field states
    /// in place of an Option.
    Capped { uncapped: &'static str },
    /// An EXISTS verdict: answer-set equality (one boolean row).
    Exists,
    /// An answer-shape decline (full enumeration, ordered cap, path
    /// count): identical rows, zero engagements — the fall-through's
    /// own proof.
    Decline,
}

/// One statement shape of the differential vocabulary: the template
/// (`{mode}` the match-mode prefix slot, `{body}` the pattern slot) and
/// the consumer it proves.
struct Statement {
    template: &'static str,
    kind: Kind,
}

/// The statement vocabulary: the consumers ADR-0478 Decision 2 serves
/// (the distinct-endpoint family with a riding boundary WHERE, the
/// capped pair `LIMIT` / `SKIP + LIMIT`, the EXISTS subquery) plus the
/// three decline representatives (full enumeration, the ordered cap,
/// the path count) — ten shapes, each run under both pools.
const STATEMENTS: &[Statement] = &[
    Statement {
        template: "MATCH {mode}{body} RETURN DISTINCT y AS y",
        kind: Kind::DistinctEndpoints,
    },
    Statement {
        template: "MATCH {mode}{body} RETURN count(DISTINCT y) AS c",
        kind: Kind::DistinctEndpoints,
    },
    Statement {
        template: "MATCH {mode}{body} RETURN min(y.id) AS m",
        kind: Kind::DistinctEndpoints,
    },
    Statement {
        template: "MATCH {mode}{body} WHERE y.id > 1 RETURN count(DISTINCT y) AS c",
        kind: Kind::DistinctEndpoints,
    },
    Statement {
        template: "MATCH {mode}p = {body} RETURN p LIMIT 3",
        kind: Kind::Capped {
            uncapped: "MATCH {mode}p = {body} RETURN p",
        },
    },
    Statement {
        template: "MATCH {mode}p = {body} RETURN p, y.id AS y SKIP 1 LIMIT 3",
        kind: Kind::Capped {
            uncapped: "MATCH {mode}p = {body} RETURN p, y.id AS y",
        },
    },
    Statement {
        template: "RETURN EXISTS { MATCH {mode}{body} } AS hit",
        kind: Kind::Exists,
    },
    Statement {
        template: "MATCH {mode}{body} RETURN x.id AS x, y.id AS y",
        kind: Kind::Decline,
    },
    Statement {
        template: "MATCH {mode}p = {body} RETURN p, y.id AS y ORDER BY y.id LIMIT 3",
        kind: Kind::Decline,
    },
    Statement {
        template: "MATCH {mode}{body} RETURN count(*) AS n",
        kind: Kind::Decline,
    },
];

/// The match-mode prefixes the statement vocabulary runs under: the
/// default pool (the unwritten mode, DIFFERENT RELATIONSHIPS ⇒ TRAIL)
/// and the walk pool (bounded REPEATABLE ELEMENTS — the only walk
/// spelling the parser accepts).
const MODES: &[&str] = &["", "REPEATABLE ELEMENTS "];
/// The default pool's index in [`MODES`].
const DEFAULT_POOL: usize = 0;
/// The walk pool's index in [`MODES`].
const WALK: usize = 1;
/// The capped templates' emitted-row bound (`LIMIT 3` and
/// `SKIP 1 LIMIT 3` alike).
const CAPPED_WANT: usize = 3;

/// One statement of the vocabulary: the template with its mode and
/// pattern slots substituted.
fn statement_text(template: &str, body: &str, mode: usize) -> String {
    template
        .replace("{mode}", MODES[mode])
        .replace("{body}", body)
}

/// One statement's A/B runs plus the drained counter facts — the
/// differential's one observation of a statement.
struct StatementAb {
    engaged: QueryResult,
    dfs: QueryResult,
    /// The frontier ran for the engaged spelling (its own counters).
    ran: bool,
    /// Every kernel's engagement count across both runs.
    engagements: u32,
}

/// The one statement-level A/B (the rider idiom, statement-shaped): the
/// engaged run, the counters drained off it, the stood-down run, and
/// the stand-down's own proof — the frontier never runs under the hook.
fn statement_ab(g: &Graph, cypher: &str) -> StatementAb {
    let engaged = exec_cypher(g, cypher);
    let ran = take_frontier_counters().levels > 0;
    let dfs = with_frontier_disabled(|| exec_cypher(g, cypher));
    let stood_down = take_frontier_counters().levels > 0;
    let engagements = take_kernel_engagements();
    assert!(!stood_down, "the stand-down hook holds for {cypher}");
    StatementAb {
        engaged,
        dfs,
        ran,
        engagements,
    }
}

/// The capped answer's per-row validity (ADR-0478 Decision 4's half of
/// the differential): with the frontier engaged, every row is a
/// complete binding the DFS's pipeline would keep — the path variable a
/// `Value::Path` anchored at the seed, inside the NFA's own hop bounds,
/// rel-distinct under the default pool (TRAIL answers) with rel reuse
/// legal under the walk pool — and byte-equal to one row of the
/// stood-down DFS's uncapped enumeration of the same statement. The row
/// count is bounded by the cap; the row set is not compared (a
/// contract-free prefix both ways).
fn assert_capped_rows(
    g: &Graph,
    cypher: &str,
    result: &QueryResult,
    seed: NodeId,
    walk_pool: bool,
    oracle: &QueryResult,
) {
    let nfa = compile(g, &patterns(cypher)[0], PatternEngine::Auto)
        .unwrap_or_else(|| panic!("{cypher} should compile"));
    // An unbounded quantifier's hop ceiling is the engine's own
    // unbounded default-pool cap: the rel count (the satiety cap,
    // ADR-0471 §3) — the next rel id is the count, ids sequential
    // from zero.
    let max = nfa.max_hops.unwrap_or_else(|| {
        u32::try_from(g.next_rel_id()).expect("a test graph's rel count fits u32")
    });
    let p = result
        .columns
        .iter()
        .position(|column| column == "p")
        .unwrap_or_else(|| panic!("{cypher}: no `p` column"));
    assert!(
        result.rows.len() <= CAPPED_WANT,
        "{cypher}: the cap bounds the rows: {}",
        result.rows.len()
    );
    for row in &result.rows {
        let Value::Path(path) = &row[p] else {
            panic!("{cypher}: the path variable binds a path: {row:?}")
        };
        assert_eq!(
            path.nodes.first().map(|node| node.id),
            Some(seed),
            "{cypher}: the witness anchors at the seed: {row:?}"
        );
        assert!(
            u32::try_from(path.rels.len()).is_ok_and(|hops| nfa.min_hops <= hops && hops <= max),
            "{cypher}: the witness is hop-bounded: {row:?}"
        );
        if !walk_pool {
            let mut seen = HashSet::new();
            assert!(
                path.rels.iter().all(|rel| seen.insert(rel.id)),
                "{cypher}: the default pool answers TRAIL rows: {row:?}"
            );
        }
        assert!(
            oracle.rows.contains(row),
            "{cypher}: the row is one of the DFS's uncapped rows: {row:?}"
        );
    }
}

/// One statement's differential, the suite's one assertion policy
/// (a-2): answer-set equality for the distinct-endpoint and EXISTS
/// answers; per-row validity for an engaged capped answer (Decision 4 —
/// BFS-discovery order replaces DFS order, and the first-cap row set is
/// a contract-free prefix); identical rows and zero engagements for
/// every decline. Returns whether the frontier ran — the coverage
/// matrix's aggregate reads it.
fn assert_statement(
    g: &Graph,
    statement: &Statement,
    body: &str,
    mode: usize,
    seed: NodeId,
) -> bool {
    let cypher = statement_text(statement.template, body, mode);
    let ab = statement_ab(g, &cypher);
    if ab.ran {
        assert_eq!(ab.engagements, 1, "exactly the frontier took {cypher}");
    } else {
        assert_eq!(ab.engagements, 0, "no kernel takes the declined {cypher}");
    }
    match statement.kind {
        Kind::Capped { uncapped } if ab.ran => {
            assert_eq!(ab.engaged.columns, ab.dfs.columns, "{cypher}");
            let oracle =
                with_frontier_disabled(|| exec_cypher(g, &statement_text(uncapped, body, mode)));
            assert_capped_rows(g, &cypher, &ab.engaged, seed, mode == WALK, &oracle);
        }
        Kind::Decline => {
            assert!(!ab.ran, "the answer-shape gate declines {cypher}");
            assert_eq!(
                ab.engaged.rows, ab.dfs.rows,
                "the decline is the DFS's own rows: {cypher}"
            );
        }
        _ if ab.ran => {
            assert!(
                same_rows(&ab.engaged, &ab.dfs),
                "rows diverge for {cypher}:\n  frontier: {:?}\n  dfs:      {:?}",
                ab.engaged.rows,
                ab.dfs.rows
            );
        }
        _ => {
            assert_eq!(
                ab.engaged.rows, ab.dfs.rows,
                "the declined {cypher} is the DFS's own rows"
            );
        }
    }
    ab.ran
}

/// The coverage matrix's fixture: a dense cyclic `KNOWS`/`LIKES`
/// multigraph — a parallel `KNOWS` pair (0↔1), `KNOWS` self-loops at 1
/// and 3, a `LIKES` self-loop at 2, a ring plus chords, both types
/// crossing — the re-arrival shapes (parallel rels, self-loops) at a
/// density the cost gate engages. Five of the ten vocabulary patterns
/// engage on it; the alternations and the context+round shapes decline
/// on the cost gate (the degree estimate does not scale a multi-type
/// rel) — both arms of every cell are the gates' truth, never a
/// fixture-tuned expectation.
fn multigraph_fixture() -> Built {
    build(
        &[&["N"], &["N"], &["N"], &["N"], &["N"], &["N"]],
        &[
            (0, 1, "KNOWS"),
            (1, 0, "KNOWS"),
            (1, 1, "KNOWS"),
            (3, 3, "KNOWS"),
            (2, 2, "LIKES"),
            (0, 2, "KNOWS"),
            (2, 3, "KNOWS"),
            (3, 4, "KNOWS"),
            (4, 5, "KNOWS"),
            (5, 0, "KNOWS"),
            (1, 3, "KNOWS"),
            (2, 4, "KNOWS"),
            (3, 5, "KNOWS"),
            (4, 0, "KNOWS"),
            (1, 2, "KNOWS"),
            (0, 3, "LIKES"),
            (1, 4, "LIKES"),
            (2, 5, "LIKES"),
            (0, 1, "LIKES"),
        ],
    )
}

/// The deterministic coverage matrix: the whole statement vocabulary ×
/// both pools × the whole pattern vocabulary on the multigraph fixture
/// — 200 statement A/Bs in one test, plus the engagement aggregate that
/// keeps the differential honest: every engage-able (statement, pool)
/// cell engaged for at least one pattern, so a matcher or cost-gate
/// regression that silently declines the vocabulary surfaces here as a
/// missing cell, not only as silently-identical rows. The walk-pool
/// EXISTS cell is the one documented exception (its own pin below).
#[test]
fn statement_vocabulary_matrix_matches_the_dfs_stand_down() {
    let built = multigraph_fixture();
    const SEED: usize = 2;
    let anchor = built.nodes[SEED];
    let mut engaged: HashSet<(usize, usize)> = HashSet::new();
    for pattern in 0..VOCAB.len() {
        let body = pattern_body(pattern, SEED);
        for (index, statement) in STATEMENTS.iter().enumerate() {
            for mode in 0..MODES.len() {
                if assert_statement(&built.graph, statement, &body, mode, anchor) {
                    engaged.insert((index, mode));
                }
            }
        }
    }
    for (index, statement) in STATEMENTS.iter().enumerate() {
        for mode in 0..MODES.len() {
            let engageable = matches!(
                statement.kind,
                Kind::DistinctEndpoints | Kind::Capped { .. }
            ) || (statement.kind == Kind::Exists && mode == DEFAULT_POOL);
            if engageable {
                assert!(
                    engaged.contains(&(index, mode)),
                    "statement {index} under pool {mode} never engaged"
                );
            }
        }
    }
}

// The statement-level A/B differential over the generated corpus
// (a-2): every drawn multigraph — the rearival suite's shared
// generator, so the same parallel rels, self-loops, mixed directions,
// and both types — runs the whole statement vocabulary, both pools,
// engaged vs stood down. The bounded vocabulary keeps the stood-down
// DFS's enumeration finite; the unbounded spellings are the hand-built
// fixtures' territory (a generated dense multigraph reproduces the
// PathDB blowup class — the rearival suite's own substitution
// precedent). (A plain comment: rustdoc does not document macro
// invocations.)
proptest! {
    #![proptest_config(config_with_floor(256))]
    #[test]
    fn differential_statements_match_the_dfs_stand_down(
        (n, edges, seed, pattern) in multigraph_case(),
    ) {
        let body = pattern_body(pattern, seed);
        let built = multigraph(n, &edges);
        let anchor = built.nodes[seed];
        for statement in STATEMENTS {
            for mode in 0..MODES.len() {
                assert_statement(&built.graph, statement, &body, mode, anchor);
            }
        }
    }
}

/// The walk pool's row-shaped statements (bounded REPEATABLE
/// ELEMENTS): the distinct-endpoint answers engage through the match
/// ladder and answer the DFS's REPEATABLE spelling exactly — with the
/// boundary WHERE riding as an arrival check — and the capped emission
/// answers per-row-valid witnesses (per pool: rel reuse legal).
#[test]
fn walk_pool_row_statements_match_the_dfs_repeatable_spelling() {
    let g = ring_graph();
    differential(
        &g,
        "MATCH REPEATABLE ELEMENTS (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
         RETURN count(DISTINCT y.id) AS c",
    );
    differential(
        &g,
        "MATCH REPEATABLE ELEMENTS (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
         RETURN min(y.id) AS m",
    );
    differential(
        &g,
        "MATCH REPEATABLE ELEMENTS (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
         WHERE y.id <> 'n2' RETURN DISTINCT y.id AS y",
    );
    let query =
        "MATCH REPEATABLE ELEMENTS p = (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN p LIMIT 3";
    let engaged = exec_cypher(&g, query);
    assert_eq!(
        take_kernel_engagements(),
        1,
        "the frontier engages for {query}"
    );
    assert!(
        take_frontier_counters().levels > 0,
        "the frontier ran for {query}"
    );
    let oracle = with_frontier_disabled(|| {
        exec_cypher(
            &g,
            "MATCH REPEATABLE ELEMENTS p = (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) RETURN p",
        )
    });
    assert_capped_rows(&g, query, &engaged, seed_id(&g, "n0"), true, &oracle);
}

/// The capped consumer's `SKIP + LIMIT` shape (the cap rider's `Plain`
/// want): the frontier engages, the emitted rows stay inside the cap,
/// and each is one of the stood-down DFS's uncapped rows.
#[test]
fn capped_skip_and_limit_stay_valid_under_the_frontier() {
    let g = ring_graph();
    let query = "MATCH p = (x:Geo {id:'n0'})-[:LINK*1..4]-(y:Geo) RETURN p SKIP 2 LIMIT 3";
    let engaged = exec_cypher(&g, query);
    assert_eq!(
        take_kernel_engagements(),
        1,
        "the frontier engages for {query}"
    );
    assert!(
        take_frontier_counters().levels > 0,
        "the frontier ran for {query}"
    );
    let oracle = with_frontier_disabled(|| {
        exec_cypher(
            &g,
            "MATCH p = (x:Geo {id:'n0'})-[:LINK*1..4]-(y:Geo) RETURN p",
        )
    });
    assert_capped_rows(&g, query, &engaged, seed_id(&g, "n0"), false, &oracle);
    let dfs = with_frontier_disabled(|| exec_cypher(&g, query));
    assert!(
        dfs.rows.len() <= 3,
        "the DFS cap rider also halts at the cap"
    );
}

/// The EXISTS arm's false verdict: no matching target — the frontier
/// runs to its fixpoint and answers `false` in milliseconds, and the
/// stood-down probe answers the same boolean. The fixture is the ring,
/// not the web: a false probe verdict is the one answer that cannot
/// ride the probe's first-hit early exit — the DFS must exhaust its
/// full unbounded trail enumeration, which the ring finishes and the
/// web never would (the web fixture's own rule: its differentials ride
/// the DFS's early exits, never its full walk).
#[test]
fn exists_false_arm_runs_to_fixpoint_identically() {
    let g = ring_graph();
    let query = "RETURN EXISTS { MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo {id:'zz'}) } AS hit";
    let engaged = differential(&g, query);
    assert_eq!(engaged.rows, vec![vec![Value::Bool(false)]]);
}

/// The walk-pool EXISTS shape is a structural decline, pinned: the
/// exists kernel arm is the plain-match route (`exists_probe`'s
/// `plain_match` admits only the DIFFERENT RELATIONSHIPS mode), and the
/// subquery's own MATCH carries no RETURN suffix for the row rung's
/// consumer decode — so a REPEATABLE ELEMENTS subquery always answers
/// on the DFS probe: identical rows, zero engagements, never a wrong
/// answer. (The bounded walk-pool ROW shapes engage through the match
/// ladder — the matrix and the proptest carry those; relaxing this
/// decline is rider-wiring work, not a differential finding.)
#[test]
fn walk_pool_exists_statements_decline_to_the_probe() {
    let g = ring_graph();
    let declined = differential_declined(
        &g,
        "RETURN EXISTS { MATCH REPEATABLE ELEMENTS \
         (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo {id:'n3'}) } AS hit",
    );
    assert_eq!(declined.rows, vec![vec![Value::Bool(true)]]);
}

/// The re-arrival rule's named shapes (the rearival suite's graphs) as
/// whole statements through the seam: the unbounded spellings engage
/// (cost rule (a) — an unbounded quantifier engages on shape alone), so
/// the rule's trail proofs and below-min candidates run inside the
/// statement engine, and every answer matches the stood-down DFS. The
/// bounded below-min spelling on the two-node graph stays on the DFS
/// (the cost gate: paths ≈ endpoints) — its rule pin lives at the
/// direct seam (`rearival_tests`), and the engaged bounded coverage
/// rides the generated corpus and the matrix fixture, whose graphs
/// carry the self-loops and parallel rels.
#[test]
fn re_arrival_shapes_answer_identically_as_statements() {
    // The measured divergence shape (the KNOWS triangle): the seed is a
    // trail endpoint only through the 3-rel loop — the rule's proof.
    let triangle = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS"), (0, 2, "KNOWS")],
    );
    differential(
        &triangle.graph,
        "MATCH (x:N {id: 0})-[:KNOWS*]-(y:N) RETURN DISTINCT y AS y",
    );
    let capped = "MATCH p = (x:N {id: 0})-[:KNOWS*]-(y:N) RETURN p LIMIT 2";
    let engaged = exec_cypher(&triangle.graph, capped);
    assert_eq!(
        take_kernel_engagements(),
        1,
        "the frontier engages for {capped}"
    );
    let oracle = with_frontier_disabled(|| {
        exec_cypher(
            &triangle.graph,
            "MATCH p = (x:N {id: 0})-[:KNOWS*]-(y:N) RETURN p",
        )
    });
    assert_capped_rows(
        &triangle.graph,
        capped,
        &engaged,
        triangle.nodes[0],
        false,
        &oracle,
    );

    // The parallel-rel graph: the seed's 2-walk is a TRAIL over the two
    // parallels — the rule's search proof, inside the statement.
    let parallel = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS"), (0, 1, "KNOWS")]);
    differential(
        &parallel.graph,
        "MATCH (x:N {id: 0})-[:KNOWS*]-(y:N) RETURN DISTINCT y AS y",
    );

    // The self-loop graph: the 2-trail 0-1-1 closes over the loop, the
    // seed's re-arrival rejected.
    let self_loop = build(&[&["N"], &["N"]], &[(0, 1, "KNOWS"), (1, 1, "KNOWS")]);
    differential(
        &self_loop.graph,
        "MATCH (x:N {id: 0})-[:KNOWS*]-(y:N) RETURN DISTINCT y AS y",
    );

    // Remedy (b)'s counterexample graph: no unbounded KNOWS trail
    // beyond node 2 — the type-blind admission would have answered 1.
    let type_blind = build(
        &[&["N"], &["N"], &["N"]],
        &[(0, 1, "KNOWS"), (1, 2, "KNOWS"), (1, 2, "LIKES")],
    );
    differential(
        &type_blind.graph,
        "MATCH (x:N {id: 0})-[:KNOWS*]-(y:N) RETURN DISTINCT y AS y",
    );

    // The below-min candidate graph (the walk-pool pin's own fixture,
    // `rearival_tests`), bounded: the self-loop makes the cost model
    // price a branching recursion, so the frontier engages — rows
    // identical.
    differential(
        &build(&[&["N"], &["N"]], &[(1, 0, "KNOWS"), (1, 1, "KNOWS")]).graph,
        "MATCH (x:N {id: 1})-[:KNOWS*2..3]-(y:N) RETURN DISTINCT y AS y",
    );
}

/// (folded rider-integration review finding, medium) The boundary
/// nodes' inline element predicates — a `WHERE` on the node pattern
/// itself (CIP-14), a dynamic label expression — decline through
/// `frontier_match::boundary_target`: `compile_node` reads neither, so
/// riding one would silently drop the check. Every shape here is
/// otherwise frontier-eligible (unbounded, so the cost gate's rule (a)
/// would engage it) and every predicate actually filters, so deleting
/// the decline would engage the frontier and answer with the predicate
/// dropped — a different row count in this pin, a silent wrong answer
/// everywhere else.
#[test]
fn boundary_inline_predicates_decline_through_the_walker() {
    let g = ring_graph();
    // A boundary inline WHERE on the target: the answer excludes n3 of
    // the six trail endpoints — the dropped-predicate answer is 6.
    let target = differential_declined(
        &g,
        "MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo WHERE y.id <> 'n3') \
         RETURN count(DISTINCT y.id) AS c",
    );
    assert_eq!(target.rows, vec![vec![Value::Int(5)]]);
    // A boundary inline WHERE on the seed: only n2 seeds — the
    // dropped-predicate answer seeds every node.
    let seed_side = differential_declined(
        &g,
        "MATCH (x:Geo WHERE x.id = 'n2')-[:LINK*]-(y:Geo) \
         RETURN count(DISTINCT y.id) AS c",
    );
    assert_eq!(seed_side.rows, vec![vec![Value::Int(6)]]);
    // A dynamic label expression on the target (`Geo|Other` — not a
    // static conjunction, so `label_index_seekable` declines it): three
    // of the four trail endpoints — the dropped-predicate answer is 4.
    let mixed = mixed_label_ring();
    let labeled = differential_declined(
        &mixed,
        "MATCH (x:Geo {id:'a'})-[:LINK*]-(y:Geo|Other) \
         RETURN count(DISTINCT y.id) AS c",
    );
    assert_eq!(labeled.rows, vec![vec![Value::Int(3)]]);
}

/// A four-node `LINK` ring with mixed labels — the dynamic
/// label-expression decline's fixture: `Geo`, `Other`, `Web`, and one
/// node carrying both `Geo` and `Other`.
fn mixed_label_ring() -> Graph {
    let b = Builder::new();
    let a = b.node("a", &["Geo"]);
    let c = b.node("c", &["Other"]);
    let d = b.node("d", &["Web"]);
    let e = b.node("e", &["Geo", "Other"]);
    b.rel(a, c, "LINK", &[]);
    b.rel(c, d, "LINK", &[]);
    b.rel(d, e, "LINK", &[]);
    b.rel(e, a, "LINK", &[]);
    b.done()
}
