//! The chain count fold's differential suite (LABS-1293): every statement
//! runs through the differential core — the kernel path and the kernels-
//! stood-down walker must answer identically, and the statement asserts
//! its own engagement — over one adversarial multigraph: self-loops at
//! the start, middle and end, parallel same-type and cross-type edges,
//! reciprocal pairs, a hub, unlabelled nodes.
//!
//! The engagement each statement expects is stated per class below, and
//! every class names the gate that decides it — so a silent decline, or a
//! rung quietly taking a shape it must not, fails one assertion instead
//! of hiding inside an aggregate.

use relate_core::Value;

use crate::graph::Graph;

use super::super::kernels::matcher::MAX_CHAIN_HOPS;
use super::super::test_support::Builder;
use super::differential;

fn adversarial_graph() -> Graph {
    let b = Builder::new();
    let a = b.node("a", &["U"]);
    let n: Vec<_> = (0..7).map(|i| b.node(&format!("n{i}"), &["U"])).collect();
    let bare = b.node("bare", &[]);
    let hub = b.node("hub", &["U", "Hub"]);
    // Self-loops at the start, a middle node and an end node.
    b.rel(a, a, "F", &[]);
    b.rel(n[1], n[1], "F", &[]);
    b.rel(n[3], n[3], "F", &[]);
    // Parallel same-type and cross-type edges.
    b.rel(a, n[0], "F", &[]);
    b.rel(a, n[0], "F", &[]);
    b.rel(a, n[0], "G", &[]);
    // Reciprocal pairs.
    b.rel(n[0], n[1], "F", &[]);
    b.rel(n[1], n[0], "F", &[]);
    b.rel(n[1], n[2], "F", &[]);
    b.rel(n[2], n[3], "G", &[]);
    b.rel(n[3], n[4], "F", &[]);
    b.rel(n[4], a, "F", &[]);
    // Unlabelled node and the hub.
    b.rel(n[2], bare, "F", &[]);
    b.rel(bare, n[3], "F", &[]);
    for m in &n {
        b.rel(*m, hub, "F", &[]);
        b.rel(hub, *m, "F", &[]);
    }
    b.rel(a, hub, "F", &[]);
    b.done()
}

/// One step spelling as the sweep states it, with the engagement its
/// queries assert: the single-hop form (repeated `k` times) and the
/// starred form (with `{k}` where the hop count goes) — both stated,
/// never derived from one another — plus whether the count fold unrolls
/// the starred form, and the first `k` at which the spelled form engages
/// a kernel (`None` when no `k` does; a two-hop count rides the path
/// aggregate fold, which is no kernel).
struct Step {
    hop: &'static str,
    star: &'static str,
    star_engages: bool,
    spelled_engages_from: Option<usize>,
}

/// The directed single-hop steps — the family the count fold's own gates
/// accept (a static type list or none, and a directed hop whose trail
/// corrections the tally does not need). Spelled out, they engage from
/// three hops: at two the path aggregate fold answers the count without a
/// kernel.
const DIRECTED: [Step; 4] = [
    Step {
        hop: "-[:F]->",
        star: "-[:F*{k}]->",
        star_engages: true,
        spelled_engages_from: Some(3),
    },
    Step {
        hop: "<-[:F]-",
        star: "<-[:F*{k}]-",
        star_engages: true,
        spelled_engages_from: Some(3),
    },
    Step {
        hop: "-[:G]->",
        star: "-[:G*{k}]->",
        star_engages: true,
        spelled_engages_from: Some(3),
    },
    Step {
        hop: "-->",
        star: "-[*{k}]->",
        star_engages: true,
        spelled_engages_from: Some(3),
    },
];

/// An undirected hop: the count fold declines it (its trail corrections
/// are the walker's) and nothing takes the starred form, but spelled out
/// at three-plus hops the chain **rows** kernel takes the clause — a
/// kernel engagement all the same.
const UNDIRECTED: Step = Step {
    hop: "-[:F]-",
    star: "-[:F*{k}]-",
    star_engages: false,
    spelled_engages_from: Some(3),
};

/// A disjunctive type list: not the static conjunction the compile's type
/// proof requires, so every rung declines it, spelled or starred.
const DISJUNCTIVE: Step = Step {
    hop: "-[:F|G]->",
    star: "-[:F|G*{k}]->",
    star_engages: false,
    spelled_engages_from: None,
};

/// Every step class the sweep spells, one population.
fn all_steps() -> impl Iterator<Item = &'static Step> {
    DIRECTED.iter().chain([&UNDIRECTED, &DISJUNCTIVE])
}

const LABELLED_HEADS: [&str; 2] = ["(a:U {id:'a'})", "(a:U)"];
const BARE_HEAD: &str = "(a)";
const BARE_TAIL: &str = "(b)";
const LABELLED_TAILS: [&str; 2] = ["(b:U)", "(b:Hub)"];

fn spelled(head: &str, tail: &str, step: &Step, k: usize) -> String {
    format!(
        "MATCH {head}{}{tail} RETURN count(b)",
        vec![step.hop; k].join("()")
    )
}

fn starred(head: &str, tail: &str, step: &Step, k: usize) -> String {
    format!(
        "MATCH {head}{}{tail} RETURN count(b)",
        step.star.replace("{k}", &k.to_string())
    )
}

impl Step {
    /// Whether the spelled-out form at `k` hops engages a kernel — the
    /// one spelling of the step's policy, consulted by every sweep.
    fn spelled_engages(&self, k: usize) -> bool {
        self.spelled_engages_from.is_some_and(|from| k >= from)
    }
}

#[test]
fn chain_count_matches_the_walker_on_adversarial_shapes() {
    let g = adversarial_graph();
    let mut nonzero_answers = 0usize;
    // One sweep over every step class: each statement asserts its own
    // engagement from the step's stated policy, and the two spellings of
    // one step denote the same query, so their answers tie — the tie is
    // what binds each `Step`'s `hop` and `star` to one edge
    // specification.
    for head in LABELLED_HEADS {
        for tail in std::iter::once(BARE_TAIL).chain(LABELLED_TAILS) {
            for step in all_steps() {
                for k in 2..=MAX_CHAIN_HOPS {
                    let starred_rows =
                        differential(&g, &starred(head, tail, step, k), step.star_engages).rows;
                    nonzero_answers +=
                        usize::from(step.star_engages && starred_rows != vec![vec![Value::Int(0)]]);
                    let spelled_query = spelled(head, tail, step, k);
                    let spelled_rows =
                        differential(&g, &spelled_query, step.spelled_engages(k)).rows;
                    assert_eq!(spelled_rows, starred_rows, "{spelled_query}");
                }
            }
        }
    }
    // A bare scan head with a labelled tail: the cost gate's estimate
    // from an unlabelled start still prices the spelled chain by the
    // step's own policy — the labelled tail re-prices it under the stage
    // ceiling — so the expectation stays the step's, derived, never
    // re-stated.
    for tail in LABELLED_TAILS {
        for step in all_steps() {
            for k in 2..=MAX_CHAIN_HOPS {
                let engages = step.spelled_engages(k);
                differential(&g, &spelled(BARE_HEAD, tail, step, k), engages);
            }
        }
    }
    // A bare scan head with a bare tail: the cost gate's estimate from an
    // unlabelled start declines every chain outright — the block's one
    // fact, stated once here, overriding the step policies above.
    for step in all_steps() {
        for k in 2..=MAX_CHAIN_HOPS {
            differential(&g, &starred(BARE_HEAD, BARE_TAIL, step, k), false);
            differential(&g, &spelled(BARE_HEAD, BARE_TAIL, step, k), false);
        }
    }
    // The sweep is not vacuous: the engaging shapes answer over real paths.
    assert!(nonzero_answers > 0, "the sweep never answered nonzero");
}

#[test]
fn chain_count_engages_on_count_star() {
    // The one engaging shape outside the sweep's `count(<var>)`
    // vocabulary.
    let g = adversarial_graph();
    let q = "MATCH (a:U {id:'a'})-[:F*2]->(b) RETURN count(*)";
    let result = differential(&g, q, true);
    assert_ne!(result.rows, vec![vec![Value::Int(0)]], "vacuous: {q}");
}

#[test]
fn chain_count_declines_what_it_cannot_prove() {
    let g = adversarial_graph();
    for q in [
        // ranged, named and over-long spellings keep the walker
        "MATCH (a:U {id:'a'})-[:F*2..3]->(b) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[r:F*3]->(b) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[:F*5]->(b) RETURN count(b)",
        // a zero-length hop has no fixed-hop rendering: unrolling it
        // would drop its end node, so the spelling keeps the walker
        "MATCH (a:U {id:'a'})-[:F*0]->(b) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[:F*0]->(m:U)-[:G]->()-[:H]->(b) RETURN count(b)",
        // one fixed hop beside an over-long `*k` unrolls past the chain
        // bound a lone `*k` does not
        "MATCH (a:U {id:'a'})-[:F]->(b)-[:G*4]->(c) RETURN count(c)",
    ] {
        differential(&g, q, false);
    }
}
