//! Checkpoint-facing graph API: restoring checkpoint rows into a graph
//! being loaded, and the payload sizes and dirty ids a checkpoint reads.

use super::id_vec::IdVec;
use super::*;
use crate::store::NameCache;

/// Every checkpoint row is restored as a version born at ts 1.
pub(super) const RESTORED_XMIN: u64 = 1;

/// Install a slice's checkpoint rows as each id's only version, inline,
/// under one store lock. Before any write a slot holds at most an earlier
/// row for the same id — overlapping legacy parts (ADR-0183) can hold an id
/// twice — and the later row replaces it rather than stacking a second
/// live version a delete would leave visible. One lock for the slice, not
/// one per row: concurrent load workers would otherwise convoy on the
/// store's lock and land slower than the serial load. The slice's rows are
/// admitted whole before the first write: a slice with a committed write in
/// it refuses without restoring a single row.
fn restore_slice<Live>(map: &IdVec<Chain<Live>>, rows: Vec<(i64, Live)>) -> Result<()> {
    let rows = rows
        .into_iter()
        .map(|(id, live)| (id, Chain::inline(RESTORED_XMIN, live)))
        .collect();
    map.install_batch(rows, |previous| match previous {
        None | Some(Chain::Inline { .. }) => Ok(()),
        Some(Chain::Spilled(_)) => Err(relate_core::Error::storage(
            "checkpoint restore into a graph with committed writes",
        )),
    })
}

/// Checkpoint payload of `id`'s version visible at `ts`, measured by
/// `bytes`; `None` when no version is visible.
fn visible_checkpoint_bytes<Live>(
    map: &IdVec<Chain<Live>>,
    id: i64,
    ts: u64,
    bytes: impl FnOnce(&Live) -> Result<u64>,
) -> Result<Option<u64>> {
    let Some(vers) = map.get(&id) else {
        return Ok(None);
    };
    visible_version(&vers, ts)
        .map(|v| bytes(v.live))
        .transpose()
}

impl Graph {
    /// Node ids changed since the last `clear_checkpoint_dirty` (ADR-0022).
    pub fn dirty_node_ids(&self) -> HashSet<NodeId> {
        self.dirty_nodes.lock().unwrap().clone()
    }

    /// Relationship ids changed since the last `clear_checkpoint_dirty`.
    pub fn dirty_rel_ids(&self) -> HashSet<RelId> {
        self.dirty_rels.lock().unwrap().clone()
    }

    pub fn clear_checkpoint_dirty(&self) {
        self.dirty_nodes.lock().unwrap().clear();
        self.dirty_rels.lock().unwrap().clear();
    }

    /// Variable checkpoint payload of the node visible at `ts`: label bytes
    /// plus tagged property JSON. The Parquet codec adds its Int64 column
    /// width. Borrows compact properties; does not build the property map.
    pub fn node_checkpoint_bytes(&self, id: NodeId, ts: u64) -> Result<Option<u64>> {
        visible_checkpoint_bytes(&self.nodes, id, ts, |live| {
            live.checkpoint_bytes(&self.interner)
        })
    }

    /// Variable checkpoint payload of the relationship visible at `ts`:
    /// type-name bytes plus tagged property JSON. The Parquet codec adds
    /// its Int64 column width.
    pub fn rel_checkpoint_bytes(&self, id: RelId, ts: u64) -> Result<Option<u64>> {
        visible_checkpoint_bytes(&self.rels, id, ts, |live| {
            live.checkpoint_bytes(&self.interner)
        })
    }

    /// Restore a node from a checkpoint (visible at commit_ts). Loading an
    /// id again replaces its row; the later row wins. Only valid in the
    /// graph's restore phase, before any write and before
    /// `Graph::finish_restore` closes it: either is `Error::Storage`. The
    /// label index is built by `Graph::finish_restore` after the load.
    pub fn load_checkpoint_node(&self, node: Node) -> Result<()> {
        self.load_checkpoint_nodes(&[node])
    }

    /// Restore a relationship from a checkpoint. Loading an id again
    /// replaces its row, endpoints and type included; the later row wins.
    /// Only valid in the graph's restore phase, before any write and before
    /// `Graph::finish_restore` closes it: either is `Error::Storage`.
    /// Adjacency is built from the restored rows by `Graph::finish_restore`
    /// after the load.
    pub fn load_checkpoint_rel(&self, rel: Relationship) -> Result<()> {
        self.load_checkpoint_rels(&[rel])
    }

    /// Install a slice of checkpoint nodes — one decoded batch, a
    /// window-disjoint run of ids from one part. The installer validates
    /// every row id in the slice, so a caller does not repeat the check;
    /// the next-id bump, the catalog update and the store lock are one per
    /// slice instead of one per row, so concurrent load workers install
    /// without contending on a per-row lock.
    pub fn load_checkpoint_nodes(&self, nodes: &[Node]) -> Result<()> {
        self.begin_restoring()?;
        let mut next = 0;
        for node in nodes {
            next = next.max(checked_persisted_id(node.id, "node")?);
        }
        self.next_node_id.fetch_max(next, Ordering::Relaxed);
        self.catalog
            .lock()
            .unwrap()
            .record_labels(nodes.iter().flat_map(|node| node.labels.iter().cloned()));
        let mut names = NameCache::default();
        let rows = nodes
            .iter()
            .map(|node| {
                let live = LiveNode::from_parts_cached(
                    &self.interner,
                    &mut names,
                    &node.labels,
                    &node.properties,
                );
                (node.id, live)
            })
            .collect();
        restore_slice(&self.nodes, rows)
    }

    /// Relationship sibling of [`Self::load_checkpoint_nodes`]: it
    /// validates each row's id and both endpoints, and the later row wins
    /// by id, endpoints and type included.
    pub fn load_checkpoint_rels(&self, rels: &[Relationship]) -> Result<()> {
        self.begin_restoring()?;
        let mut next = 0;
        for rel in rels {
            next = next.max(checked_persisted_id(rel.id, "relationship")?);
            checked_persisted_id(rel.start, "relationship start")?;
            checked_persisted_id(rel.end, "relationship end")?;
        }
        self.next_rel_id.fetch_max(next, Ordering::Relaxed);
        {
            let mut catalog = self.catalog.lock().unwrap();
            for rel in rels {
                catalog.record_rel_type(rel.rel_type.clone());
            }
        }
        let mut names = NameCache::default();
        let rows = rels
            .iter()
            .map(|rel| {
                let live = LiveRel::from_parts_cached(
                    &self.interner,
                    &mut names,
                    rel.start,
                    rel.end,
                    &rel.rel_type,
                    &rel.properties,
                );
                (rel.id, live)
            })
            .collect();
        restore_slice(&self.rels, rows)
    }

    /// End a checkpoint load, after its last row: close the restore phase
    /// first — "the load is over" is true from the first step of
    /// finalization, so no slice is admitted while the tail runs — then
    /// build adjacency (the build itself emits the durable shape, the
    /// chunks at or above the seal floor straight into their folded form,
    /// ADR-0608) and clear the dirty ids, which the checkpoint already
    /// holds. Called once per load. The index rebuild is deliberately NOT
    /// here: the open flow's single post-replay site runs it exactly once
    /// per open, after the graph is final (snapshot plus replayed tail) — a
    /// snapshot+tail reopen must not pay the whole-graph rebuild twice
    /// (LABS-1112).
    #[doc(hidden)]
    pub fn finish_restore(&self) {
        self.close_restoring();
        self.rebuild_adj();
        self.clear_checkpoint_dirty();
    }

    /// A checkpoint load's entry: take the graph's restore phase for a
    /// slice. The phase is one latch — born open, closed by
    /// [`Self::close_restoring`] — so every later slice of the same load
    /// takes the same open latch; a graph that left it is refused in
    /// [`Self::check_restoring`]. The one sanctioned re-take is the open
    /// path's retained-family window,
    /// [`Self::retained_families_restore_step`], below.
    fn begin_restoring(&self) -> Result<()> {
        if self.in_write.load(Ordering::Acquire) {
            return Err(relate_core::Error::storage(
                "checkpoint restore while a write is open",
            ));
        }
        self.check_restoring()
    }

    /// The open path's retained-family window (ADR-0610): the codec
    /// tail's [`Self::finish_restore`] closed the phase its rows opened,
    /// and the retained `transactions/` and `chains/` families are the
    /// same open's later restore slices — this re-takes the phase for
    /// the load step and closes it after, whatever the step returns.
    /// [`Self::begin_restoring`]'s write guard applies, so the window is
    /// the open path's construction only (after the codec load, before
    /// the WAL replay or any write), and the re-take is **one-shot**
    /// (`retained_window_taken`): a second call — a caller reaching the
    /// step after the open completed, when the in-write guard alone
    /// would pass and a published commit already left the graph with
    /// committed writes — is refused, so the latch cannot re-open and
    /// let a load overwrite live rows. Adjacency is never rebuilt here:
    /// each family chain replaces the image's row slot-for-slot (its
    /// newest version is that row), so the codec tail's build stands.
    pub fn retained_families_restore_step(
        &self,
        load: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        if self.in_write.load(Ordering::Acquire) {
            return Err(relate_core::Error::storage(
                "retained-family restore while a write is open",
            ));
        }
        if self.retained_window_taken.swap(true, Ordering::AcqRel) {
            return Err(relate_core::Error::storage(
                "retained-family restore window re-entered after the open's construction",
            ));
        }
        self.restoring.store(true, Ordering::Release);
        let loaded = load();
        self.close_restoring();
        loaded
    }

    /// The latch's read half: refuse a load the graph no longer accepts —
    /// the restore phase closed by `finish_restore` or a published commit.
    /// The phase, never a slot's version-chain shape, is the evidence: a
    /// committed single-version write leaves the `Inline` chain a restored
    /// row itself holds, and shape cannot tell the two apart.
    pub(super) fn check_restoring(&self) -> Result<()> {
        if !self.restoring.load(Ordering::Acquire) {
            return Err(relate_core::Error::storage(
                "checkpoint restore into a graph with committed writes",
            ));
        }
        Ok(())
    }

    /// The latch's closing transition. Every entry that ends the restore
    /// phase — [`Self::finish_restore`], a published commit, a redo
    /// apply — closes through here; the open path's retained-family
    /// window ([`Self::retained_families_restore_step`]) is the one
    /// re-take, and no phase re-opens after a commit.
    pub(super) fn close_restoring(&self) {
        self.restoring.store(false, Ordering::Release);
    }
}
