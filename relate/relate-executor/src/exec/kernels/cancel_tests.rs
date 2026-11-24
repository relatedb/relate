//! The contracted kernels observe a cancelled statement inside their inner
//! loops, not only between outer units (ADR-0111): one predecessor row can
//! land thousands of successors, and the hub-star pairing loop is quadratic
//! in one hub's sides. Each test runs the kernel directly on a graph whose
//! expensive phase sits under a single outer unit, with the token already
//! cancelled and fewer than one sampling window of outer ticks — so an
//! unpatched kernel runs to its end and returns rows.

use std::collections::BTreeMap;

use relate_core::{CancelToken, Error, Result};
use relate_ir::{Clause, MatchMode};

use crate::ctx::ExecCtx;
use crate::exec::test_support::{clauses_of, Builder};
use crate::graph::Graph;
use crate::metrics::KernelRung;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::pushdown::Residual;
use super::{matcher, run, KernelMode, KernelOutput};

/// Run `clauses`' single `MATCH` through its contracted kernel with an
/// already-cancelled token — the kernel entry, never the statement, so the
/// only cancellation the run can observe is the one its own loops sample.
/// `mode` is the run's own mode (rows or existence); `expect` is the rung the
/// shape must compile to, so a test cannot silently exercise the other
/// kernel.
fn cancelled_kernel_run(
    clauses: &[Clause],
    graph: &Graph,
    mode: KernelMode<'_>,
    expect: KernelRung,
) -> Result<KernelOutput> {
    let token = CancelToken::new();
    token.cancel();
    let view = ExecGraph::read(graph).with_cancel(Some(&token));
    let params = BTreeMap::new();
    let ctx = ExecCtx::new(&view, &params);
    let Clause::Match { patterns, .. } = &clauses[0] else {
        panic!("the test statement opens on a MATCH");
    };
    let contract = matcher::compile(
        patterns,
        MatchMode::DifferentRelationships,
        &Row::new(),
        &[],
    )
    .expect("the shape is a kernel contract");
    assert_eq!(
        contract.rung(),
        expect,
        "the shape compiles to the expected rung"
    );
    run(ctx, &contract, &Residual::empty(), mode)
}

/// The typed pin: a cancelled kernel run fails with [`Error::Cancelled`], not
/// an `Execution` error whose text happens to read "query cancelled" — the
/// same contract `relate/tests/cancellation.rs`'s helper states.
fn assert_kernel_cancelled(output: Result<KernelOutput>, kernel: &str) {
    match output {
        Err(error) => assert!(
            matches!(error, Error::Cancelled),
            "{kernel}: expected the typed Error::Cancelled, got {error:?}"
        ),
        Ok(_) => panic!("{kernel}: a cancelled statement ran to the end of the kernel"),
    }
}

/// A two-hop chain over one start row: the first hop lands two mid nodes,
/// each of which fans out to 2 000 leaves. The whole 4 000-row expansion
/// happens under two outer rows, so unless the per-candidate loop samples
/// the token the statement returns its rows despite the cancel.
fn chain_fan() -> Graph {
    let b = Builder::new();
    let start = b.node("s", &["P"]);
    let m1 = b.node("m1", &["M"]);
    let m2 = b.node("m2", &["M"]);
    b.rel(start, m1, "E", &[]);
    b.rel(start, m2, "E", &[]);
    for _ in 0..2_000 {
        let leaf = b.anon(&["L"]);
        b.rel(m1, leaf, "E", &[]);
        b.rel(m2, leaf, "E", &[]);
    }
    b.done()
}

/// The chain corpus: its projection is also the existence mode's counted read
/// (`n.id`), so the counted expression and the compiled pattern come from the
/// same statement.
const CHAIN_CYPHER: &str = "MATCH (s:P)-[:E]->(m:M)-[:E]->(n:L) RETURN n.id AS id";

#[test]
fn the_chain_kernel_observes_a_cancel_inside_a_rows_adjacency() {
    let clauses = clauses_of(CHAIN_CYPHER);
    let output = cancelled_kernel_run(
        &clauses,
        &chain_fan(),
        KernelMode::Rows { want: None },
        KernelRung::Chain { hops: 2 },
    );
    assert_kernel_cancelled(output, "chain");
}

/// The existence mode's scan is one pass over the same stage: the counted
/// property is absent from every leaf, so the answer is `false` only after
/// the whole stage is visited — and a cancelled statement must stop inside
/// the scan instead.
#[test]
fn the_chain_kernel_observes_a_cancel_inside_an_existence_scan() {
    let clauses = clauses_of(CHAIN_CYPHER);
    let Clause::Return { items, .. } = &clauses[1] else {
        panic!("the counted expression's clause");
    };
    let output = cancelled_kernel_run(
        &clauses,
        &chain_fan(),
        KernelMode::Exists {
            counted: Some(&items[0].expr),
        },
        KernelRung::Chain { hops: 2 },
    );
    assert_kernel_cancelled(output, "chain-exists");
}

/// The open hub-star's quadratic phase: one hub, 40 side-A rows and 40
/// side-B rows, one outer (hub) unit and 1 600 pairings under it. The
/// per-hub tick alone never reaches the sampling window.
const HUB_CYPHER: &str = "MATCH (a:A)-[:X]->(h:Hub), (h)-[:Y]->(b:B) RETURN h";

#[test]
fn the_hub_star_kernel_observes_a_cancel_inside_its_pairing_loop() {
    let b = Builder::new();
    let hub = b.node("h", &["Hub"]);
    for _ in 0..40 {
        let a = b.anon(&["A"]);
        b.rel(a, hub, "X", &[]);
    }
    for _ in 0..40 {
        let bb = b.anon(&["B"]);
        b.rel(hub, bb, "Y", &[]);
    }
    let graph = b.done();
    let clauses = clauses_of(HUB_CYPHER);
    let output = cancelled_kernel_run(
        &clauses,
        &graph,
        KernelMode::Rows { want: None },
        KernelRung::HubStar,
    );
    assert_kernel_cancelled(output, "hub-star");
}
