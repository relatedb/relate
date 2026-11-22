//! The kernel ratio guard (REL8-1969): every traversal kernel must beat
//! the stood-down general engine by [`MIN_RATIO`] or more on its own
//! engaging shape, or it is dead weight the repo still maintains and
//! reviews. Each case runs its statement twice — the default routing
//! (the kernel) and the stood-down lane ([`walk_only`]'s twin graph, the
//! `RELATE_PATTERN_ENGINE=walk` switch's in-process spelling: the chain,
//! hub-star and frontier kernels all stand down, the per-row walker
//! serves) — and asserts three things:
//!
//! 1. **Engagement**: the kernel took the clause on the default lane
//!    (exactly one engagement — the case's shape is the kernel's own)
//!    and nothing on the stood-down lane. Without this pin a routing
//!    regression turns the case into a vacuous A/A comparison.
//! 2. **Parity**: both lanes answer the same rows (a correctness
//!    cross-check for free).
//! 3. **Ratio**: the stood-down lane's median ([`percentile`] at 0.50)
//!    is at least [`MIN_RATIO`] times the default lane's median, over
//!    interleaved timed rounds so both lanes share whatever drift the
//!    host adds mid-test. The ratio cancels CI-host variance inside the
//!    pair; an absolute bound cannot (the house style's generous
//!    absolute bounds guard *stalls*, not crossovers).
//!
//! When general-engine work (a faster walker, a better rung) closes a
//! gap, this guard fails loudly and the decision becomes retire-or-widen
//! instead of a silent crossover. Every new traversal kernel joins this
//! suite in the PR it lands in. The floor is raised, never lowered,
//! without numbers.
//!
//! **Measured at landing** (this box, `cargo test`'s optimized test
//! profile): chain count fold 51x, hub-star 24-28x, frontier count
//! ~2500x — every case clears the floor with margin. Two shapes were
//! measured and deliberately **excluded**, with their numbers on the
//! ticket: the chain kernel's two-hop *rows* shape pays only 1.34-1.50x
//! (at the crossover — a retire-or-widen finding, not a guardable case;
//! a case asserting 1.5x there would flake on host noise), and the batch
//! scan-filter chain pays 0.92-1.0x standalone (it is the substrate the
//! traversal kernels ride, not a standalone winner over the per-row scan
//! — its correctness differential stays in `exec/batch_tests.rs`).

use std::time::{Duration, Instant};

use relate_test_support::percentile;

use crate::exec::test_support::{exec_cypher, layered, same_rows, star, walk_only};
use crate::graph::Graph;

use super::take_kernel_engagements;

/// The ratio floor: a kernel paying less than 1.5x over the stood-down
/// lane on its own engaging shape is not paying for its maintenance.
/// Raise with numbers, never lower.
const MIN_RATIO: f64 = 1.5;

/// One lane of a guard case: its graph and its timed samples. The two
/// lanes live in one table — lane 0 the default routing, lane 1 its
/// stood-down twin — so every step below runs per lane and no future
/// change can land on one lane and be missed on the other.
struct Lane {
    graph: Graph,
    samples: Vec<Duration>,
}

/// One guard case: the engagement and parity pins, then the interleaved
/// A/B medians. One warmup pass pages both fixtures in before `rounds`
/// timed pairs, each pair running lane 0 first so the pair's two
/// measurements share one moment of host state.
fn guard_case(fixture: impl Fn() -> Graph, cypher: &str, rounds: usize) {
    let mut lanes = [
        Lane {
            graph: fixture(),
            samples: Vec::with_capacity(rounds),
        },
        Lane {
            graph: walk_only(fixture()),
            samples: Vec::with_capacity(rounds),
        },
    ];
    // The lanes named once for the pins: the default routing's graph and
    // its stood-down twin's.
    let [default, stood_down] = &lanes;
    let default_rows = exec_cypher(&default.graph, cypher);
    assert_eq!(
        take_kernel_engagements(),
        1,
        "the kernel engages the case's shape"
    );
    let stood_down_rows = exec_cypher(&stood_down.graph, cypher);
    assert_eq!(
        take_kernel_engagements(),
        0,
        "the stood-down lane serves nothing"
    );
    assert!(
        same_rows(&default_rows, &stood_down_rows),
        "both lanes answer the same rows"
    );
    for lane in &mut lanes {
        exec_cypher(&lane.graph, cypher);
    }
    for _ in 0..rounds {
        for lane in &mut lanes {
            let start = Instant::now();
            exec_cypher(&lane.graph, cypher);
            lane.samples.push(start.elapsed());
        }
    }
    // The lanes named once for the medians: the kernel's and the
    // stood-down lane's, the fixed arity the table already carries.
    let [kernel_median, walked_median] = lanes.map(|lane| percentile(&lane.samples, 0.50));
    let ratio = walked_median.as_secs_f64() / kernel_median.as_secs_f64();
    eprintln!("kernel {kernel_median:?} vs stood-down {walked_median:?} (ratio {ratio:.2})");
    assert!(
        ratio >= MIN_RATIO,
        "the kernel pays only {ratio:.2}x over the stood-down lane \
         (kernel {kernel_median:?}, stood-down {walked_median:?}) — retire or widen it"
    );
}

/// The chain kernel's count fold (ADR-0473, LABS-1293): a starred exact
/// hop count over a width-60 fan is 216,000 trails the fold tallies
/// columnar while the walker emits one row each.
#[test]
fn chain_count_beats_the_stood_down_walker() {
    guard_case(
        || layered(3, 60),
        "MATCH (s:N {id: 0})-[:R*3]->(t) RETURN count(t) AS c",
        7,
    );
}

/// The hub-star kernel (ADR-0473): a shared-terminal star whose per-hub
/// combination is one reused buffer against the walker's per-row walks.
#[test]
fn hub_star_beats_the_stood_down_walker() {
    guard_case(
        || star(400, 40),
        "MATCH (h:H)-[:A]->(t:T), (h)-[:B]->(t) RETURN count(*) AS c",
        7,
    );
}

/// The frontier family's count contraction (ADR-0478): a nine-level
/// layered graph is 349,525 walks the stood-down walker emits one row
/// each and the contraction never enumerates — the widest gap the guard
/// carries, so three rounds suffice.
#[test]
fn frontier_count_beats_the_stood_down_walker() {
    guard_case(
        || layered(9, 4),
        "MATCH REPEATABLE ELEMENTS (s:N {id: 0})-[:R*1..9]->(d) \
         RETURN count(d) AS walks",
        3,
    );
}
