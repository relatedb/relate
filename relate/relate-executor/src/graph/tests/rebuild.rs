use super::*;
use crate::graph::count_state::CountStats;
use crate::graph::degree_stats::DegreeHistograms;
use crate::graph::rebuild::take_adj_builds;
use crate::graph::shard::take_shards_run;
use crate::graph::test_support::{endpoints, live_rows, node_adjacencies, NodeAdjacency};

/// An aborted write drops its relationships from adjacency: `abort_write`
/// still rebuilds it from the live versions.
#[test]
fn abort_write_rebuilds_adjacency() {
    let g = Graph::default();
    let (a, b) = endpoints(&g);
    g.begin_write().unwrap();
    let kept = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    g.begin_write().unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    take_adj_builds();
    g.abort_write();
    assert_eq!(take_adj_builds(), 1);
    assert_eq!(g.degree_out(a), [kept]);
    assert_eq!(g.degree_in(b), [kept]);
}

/// A payload-installed load records no adjacency build — the chunks landed
/// with the payload — while the row re-fold records one: the counter moves
/// inside `rebuild_adj_from`'s `RelRows` arm.
#[test]
fn the_adjacency_build_counter_counts_only_the_row_refold() {
    let g = Graph::default();
    take_adj_builds();
    g.rebuild_adj_from(relate_engine_api::AdjacencySource::Payload);
    assert_eq!(take_adj_builds(), 0);
    g.rebuild_adj_from(relate_engine_api::AdjacencySource::RelRows);
    assert_eq!(take_adj_builds(), 1);
}

/// Everything `rebuild_adj` publishes: both adjacency directions with their
/// hop order, the degree histograms, the count bundle itself, and the
/// `used_bytes` the rebuild's tail recomputes from them. The bundle is held
/// whole, not re-listed field by field — a field added to `CountStats`
/// later is covered by this comparison, not silently escaped by it, and
/// this is the one test that would catch a new fold's sharding bug.
#[derive(Debug, PartialEq)]
struct RebuildState {
    adjacency: Vec<NodeAdjacency>,
    histograms: DegreeHistograms,
    stats: CountStats,
    used_bytes: u64,
}

fn rebuild_state(g: &Graph) -> RebuildState {
    RebuildState {
        adjacency: node_adjacencies(g),
        histograms: g.degree_histograms.lock().unwrap().clone(),
        stats: g.count_stats.lock().unwrap().clone(),
        used_bytes: g.used_bytes(),
    }
}

/// The one fixture both sharded-equality tests stage: six start nodes as
/// `S{i % 3}` and five end nodes as `E{i % 2}`, inside the caller's open
/// write. Each test adds only the rel loop its assertion needs.
const SEED_TYPES: [&str; 3] = ["T0", "T1", "T2"];

fn seeded(g: &Graph) -> (Vec<NodeId>, Vec<NodeId>) {
    let starts: Vec<NodeId> = (0..6)
        .map(|i| {
            g.create_node(vec![format!("S{}", i % 3)], BTreeMap::new())
                .unwrap()
        })
        .collect();
    let ends: Vec<NodeId> = (0..5)
        .map(|i| {
            g.create_node(vec![format!("E{}", i % 2)], BTreeMap::new())
                .unwrap()
        })
        .collect();
    (starts, ends)
}

/// The sharded rebuild is the single-shard one. `shard::shard_rows` keys
/// on a node endpoint, so a shard owns every row of every node it carries
/// and nothing the rebuild publishes depends on where the shard boundaries
/// fall: `from_runs` derives each node's chunk order from its own runs'
/// first rel ids and sorts its hops `cmp_ts_desc`, both total orders over
/// that node's hops, and both folds are order-independent aggregates.
#[test]
fn sharded_rebuild_matches_the_single_shard_one() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let (starts, ends) = seeded(&g);
    // Every start node carries one rel per type, so its out-adjacency has
    // several chunks to order and hops to sort; the ends take several rels
    // each, the same on the incoming side. The vocabulary is bound once: the
    // loop bound and the name both read it, so neither can drift.
    const TYPES: [&str; 3] = SEED_TYPES;
    for (i, &start) in starts.iter().enumerate() {
        for j in 0..TYPES.len() {
            g.create_rel(
                start,
                ends[(i + j) % ends.len()],
                TYPES[(i + j) % TYPES.len()].to_string(),
                BTreeMap::new(),
            )
            .unwrap();
        }
    }
    g.commit_write();

    g.rebuild_adj();
    // One rebuild is four `run_shards` calls — both adjacency sides, the
    // degree fold, the count fold — so the single-shard run accumulates 4
    // and the four-shard run 16. Asserting the sum pins every one of the
    // four, the adjacency build included.
    assert_eq!(take_shards_run(), 4);
    let serial = rebuild_state(&g);
    crate::graph::shard::with_workers(4, || g.rebuild_adj());
    assert_eq!(take_shards_run(), 16);
    assert_eq!(rebuild_state(&g), serial);
}

/// The post-install phase a reopen actually runs: `finish_restore` over
/// checkpoint-loaded rows publishes the sharded build's state exactly as the
/// single-shard one's. The rebuild-level equality test above pins
/// `rebuild_adj` on a committed-write graph; this pins the phase's own entry
/// point after the load, through the same whole-bundle comparison.
#[test]
fn finish_restore_sharded_matches_the_single_shard_restore() {
    let writer = Graph::default();
    writer.begin_write().unwrap();
    let (starts, ends) = seeded(&writer);
    for (i, &start) in starts.iter().enumerate() {
        for rel_type in &SEED_TYPES[..2] {
            writer
                .create_rel(
                    start,
                    ends[i % ends.len()],
                    rel_type.to_string(),
                    BTreeMap::new(),
                )
                .unwrap();
        }
    }
    writer.commit_write();

    let ts = writer.commit_ts();
    let (nodes, rels) = live_rows(&writer, ts);
    let restore = |workers: usize| {
        let g = Graph::default();
        g.set_commit_ts(ts);
        for node in nodes.clone() {
            g.load_checkpoint_node(node).unwrap();
        }
        for rel in rels.clone() {
            g.load_checkpoint_rel(rel).unwrap();
        }
        crate::graph::shard::with_workers(workers, || {
            g.finish_restore(relate_engine_api::AdjacencySource::RelRows)
        });
        g
    };

    let serial = restore(1);
    assert_eq!(take_shards_run(), 4, "the one shard is the serial walk");
    let sharded = restore(4);
    assert_eq!(
        take_shards_run(),
        16,
        "every consuming phase ran on four shards"
    );
    assert_eq!(rebuild_state(&sharded), rebuild_state(&serial));
}
