//! The fan-cost family of the endpoint anchors: the far set, the batched
//! sweep (ADR-0612) and the far-side listing (ADR-0690) change cost, never
//! answers. Beside the anchor arms they refine, it shares their graph
//! builder and shape table.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use relate_core::Value;

use crate::exec::test_support::{exec_autocommit, exec_write_uncommitted, Builder};
use crate::graph::Graph;

use super::super::anchor::{take_far_listings, without_far_set};
use super::super::fan_drivers::{take_batches, without_batched_fan};
use super::super::fixtures::{gpm, run, sorted, DirectionExt, Selector, DIRS, SELECTORS};
use super::{comparable, graph, query, AnchorArm, Shape, SEED, SHAPES};

/// The far-side bitset changes cost, never answers: every fan shape whose
/// far side is listed returns the same rows with the set on and off, and
/// never follows more relationships with it.
#[test]
fn the_far_set_changes_cost_not_answers() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x1698);
    let mut saved_any = false;
    for shape in SHAPES.iter().filter(|s| s.far_listed) {
        let mut saved = false;
        for _ in 0..3 {
            let g = graph(&mut rng, shape.n, shape.groups, shape.rels);
            for sel in SELECTORS {
                for dir in DIRS {
                    let q = query(shape, sel, dir);
                    // The batched fan has its own cost claim (below); the
                    // far set's is measured on the per-origin fan.
                    let (on, on_counts) = without_batched_fan(|| run(&g, &q));
                    let (off, off_counts) = without_batched_fan(|| without_far_set(|| run(&g, &q)));
                    assert_eq!(
                        comparable(on, sel),
                        comparable(off, sel),
                        "far set changed the answer of {q}"
                    );
                    assert!(on_counts.expands <= off_counts.expands, "{q}");
                    saved |= on_counts.expands < off_counts.expands;
                }
            }
        }
        // The backward listed fan (far = the start list) must itself save.
        assert!(
            saved || shape.arm != AnchorArm::FanBackward,
            "{}: the early stop never saved an expansion",
            shape.label()
        );
        saved_any |= saved;
    }
    assert!(saved_any, "the early stop never saved an expansion");
}

/// The batched fan (ADR-0612) answers what the per-origin layered fan
/// answers: every fan shape with a listed far side, on graphs wide enough
/// that the origin list crosses the 64-lane batch boundary, unreachable
/// pairs included (the sparse graphs leave components apart).
#[test]
fn the_batched_fan_changes_cost_not_answers() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x1698_0002);
    let wide = [
        // 150 origins (three batches, the last short) against 150.
        ("fan wide", "(a {g: 0})", "(b {g: 1})", 300, 2, 330),
        // The same, sparse: most pairs unreachable.
        ("fan sparse", "(a {g: 0})", "(b {g: 1})", 300, 2, 140),
        // Exactly one full batch.
        ("fan 64", "(a {g: 0})", "(b {g: 1})", 128, 2, 150),
    ];
    let mut batched = false;
    for (name, start, end, n, groups, rels) in wide {
        let shape = Shape {
            arm: AnchorArm::FanForward,
            name,
            far_listed: true,
            cols: "a.id, b.id, length(p)",
            start,
            end,
            n,
            groups,
            rels,
        };
        let g = graph(&mut rng, n, groups, rels);
        for sel in SELECTORS {
            for dir in DIRS {
                let q = query(&shape, sel, dir);
                let _ = take_batches();
                let (on, _) = run(&g, &q);
                batched |= take_batches() > 0;
                let (off, _) = without_batched_fan(|| run(&g, &q));
                assert_eq!(
                    comparable(on, sel),
                    comparable(off, sel),
                    "{name}: the batch changed the answer of {q}"
                );
            }
        }
    }
    assert!(batched, "no fan took the batched sweep");
}

/// A pair predicate in the `WHERE` (decided per partition, after the
/// sweep), an end pattern the far list does not decide alone, and the
/// backward fan all take the batched sweep to the same rows.
#[test]
fn the_batched_fan_keeps_the_end_admission_and_pair_predicates() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x1698_0003);
    let g = graph(&mut rng, 200, 2, 260);
    let queries = [
        "MATCH p = ANY SHORTEST (a {g: 0})-[:R*..4]-(b {g: 1}) \
         WHERE a.id <> b.id RETURN a.id, b.id, length(p)",
        "MATCH p = ALL SHORTEST (a {g: 0})-[:R*..3]->(b {g: 0}) \
         RETURN a.id, b.id, length(p), [n IN nodes(p) | n.id]",
        "MATCH p = SHORTEST 1 (a {g: 0})<-[:R*..4]-(b {g: 1}) \
         RETURN a.id, b.id, length(p)",
    ];
    for q in queries {
        let _ = take_batches();
        let (on, _) = run(&g, q);
        assert!(take_batches() > 0, "the batched sweep must run for {q}");
        let (off, _) = without_batched_fan(|| run(&g, q));
        assert_eq!(sorted(on), sorted(off), "{q}");
    }
}

/// A node-id span past the sweep's tables keeps the per-origin fan: the
/// same rows, no batch run.
#[test]
fn a_wide_id_span_keeps_the_per_origin_fan() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x1698_0004);
    let g = graph(&mut rng, 100, 2, 140);
    let q = "MATCH p = ANY SHORTEST (a {g: 0})-[:R*..4]-(b {g: 1}) RETURN a.id, b.id, length(p)";
    let (rows, _) = run(&g, q);
    let _ = take_batches();
    let (capped, _) = crate::shortest::multi_source::with_span_cap(8, || run(&g, q));
    assert_eq!(take_batches(), 0);
    assert_eq!(sorted(rows), sorted(capped));
}

/// The sweep keeps no predecessors, so work follows the graph, not the
/// path count: 2^40 shortest trails through a chain of diamonds are never
/// enumerated for a single-path selector, and the sweep follows each
/// relationship about once per batch and each of the 25 trail rebuilds a
/// bidirectional search's share, so expansions stay a small multiple of
/// the relationship count.
#[test]
fn the_sweep_does_not_scale_with_path_count() {
    let b = Builder::new();
    let head = b.prop_node("head", &["N"], &[("g", Value::Int(2))]);
    let mut rels = 0u64;
    // Five starts feed the first diamond: 5 x 5 pairs, past the pair
    // product's bound, so the fan answers.
    for i in 0..5 {
        let s = b.prop_node(&format!("s{i}"), &["N"], &[("g", Value::Int(0))]);
        b.rel(s, head, "R", &[]);
        rels += 1;
    }
    let mut prev = head;
    for i in 0..40 {
        let (l, r, next) = (
            b.prop_node(&format!("l{i}"), &["N"], &[("g", Value::Int(2))]),
            b.prop_node(&format!("r{i}"), &["N"], &[("g", Value::Int(2))]),
            b.prop_node(&format!("j{i}"), &["N"], &[("g", Value::Int(2))]),
        );
        for (x, y) in [(prev, l), (prev, r), (l, next), (r, next)] {
            b.rel(x, y, "R", &[]);
        }
        rels += 4;
        prev = next;
    }
    for i in 0..5 {
        let t = b.prop_node(&format!("t{i}"), &["N"], &[("g", Value::Int(1))]);
        b.rel(prev, t, "R", &[]);
        rels += 1;
    }
    let g = b.done();
    let q = "MATCH p = ANY SHORTEST (a {g: 0})-[:R*..200]->(b {g: 1}) RETURN length(p)";
    let _ = take_batches();
    let (rows, counts) = run(&g, q);
    assert!(take_batches() > 0, "the fan must take the batched sweep");
    assert_eq!(rows.len(), 25, "one trail per (start, end) pair");
    assert!(
        counts.expands < 30 * rels,
        "{} expansions over {rels} relationships",
        counts.expands
    );
}

/// A write view's scratch-created nodes carry ids past the committed
/// counter: the batched fan's tables must cover them. Twenty starts and
/// twenty ends are all created in the statement that then fans over them.
#[test]
fn the_batched_fan_covers_scratch_created_nodes() {
    let g = Graph::default();
    let q = "UNWIND range(0, 19) AS i \
             CREATE (:N {id: 's' + toString(i), g: 0})-[:R]->(m:N {id: 'm' + toString(i), g: 2})\
                    -[:R]->(:N {id: 't' + toString(i), g: 1}) \
             WITH count(*) AS made \
             MATCH p = ANY SHORTEST (a {g: 0})-[:R*..3]->(b {g: 1}) \
             RETURN a.id, b.id, length(p)";
    let _ = take_batches();
    let rows = exec_write_uncommitted(&g, q).rows;
    assert!(take_batches() > 0, "the fan must take the batched sweep");
    let oracle = without_batched_fan(|| exec_write_uncommitted(&g, q).rows);
    assert_eq!(rows.len(), 20);
    assert_eq!(sorted(rows), sorted(oracle));
}

/// A trailing batch of one origin (65 origins = 64 + 1) takes the layered
/// fan, and the answer is the per-origin fan's.
#[test]
fn a_lone_trailing_origin_keeps_the_layered_fan() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x1698_0005);
    // 130 nodes in two groups: 65 origins, 65 far nodes.
    let g = graph(&mut rng, 130, 2, 200);
    let q = "MATCH p = ANY SHORTEST (a {g: 0})-[:R*..4]-(b {g: 1}) RETURN a.id, b.id, length(p)";
    let _ = take_batches();
    let (rows, _) = run(&g, q);
    assert_eq!(take_batches(), 1, "only the full batch of 64 sweeps");
    let (off, _) = without_batched_fan(|| run(&g, q));
    assert_eq!(
        comparable(rows, SELECTORS[0]),
        comparable(off, SELECTORS[0])
    );
}

/// Label-only endpoints (no property map to seek) take the batched sweep:
/// the far side is listed from its label on the fan's behalf. Across the
/// selectors and directions, a rel-type union, ties, cycles and parallel
/// edges, the rows are the per-origin fan's and the GPM walk's.
#[test]
fn label_only_endpoints_take_the_batched_sweep() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x0334);
    let b = Builder::new();
    let mut nodes = Vec::new();
    for (label, count) in [("A", 90usize), ("H", 40), ("E", 20)] {
        for i in 0..count {
            nodes.push(b.prop_node(&format!("{label}{i}"), &[label], &[]));
        }
    }
    for _ in 0..320 {
        let (x, y) = (
            rng.random_range(0..nodes.len()),
            rng.random_range(0..nodes.len()),
        );
        let ty = ["R", "S", "T"][rng.random_range(0..3)];
        b.rel(nodes[x], nodes[y], ty, &[]);
    }
    let g = b.done();
    let mut batched = false;
    for sel in SELECTORS {
        for dir in DIRS {
            let Selector { open, close, .. } = sel;
            let (l, r) = dir.arrows();
            let q = format!(
                "MATCH p = {open}(a:A){l}[:R|S*..5]{r}(e:E){close} \
                 RETURN a.id, e.id, length(p), [n IN nodes(p) | n.id]"
            );
            let _ = take_batches();
            let (on, counts) = run(&g, &q);
            batched |= take_batches() > 0;
            assert!(counts.route_runs > 0, "the route must take {q}");
            let (off, _) = without_batched_fan(|| without_far_set(|| run(&g, &q)));
            assert_eq!(comparable(on.clone(), sel), comparable(off, sel), "{q}");
            assert_eq!(comparable(on, sel), comparable(gpm(&g, &q), sel), "{q}");
        }
    }
    assert!(batched, "no label-only fan took the batched sweep");
}

/// A label-only far side no node carries is the empty answer — and the
/// route still takes the pattern, with no search run.
#[test]
fn an_empty_label_only_far_side_is_the_empty_pair() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x0335);
    let g = graph(&mut rng, 40, 2, 60);
    for q in [
        "MATCH p = ANY SHORTEST (a)-[:R*..4]->(b:Missing) RETURN length(p)",
        "MATCH p = ANY SHORTEST (a:Missing)-[:R*..4]->(b) RETURN length(p)",
    ] {
        let (rows, _) = run(&g, q);
        assert!(rows.is_empty(), "{q}");
        assert!(gpm(&g, q).is_empty(), "{q}");
    }
}

/// A far node that is also an origin and lies on a cycle: the origin's own
/// level settles it, yet a positive lower bound still owes the closed
/// trails back to it, so the far set must not retire the origin there.
/// Several origins and a labelled far side, so the far set is built and
/// the batched sweep runs.
#[test]
fn an_origin_that_is_the_only_far_node_keeps_its_closed_trails() {
    let g = Graph::default();
    exec_autocommit(
        &g,
        "CREATE (x:L:F {id: 'x'}), (y:L {id: 'y'}), (z:L {id: 'z'}), \
         (x)-[:R]->(y), (y)-[:R]->(x), (x)-[:R]->(x), (z)-[:R]->(x)",
    )
    .unwrap();
    for (q, sweeps) in [
        (
            "MATCH p = ALL SHORTEST (a:L)-[:R*1..3]->(b:F) RETURN a.id, length(p)",
            true,
        ),
        (
            "MATCH p = ANY SHORTEST (a:L)-[:R*1..3]-(b:F) RETURN a.id, length(p)",
            true,
        ),
        // The k selector rides the class walk, not the sweep.
        (
            "MATCH p = SHORTEST 2 (a:L)-[:R*1..3]->(b:F) RETURN a.id, length(p)",
            false,
        ),
    ] {
        let _ = take_batches();
        let (rows, _) = run(&g, q);
        assert_eq!(take_batches() > 0, sweeps, "the far set's sweep for {q}");
        assert_eq!(sorted(rows.clone()), gpm(&g, q), "{q}");
        assert!(
            rows.iter().any(|r| r[0] == Value::String("x".into())),
            "{q}"
        );
    }
}

/// A label-less far side has no bucket to list: it would read the whole
/// node store per input row, so the fan keeps testing each reached node
/// (no far set, no batched sweep) and answers what the GPM walk answers.
#[test]
fn a_label_less_far_side_is_not_listed() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x0336);
    let g = graph(&mut rng, 60, 2, 90);
    let q = "MATCH p = ANY SHORTEST (a {g: 0})-[:R*..4]->(b) RETURN a.id, b.id, length(p)";
    let _ = take_batches();
    let (rows, _) = run(&g, q);
    assert_eq!(
        take_batches(),
        0,
        "a label-less far side must stay unlisted"
    );
    assert_eq!(
        comparable(rows, SELECTORS[0]),
        comparable(gpm(&g, q), SELECTORS[0])
    );
}

/// A far side spelled as a label disjunction is not a route pattern at all
/// (`shortest::followable` wants a static conjunction), so it never reaches
/// the anchor's far listing: the route declines it and the GPM walk answers.
#[test]
fn a_disjunction_far_side_stays_off_the_route() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 0x0337);
    let b = Builder::new();
    let mut nodes = Vec::new();
    for (label, count) in [("A", 40usize), ("E", 10), ("F", 10)] {
        for i in 0..count {
            nodes.push(b.prop_node(&format!("{label}{i}"), &[label], &[]));
        }
    }
    for _ in 0..160 {
        let (x, y) = (
            rng.random_range(0..nodes.len()),
            rng.random_range(0..nodes.len()),
        );
        b.rel(nodes[x], nodes[y], "R", &[]);
    }
    let g = b.done();
    let q = "MATCH p = ANY SHORTEST (a:A)-[:R*..4]->(e:E|F) RETURN a.id, e.id, length(p)";
    let _ = take_far_listings();
    let (rows, counts) = run(&g, q);
    assert_eq!(counts.route_runs, 0, "the route must decline {q}");
    assert_eq!(take_far_listings(), 0);
    assert!(!rows.is_empty());
}
