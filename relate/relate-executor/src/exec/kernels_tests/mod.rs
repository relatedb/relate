//! ADR-0473's contracted kernels (LABS-644): the differential contract —
//! every query a kernel takes answers exactly what the per-row path
//! answers — plus the engagement gate's declines, the relationship-
//! uniqueness rules, the null semantics, and the error parity. The
//! openCypher TCK remains the wide net; these pin the kernels' own
//! behaviors, one suite per family.
//!
//! Rows compare as multisets: the kernels' emit order is their own
//! (hub-major, stage-major), the per-row walker's is its walk's, and every
//! consumer above this layer — the TCK's any-order comparisons, the
//! latency suite's answer checks — is order-insensitive by contract. The
//! columns compare exactly.
//!
//! Aggregate returns route to the path aggregate fold before the `MATCH`
//! arm (q19's route), so the rows-mode differentials return plain
//! bindings under `LIMIT` and the existence differentials ride the
//! boolean-count fold — the two roads that reach the kernels.

//! One file per family: the chain, chain-count, chain-gate and hub-star
//! differential suites, the LABS-990 halt suite, the semantics suites,
//! and the engagement gate's suites — this module keeps the shared
//! populations and the differential core.

mod chain;
mod chain_count;
mod chain_gate;
mod gate;
mod halt;
mod hub_star;
mod semantics;

use relate_core::Result;

use crate::graph::Graph;

use super::kernels::{take_kernel_engagements, with_kernels_disabled};
use super::test_support::{error_of, exec_autocommit, same_rows, try_exec_cypher, Builder};

/// The populations the families run over: a chain corridor (Alpha → Beta
/// → Gamma over R and S), hubs with typed spokes (A, B) into tags, and a
/// hub whose C spoke reaches a tag through a mid node (the 2-hop side).
pub(super) fn families() -> Graph {
    let g = Builder::new().done();
    exec_autocommit(
        &g,
        "CREATE (a1:Alpha {k: 1, s: 'ash'}),
               (a2:Alpha {k: 2}),
               (a3:Alpha {k: 3, s: 'elm'}),
               (b1:Beta {k: 10}),
               (b2:Beta {k: 20}),
               (g1:Gamma {name: 'one'}),
               (g2:Gamma {name: 'two'}),
               (h1:Hub {k: 5}),
               (h2:Hub {k: 6}),
               (t1:Tag {name: 'x'}),
               (t2:Tag {name: 'y'}),
               (m1:Mid {k: 1}),
               (m2:Mid {k: 2}),
               (a1)-[:R]->(b1), (a2)-[:R]->(b2), (a3)-[:R]->(b1),
               (b1)-[:S]->(g1), (b2)-[:S]->(g2), (b1)-[:S]->(g2),
               (h1)-[:A]->(t1), (h1)-[:A]->(t2), (h1)-[:B]->(t1),
               (h2)-[:A]->(t2), (h2)-[:B]->(t2),
               (h1)-[:C]->(m1)-[:D]->(t2),
               (h2)-[:C]->(m2)-[:D]->(t1)",
    )
    .unwrap();
    g
}

/// Whether a differential expects a kernel to take the statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Engagement {
    /// A kernel takes the statement exactly once.
    Engaged,
    /// No kernel takes it (a gate decline).
    Declined,
}

impl Engagement {
    /// The kernel-engagement count this expectation asserts.
    fn count(self) -> u32 {
        match self {
            Engagement::Engaged => 1,
            Engagement::Declined => 0,
        }
    }
}

/// The differential: `cypher` must answer identically with the kernels on
/// and forced off, and (when [`Engagement::Engaged`]) a kernel must have taken it exactly
/// once. Returns the kernel-path result.
pub(super) fn differential(g: &Graph, cypher: &str, engages: Engagement) -> super::QueryResult {
    differential_with(cypher, engages, || try_exec_cypher(g, cypher))
}

/// The differential core, over any run: `run` is the whole statement
/// execution (a parameterized run, a run on an open transaction's scratch),
/// done once with the kernels on and once forced off; `label` names the
/// query in a failure. A run that errors panics naming the query (the
/// helper's contract is to assert — a `Result` return advertised a failure
/// mode no caller acted on; the error comparison has its own helper,
/// [`error_differential`]).
pub(super) fn differential_with(
    label: &str,
    engages: Engagement,
    run: impl Fn() -> Result<super::QueryResult>,
) -> super::QueryResult {
    // The run first, the counter taken on both outcomes: an errored
    // kernel run must not leave its engagement count for a later assert
    // to read.
    let ran = run();
    let engagements = take_kernel_engagements();
    let kernaled = ran.unwrap_or_else(|error| panic!("the differential runs {label}: {error}"));
    assert_eq!(
        engagements,
        engages.count(),
        "engagement for {label}: expected {}, took {}",
        engages.count(),
        engagements
    );
    let per_row = with_kernels_disabled(run)
        .unwrap_or_else(|error| panic!("the differential runs {label}: {error}"));
    assert_eq!(
        kernaled.columns, per_row.columns,
        "columns diverge for {label}"
    );
    // The multiset comparison through the one spelling —
    // `test_support::same_rows`, never a local sort (F-3-4).
    assert!(same_rows(&kernaled, &per_row), "rows diverge for {label}");
    kernaled
}

/// The error differential: `cypher` errors on the kernel path and on the
/// per-row path with the same message, and a kernel took it (a gate that
/// stopped engaging the shape would silently turn both sides into the
/// per-row path and the parity assert vacuous). The engagement take is also
/// the drain: the errored run's count must not outlive the test. Returns
/// the message.
pub(super) fn error_differential(g: &Graph, cypher: &str) -> String {
    let kernaled = error_of(try_exec_cypher(g, cypher));
    assert_eq!(
        take_kernel_engagements(),
        1,
        "the kernel path takes {cypher}"
    );
    let per_row = with_kernels_disabled(|| error_of(try_exec_cypher(g, cypher)));
    assert_eq!(kernaled, per_row, "errors diverge for {cypher}");
    kernaled
}
