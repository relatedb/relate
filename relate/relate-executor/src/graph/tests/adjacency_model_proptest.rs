//! Adjacency state-machine property suite (ADR-0552, conventions per
//! ADR-0268): a red-line guard for the sealed+delta store rework.
//!
//! Every case drives one random sequence of adjacency operations —
//! relationship **create** (`Graph::create_rel`, with or without an integer
//! `ts` property), **ts-update** (`Graph::set_property_rel(id, "ts", …)`,
//! which routes through `adj_reposition`), and **delete**
//! (`Graph::delete_rel`) — applied to the real `Graph` and, in parallel, to
//! a plain [`BTreeMap`] reference model. After **every** op the full
//! adjacency of every node, in both directions, is compared chunk-by-chunk
//! and hop-by-hop.
//!
//! # What the property pins
//!
//! The comparison is over **observable** adjacency: the per-(node, type,
//! direction) iteration order and contents that `for_each_typed_ends` /
//! `neighbors_at` expose — chunks in first-seen type order, hops
//! missing-`ts`-first then `ts` DESC with newest-relationship-first ties
//! (ADR-0035, ADR-0203, `cmp_ts_desc`). A delete is an MVCC tombstone: the
//! hop stays in today's `TypedAdj` and is filtered by visibility, so the
//! real side is observed through [`Graph::rel_exists`]. The model mirrors
//! that physical policy (a tombstoned hop keeps its chunk slot) so the
//! comparison itself stays purely observable: the rework may change the
//! internals (sealed blocks, delta overlays, eager hop removal) and the
//! suite stays green as long as the observable iteration does not move.
//! That is the guard's contract: it pins behaviour, not layout.
//!
//! # The eight properties
//!
//! 1. [`adjacency_matches_reference_model`] — reference-model equivalence
//!    after every op of a random create / ts-update / delete / re-add
//!    sequence (the proptest).
//! 2. [`adjacency_model_proptest_snapshot_freeze`] — a pinned read
//!    timestamp observes its frozen adjacency across later writes
//!    (ignored: a repositioned delta hop carries no `xmin`).
//! 3. [`adjacency_model_proptest_compaction_confluence`] — a fold at any
//!    interleaving never moves observable adjacency, and a second fold at
//!    the same watermark is a no-op.
//! 4. [`add_then_delete_nets_nothing`].
//! 5. [`ts_update_of_only_hop`].
//! 6. [`equal_ts_ties_newest_first_and_missing_ts_first`].
//! 7. [`multiple_type_chunks_iterate_in_first_seen_order`].
//! 8. [`delete_everything_leaves_empty_observable_adjacency`].
//!
//! # Conventions (ADR-0268)
//!
//! Case count comes from [`config`] — proptest's 256-case default with the
//! `PROPTEST_CASES` environment override, never pinned in code, so a
//! scheduled soak can scale the load (`PROPTEST_CASES=2000`). Failing seeds
//! persist to `proptest-regressions/` at the crate root, mirroring this
//! file's module path, and are committed as permanent regression tests; no
//! entry is removed without a tracked reason.
//!
//! # Wave-2 stubs
//!
//! The snapshot-freeze property is still an `#[ignore]`-d stub: a
//! reposition's delta hop carries no `xmin`, so a snapshot older than the
//! reposition sees the relocated hop beside its tombstoned sealed position.
//! It is deliberately not faked against the current store.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use proptest::test_runner::Config;
use relate_core::{NodeId, RelId, Value};

use super::super::Graph;

/// Shared proptest configuration: `Config::default()`, which already reads
/// `PROPTEST_CASES` / `PROPTEST_RNG_SEED` and friends. Do not pin `cases`
/// here — naming it would shadow the env override (ADR-0268).
fn config() -> Config {
    Config::default()
}

/// Fixed pool of pre-created endpoint nodes. Relationship ids are the
/// engine's own monotonic allocation (`create_rel` draws from
/// `next_rel_id`), so re-added relationships always get fresh ids — the
/// model never has to reason about id reuse.
const NODE_POOL: usize = 6;

/// Fixed relationship-type alphabet, so several types compete for chunks
/// inside one node's adjacency.
const TYPES: [&str; 3] = ["TA", "TB", "TC"];

/// One hop in the reference model: `(ts, id)`, mirroring `AdjHop`.
type ModelHop = (Option<i64>, RelId);

/// A node's chunk list in one direction: `(type_id, hops)` pairs in
/// first-seen type order — the model's mirror of `TypedAdj::chunks`.
type ModelChunks = Vec<(u32, Vec<ModelHop>)>;

/// The reference model: per (node, direction) the chunk list — a
/// `Vec<(type_id, Vec<ModelHop>)>` in first-seen type order, each chunk
/// sorted by the same comparator as `cmp_ts_desc` (missing-`ts` first,
/// then `ts` DESC, then id DESC), plus the set of tombstoned relationship
/// ids.
///
/// The chunks mirror the physical `TypedAdj` policy exactly: a delete only
/// tombstones (the hop keeps its slot), while a reposition removes-then-
/// reinserts and so drops an emptied chunk before pushing the type afresh
/// at the end. Emptied chunks are dropped at comparison time, on both
/// sides — that is the observable shape `for_each_typed_ends` exposes.
#[derive(Debug, Default)]
struct ModelAdj {
    /// `(node, is_outgoing)` → chunks.
    adj: BTreeMap<(NodeId, bool), ModelChunks>,
    /// Tombstoned relationship ids, filtered out at comparison time.
    dead: BTreeSet<RelId>,
}

/// The exact `cmp_ts_desc` ordering (ADR-0035 / ADR-0203), restated for the
/// model: NULL `ts` first, then `ts` descending, id descending on ties.
fn cmp_model_hop(a: &ModelHop, b: &ModelHop) -> std::cmp::Ordering {
    match (a.0, b.0) {
        (None, None) => b.1.cmp(&a.1),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(ta), Some(tb)) => tb.cmp(&ta).then(b.1.cmp(&a.1)),
    }
}

/// Sorted insert replicating `insert_hop`: an element already present
/// (comparator-equal, i.e. same `ts` and same id) is not inserted twice.
fn model_insert_hop(hops: &mut Vec<ModelHop>, hop: ModelHop) {
    if let Err(pos) = hops.binary_search_by(|h| cmp_model_hop(h, &hop)) {
        hops.insert(pos, hop);
    }
}

impl ModelAdj {
    /// Mirror of `TypedAdj::insert`: find-or-push the type chunk, then
    /// sorted-insert the hop.
    fn insert(&mut self, node: NodeId, out: bool, type_id: u32, id: RelId, ts: Option<i64>) {
        let chunks = self.adj.entry((node, out)).or_default();
        match chunks.iter_mut().find(|(t, _)| *t == type_id) {
            Some((_, hops)) => model_insert_hop(hops, (ts, id)),
            None => chunks.push((type_id, vec![(ts, id)])),
        }
    }

    /// Mirror of `TypedAdj::reposition`: remove the hop, drop the chunk if
    /// that emptied it, then sorted-insert into the (recreated, so
    /// last-positioned) type chunk. A write that leaves the `ts` unchanged
    /// never reaches the store's reposition (`old_ts == new_ts`), so it
    /// must not reorder chunks here either.
    fn reposition(&mut self, node: NodeId, out: bool, type_id: u32, id: RelId, ts: Option<i64>) {
        let chunks = self.adj.entry((node, out)).or_default();
        let unchanged = chunks
            .iter()
            .any(|(t, hops)| *t == type_id && hops.iter().any(|h| h.1 == id && h.0 == ts));
        if unchanged {
            return;
        }
        if let Some(ci) = chunks.iter().position(|(t, _)| *t == type_id) {
            let emptied = {
                let hops = &mut chunks[ci].1;
                if let Some(i) = hops.iter().position(|h| h.1 == id) {
                    hops.remove(i);
                }
                hops.is_empty()
            };
            if emptied {
                chunks.remove(ci);
            }
        }
        match chunks.iter_mut().find(|(t, _)| *t == type_id) {
            Some((_, hops)) => model_insert_hop(hops, (ts, id)),
            None => chunks.push((type_id, vec![(ts, id)])),
        }
    }

    /// A delete is an MVCC tombstone: the physical hop stays and visibility
    /// filters it, so the model only records the id. A tombstoned hop keeps
    /// its chunk slot, exactly as in the store.
    fn delete(&mut self, id: RelId) {
        self.dead.insert(id);
    }

    /// The observable chunks of `node` in one direction: tombstoned hops
    /// filtered out and emptied chunks dropped.
    fn observable(&self, node: NodeId, out: bool) -> ModelChunks {
        self.adj
            .get(&(node, out))
            .map(|chunks| {
                chunks
                    .iter()
                    .map(|(t, hops)| {
                        let hops: Vec<ModelHop> = hops
                            .iter()
                            .copied()
                            .filter(|h| !self.dead.contains(&h.1))
                            .collect();
                        (*t, hops)
                    })
                    .filter(|(_, hops)| !hops.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One generated adjacency operation.
#[derive(Clone, Debug)]
enum AdjOp {
    /// Create a relationship `start -> end` of `TYPES[ty]` with `ts` as its
    /// `ts` property (`None` = no `ts` property at all).
    Create {
        start: usize,
        end: usize,
        ty: usize,
        ts: Option<i64>,
    },
    /// Set `ts` on a live relationship: `slot` indexes into the sorted live
    /// id set at apply time (a no-op when no relationship is live), so the
    /// strategy never has to know which ids the engine allocated.
    SetTs { slot: u16, ts: Option<i64> },
    /// Delete a live relationship, indexed the same way.
    Delete { slot: u16 },
}

/// `ts` alphabet: 20% missing, a small dense range to force equal-`ts`
/// ties, and the `i64` extremes plus zero to pin the comparator at the
/// edges.
fn ts_strategy() -> impl Strategy<Value = Option<i64>> {
    prop_oneof![
        2 => Just(None),
        6 => (0i64..64).prop_map(Some),
        1 => prop_oneof![Just(i64::MIN), Just(i64::MAX), Just(0i64)].prop_map(Some),
    ]
}

fn adj_op_strategy() -> impl Strategy<Value = AdjOp> {
    prop_oneof![
        5 => (
                0..NODE_POOL,
                0..NODE_POOL,
                0..TYPES.len(),
                ts_strategy()
            )
            .prop_map(|(start, end, ty, ts)| AdjOp::Create { start, end, ty, ts }),
        3 => (any::<u16>(), ts_strategy())
            .prop_map(|(slot, ts)| AdjOp::SetTs { slot, ts }),
        2 => any::<u16>().prop_map(|slot| AdjOp::Delete { slot }),
    ]
}

/// The observable adjacency of `node` in one direction: each type chunk's
/// merged delta+sealed read stream at the current commit ts (ADR-0551), with
/// hops whose relationship is no longer live filtered out and emptied chunks
/// dropped — the same shape the model maintains, whether or not the chunk
/// sealed.
fn observable(g: &Graph, node: NodeId, out: bool) -> ModelChunks {
    let (map, sealed) = if out {
        (&g.out_adj, &g.sealed_out)
    } else {
        (&g.in_adj, &g.sealed_in)
    };
    let Some(adj) = map.get(&node) else {
        return Vec::new();
    };
    let region = sealed.get(&node);
    let ts = g.commit_ts();
    adj.chunks
        .iter()
        .map(|(type_id, _)| {
            let mut hops = Vec::new();
            adj.for_each_hop_of(region.as_deref(), *type_id, ts, |hop| {
                hops.push((hop.ts, hop.id))
            });
            let hops: Vec<ModelHop> = hops
                .into_iter()
                .filter(|(_, id)| g.rel_exists(*id))
                .collect();
            (*type_id, hops)
        })
        .filter(|(_, hops)| !hops.is_empty())
        .collect()
}

/// The full-adjacency assertion after every op: every node's chunks, both
/// directions, byte-identical between the real store and the model.
fn assert_adjacency_eq(g: &Graph, model: &ModelAdj, nodes: &[NodeId]) {
    for &node in nodes {
        for out in [true, false] {
            let real = observable(g, node, out);
            let expected = model.observable(node, out);
            assert_eq!(
                real, expected,
                "adjacency divergence for node {node} (out={out})"
            );
        }
    }
}

/// Apply one op to the real graph (as its own committed write statement)
/// and to the model. `live` is the sorted set of currently live
/// relationship ids, shared by both sides.
fn apply_op(
    g: &Graph,
    model: &mut ModelAdj,
    nodes: &[NodeId],
    live: &mut Vec<RelId>,
    type_ids: &[u32],
    op: &AdjOp,
) {
    match *op {
        AdjOp::Create { start, end, ty, ts } => {
            let mut props = BTreeMap::new();
            if let Some(t) = ts {
                props.insert("ts".to_string(), Value::Int(t));
            }
            g.begin_write().unwrap();
            let id = g
                .create_rel(nodes[start], nodes[end], TYPES[ty].to_string(), props)
                .unwrap();
            g.commit_write();
            model.insert(nodes[start], true, type_ids[ty], id, ts);
            model.insert(nodes[end], false, type_ids[ty], id, ts);
            live.push(id);
        }
        AdjOp::SetTs { slot, ts } => {
            if live.is_empty() {
                return;
            }
            live.sort_unstable();
            let id = live[(slot as usize) % live.len()];
            let (start, end, type_id) = {
                let rel = g.rel(id).expect("live relationship materializes");
                (
                    rel.start,
                    rel.end,
                    g.interner.get_id(&rel.rel_type).unwrap(),
                )
            };
            let value = match ts {
                Some(t) => Value::Int(t),
                None => Value::Null,
            };
            g.begin_write().unwrap();
            g.set_property_rel(id, "ts", value);
            g.commit_write();
            model.reposition(start, true, type_id, id, ts);
            model.reposition(end, false, type_id, id, ts);
        }
        AdjOp::Delete { slot } => {
            if live.is_empty() {
                return;
            }
            live.sort_unstable();
            let idx = (slot as usize) % live.len();
            let id = live.remove(idx);
            g.begin_write().unwrap();
            g.delete_rel(id);
            g.commit_write();
            model.delete(id);
        }
    }
}

proptest! {
    #![proptest_config(config())]

    /// Reference-model equivalence of observable adjacency: after every op
    /// of a random create / ts-update / delete / re-add sequence, the real
    /// store's per-(node, type, direction) iteration is byte-identical to
    /// the plain `BTreeMap` model. Must pass on the current store
    /// semantics; it is the red-line guard for the sealed+delta rework.
    #[test]
    fn adjacency_matches_reference_model(ops in prop::collection::vec(adj_op_strategy(), 0..=200)) {
        let g = Graph::default();
        let mut nodes = Vec::with_capacity(NODE_POOL);
        g.begin_write().unwrap();
        for _ in 0..NODE_POOL {
            nodes.push(g.create_node(vec!["N".into()], BTreeMap::new()).unwrap());
        }
        g.commit_write();
        // Intern the type alphabet up front so chunk type ids are stable
        // across the whole case.
        let type_ids: Vec<u32> = TYPES.iter().map(|t| g.interner.intern(t)).collect();

        let mut model = ModelAdj::default();
        let mut live: Vec<RelId> = Vec::new();
        for op in &ops {
            apply_op(&g, &mut model, &nodes, &mut live, &type_ids, op);
            assert_adjacency_eq(&g, &model, &nodes);
        }
    }

    /// Compaction confluence (ADR-0551): the fold fires at an arbitrary
    /// interleaving with the op sequence — every due chunk, every tombstone
    /// reclaimable — and never moves observable adjacency. The never-folded
    /// model is the reference. Folding twice at the same watermark is the
    /// same as folding once (idempotence): the second pass finds nothing due.
    #[test]
    fn adjacency_model_proptest_compaction_confluence(
        steps in prop::collection::vec((adj_op_strategy(), any::<bool>()), 0..=200)
    ) {
        let g = Graph::default();
        let mut nodes = Vec::with_capacity(NODE_POOL);
        g.begin_write().unwrap();
        for _ in 0..NODE_POOL {
            nodes.push(g.create_node(vec!["N".into()], BTreeMap::new()).unwrap());
        }
        g.commit_write();
        let type_ids: Vec<u32> = TYPES.iter().map(|t| g.interner.intern(t)).collect();

        let mut model = ModelAdj::default();
        let mut live: Vec<RelId> = Vec::new();
        for (op, fold) in &steps {
            apply_op(&g, &mut model, &nodes, &mut live, &type_ids, op);
            if *fold {
                g.compact_adjacency_at_watermark(u64::MAX);
                g.compact_adjacency_at_watermark(u64::MAX);
            }
            assert_adjacency_eq(&g, &model, &nodes);
        }
    }
}

// ---------------------------------------------------------------------------
// Wave-2 stub (sealed+delta-adjacency feature).
// ---------------------------------------------------------------------------

/// TODO(sealed-delta-adjacency): a pinned read timestamp observes identical
/// adjacency across a snapshot freeze — take a reader snapshot (`read_ts`)
/// mid-sequence, seal the adjacency blocks, and assert `neighbors_at` at the
/// pinned ts is unchanged. Not writable yet: a reposition's delta hop carries
/// no `xmin`, so a snapshot older than the reposition sees the relocated hop
/// beside its tombstoned sealed position.
#[test]
#[ignore = "needs hop-level xmin versioning (a repositioned delta hop is visible to older snapshots)"]
fn adjacency_model_proptest_snapshot_freeze() {}

// ---------------------------------------------------------------------------
// Edge cases as plain unit tests.
// ---------------------------------------------------------------------------

/// Create followed by delete nets nothing: the observable adjacency is
/// empty afterwards, in both directions.
#[test]
fn add_then_delete_nets_nothing() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    g.begin_write().unwrap();
    let r = g
        .create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(7))]),
        )
        .unwrap();
    g.commit_write();
    assert_eq!(observable(&g, a, true).len(), 1);
    g.begin_write().unwrap();
    g.delete_rel(r);
    g.commit_write();
    assert!(observable(&g, a, true).is_empty());
    assert!(observable(&g, b, false).is_empty());
}

/// A ts-update of the only hop in a chunk keeps it the only hop, with the
/// new ts.
#[test]
fn ts_update_of_only_hop() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    g.begin_write().unwrap();
    let r = g
        .create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(1))]),
        )
        .unwrap();
    g.commit_write();
    g.begin_write().unwrap();
    g.set_property_rel(r, "ts", Value::Int(9));
    g.commit_write();
    assert_eq!(
        observable(&g, a, true),
        vec![(g.interner.get_id("T").unwrap(), vec![(Some(9), r)])]
    );
    // And removing ts entirely moves the hop to the missing-ts head.
    g.begin_write().unwrap();
    g.set_property_rel(r, "ts", Value::Null);
    g.commit_write();
    assert_eq!(
        observable(&g, a, true),
        vec![(g.interner.get_id("T").unwrap(), vec![(None, r)])]
    );
}

/// Equal-ts hops iterate newest-relationship-first (ADR-0203), and a
/// missing-ts hop precedes all ts-carrying hops (ADR-0035).
#[test]
fn equal_ts_ties_newest_first_and_missing_ts_first() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let mut ids = Vec::new();
    g.begin_write().unwrap();
    for _ in 0..3 {
        ids.push(
            g.create_rel(
                a,
                b,
                "T".into(),
                BTreeMap::from([("ts".into(), Value::Int(5))]),
            )
            .unwrap(),
        );
    }
    ids.push(g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap());
    g.commit_write();
    let tid = g.interner.get_id("T").unwrap();
    let hops = &observable(&g, a, true)[0];
    assert_eq!(hops.0, tid);
    assert_eq!(
        hops.1,
        vec![
            (None, ids[3]),
            (Some(5), ids[2]),
            (Some(5), ids[1]),
            (Some(5), ids[0])
        ]
    );
}

/// Chunks iterate in first-seen type order, and each type keeps its own
/// ts-DESC order independently (multi-chunk degree).
#[test]
fn multiple_type_chunks_iterate_in_first_seen_order() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    g.begin_write().unwrap();
    let r1 = g
        .create_rel(
            a,
            b,
            "TB".into(),
            BTreeMap::from([("ts".into(), Value::Int(1))]),
        )
        .unwrap();
    let r2 = g
        .create_rel(
            a,
            b,
            "TA".into(),
            BTreeMap::from([("ts".into(), Value::Int(2))]),
        )
        .unwrap();
    let r3 = g
        .create_rel(
            a,
            b,
            "TB".into(),
            BTreeMap::from([("ts".into(), Value::Int(3))]),
        )
        .unwrap();
    g.commit_write();
    let ta = g.interner.get_id("TA").unwrap();
    let tb = g.interner.get_id("TB").unwrap();
    assert_eq!(
        observable(&g, a, true),
        vec![
            (tb, vec![(Some(3), r3), (Some(1), r1)]),
            (ta, vec![(Some(2), r2)])
        ]
    );
}

/// The whole op alphabet nets out: build a shape, delete every
/// relationship, and the observable adjacency is empty everywhere.
#[test]
fn delete_everything_leaves_empty_observable_adjacency() {
    let g = Graph::default();
    let (a, b) = crate::graph::test_support::endpoints(&g);
    let mut ids = Vec::new();
    g.begin_write().unwrap();
    for (i, ty) in ["TA", "TB", "TC", "TA"].iter().enumerate() {
        let props = if i % 2 == 0 {
            BTreeMap::from([("ts".into(), Value::Int(i as i64))])
        } else {
            BTreeMap::new()
        };
        ids.push(g.create_rel(a, b, (*ty).into(), props).unwrap());
    }
    g.commit_write();
    for id in ids {
        g.begin_write().unwrap();
        g.delete_rel(id);
        g.commit_write();
    }
    for node in [a, b] {
        assert!(observable(&g, node, true).is_empty());
        assert!(observable(&g, node, false).is_empty());
    }
}
