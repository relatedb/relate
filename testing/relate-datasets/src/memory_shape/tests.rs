//! The memory-shape run's tests, split from the run itself at the
//! thousand-line ceiling (the bulk.rs / loadtest.rs shape).
use super::*;
use crate::shapes::{
    census, rss_matrix, ShapeScale, DEFAULT_LOGICAL_TARGET_BYTES, DEFAULT_NODE_ATTRIBUTES,
    MID_DEGREE_AVG, MID_DEGREE_GROUP,
};

/// One shape probed at a limit, in its own temp store — the shared
/// scaffold every fill test below used to repeat (limit, live lock,
/// tempdir, store path, `probe_store` call). Returns the tempdir (kept
/// alive by the caller), the store path, and the probe's `(used, disk,
/// nodes, rels)`.
fn probe_at(shape: GraphShape, limit: u64) -> (tempfile::TempDir, PathBuf, u64, u64, u64, u64) {
    let live = Mutex::new(Live::default());
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(shape.to_string());
    let probed = probe_store(shape, 1, IndexCounts::NONE, limit, &store, &live, false).unwrap();
    (dir, store, probed.0, probed.1, probed.2, probed.3)
}

// Disk tier (ADR-0251): `probe_store` checkpoints and measures the
// on-disk store directory (`store_directory_bytes` →
// `relate_storage::store_info`, which rejects `:memory:` — an
// in-memory store has no on-disk layout), so these tests keep a real
// filesystem directory.
#[test]
fn flat_fill_stops_when_the_limit_is_hit() {
    let limit = 1024 * 1024;
    let (_dir, store, used, disk, nodes, rels) = probe_at(GraphShape::Flat, limit);
    assert!(
        used <= limit,
        "committed {used} bytes above the {limit} limit"
    );
    assert!(
        used > limit / 2,
        "stopped at {used} bytes, far under the {limit} limit"
    );
    assert!(nodes >= 2_000 && nodes.is_multiple_of(2_000));
    assert_eq!(rels, 0);
    assert!(disk > 0, "checkpointed store directory is empty");
    assert_eq!(disk, store_directory_bytes(&store).unwrap());
    assert!(
        store.join("wal").is_dir(),
        "WAL is part of the directory size"
    );
}

#[test]
fn ring_fill_closes_when_the_next_batch_does_not_fit() {
    // One closed ring of 2,000 nodes. The next node batch has to miss,
    // and the ring has to be closed when it does. A relationship batch
    // can miss while another node batch still fits, which leaves the
    // ring open; 1.25 MB sits under that second node batch and over
    // the closed ring.
    let limit = 1_250_000;
    let (_dir, _store, used, _, nodes, rels) = probe_at(GraphShape::Ring, limit);
    assert!(
        used <= limit,
        "committed {used} bytes above the {limit} limit"
    );
    assert_eq!(
        nodes, rels,
        "ring did not close ({nodes} nodes, {rels} rels, {used} bytes)"
    );
    assert!(nodes >= 2_000, "used {used} bytes");
}

#[test]
fn fixed_counts_stop_on_a_batch_boundary() {
    let live = Mutex::new(Live::default());
    // The canonical inventory the loadtest itself iterates: a shape added
    // to the matrix is filled here too, never silently skipped.
    for shape in rss_matrix() {
        let target = shape_target(shape);
        let filled = fill_to_target(
            shape,
            1,
            IndexCounts::NONE,
            u64::MAX,
            &live,
            "cursor",
            |_| Ok(()),
        )
        .unwrap_or_else(|err| panic!("{shape} {err}"));
        assert_eq!(filled.nodes, target.nodes, "{shape}");
        assert_eq!(filled.rels, target.rels, "{shape}");
    }
}

/// The mid-degree fill target is the census solver's own answer at the
/// default target, so the fixed table and the sizing arithmetic cannot
/// drift apart: a charge constant or ladder edit moves the census, and
/// this fails with the numbers.
#[test]
fn mid_degree_target_matches_the_census_solver() {
    let solved = census(
        GraphShape::MidDegree,
        ShapeScale::Large {
            logical_bytes: DEFAULT_LOGICAL_TARGET_BYTES,
        },
        IndexCounts::NONE,
        DEFAULT_NODE_ATTRIBUTES,
    )
    .unwrap();
    let target = shape_target(GraphShape::MidDegree);
    assert_eq!(
        (target.nodes, target.rels),
        (solved.nodes, solved.relationships),
        "the fixed mid-degree target and the census solver disagree"
    );
}

#[test]
fn dense_ring_fill_links_each_window_before_the_next() {
    let limit = 4 * 1024 * 1024;
    let (_dir, _store, used, _, nodes, rels) = probe_at(GraphShape::DenseRing, limit);
    assert!(
        used <= limit,
        "committed {used} bytes above the {limit} limit"
    );
    assert!(nodes >= 100, "used {used} bytes");
    assert_eq!(
        rels,
        nodes * 20,
        "last window was not linked ({nodes} nodes, {rels} rels, {used} bytes)"
    );
}

#[test]
fn mid_degree_fill_links_every_node_at_its_ladder_degree() {
    let limit = 8 * 1024 * 1024;
    let (_dir, _store, used, _, nodes, rels) = probe_at(GraphShape::MidDegree, limit);
    assert!(
        used <= limit,
        "committed {used} bytes above the {limit} limit"
    );
    // Every statement is a whole ladder group, so the fill's node count is
    // always a group multiple and the relationship count is exactly the
    // ladder's average per node — the facts the census charges.
    assert!(nodes >= 32, "used {used} bytes");
    assert_eq!(
        nodes % u64::from(MID_DEGREE_GROUP),
        0,
        "fill stopped mid-group ({nodes} nodes)"
    );
    assert_eq!(
        rels,
        nodes * MID_DEGREE_AVG,
        "a group was not fully linked ({nodes} nodes, {rels} rels, {used} bytes)"
    );
}
