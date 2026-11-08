//! Profiling for the R-tree POINT index (the LABS-1148 benchmark of
//! record): candidate counts and probe/residual time at index level, and
//! end-to-end latency through the seek path. The grid numbers this
//! replaced are recorded on the issue as the baseline.
//!
//! Run (release, single-threaded, verbose):
//!
//! ```text
//! cargo test -p relate-executor --release --lib spatial_profile -- \
//!     --ignored --nocapture --test-threads=1
//! ```

use std::time::Instant;

use crate::graph::declared::{point_insert, spatial_ids, DrainState, PointIndex};
use crate::graph::{spatial_holds, SpatialSeek};
use relate_core::{NodeId, Value};
use relate_test_support::rng::Rng;
use relate_test_support::sampling::process_rss_bytes;
use relate_test_support::values::point;

/// WGS-84 points, 90% inside one metro area (a ~0.3° × 0.25° box around
/// central London), 10% spread across a ~4° × 2.5° region — the dense
/// regional dataset the issue describes.
fn metro_dataset(n: usize) -> Vec<(f64, f64)> {
    let mut rng = Rng(0x5eed_1148);
    let range = |rng: &mut Rng, lo: f64, hi: f64| lo + (hi - lo) * rng.unit();
    (0..n)
        .map(|i| {
            if i % 10 < 9 {
                (range(&mut rng, -0.25, 0.05), range(&mut rng, 51.40, 51.65))
            } else {
                (range(&mut rng, -2.5, 1.8), range(&mut rng, 50.5, 53.0))
            }
        })
        .collect()
}

fn origin(x: f64, y: f64) -> Value {
    point(4326, x, y)
}

fn point_at(pts: &[(f64, f64)], id: NodeId) -> Value {
    let (x, y) = pts[id as usize];
    origin(x, y)
}

/// Resident set in bytes. `None` when the platform probe is unavailable —
/// the memory line is skipped rather than reporting a fabricated zero.
/// True matches for a distance seek, by the same residual predicate the
/// seek path applies.
fn residual_distance(
    pts: &[(f64, f64)],
    cands: &std::collections::HashSet<NodeId>,
    seek: &SpatialSeek,
) -> (usize, std::time::Duration) {
    let t = Instant::now();
    let matches = cands
        .iter()
        .filter(|id| spatial_holds(&point_at(pts, **id), seek))
        .count();
    (matches, t.elapsed())
}

/// Index-level profile, 1M points: the bulk append phase, the sealed bulk
/// load, then seeks.
#[test]
#[ignore = "profile: run explicitly with --ignored --nocapture"]
fn profile_point_index() {
    const N: usize = 1_000_000;
    let pts = metro_dataset(N);
    let before = process_rss_bytes();
    let mut st = PointIndex::default();
    let t0 = Instant::now();
    for (i, &(x, y)) in pts.iter().enumerate() {
        point_insert(&mut st, i as NodeId, 4326, x, y, None);
    }
    let append_phase = t0.elapsed();
    let t0 = Instant::now();
    st.seal();
    let bulk_build = t0.elapsed();
    let rss = match (before, process_rss_bytes()) {
        (Some(before), Some(after)) => {
            format!(" rss_growth={} MiB", (after - before) / 1024 / 1024)
        }
        _ => " rss n/a".to_string(),
    };
    println!("n={N} bulk_append={append_phase:?} bulk_seal={bulk_build:?}{rss}");

    // Distance seeks at the metro centre, city-scale radii outward.
    let (cx, cy) = (-0.10, 51.52);
    println!(
        "{:>8} {:>12} {:>14} {:>12} {:>12}",
        "radius", "candidates", "probe", "residual", "true_matches"
    );
    for radius_m in [250.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 50000.0] {
        let seek = SpatialSeek::Distance {
            origin: origin(cx, cy),
            max: radius_m,
            inclusive: true,
        };
        let mut probe = std::time::Duration::ZERO;
        let mut cands = Default::default();
        for _ in 0..5 {
            let t = Instant::now();
            cands = spatial_ids(&st, &seek);
            probe += t.elapsed();
        }
        probe /= 5;
        let (matches, residual) = residual_distance(&pts, &cands, &seek);
        println!(
            "{:>6}m {:>12} {:>10?}/q {:>10?}/q {:>12}",
            radius_m as u64,
            cands.len(),
            probe,
            residual,
            matches
        );
    }

    // Bounding-box seeks, ~700 m and ~7 km sides.
    for side in [0.005, 0.05] {
        let seek = SpatialSeek::BBox {
            lo: origin(cx - side, cy - side),
            hi: origin(cx + side, cy + side),
        };
        let t = Instant::now();
        let cands = spatial_ids(&st, &seek);
        let probe = t.elapsed();
        println!(
            "bbox side={:.3}° candidates={} probe={:?}",
            side * 2.0,
            cands.len(),
            probe
        );
    }
}

/// End-to-end through `lookup_spatial_seek` (drain + seek + snapshot
/// re-check) on a 200k-node graph.
#[test]
#[ignore = "profile: run explicitly with --ignored --nocapture"]
fn profile_point_index_end_to_end() {
    let n = 200_000;
    let pts = metro_dataset(n);
    let g = crate::graph::Graph::default();
    g.begin_write().unwrap();
    let mut ids = Vec::with_capacity(n);
    for &(x, y) in &pts {
        ids.push(
            g.create_node(
                vec!["P".into()],
                std::iter::once(("loc".to_string(), origin(x, y))).collect(),
            )
            .unwrap(),
        );
    }
    g.commit_write();
    let t0 = Instant::now();
    g.materialize_point_index("P", "loc");
    let build = t0.elapsed();
    println!("[e2e] n={n} materialize={build:?}");

    for (radius_m, reps) in [(500.0, 50), (5000.0, 50)] {
        let mut origins: Vec<(f64, f64)> = pts
            .iter()
            .enumerate()
            .skip(7)
            .step_by(n / reps)
            .map(|(_, p)| *p)
            .take(reps)
            .collect();
        origins.sort_by(|a, b| a.0.total_cmp(&b.0));
        let t = Instant::now();
        let mut result_sizes = Vec::new();
        for &(ox, oy) in &origins {
            let seek = SpatialSeek::Distance {
                origin: origin(ox, oy),
                max: radius_m,
                inclusive: true,
            };
            result_sizes.push(
                g.lookup_spatial_seek(&["P".into()], "loc", &seek, g.commit_ts())
                    .unwrap()
                    .len(),
            );
        }
        let per_q = t.elapsed() / reps as u32;
        result_sizes.sort_unstable();
        println!(
            "[e2e] radius={radius_m}m per_query={per_q:?} \
             results p10={} p50={}",
            result_sizes[reps / 10],
            result_sizes[reps / 2]
        );
    }

    // Incremental maintenance: 10k property updates drain on the next seek.
    g.begin_write().unwrap();
    for id in &ids[..10_000] {
        g.set_property_node(*id, "loc", origin(-0.10, 51.52));
    }
    g.commit_write();
    let seek = SpatialSeek::Distance {
        origin: origin(-0.10, 51.52),
        max: 500.0,
        inclusive: true,
    };
    let t = Instant::now();
    let drained = g
        .lookup_spatial_seek(&["P".into()], "loc", &seek, g.commit_ts())
        .unwrap()
        .len();
    println!(
        "[e2e] drain_10k_dirty+seek={:?} results={drained}",
        t.elapsed()
    );
}
