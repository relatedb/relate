//! The memoised admission's differential pins (LABS-1478): a graph large
//! enough to pass the memo's warm-up and to span several bitmap pages, with
//! labels that hit, miss and overlap, answered engaged (memoised) and with
//! the kernels stood down (the per-row walker). The two must be equal, and
//! the engaged leg must report an engagement so a silent decline cannot pass.

use crate::exec::test_support::{exec_cypher, Builder};
use crate::graph::Graph;

use super::{take_kernel_engagements, with_kernels_disabled};

/// More nodes than the warm-up has checks, over several 4096-id pages.
const NODES: usize = 9_000;

fn wide_graph() -> Graph {
    let b = Builder::new();
    let nodes: Vec<_> = (0..NODES)
        .map(|i| {
            let labels: &[&str] = match i % 6 {
                0 => &["U", "Hub"],
                1 | 2 => &["U"],
                3 => &["V"],
                4 => &["U", "V"],
                _ => &[],
            };
            b.node(&format!("n{i}"), labels)
        })
        .collect();
    for (i, &from) in nodes.iter().enumerate() {
        for (step, ty) in [(1, "F"), (7, "F"), (13, "G"), (NODES / 2 + 1, "F")] {
            b.rel(from, nodes[(i * step + 1) % NODES], ty, &[]);
        }
        // A self-loop and a parallel edge now and then.
        if i % 11 == 0 {
            b.rel(from, from, "F", &[]);
            b.rel(from, nodes[(i + 1) % NODES], "F", &[]);
        }
    }
    b.done()
}

#[test]
fn memoised_chain_count_matches_the_walker_past_the_warm_up() {
    let g = wide_graph();
    let mut engaged_queries = 0;
    for tail in ["(b)", "(b:U)", "(b:Hub)", "(b:U:V)", "(b:V)", "(b:Missing)"] {
        for steps in [
            "-[:F]->()-[:F]->",
            "-[:F|G]->()-[:F]->()-[:G]->",
            "-[:F]->()-[:F]->()-[:F]->",
        ] {
            let q = format!("MATCH (a:U){steps}{tail} RETURN count(*)");
            let _ = take_kernel_engagements();
            let engaged = exec_cypher(&g, &q).rows;
            let hits = take_kernel_engagements();
            let walker = with_kernels_disabled(|| exec_cypher(&g, &q).rows);
            assert_eq!(engaged, walker, "{q}");
            engaged_queries += usize::from(hits > 0);
        }
    }
    assert!(engaged_queries > 0, "no query engaged the kernel");
}

#[test]
fn memoised_rows_hop_matches_the_walker_past_the_warm_up() {
    let g = wide_graph();
    for q in [
        "MATCH (a:Hub)-[:F]->(x:U)-[:F]->(b:V) RETURN count(*), count(DISTINCT b)",
        "MATCH (a:Hub)-[:F]->(x:U)-[:F|G]->(b:U:V) RETURN count(*), count(DISTINCT x)",
        "MATCH (a:V)-[:F]->(x)-[:G]->(b:Hub) RETURN count(*), count(DISTINCT a)",
    ] {
        let engaged = exec_cypher(&g, q).rows;
        let walker = with_kernels_disabled(|| exec_cypher(&g, q).rows);
        assert_eq!(engaged, walker, "{q}");
    }
}
