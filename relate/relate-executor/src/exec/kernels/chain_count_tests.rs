//! The chain count fold's differential pins (LABS-1293): every statement
//! runs engaged and with the kernels stood down (the per-row walker) over
//! one adversarial multigraph — self-loops at the start, middle and end,
//! parallel same-type and cross-type edges, reciprocal pairs, a hub,
//! unlabelled nodes — and the two answers must be equal. The engaged leg
//! must also report an engagement, so a silent decline cannot pass.

use relate_core::Value;

use crate::exec::test_support::{exec_cypher, Builder};
use crate::graph::Graph;

use super::{take_kernel_engagements, with_kernels_disabled};

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

fn answers(cypher: &str) -> (Vec<Vec<Value>>, u32, Vec<Vec<Value>>) {
    let g = adversarial_graph();
    let _ = take_kernel_engagements();
    let engaged = exec_cypher(&g, cypher).rows;
    let hits = take_kernel_engagements();
    let walker = with_kernels_disabled(|| exec_cypher(&g, cypher).rows);
    (engaged, hits, walker)
}

#[test]
fn chain_count_matches_the_walker_on_adversarial_shapes() {
    let mut engaged_shapes = 0usize;
    let heads = ["(a:U {id:'a'})", "(a:U)", "(a)"];
    let tails = ["(b)", "(b:U)", "(b:Hub)"];
    let steps = [
        "-[:F]->",
        "-[:F|G]->",
        "-[:F]-",
        "<-[:F]-",
        "-[:G]->",
        "-->",
    ];
    for head in heads {
        for tail in tails {
            for step in steps {
                for k in 2..=super::matcher::MAX_CHAIN_HOPS {
                    let spelled = format!(
                        "MATCH {head}{}{tail} RETURN count(*)",
                        vec![step; k].join("()")
                    );
                    let star = if step == "-->" {
                        format!("-[*{k}]->")
                    } else {
                        step.replacen(']', &format!("*{k}]"), 1)
                    };
                    let starred = format!("MATCH {head}{star}{tail} RETURN count(b)");
                    for q in [&spelled, &starred] {
                        let (engaged, hits, walker) = answers(q);
                        assert_eq!(engaged, walker, "{q}");
                        assert!(hits <= 1, "{q}");
                        engaged_shapes += usize::from(hits == 1);
                    }
                }
            }
        }
    }
    // Directed single-type shapes engage; undirected ones decline to the
    // walker, so the sweep must still see engagements.
    assert!(engaged_shapes > 0, "the sweep never engaged the fold");
}

#[test]
fn chain_count_engages_on_plain_and_starred_spellings() {
    for q in [
        "MATCH (a:U {id:'a'})-[:F]->()-[:F]->()-[:F]->(b:U) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[:F*3]->(b:U) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[:F*2]->(b) RETURN count(*)",
    ] {
        let (engaged, hits, walker) = answers(q);
        assert_eq!(engaged, walker, "{q}");
        assert_ne!(engaged, vec![vec![Value::Int(0)]], "vacuous: {q}");
        assert_eq!(hits, 1, "{q}");
    }
}

#[test]
fn chain_count_declines_what_it_cannot_prove() {
    for q in [
        // ranged, named and over-long spellings keep the walker
        "MATCH (a:U {id:'a'})-[:F*2..3]->(b) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[r:F*3]->(b) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[:F*5]->(b) RETURN count(b)",
        "MATCH (a:U {id:'a'})-[:F*3]-(b) RETURN count(b)",
    ] {
        let (engaged, hits, walker) = answers(q);
        assert_eq!(engaged, walker, "{q}");
        assert_eq!(hits, 0, "{q}");
    }
}
