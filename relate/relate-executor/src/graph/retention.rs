//! ADR-0610's retention: the checkpoint-facing chain export and restore.
//! While the `value-provenance` preview feature is on for an open, the
//! GC horizon drop exempts every version (the exemption lives in
//! [`super::index::Graph::gc_versions`]) and the checkpoint persists the
//! version chains it used to collapse. The export is the dirty delta —
//! every element touched since the last checkpoint with its *complete*
//! chain, every version's `xmin`/`xmax` beside the row it installed — so
//! a later part supersedes an earlier one per element and the fold needs
//! no per-version patching: a tombstone set by any commit (a retained one
//! or an off-period crash-window one) is already in the touched element's
//! next part. The restore replaces the checkpoint image's collapsed row
//! with the real chain, so the walk answers the same events after a
//! reopen, real `xmin` preserved — the image's restored rows are born at
//! [`super::restore::RESTORED_XMIN`] and would otherwise re-date every
//! key.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use relate_core::{PreviewFeature, Result};
use relate_engine_api::{
    RetainedNodeChain, RetainedNodeVersion, RetainedRelChain, RetainedRelVersion,
};

use super::chain::{Chain, Ver, VerRef};
use super::id_vec::Entry;
use super::id_vec::IdVec;
use super::{Graph, LiveNode, LiveRel};
use crate::memory::fit_reserved;
use crate::store::Interner;

impl Graph {
    /// Whether this open enabled `value-provenance` retention (ADR-0610)
    /// — the retained-chain family's publish/drop switch and the GC
    /// exemption's gate. The preview set is resolved once per open.
    pub fn value_provenance_retention(&self) -> bool {
        self.preview().enabled(PreviewFeature::ValueProvenance)
    }

    /// Collapse this open's retained history when the `value-provenance`
    /// opt-in is absent (ADR-0610) — the retention boundary's one rule,
    /// consulted by the checkpoint pin: an open without the opt-in
    /// bounds provenance at the last provenance-enabled open, and its
    /// checkpoint clears the retained record table (the records'
    /// in-memory half) then. The durable half — the family publish's
    /// answer to the now-empty table inside the checkpoint, which writes
    /// no new part and drops the family, the retained version chains'
    /// halves riding the same publish — follows from the same switch
    /// through the publish's own consult ([`Self::value_provenance_retention`],
    /// ADR-0610); a store that never opted in holds an empty table and
    /// no family, so the clear is its no-op and the byte-for-byte
    /// guarantee stands.
    pub fn collapse_retained_if_disabled(&self) {
        if !self.value_provenance_retention() {
            self.clear_transactions();
        }
    }

    /// Push the node chains the retained-chain family publishes this
    /// checkpoint (ADR-0610) to `sink`, one element's complete chain at
    /// a time: every node dirty since the last checkpoint, ids sorted
    /// so one checkpoint's delta is deterministic bytes. The publish's
    /// bounded fold (ADR-0555) — the family step serializes and appends
    /// each chain as it arrives, never a whole-delta Vec. Empty on a
    /// store that never opted in: the retention switch gates the fold
    /// itself, at its one home below.
    pub fn retained_node_chains_into(
        &self,
        sink: &mut dyn FnMut(&RetainedNodeChain) -> Result<()>,
    ) -> Result<()> {
        self.retained_chains_into(
            self.dirty_node_ids(),
            &self.nodes,
            retained_node_version,
            |id, versions| RetainedNodeChain { id, versions },
            sink,
        )
    }

    /// [`Self::retained_node_chains_into`]'s twin over the relationship
    /// chains.
    pub fn retained_rel_chains_into(
        &self,
        sink: &mut dyn FnMut(&RetainedRelChain) -> Result<()>,
    ) -> Result<()> {
        self.retained_chains_into(
            self.dirty_rel_ids(),
            &self.rels,
            retained_rel_version,
            |id, versions| RetainedRelChain { id, versions },
            sink,
        )
    }

    /// The two chain folds' one body (ADR-0610) — the shared half the
    /// node and relationship exporters above delegate to, each
    /// supplying only its chain map, its version builder and its chain
    /// row. The retention switch gates here, its one home
    /// ([`Self::value_provenance_retention`], the publish's own
    /// consult): a store that never opted in answers nothing, keeping
    /// the trait's "empty on a store that never opted in" promise
    /// without relying on every caller to check the switch first. The
    /// dirty delta's ids are sorted so one checkpoint's delta is
    /// deterministic bytes, and each dirty element's complete chain is
    /// built and handed to `sink` one at a time — the publish's bounded
    /// fold (ADR-0555), never a whole-delta Vec. A dirty id with no
    /// slot (an aborted create's leftover mark) contributes nothing,
    /// and so does an empty chain (the only other empty case).
    fn retained_chains_into<Live, Version, Out>(
        &self,
        ids: impl IntoIterator<Item = i64>,
        map: &IdVec<Chain<Live>>,
        version_of: impl Fn(VerRef<'_, Live>, i64, &Interner) -> Version,
        chain_of: impl Fn(i64, Vec<Version>) -> Out,
        sink: &mut dyn FnMut(&Out) -> Result<()>,
    ) -> Result<()> {
        if !self.value_provenance_retention() {
            return Ok(());
        }
        let mut ids: Vec<_> = ids.into_iter().collect();
        ids.sort_unstable();
        for id in ids {
            let versions: Vec<_> = map
                .get(&id)
                .map(|vers| {
                    vers.iter()
                        .map(|v| version_of(v, id, &self.interner))
                        .collect()
                })
                .unwrap_or_default();
            // An aborted create's leftover dirty mark has no slot — and
            // an empty chain (the only other empty case) — contributes
            // nothing to the delta.
            if versions.is_empty() {
                continue;
            }
            sink(&chain_of(id, versions))?;
        }
        Ok(())
    }

    /// Restore one node's complete retained version chain (ADR-0610) —
    /// the family load's install: the image's collapsed row for the id
    /// (an inline slot) is replaced, an id the image never held (a
    /// deleted element's tombstoned chain) is inserted. Only valid on a
    /// graph being restored, before any write.
    pub fn restore_retained_node_chain(&self, chain: RetainedNodeChain) -> Result<()> {
        self.check_restoring()?;
        let next = super::checked_persisted_id(chain.id, "node")?;
        self.next_node_id.fetch_max(next, Ordering::Relaxed);
        let RetainedNodeChain { id, versions } = chain;
        let mut vers = Vec::with_capacity(versions.len());
        for version in versions {
            // Every version was the live row at its commit, so its labels
            // were catalog-recorded then — the union restores exactly
            // that vocabulary.
            let labels = Arc::unwrap_or_clone(version.labels);
            self.catalog
                .lock()
                .unwrap()
                .record_labels(labels.iter().cloned());
            let properties = BTreeMap::from(Arc::unwrap_or_clone(version.properties));
            vers.push(Ver {
                xmin: version.xmin,
                xmax: version.xmax,
                live: LiveNode::from_parts(&self.interner, labels, properties),
            });
        }
        restore_chain(&self.nodes, id, vers)
    }

    /// [`Self::restore_retained_node_chain`]'s twin over the
    /// relationship chains.
    pub fn restore_retained_rel_chain(&self, chain: RetainedRelChain) -> Result<()> {
        self.check_restoring()?;
        let next = super::checked_persisted_id(chain.id, "relationship")?;
        self.next_rel_id.fetch_max(next, Ordering::Relaxed);
        let RetainedRelChain { id, versions } = chain;
        let mut vers = Vec::with_capacity(versions.len());
        for version in versions {
            // A rel version's endpoints and type were live at its commit;
            // one spelling restores the type's catalog vocabulary.
            super::checked_persisted_id(version.start, "relationship start")?;
            super::checked_persisted_id(version.end, "relationship end")?;
            self.catalog
                .lock()
                .unwrap()
                .record_rel_type(version.rel_type.clone());
            let properties = BTreeMap::from(Arc::unwrap_or_clone(version.properties));
            vers.push(Ver {
                xmin: version.xmin,
                xmax: version.xmax,
                live: LiveRel::from_parts(
                    &self.interner,
                    version.start,
                    version.end,
                    &version.rel_type,
                    properties,
                ),
            });
        }
        restore_chain(&self.rels, id, vers)
    }
}

/// One node version as the family's serialized row (ADR-0610): the
/// bounds beside the row, in the WAL ops' own field types.
fn retained_node_version(
    version: VerRef<'_, LiveNode>,
    id: i64,
    interner: &Interner,
) -> RetainedNodeVersion {
    let node = version.live.to_node(id, interner);
    RetainedNodeVersion {
        xmin: version.xmin,
        xmax: version.xmax,
        labels: node.labels,
        properties: node.properties,
    }
}

/// One relationship version as the family's serialized row (ADR-0610).
fn retained_rel_version(
    version: VerRef<'_, LiveRel>,
    id: i64,
    interner: &Interner,
) -> RetainedRelVersion {
    let rel = version.live.to_rel(id, interner);
    RetainedRelVersion {
        xmin: version.xmin,
        xmax: version.xmax,
        start: rel.start,
        end: rel.end,
        rel_type: rel.rel_type,
        properties: rel.properties,
    }
}

/// Install one element's restored chain (ADR-0610) — the same slot
/// contract the image's row restore holds: replace an inline row, insert
/// a vacant id, refuse a spilled slot (a graph with committed writes is
/// no longer being restored). A part that decoded an element with no
/// versions is refused here — a chain is never empty (the inline row and
/// the spilled list both hold at least one version), so installing one
/// would seed the later `expect` panics; the load names the part.
fn restore_chain<Live>(map: &IdVec<Chain<Live>>, id: i64, mut vers: Vec<Ver<Live>>) -> Result<()> {
    if vers.is_empty() {
        return Err(relate_core::Error::storage(
            "retained-chain restore of an element with no versions",
        ));
    }
    let chain = if vers.len() == 1 && vers[0].xmax == 0 {
        let Ver { xmin, live, .. } = vers.pop().expect("one version");
        Chain::inline(xmin, live)
    } else {
        fit_reserved(&mut vers);
        Chain::Spilled(vers)
    };
    match map.entry(id) {
        Entry::Vacant(mut slot) => {
            slot.insert(chain);
        }
        Entry::Occupied(mut slot) => match slot.get_mut() {
            prev @ Chain::Inline { .. } => *prev = chain,
            Chain::Spilled(_) => {
                return Err(relate_core::Error::storage(
                    "checkpoint restore into a graph with committed writes",
                ));
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_core::{NodeId, PreviewFeature, PreviewFeatures, PropertyMap, Value};
    use relate_engine_api::{RecordType, TransactionRecord};

    use super::*;

    /// The retention-on fixture: a default graph with the
    /// `value-provenance` opt-in installed — the preview set an
    /// opted-in store resolves at open, so every export-side test below
    /// runs behind the fold's gate. The restore side of each roundtrip
    /// stays a plain default graph: the load installs whatever the
    /// parts hold regardless of the switch (the collapse's own rule,
    /// consulted only at the next checkpoint).
    fn gated() -> Graph {
        let mut g = Graph::default();
        g.configure_preview(PreviewFeatures::none().with_feature(PreviewFeature::ValueProvenance));
        g
    }

    /// The write-window fixture every test below drives its edits
    /// through: one committed edit per timestamp.
    fn write_at(g: &Graph, edit: impl FnOnce(&Graph)) {
        g.begin_write().unwrap();
        edit(g);
        g.commit_write();
    }

    /// A retained record for the commit at `commit_ts`, seeded through
    /// the one insert the live pipeline itself uses — carrying the
    /// provenance field the table's retention policy reads.
    fn retained(g: &Graph, commit_ts: u64) {
        g.record_transaction(TransactionRecord {
            lsn: 10 * commit_ts,
            commit_ts,
            metadata: None,
            changed_keys: Some(Vec::new()),
            record_type: RecordType::default(),
        });
    }

    /// A node born at commit 1 with `k` overwritten at commits 2 and 3,
    /// each commit retaining its record.
    fn three_version_node(g: &Graph) {
        let mut birth = BTreeMap::new();
        birth.insert("k".to_string(), Value::Int(1));
        write_at(g, |g| {
            g.create_node(vec!["A".into()], birth).unwrap();
        });
        write_at(g, |g| g.set_property_node(0, "k", Value::Int(2)));
        write_at(g, |g| g.set_property_node(0, "k", Value::Int(3)));
        for commit_ts in 1..=3 {
            retained(g, commit_ts);
        }
    }

    /// The walk's events as `(commitTs, value)` pairs — the shape every
    /// assertion below reads.
    fn events(g: &Graph, id: NodeId) -> Vec<(u64, Value)> {
        g.node_property_events(id, "k", None)
            .into_iter()
            .map(|event| (event.record.commit_ts, event.value))
            .collect()
    }

    /// Every node chain of the dirty delta, collected through the
    /// family publish's own fold — the tests' read of the delta.
    fn node_chains(g: &Graph) -> Vec<RetainedNodeChain> {
        let mut chains = Vec::new();
        g.retained_node_chains_into(&mut |chain| {
            chains.push(chain.clone());
            Ok(())
        })
        .unwrap();
        chains
    }

    /// [`node_chains`]'s twin over the relationship chains.
    fn rel_chains(g: &Graph) -> Vec<RetainedRelChain> {
        let mut chains = Vec::new();
        g.retained_rel_chains_into(&mut |chain| {
            chains.push(chain.clone());
            Ok(())
        })
        .unwrap();
        chains
    }

    #[test]
    fn the_delta_is_every_dirty_elements_complete_chain() {
        let g = gated();
        three_version_node(&g);
        write_at(&g, |g| {
            g.create_node(vec![], BTreeMap::new()).unwrap();
        });
        // Both touched elements are dirty; the first carries its whole
        // three-version chain, the second its single inline row.
        let chains = node_chains(&g);
        assert_eq!(chains.len(), 2);
        assert_eq!(chains[0].id, 0);
        assert_eq!(chains[0].versions.len(), 3);
        assert_eq!(
            (chains[0].versions[0].xmin, chains[0].versions[0].xmax),
            (1, 2)
        );
        assert_eq!(
            (chains[0].versions[1].xmin, chains[0].versions[1].xmax),
            (2, 3)
        );
        assert_eq!(
            (chains[0].versions[2].xmin, chains[0].versions[2].xmax),
            (3, 0)
        );
        assert_eq!(chains[1].versions.len(), 1);
        // Clearing the checkpoint dirty set empties the delta: the next
        // checkpoint's part carries only what the next interval touched.
        g.clear_checkpoint_dirty();
        assert!(node_chains(&g).is_empty());
        assert!(rel_chains(&g).is_empty());
    }

    #[test]
    fn a_restore_replaces_the_collapsed_row_with_real_xmin() {
        let g = gated();
        three_version_node(&g);
        let chain = node_chains(&g).into_iter().next().unwrap();
        // The reload: the image's collapsed row (inline, born at the
        // restored row's ts 1) is replaced by the family's real chain,
        // and the walk answers the same events with real commitTs.
        let reloaded = Graph::default();
        let mut row = BTreeMap::new();
        row.insert("k".to_string(), Value::Int(3));
        reloaded
            .load_checkpoint_node(relate_core::Node {
                id: 0,
                labels: Arc::new(vec!["A".to_string()]),
                properties: Arc::new(PropertyMap::from(row)),
            })
            .unwrap();
        reloaded.restore_retained_node_chain(chain).unwrap();
        for commit_ts in 1..=3 {
            retained(&reloaded, commit_ts);
        }
        assert_eq!(
            events(&reloaded, 0),
            [(1, Value::Int(1)), (2, Value::Int(2)), (3, Value::Int(3))]
        );
    }

    #[test]
    fn a_tombstoned_chain_restores_into_a_vacant_slot() {
        let g = gated();
        three_version_node(&g);
        // A delete tombstones the whole chain; the delta still carries it
        // (deleted elements stay auditable, ADR-0610) and the restore
        // inserts it into a graph whose image never held the id.
        write_at(&g, |g| g.delete_node(0, false).unwrap());
        let chain = node_chains(&g)
            .into_iter()
            .find(|chain| chain.id == 0)
            .unwrap();
        assert!(chain.versions.iter().all(|version| version.xmax != 0));
        let reloaded = Graph::default();
        reloaded.restore_retained_node_chain(chain).unwrap();
        // Nothing is visible at or past the tombstone — a dead chain
        // answers no query — and the chain is there for the audit walk's
        // events.
        assert!(reloaded.node_at(0, 3).is_some());
        assert!(reloaded.node_at(0, 4).is_none());
        for commit_ts in 1..=3 {
            retained(&reloaded, commit_ts);
        }
        assert_eq!(
            events(&reloaded, 0),
            [(1, Value::Int(1)), (2, Value::Int(2)), (3, Value::Int(3))]
        );
    }

    #[test]
    fn relationship_chains_roundtrip_the_same_contract() {
        let g = gated();
        write_at(&g, |g| {
            let a = g.create_node(vec![], BTreeMap::new()).unwrap();
            let b = g.create_node(vec![], BTreeMap::new()).unwrap();
            let mut props = BTreeMap::new();
            props.insert("k".to_string(), Value::Int(1));
            g.create_rel(a, b, "R".into(), props).unwrap();
        });
        write_at(&g, |g| g.set_property_rel(0, "k", Value::Int(2)));
        for commit_ts in 1..=2 {
            retained(&g, commit_ts);
        }
        let chains = rel_chains(&g);
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].versions.len(), 2);
        assert_eq!(chains[0].versions[0].rel_type, "R");
        let reloaded = Graph::default();
        reloaded
            .restore_retained_rel_chain(chains.into_iter().next().unwrap())
            .unwrap();
        for commit_ts in 1..=2 {
            retained(&reloaded, commit_ts);
        }
        let events: Vec<(u64, Value)> = reloaded
            .rel_property_events(0, "k", None)
            .into_iter()
            .map(|event| (event.record.commit_ts, event.value))
            .collect();
        assert_eq!(events, [(1, Value::Int(1)), (2, Value::Int(2))]);
    }

    /// The retention switch gates the fold itself (ADR-0610): a graph
    /// with dirty, versioned elements but no opt-in answers no chain —
    /// the trait's "empty on a store that never opted in" promise, kept
    /// at the fold's one home rather than assumed of every caller.
    #[test]
    fn the_exports_answer_nothing_without_the_opt_in() {
        let g = Graph::default();
        three_version_node(&g);
        assert!(node_chains(&g).is_empty());
        assert!(rel_chains(&g).is_empty());
    }

    /// A chain with no versions is not a chain (ADR-0610): the inline
    /// row and the spilled list both hold at least one version, so a
    /// part that decoded `"versions":[]` is refused at the restore —
    /// never installed as an empty `Chain::Spilled` whose later reads
    /// panic. The graph-level arm of the planted-part test
    /// `a_part_with_no_versions_fails_the_open_loudly`.
    #[test]
    fn a_restore_of_no_versions_is_refused() {
        let g = Graph::default();
        let error = g
            .restore_retained_node_chain(RetainedNodeChain {
                id: 0,
                versions: Vec::new(),
            })
            .expect_err("an empty versions array is refused");
        assert!(
            error
                .to_string()
                .contains("retained-chain restore of an element with no versions"),
            "{error}"
        );
    }
}
