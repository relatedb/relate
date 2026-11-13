//! The frontier matcher arm's suite: one named decline per gate (the
//! structural proofs, the answer-shape vocabulary, the cost gate — each
//! naming the [`FrontierDecline`] it asserts), and one engage companion
//! per timeout class of the coverage report
//! (the flat
//! `hops` family, the `rounds` groups, the concatenation context — every
//! one engaging on the unbounded quantifier, the cost gate's rule (a)).

use std::collections::BTreeMap;

use relate_core::Value;
use relate_ir::{MatchMode, NodePattern, PatternElement, RelPattern};

use super::super::graph_stats::GraphStats;
use super::super::test_support::Builder;
use super::frontier_match::{compile_frontier, BoundaryPair, Consumer};

/// The distinct-endpoint consumer with no boundary-pair comparison.
const DISTINCT: Consumer = Consumer::DistinctEndpoints {
    pair: BoundaryPair::Unconstrained,
};
use super::nfa::Pool;
use crate::exec::test_support::{
    complete_graph, decide, declines_with, line_graph, named_node, one_hop_rel, path_pattern,
    quantified, var_length_rel,
};
use crate::frontier_decline::FrontierDecline;
use crate::graph::Graph;
use crate::row::Row;

/// A Person-centric cyclic graph: a `knows` triangle (p0-p1-p2), a
/// detached pair, and the `likes`/`hasCreator`/`studyAt` legs the
/// rounds/concatenation classes hop over — every type the corpus names
/// interned, every label the boundary nodes carry present.
fn corpus_graph() -> Graph {
    let b = Builder::new();
    let persons: Vec<_> = (0..5)
        .map(|i| b.node(&format!("p{i}"), &["Person"]))
        .collect();
    b.rel(persons[0], persons[1], "KNOWS", &[]);
    b.rel(persons[1], persons[2], "KNOWS", &[]);
    b.rel(persons[2], persons[0], "KNOWS", &[]);
    b.rel(persons[3], persons[4], "KNOWS", &[]);
    let comment = b.node("c0", &["Comment"]);
    let org = b.node("o0", &["Organisation"]);
    b.rel(persons[0], comment, "HAS_CREATOR", &[]);
    b.rel(persons[1], comment, "LIKE", &[]);
    b.rel(persons[0], org, "STUDY_AT", &[]);
    b.done()
}

/// Whether the statement engages the frontier.
fn engages(cypher: &str, graph: &Graph) -> bool {
    decide(cypher, graph, false, &Row::new(), |decision| {
        decision.is_ok()
    })
}

// The engage companions — the 42 timeout classes' shapes per the
// coverage report, every one an unbounded quantifier (the cost gate's
// rule (a): the DFS cannot halt below its rel-count cap).
#[test]
fn engages_the_timeout_classes() {
    let graph = corpus_graph();
    // The bare flat class (q9 `knows+`): one state, one self-loop.
    decide(
        "MATCH p = (x:Person)-[:KNOWS*1..]-(y) WHERE x.ID = 1 RETURN p LIMIT 100",
        &graph,
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("q9 engages: {e:?}"));
            assert_eq!(contract.consumer, Consumer::Capped { want: 100 });
            assert_eq!((contract.nfa.min_hops, contract.nfa.max_hops), (1, None));
            assert_eq!(
                contract.pool,
                Pool::Default,
                "the default pool is trail semantics"
            );
        },
    );
    // The rounds class (q37 `(likes.hasCreator)+`): the two-state cycle.
    decide(
        "MATCH p = (x:Person)((a)-[:LIKE]-(b)-[:HAS_CREATOR]-(c)){1,}(y) \
         WHERE x.ID = 1 RETURN p LIMIT 100",
        &graph,
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("q37 engages: {e:?}"));
            assert_eq!(contract.consumer, Consumer::Capped { want: 100 });
            assert_eq!(contract.nfa.states, 2);
            assert_eq!(contract.nfa.max_hops, None);
        },
    );
    // The scan+segment classes: the trailing context hop (q67
    // `(knows+).studyAt`) and the leading one (q86 `knows.(knows+)`).
    for cypher in [
        "MATCH p = (x:Person)-[:KNOWS*1..]-()-[:STUDY_AT]-(y) WHERE x.ID = 1 \
         RETURN p LIMIT 100",
        "MATCH p = (x:Person)-[:KNOWS]-()-[:KNOWS*1..]-(y) WHERE x.ID = 1 \
         RETURN p LIMIT 100",
    ] {
        decide(cypher, &graph, false, &Row::new(), |decision| {
            assert!(decision.is_ok(), "{cypher} engages");
        });
    }
    // The per-hop union class (q47 `(A|B)+`): one flat quantifier whose
    // single inner rel carries the union as its static type list.
    assert!(engages(
        "MATCH p = (x:Person)-[:KNOWS|STUDY_AT*1..]-(y) WHERE x.ID = 1 \
         RETURN p LIMIT 100",
        &graph
    ));
}

// The distinct-endpoint answer shapes engage: the DISTINCT projection,
// the endpoint folds, and each aggregate the vocabulary admits.
#[test]
fn engages_distinct_endpoint_answers() {
    let graph = corpus_graph();
    for cypher in [
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN DISTINCT y",
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN count(DISTINCT y)",
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN collect(DISTINCT y)",
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN min(y.k)",
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN max(y.k)",
        // A grouped fold over the boundary pair — one row per (seed,
        // endpoint) pair, the per-seed endpoint count.
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN x, count(DISTINCT y)",
        // A riding LIMIT only truncates the endpoint rows afterwards.
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN DISTINCT y LIMIT 5",
    ] {
        decide(cypher, &graph, false, &Row::new(), |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("{cypher} engages: {e:?}"));
            assert_eq!(
                contract.consumer,
                Consumer::DistinctEndpoints {
                    pair: BoundaryPair::Unconstrained
                },
                "{cypher}"
            );
        });
    }
}

// The EXISTS probe arm engages — the wiring constructs the verdict
// consumer directly, no suffix decode involved.
#[test]
fn engages_the_exists_probe_arm() {
    let graph = corpus_graph();
    decide(
        "MATCH (x:Person)-[:KNOWS*0..]-(y:Person) RETURN y",
        &graph,
        true,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("the probe arm engages: {e:?}"));
            assert_eq!(contract.consumer, Consumer::Exists);
            assert_eq!(
                contract.nfa.min_hops, 0,
                "q12 knows* is the zero-length arm"
            );
        },
    );
}

// The cost gate's rule (b): a bounded recursion whose estimated paths
// reach the factor over its estimated distinct endpoints engages — and
// the bounded low-multiplicity (acyclic-schema) recursion stays on the
// DFS.
#[test]
fn the_cost_gate_prices_the_dedup_ratio() {
    // The line graph: paths (2) stay under 4x the endpoint pool (4) —
    // the DFS is already output-bound and keeps the clause.
    declines_with(
        "MATCH (x:Person)-[:KNOWS*1..2]-(y:Person) RETURN DISTINCT y",
        &line_graph(),
        FrontierDecline::Cost,
    );
    // The complete graph: the degree machinery chains 40 estimated paths
    // against an endpoint pool of 5 — 4x (20) reached, the frontier
    // engages on the bounded shape alone.
    decide(
        "MATCH (x:Person)-[:KNOWS*1..3]-(y:Person) RETURN DISTINCT y",
        &complete_graph(),
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("the bounded ratio engages: {e:?}"));
            assert_eq!(contract.consumer, DISTINCT);
            assert_eq!(
                contract.nfa.max_hops,
                Some(3),
                "the bounded shape, rule (b)"
            );
        },
    );
}

/// A layered Person DAG: seven layers of four, every node linked
/// outgoing `KNOWS` to all four of the next layer — 4^k paths of length
/// k from one seed onto only four endpoints per layer, every hop the
/// (Person, KNOWS) degree tail's top bucket.
fn layered_graph() -> Graph {
    let b = Builder::new();
    let nodes: Vec<_> = (0..28)
        .map(|i| b.node(&format!("n{i}"), &["Person"]))
        .collect();
    for layer in 0..6 {
        for a in 0..4 {
            for c in 0..4 {
                b.rel(
                    nodes[layer * 4 + a],
                    nodes[(layer + 1) * 4 + c],
                    "KNOWS",
                    &[],
                );
            }
        }
    }
    b.done()
}

// The cost gate prices a branching bounded recursion as counts on both
// sides: the path chain multiplies past the rel type's total after the
// first hop (paths revisit nodes, so the distinct-seed degree-sum clamp
// does not hold), and an unlabeled target's distinct endpoints are
// bounded by the store's node population, never the ladder's rank
// sentinel — so `count(DISTINCT y)` / `collect(DISTINCT y)` over an
// unlabeled endpoint engage the endpoint-set consumer instead of
// enumerating 4^6 paths. The labeled-target twin engages too; the line
// graph's low-multiplicity recursion still keeps the DFS
// (`the_cost_gate_prices_the_dedup_ratio`).
#[test]
fn the_cost_gate_engages_a_branching_recursion_onto_an_unlabeled_target() {
    let graph = layered_graph();
    for cypher in [
        "MATCH (x:Person)-[:KNOWS*1..6]->(y) RETURN count(DISTINCT y)",
        "MATCH (x:Person)-[:KNOWS*1..6]->(y) RETURN collect(DISTINCT y)",
        "MATCH (x:Person)-[:KNOWS*1..6]->(y:Person) RETURN count(DISTINCT y)",
    ] {
        decide(cypher, &graph, false, &Row::new(), |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("{cypher} engages: {e:?}"));
            assert_eq!(
                contract.consumer,
                Consumer::DistinctEndpoints {
                    pair: BoundaryPair::Unconstrained
                },
                "{cypher}"
            );
        });
    }
}

// Bounded WALK mode engages with walk semantics — the frontier's native
// pool — while its unbounded spelling declines (the authored rejection
// stands, ADR-0478 §5) and full enumeration declines under it like every
// other mode.
#[test]
fn bounded_walk_mode_engages_with_walk_semantics() {
    let graph = complete_graph();
    decide(
        "MATCH REPEATABLE ELEMENTS p = (x:Person)-[:KNOWS*1..3]-(y:Person) \
         RETURN p LIMIT 5",
        &graph,
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("bounded WALK engages: {e:?}"));
            assert_eq!(
                contract.pool,
                Pool::Walk,
                "the REPEATABLE pool is walk semantics"
            );
            assert_eq!(contract.consumer, Consumer::Capped { want: 5 });
        },
    );
    // The default pool under the same bounded shape stays trail
    // semantics — the re-arrival rule's territory.
    decide(
        "MATCH p = (x:Person)-[:KNOWS*1..3]-(y:Person) RETURN p LIMIT 5",
        &graph,
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("the default pool: {e:?}"));
            assert_eq!(contract.pool, Pool::Default);
        },
    );
    // The capped consumer carries the per-witness mode checks
    // (`frontier_emit`): an explicit TRAIL rides the default pool
    // exactly like the unwritten mode, and ACYCLIC/SIMPLE reach the
    // frontier through their per-witness cycle checks — declining them
    // is not an available answer for the capped shape.
    decide(
        "MATCH p = TRAIL (x:Person)-[:KNOWS*1..3]-(y:Person) RETURN p LIMIT 5",
        &graph,
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("capped TRAIL engages: {e:?}"));
            assert_eq!(
                contract.pool,
                Pool::Default,
                "the default pool checks trails"
            );
        },
    );
    for mode in ["ACYCLIC", "SIMPLE"] {
        decide(
            &format!("MATCH p = {mode} (x:Person)-[:KNOWS*1..3]-(y:Person) RETURN p LIMIT 5"),
            &graph,
            false,
            &Row::new(),
            |decision| {
                let contract = decision.unwrap_or_else(|e| panic!("capped {mode} engages: {e:?}"));
                assert_eq!(
                    contract.pool,
                    Pool::Default,
                    "{mode} answers in the default pool"
                );
            },
        );
    }
    // The same modes outside the capped consumer stay per-path: a
    // distinct-endpoint answer is not a pair function under them.
    for mode in ["TRAIL", "ACYCLIC", "SIMPLE"] {
        declines_with(
            &format!("MATCH p = {mode} (x:Person)-[:KNOWS*1..3]-(y:Person) RETURN DISTINCT y"),
            &graph,
            FrontierDecline::Mode,
        );
    }
    // A trail-enforcing mode under the REPEATABLE pool: rel reuse is
    // that pool's semantics — no version of the mode exists there. (The
    // parser itself rejects the spelling — "REPEATABLE ELEMENTS only
    // combines with WALK" — so the pin is hand-built IR, the gate's
    // defensive mirror for shapes the front end never lowers, the
    // unbounded-REPEATABLE precedent above.)
    {
        let mut pattern = path_pattern(vec![
            PatternElement::Node(named_node("x")),
            PatternElement::Quantified(quantified(
                vec![
                    PatternElement::Node(NodePattern::anonymous()),
                    PatternElement::Relationship(one_hop_rel(&["KNOWS"])),
                    PatternElement::Node(NodePattern::anonymous()),
                ],
                1,
                Some(3),
            )),
            PatternElement::Node(named_node("y")),
        ]);
        pattern.mode = Some(relate_ir::PathMode::Acyclic);
        let params: BTreeMap<String, Value> = BTreeMap::new();
        let stats = GraphStats::new(&graph, &params);
        let patterns = [pattern];
        let decision = compile_frontier(
            &graph,
            &patterns,
            MatchMode::RepeatableElements,
            &Row::new(),
            &[],
            Consumer::Capped { want: 5 },
            &stats,
        );
        assert_eq!(decision.err(), Some(FrontierDecline::Mode));
    }
    // (The unbounded-REPEATABLE spelling never reaches the matcher: the
    // parser itself rejects it with the authored `UnsafeRepeatableElements`
    // error — ADR-0478 §5's rejection stands upstream, and the gate's
    // unbounded-walk arm remains its defensive mirror for hand-built IR.)
    // Bounded WALK full enumeration: two walks to one node are two
    // answers — the pair-dedup is unsound for it, mode notwithstanding.
    declines_with(
        "MATCH REPEATABLE ELEMENTS p = (x:Person)-[:KNOWS*1..3]-(y) RETURN p",
        &graph,
        FrontierDecline::FullEnumeration,
    );
}

// The spine gate: a nested quantifier inside the quantified spine (a
// var-length rel or a quantified group among its inner elements)
// unrolls the spine or accumulates list-of-list groups, and the
// emission's fixed-round reconstruction cannot bind those rows — the
// DFS keeps the shape. Parser-unreachable (the front end's
// `NestedQuantification` rule rejects both spellings), so the pin is
// hand-built IR, the `declines_an_inner_node_rel_name_collision`
// precedent.
#[test]
fn declines_a_nested_quantified_spine() {
    let graph = corpus_graph();
    let nested = |inner: relate_ir::PatternElement| {
        path_pattern(vec![
            PatternElement::Node(named_node("x")),
            PatternElement::Quantified(quantified(
                vec![
                    PatternElement::Node(NodePattern::anonymous()),
                    inner,
                    PatternElement::Node(NodePattern::anonymous()),
                ],
                1,
                None,
            )),
            PatternElement::Node(named_node("y")),
        ])
    };
    let params: BTreeMap<String, Value> = BTreeMap::new();
    let stats = GraphStats::new(&graph, &params);
    // A nested var-length rel (the spelling the front end produces for
    // `*m..n` inside a group, before the compiler normalizes it)…
    let var_length = nested(PatternElement::Relationship(var_length_rel(
        &["KNOWS"],
        1,
        Some(2),
    )));
    // …and a nested quantified group (the normalized spelling).
    let group = nested(PatternElement::Quantified(quantified(
        vec![
            PatternElement::Node(NodePattern::anonymous()),
            PatternElement::Relationship(one_hop_rel(&["KNOWS"])),
            PatternElement::Node(NodePattern::anonymous()),
        ],
        1,
        Some(2),
    )));
    for pattern in [var_length, group] {
        let patterns = [pattern];
        let decision = compile_frontier(
            &graph,
            &patterns,
            MatchMode::DifferentRelationships,
            &Row::new(),
            &[],
            Consumer::Exists,
            &stats,
        );
        assert_eq!(decision.err(), Some(FrontierDecline::NestedSpine));
    }
}

// The capped shape's want is the cap rider's own `SKIP + LIMIT` count.
#[test]
fn the_capped_want_counts_skip() {
    let graph = corpus_graph();
    decide(
        "MATCH p = (x:Person)-[:KNOWS*1..]-(y) WHERE x.ID = 1 \
         RETURN p SKIP 10 LIMIT 5",
        &graph,
        false,
        &Row::new(),
        |decision| {
            let contract = decision.unwrap_or_else(|e| panic!("SKIP+LIMIT engages: {e:?}"));
            assert_eq!(contract.consumer, Consumer::Capped { want: 15 });
        },
    );
}

// The structural declines the NFA compiler owns — its declines are this
// arm's declines, one shape each from the coverage corpus's out-of-
// vocabulary neighborhoods (the compiler's own suite pins each in
// detail: `nfa_tests`).
#[test]
fn declines_the_nfa_compiler_shapes() {
    let graph = corpus_graph();
    // A rel-predicate inner hop.
    declines_with(
        "MATCH p = (x)((a)-[r:KNOWS WHERE r.since < 2010]-(b)){1,}(y) RETURN p LIMIT 100",
        &graph,
        FrontierDecline::NfaShape,
    );
    // A state count above the 64-pair bitmask ceiling (a 65-hop round).
    let mut ceiling = String::from("MATCH p = (x)((a)");
    for _ in 0..65 {
        ceiling.push_str("-[:KNOWS]-()");
    }
    ceiling.push_str("){1,}(y) RETURN p LIMIT 100");
    declines_with(&ceiling, &graph, FrontierDecline::NfaShape);
    // A second top-level quantified element.
    declines_with(
        "MATCH p = (x:Person)-[:KNOWS*1..]-(m)-[:STUDY_AT*1..]-(y) \
         RETURN p LIMIT 100",
        &graph,
        FrontierDecline::NfaShape,
    );
    // A subpath variable on the quantified group is rejected by the front
    // (42N34) before it can reach the frontier; the compiler's own decline
    // for that IR shape is pinned in `nfa_tests::declines_subpath_variable`
    // (hand-built IR).
    // A pattern-level WHERE — per-path semantics. The predicate reads a
    // group-local variable, so the front's 42I21 path-variable rule does
    // not fire.
    declines_with(
        "MATCH p = ((x:Person)-[:KNOWS*1..]-(y) WHERE x.ID > 1) RETURN p LIMIT 100",
        &graph,
        FrontierDecline::NfaShape,
    );
    // Admission-bearing nodes: a labeled inner node, and a labeled
    // interior context node.
    declines_with(
        "MATCH p = (x)((a:Person)-[:KNOWS]-(b)){1,}(y) RETURN p LIMIT 100",
        &graph,
        FrontierDecline::NfaShape,
    );
    declines_with(
        "MATCH p = (x:Person)-[:KNOWS]-(m:Person)-[:KNOWS*1..]-(y) RETURN p LIMIT 100",
        &graph,
        FrontierDecline::NfaShape,
    );
    // A path selector — the GPM shortest machinery's contract.
    declines_with(
        "MATCH p = ANY SHORTEST (x:Person)-[:KNOWS*1..]-(y) RETURN p LIMIT 100",
        &graph,
        FrontierDecline::NfaShape,
    );
}

// The consumer declines: full enumeration in its spellings, ordered
// caps, path counts, and path-consuming projections.
#[test]
fn declines_full_enumeration_consumers() {
    let graph = corpus_graph();
    let base = "MATCH p = (x:Person)-[:KNOWS*1..]-(y:Person)";
    // Uncapped row emission — of the path, of the endpoint, or through a
    // passthrough WITH.
    declines_with(
        &format!("{base} RETURN p"),
        &graph,
        FrontierDecline::FullEnumeration,
    );
    declines_with(
        &format!("{base} RETURN y"),
        &graph,
        FrontierDecline::FullEnumeration,
    );
    declines_with(
        &format!("{base} WITH x, y RETURN x"),
        &graph,
        FrontierDecline::FullEnumeration,
    );
    // A filtered WITH.
    declines_with(
        &format!("{base} WITH x WHERE x.k > 1 RETURN x"),
        &graph,
        FrontierDecline::FullEnumeration,
    );
    // A grouped suffix (explicit GROUP BY).
    declines_with(
        &format!("{base} RETURN y GROUP BY y"),
        &graph,
        FrontierDecline::FullEnumeration,
    );
    // A suffix that is not a RETURN/WITH: the rows feed a write.
    declines_with(
        &format!("{base} CREATE (z:Z)"),
        &graph,
        FrontierDecline::FullEnumeration,
    );
    // LIMIT 0: the cap rider declines it too — keep its behavior.
    declines_with(
        &format!("{base} RETURN p LIMIT 0"),
        &graph,
        FrontierDecline::FullEnumeration,
    );
}

#[test]
fn declines_ordered_caps_and_path_counts() {
    let graph = corpus_graph();
    let base = "MATCH p = (x:Person)-[:KNOWS*1..]-(y:Person)";
    // ORDER BY + LIMIT needs the full candidate set to sort.
    declines_with(
        &format!("{base} RETURN p ORDER BY length(p) LIMIT 10"),
        &graph,
        FrontierDecline::OrderedCap,
    );
    declines_with(
        &format!("{base} RETURN p ORDER BY y.k LIMIT 10"),
        &graph,
        FrontierDecline::OrderedCap,
    );
    // Path counts and multiplicity-sensitive folds.
    for suffix in [
        "RETURN count(p)",
        "RETURN count(*)",
        "RETURN count(y)",
        "RETURN collect(y)",
        "RETURN sum(y.k)",
        "RETURN avg(y.k)",
    ] {
        declines_with(
            &format!("{base} {suffix}"),
            &graph,
            FrontierDecline::PathCount,
        );
    }
    // A distinct answer that consumes the path variable or a group list —
    // only the capped witnesses carry those bindings.
    declines_with(
        &format!("{base} RETURN DISTINCT p"),
        &graph,
        FrontierDecline::PathConsumer,
    );
    declines_with(
        &format!("{base} RETURN min(length(p))"),
        &graph,
        FrontierDecline::PathConsumer,
    );
    declines_with(
        "MATCH p = (x:Person)((a)-[rs:KNOWS]-(b)){1,}(y:Person) \
         RETURN DISTINCT rs",
        &graph,
        FrontierDecline::PathConsumer,
    );
}

// The context declines: a second pattern, a carrying row, a repeated
// (pinned) binding, and conjuncts outside the closed vocabulary.
#[test]
fn declines_the_context_gates() {
    let graph = corpus_graph();
    // Two patterns: a single-pattern family.
    declines_with(
        "MATCH p = (x:Person)-[:KNOWS*1..]-(y:Person), (z:Organisation) \
         RETURN p LIMIT 10",
        &graph,
        FrontierDecline::MultiplePatterns,
    );
    // A conjunct outside the closed predicate vocabulary: a function over
    // the row, two predicates on one boundary variable, and a predicate
    // naming an interior context node.
    declines_with(
        "MATCH (x:Person)-[:KNOWS]-(m)-[:KNOWS*1..]-(y:Person) \
         WHERE size(m.k) > 1 RETURN DISTINCT y",
        &graph,
        FrontierDecline::Conjunct,
    );
    declines_with(
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) WHERE y.k > 1 AND y.j < 2 \
         RETURN DISTINCT y",
        &graph,
        FrontierDecline::Conjunct,
    );
    declines_with(
        "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) WHERE size(x.k) > 1 \
         RETURN DISTINCT y",
        &graph,
        FrontierDecline::Conjunct,
    );
    // A repeated binding the pair-dedup cannot reconcile: the target
    // repeating the seed's variable, and a rel variable repeated across
    // context hops.
    declines_with(
        "MATCH p = (x:Person)-[:KNOWS*1..]-(x) RETURN p LIMIT 10",
        &graph,
        FrontierDecline::PinnedBinding,
    );
    declines_with(
        "MATCH p = (x:Person)-[r:KNOWS]-()-[r:STUDY_AT]-()-[:KNOWS*1..]-(y) \
         RETURN p LIMIT 10",
        &graph,
        FrontierDecline::PinnedBinding,
    );
}

// An inner node variable colliding with an inner rel variable inside one
// quantified group: the same pre-bound reconciliation the top-level
// repeats carry, hiding inside the group vocabulary the parser never
// builds — no Cypher spelling binds one name as both, so the pin is
// hand-built IR (the `nfa_tests` constructors).
#[test]
fn declines_an_inner_node_rel_name_collision() {
    let graph = corpus_graph();
    let rel_a = RelPattern {
        variable: Some("a".into()),
        ..one_hop_rel(&["KNOWS"])
    };
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(named_node("a")),
                PatternElement::Relationship(rel_a),
                PatternElement::Node(named_node("b")),
            ],
            1,
            None,
        )),
        PatternElement::Node(named_node("y")),
    ]);
    let params: BTreeMap<String, Value> = BTreeMap::new();
    let stats = GraphStats::new(&graph, &params);
    let patterns = [pattern];
    let decision = compile_frontier(
        &graph,
        &patterns,
        MatchMode::DifferentRelationships,
        &Row::new(),
        &[],
        Consumer::Exists,
        &stats,
    );
    assert_eq!(decision.err(), Some(FrontierDecline::PinnedBinding));
}

// The entering row may bind exactly the seed-side variable (the
// single-seed frontier's premise); anything else declines — a prebound
// group-list pin, an outer variable.
#[test]
fn the_row_gate_binds_only_the_seed() {
    let graph = corpus_graph();
    let cypher = "MATCH (x:Person)-[:KNOWS*1..]-(y:Person) RETURN DISTINCT y";
    // An outer binding keeps the per-row walker's semantics.
    let carrying = Row::from_iter(vec![("outer".to_string(), Value::Null)]);
    decide(cypher, &graph, false, &carrying, |decision| {
        assert_eq!(decision.err(), Some(FrontierDecline::CarryingRow));
    });
    // A prebound group-list pin (the legacy exact-trail rule's shape).
    let pinned = Row::from_iter(vec![("rs".to_string(), Value::Null)]);
    decide(
        "MATCH p = (x:Person)((a)-[rs:KNOWS]-(b)){1,}(y:Person) RETURN DISTINCT y",
        &graph,
        false,
        &pinned,
        |decision| {
            assert_eq!(decision.err(), Some(FrontierDecline::CarryingRow));
        },
    );
    // The seed-side binding itself is the premise — allowed.
    let seeded = Row::from_iter(vec![("x".to_string(), Value::Null)]);
    decide(cypher, &graph, false, &seeded, |decision| {
        assert!(
            decision.is_ok(),
            "the seed binding rides: {:?}",
            decision.err()
        );
    });
}

// The allReduce conflict (ADR-0478 §2's hard decline): a MATCH-WHERE
// allReduce the inliner runs over one of the quantification's groups —
// the per-branch prefix semantics cannot survive pair dedup.
#[test]
fn declines_the_allreduce_inlined_group() {
    let b = Builder::new();
    let a = b.node("a", &["Geo"]);
    let c = b.node("c", &["Geo"]);
    let d = b.node("d", &["Geo"]);
    b.rel(a, c, "KNOWS", &[("k", Value::Float(1.0))]);
    b.rel(c, d, "KNOWS", &[("k", Value::Float(2.0))]);
    let graph = b.done();
    declines_with(
        "MATCH p = (a:Geo)((n)-[rels:KNOWS]-(m)){1,3}(b:Geo) \
         WHERE allReduce(acc = 0.0, r IN rels | acc + r.k, acc < 10) \
         RETURN p LIMIT 10",
        &graph,
        FrontierDecline::AllReduceGroup,
    );
    // Without the allReduce riding the MATCH WHERE there is no inlined
    // spec over the group, and the same shape engages (unbounded — cost
    // rule (a); the group list is consumed only through the capped
    // witnesses).
    assert!(engages(
        "MATCH p = (a:Geo)((n)-[rels:KNOWS]-(m)){1,}(b:Geo) RETURN p LIMIT 10",
        &graph,
    ));
}
