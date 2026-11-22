//! The adjacency floor's cross-cutting correctness suite: three
//! equivalences over one corpus — the adversarial multigraph (the
//! LABS-1293 differential population) at seal scale, every edge repeated
//! to [`COMPACTION_DELTA_FLOOR`] so one checkpoint fold seals every
//! (node, type) chunk.
//!
//! * the sealed arena's reads answer exactly what the kernels-disabled
//!   per-row walker answers — one side decodes the arena's bytes
//!   directly, the other runs the whole engine with the kernels stood
//!   down;
//! * the 16-byte hop plus its presence boundary reproduces, hop for hop
//!   and in order, the per-hop `Option<i64>` the pre-change fat hop
//!   carried — the sealed codec's bitmap and the delta tier's boundary
//!   alike, `Some(0)` included (the dead-fill value an absent hop's raw
//!   payload parks there);
//! * a zone-map-pruned page read loses nothing: its in-range hops are
//!   exactly the full read's, and over a present-only block the pruned
//!   read is the full read.

use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, RelId, Value};
use relate_ir::Direction;

use super::super::Graph;
use crate::adjacency::{
    cmp_ts_desc, decode_block, decode_ranged, encode_block, zone_maps, Hop, SealedArena,
    COMPACTION_DELTA_FLOOR,
};
use crate::exec::kernels::with_kernels_disabled;
use crate::exec::test_support::{rel_id, try_exec_cypher};

/// The `ts` a corpus rel carries, stamped by rel id: every fifteenth hop
/// takes `Some(0)` — the dead-fill value the raw 16-byte hop parks in an
/// absent hop's payload, which a *present* hop must still round-trip — a
/// third take descending positive stamps, a third negative ones, and the
/// rest none.
fn stamp(rid: RelId) -> Option<i64> {
    match rid.rem_euclid(15) {
        0 => Some(0),
        r if r % 3 == 0 => Some(9000 - rid),
        r if r % 3 == 2 => Some(-rid),
        _ => None,
    }
}

/// The sealed corpus: the graph, its named nodes, the interned ids of its
/// two rel types, and the merged hop stream every (node, side, type)
/// chunk must read back — the expectation the previous `Option` semantics
/// defines, assembled from the rels the corpus created and sorted
/// `cmp_ts_desc`.
struct Corpus {
    g: Graph,
    names: Vec<(&'static str, NodeId)>,
    f_id: u32,
    g_id: u32,
    /// Per node: `[out, in]`, each side's types and their expected hops.
    expected: BTreeMap<NodeId, [BTreeMap<u32, Vec<Hop>>; 2]>,
}

/// Build the adversarial corpus with every chunk sealed and `fresh`
/// extra rels left in the sealed chunks' delta — the 16-byte tier beside
/// the sealed stream. `ts` presence is stamped per rel id ([`stamp`]);
/// the fresh rels alternate presence, so a delta chunk holds both.
fn sealed_corpus(fresh: usize) -> Corpus {
    let (g, names, rels) =
        crate::graph::test_support::adversarial_multigraph(COMPACTION_DELTA_FLOOR);
    g.begin_write().unwrap();
    for (_, _, _, rid) in &rels {
        if let Some(ts) = stamp(*rid) {
            g.set_property_rel(*rid, "ts", Value::Int(ts));
        }
    }
    g.commit_write();
    // The checkpoint fold: every chunk sat at the floor, so every hop
    // lands in the sealed arena.
    g.gc_versions();

    // Fresh rels over the corpus's first edges — self-loop, parallel
    // same-type and cross-type, reciprocal — landing in sealed chunks'
    // delta, presence alternating.
    let mut created: Vec<(NodeId, NodeId, &str, RelId, Option<i64>)> = rels
        .iter()
        .map(|(from, to, t, rid)| (*from, *to, *t, *rid, stamp(*rid)))
        .collect();
    if fresh > 0 {
        g.begin_write().unwrap();
        for (i, (from, to, t, _)) in rels.iter().take(fresh).enumerate() {
            let ts = (i % 2 == 0).then_some(50_000 + i as i64);
            let props = ts.map(|v| BTreeMap::from([("ts".to_string(), Value::Int(v))]));
            let rid = g
                .create_rel(*from, *to, (*t).into(), props.unwrap_or_default())
                .unwrap();
            created.push((*from, *to, *t, rid, ts));
        }
        g.commit_write();
    }

    let f_id = g.interner.get_id("F").unwrap();
    let g_id = g.interner.get_id("G").unwrap();
    let mut expected: BTreeMap<NodeId, [BTreeMap<u32, Vec<Hop>>; 2]> = BTreeMap::new();
    for (from, to, t, rid, ts) in created {
        let tid = g.interner.get_id(t).expect("a corpus rel type is interned");
        let hop = Hop { ts, id: rid };
        // A self-loop rides both of its node's sides.
        for (node, side) in [(from, 0), (to, 1)] {
            expected
                .entry(node)
                .or_insert([BTreeMap::new(), BTreeMap::new()])[side]
                .entry(tid)
                .or_default()
                .push(hop);
        }
    }
    for sides in expected.values_mut() {
        for stream in sides.iter_mut().flat_map(|side| side.values_mut()) {
            stream.sort_unstable_by(cmp_ts_desc);
        }
    }
    Corpus {
        g,
        names,
        f_id,
        g_id,
        expected,
    }
}

impl Corpus {
    fn node(&self, name: &str) -> NodeId {
        self.names
            .iter()
            .find(|(n, _)| *n == name)
            .expect("a corpus node")
            .1
    }

    /// The merged hop stream the store reads for (node, side, type) at
    /// the commit boundary: the delta first, then the sealed stream, in
    /// `cmp_ts_desc` order — the read every adjacency consumer rides.
    fn merged(&self, node: NodeId, outgoing: bool, tid: u32) -> Vec<Hop> {
        let side = if outgoing {
            self.g.out_side()
        } else {
            self.g.in_side()
        };
        let (map, sealed) = (side.delta(), side.sealed());
        let mut hops = Vec::new();
        if let Some(adj) = map.get(&node) {
            adj.for_each_hop_of(sealed.region(node), tid, self.g.commit_ts(), |hop| {
                hops.push(hop)
            });
        }
        hops
    }

    /// The previous `Option` semantics' expectation for (node, side, type).
    fn expectation(&self, node: NodeId, outgoing: bool, tid: u32) -> Vec<Hop> {
        let side = usize::from(!outgoing);
        self.expected
            .get(&node)
            .and_then(|sides| sides[side].get(&tid))
            .cloned()
            .unwrap_or_default()
    }
}

/// The differential, at the store tier: for every corpus node, direction
/// and type filter, the hops decoded straight out of the sealed arena
/// equal the relationships the kernels-disabled per-row walker expands —
/// the contract the kernels' differential suites hold above, proven here
/// against the sealed floor underneath them.
#[test]
fn sealed_arena_reads_answer_what_the_walker_answers() {
    let c = sealed_corpus(0);
    let g = &c.g;
    let ts = g.commit_ts();

    // The corpus really sealed: no delta hop remains, and the hub's two
    // widest chunks hold their whole pattern at scale.
    for (_, node) in &c.names {
        for map in [g.out_side().delta(), g.in_side().delta()] {
            let delta: usize = map
                .get(node)
                .map(|adj| adj.chunks.iter().map(|chunk| chunk.hops.len()).sum())
                .unwrap_or(0);
            assert_eq!(delta, 0, "node {node}: every hop sealed");
        }
    }
    let hub = c.node("hub");
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&g.out_side(), hub, c.f_id),
        7 * COMPACTION_DELTA_FLOOR
    );
    assert_eq!(
        crate::graph::test_support::sealed_chunk_len(&g.in_side(), hub, c.f_id),
        8 * COMPACTION_DELTA_FLOOR
    );

    // The sealed side: the arena's own reads, per type chunk.
    let arena_ids = |node: NodeId, arena: &SealedArena, tids: &[u32]| -> Vec<RelId> {
        let mut ids = Vec::new();
        if let Some(region) = arena.region(node) {
            for tid in tids {
                ids.extend(region.visible_hops(*tid, ts).iter().map(|hop| hop.id));
            }
        }
        ids
    };
    // The walker side: the whole engine with the kernels stood down.
    let walker_ids = |name: &str, pattern: &str| -> Vec<RelId> {
        let query = format!("MATCH (a {{id: '{name}'}}){pattern} RETURN r");
        let rows = with_kernels_disabled(|| try_exec_cypher(g, &query))
            .unwrap_or_else(|error| panic!("the walker runs {query}: {error}"))
            .rows;
        rows.iter().map(|row| rel_id(&row[0])).collect()
    };

    let f = &[c.f_id];
    let t = &[c.g_id];
    let any = &[c.f_id, c.g_id];
    let mut answered = 0usize;
    for (name, node) in &c.names {
        let cases: [(Direction, &str, &[u32]); 9] = [
            (Direction::Outgoing, "-[r:F]->(b)", f),
            (Direction::Outgoing, "-[r:G]->(b)", t),
            (Direction::Outgoing, "-[r]->(b)", any),
            (Direction::Incoming, "<-[r:F]-(b)", f),
            (Direction::Incoming, "<-[r:G]-(b)", t),
            (Direction::Incoming, "<-[r]-(b)", any),
            (Direction::Both, "-[r:F]-(b)", f),
            (Direction::Both, "-[r:G]-(b)", t),
            (Direction::Both, "-[r]-(b)", any),
        ];
        for (dir, pattern, tids) in cases {
            let mut sealed = match dir {
                Direction::Outgoing => arena_ids(*node, g.out_side().sealed(), tids),
                Direction::Incoming => arena_ids(*node, g.in_side().sealed(), tids),
                // Both is the walk's own rule: the sides' union, deduped
                // by rel id — a self-loop rides both of its node's sides.
                Direction::Both => {
                    let mut ids = arena_ids(*node, g.out_side().sealed(), tids);
                    ids.extend(arena_ids(*node, g.in_side().sealed(), tids));
                    let mut seen = HashSet::new();
                    ids.retain(|id| seen.insert(*id));
                    ids
                }
            };
            let mut walker = walker_ids(name, pattern);
            answered += walker.len();
            sealed.sort_unstable();
            walker.sort_unstable();
            assert_eq!(sealed, walker, "{name} {pattern}");
        }
    }
    assert!(answered > 0, "the sweep never answered a hop");
}

/// The presence equivalence: every hop of the corpus — the fully-sealed
/// state, and fresh 16-byte delta hops beside sealed streams — reads back
/// exactly the `Option<i64>` its rel's `ts` property defines, in the
/// order the old fat hop's semantics fixed (missing first, then ts DESC,
/// ties newest first).
#[test]
fn the_sixteen_byte_hop_preserves_the_option_semantics() {
    // fresh = 0: the sealed codec's bitmap is the only presence carrier;
    // fresh > 0: the delta tier's absent boundary carries beside it.
    for fresh in [0, 12] {
        let c = sealed_corpus(fresh);
        let mut mixed_streams = 0;
        let mut zero_ts = 0;
        for (name, node) in &c.names {
            for (outgoing, side) in [(true, "out"), (false, "in")] {
                for tid in [c.f_id, c.g_id] {
                    let merged = c.merged(*node, outgoing, tid);
                    assert_eq!(
                        merged,
                        c.expectation(*node, outgoing, tid),
                        "{name}, {side} side, type {tid} (fresh {fresh})"
                    );
                    zero_ts += merged.iter().filter(|h| h.ts == Some(0)).count();
                    mixed_streams += usize::from(
                        merged.iter().any(|h| h.ts.is_none())
                            && merged.iter().any(|h| h.ts.is_some()),
                    );
                }
            }
        }
        // Not vacuous: the corpus really mixes presence within streams,
        // and present zeros exist to catch a dead-fill misread.
        assert!(mixed_streams > 0, "fresh {fresh}: no stream mixes presence");
        assert!(zero_ts > 0, "fresh {fresh}: no hop carries ts 0");
        if fresh > 0 {
            // A delta chunk really holds both presences: the 16-byte
            // tier's own mixed boundary, not only the sealed bitmap's.
            let mut mixed_delta = 0;
            for map in [c.g.out_side().delta(), c.g.in_side().delta()] {
                map.for_each(|_, adj| {
                    mixed_delta += adj
                        .chunks
                        .iter()
                        .filter(|chunk| {
                            chunk.absent_len() > 0
                                && (chunk.absent_len() as usize) < chunk.hops.len()
                        })
                        .count();
                });
            }
            assert!(
                mixed_delta > 0,
                "fresh {fresh}: no delta chunk mixes presence"
            );
        }
    }
}

/// The pruning equivalence: over the corpus's widest sealed chunk — 256
/// hops of mixed presence, positive, zero and negative stamps — a
/// zone-map-pruned read loses nothing. For every range the pruned
/// stream's in-range hops are exactly the full read's, and over a
/// present-only block (every page's zone map non-empty) the full-span
/// pruned read is the full read, page for page.
#[test]
fn a_zone_map_pruned_read_equals_the_full_read() {
    let c = sealed_corpus(0);
    let hub = c.node("hub");
    let hops = c.g.in_side().sealed().region(hub).unwrap().decoded(c.f_id);
    assert_eq!(hops.len(), 8 * COMPACTION_DELTA_FLOOR);

    let block = encode_block(&hops, &[], Some(8));
    let pages = zone_maps(&block).len();
    assert!(pages > 1, "the block pages");
    let full = decode_block(&block);
    assert_eq!(full, hops);

    // One range per sampled distinct ts — the point, its brackets, the
    // tails above and below — plus the whole span, a zero point and an
    // inverted range.
    let mut distinct: Vec<i64> = hops.iter().filter_map(|h| h.ts).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let mut ranges: Vec<(i64, i64)> = vec![
        (i64::MIN, i64::MAX),
        (0, 0),
        (-1, 1),
        // Inverted: the map's interval rule may still overlap it, no hop
        // can match it.
        (7, 6),
    ];
    for t in distinct.iter().step_by(7) {
        ranges.extend([(*t, *t), (t - 1, t + 1), (*t, i64::MAX), (i64::MIN, t - 1)]);
    }
    let mut pruned_pages = 0;
    for &(lo, hi) in &ranges {
        let (pruned, decoded) = decode_ranged(&block, lo, hi);
        pruned_pages += decoded;
        let in_range = |hop: &Hop| hop.ts.is_some_and(|t| lo <= t && t <= hi);
        let want: Vec<Hop> = full.iter().filter(|h| in_range(h)).cloned().collect();
        let got: Vec<Hop> = pruned.iter().filter(|h| in_range(h)).cloned().collect();
        assert_eq!(got, want, "range [{lo}, {hi}]");
    }
    // Pruning really prunes: the sweep decoded fewer pages than reading
    // every page for every range would, and a point range still answers.
    assert!(pruned_pages < ranges.len() * pages, "no range was pruned");
    let point = distinct[distinct.len() / 2];
    let (hit, decoded) = decode_ranged(&block, point, point);
    assert!(
        decoded < pages && !hit.is_empty(),
        "a point range prunes and answers"
    );

    // The present-only block: every page's zone map is non-empty, so the
    // full-span pruned read is the full read exactly.
    let present: Vec<Hop> = hops.iter().filter(|h| h.ts.is_some()).cloned().collect();
    let pblock = encode_block(&present, &[], Some(8));
    let (all, decoded) = decode_ranged(
        &pblock,
        *distinct.first().unwrap(),
        *distinct.last().unwrap(),
    );
    assert_eq!(decoded, zone_maps(&pblock).len());
    assert_eq!(all, decode_block(&pblock));
}
