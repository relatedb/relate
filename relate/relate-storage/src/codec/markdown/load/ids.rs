//! The load path's identity resolution (ADR-0406 §4/§5): the stable-id
//! restoration from the sidecar manifest, and the link pass that
//! resolves targets — materializing stub nodes — while it restores or
//! assigns relationship ids.
//!
//! Extracted from `load.rs` (the no-1k-line rule, LABS-311), mirroring
//! how `load/tests.rs` sits beside it: the orchestrator keeps the
//! frontmatter classification, the parse/canonicalize passes, the
//! embeddings, and the expectation recording; this module owns the two
//! halves of the stable-id contract (nodes and relationships) and the
//! target resolution between them.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Node, Relationship, Result, Value};
use relate_markdown as md;

use super::super::super::convert::props_from_cypher_json;
use super::super::super::model::INSTALL_SLICE_ROWS;
use super::super::manifest::VaultManifest;
use super::super::vault::file_name;
use super::super::{rel_identity, vault_key, DEFAULT_KEY_PROPERTY};
use super::ParsedFile;
use relate_markdown::fold_case;

/// The stable-id assignment's own state: every canonical key's id, the
/// next fresh id — the one "next free id" fact every consumer reads —
/// and the short-form link candidate index. The pair cannot desync by
/// construction: every key insertion goes through
/// [`IdAssignment::register`], which maintains both tables, the id
/// map is read through [`IdAssignment::ids`], and the allocator head
/// is read through [`IdAssignment::next_fresh_id`] —
/// [`IdAssignment::assign_fresh`] is the one writer of both halves.
pub(super) struct IdAssignment {
    ids_by_key: BTreeMap<String, i64>,
    next_fresh_id: i64,
    short_links: ShortLinkIndex,
}

impl IdAssignment {
    /// The id map's read view — the load's node pass resolves ids
    /// through it; insertion is [`Self::register`]'s alone.
    pub(super) fn ids(&self) -> &BTreeMap<String, i64> {
        &self.ids_by_key
    }

    /// The id map's by-value handoff: the load's expectation recording
    /// takes ownership of the restored ids — the sidecar's next image.
    pub(super) fn into_ids_by_key(self) -> BTreeMap<String, i64> {
        self.ids_by_key
    }

    /// The allocator head's read view — the one "next free id" fact
    /// the load's expectation recording and the graph's id floor
    /// read. The head advances only through [`Self::assign_fresh`].
    pub(super) fn next_fresh_id(&self) -> i64 {
        self.next_fresh_id
    }

    /// The one key insertion: a key enters the id map and the
    /// short-form candidate index together, and the index's membership
    /// is derived from the map insert itself — only a first insertion
    /// indexes the key, so a re-registration can never leave a stale
    /// or duplicate group entry and the two tables cannot desync.
    fn register(&mut self, key: &str, id: i64) {
        if self.ids_by_key.insert(key.to_string(), id).is_none() {
            self.short_links.insert(key);
        }
    }

    /// Hand out the next fresh id and register its key — the one
    /// fresh-id path the restore and stub materialization share.
    fn assign_fresh(&mut self, key: &str) -> i64 {
        let id = self.next_fresh_id;
        self.next_fresh_id += 1;
        self.register(key, id);
        id
    }
}

/// The rel side's assignment state, mirroring [`IdAssignment`]: the
/// assigned identity map (keyed by the canonical relationship
/// identity) and the allocator head behind the one
/// [`RelAssignment::assign`] — the validate-then-allocate contract the
/// node side runs, over the manifest invariant's rel half
/// ([`restored_id`]) — so the head cannot be advanced anywhere else
/// and the carrier is never rebuilt from loose locals.
pub(super) struct RelAssignment {
    ids: BTreeMap<String, i64>,
    next_rel_id: i64,
}

impl RelAssignment {
    /// The allocator, seeded with the manifest's recorded next free
    /// rel id — the bound every restored rel id is judged against,
    /// captured before any fresh id advances the live head.
    fn new(next_rel_id: i64) -> Self {
        Self {
            ids: BTreeMap::new(),
            next_rel_id,
        }
    }

    /// The identity map's by-value handoff — the sidecar's next image.
    pub(super) fn into_ids(self) -> BTreeMap<String, i64> {
        self.ids
    }

    /// The allocator's final next-free value.
    pub(super) fn next_rel_id(&self) -> i64 {
        self.next_rel_id
    }

    /// One relationship's id: the manifest's restored one when it
    /// holds and passes the invariant, else the next fresh id — the
    /// node side's [`IdAssignment::assign_fresh`] contract over the
    /// rel half.
    fn assign(&mut self, restored: Option<i64>, seed: i64, identity: &str) -> Result<i64> {
        let id = match restored_id(restored, seed, "relationship", identity)? {
            Some(restored) => restored,
            None => {
                let fresh = self.next_rel_id;
                self.next_rel_id += 1;
                fresh
            }
        };
        self.ids.insert(identity.to_string(), id);
        Ok(id)
    }
}

/// The manifest invariant's one home (ADR-0406 §5): a sidecar-restored
/// id must stay below the recorded next-free seed — a sidecar that
/// violates it would hand a fresh row an id a restored one already owns
/// (the later row silently replacing it), so it fails closed like
/// every other corrupt-sidecar shape. `None` is an absent identity (a
/// fresh id); the fresh allocation stays at the caller, where the
/// allocators differ.
fn restored_id(
    restored: Option<i64>,
    seed: i64,
    kind: &str,
    identity: &str,
) -> Result<Option<i64>> {
    match restored {
        Some(id) if id >= seed => Err(md::vault_error(format!(
            "sidecar {kind} id {id} for '{identity}' is not below the recorded next id {seed}"
        ))),
        other => Ok(other),
    }
}

/// Restore the stable node ids from the sidecar
/// manifest's map so id-based WAL replay after a checkpoint addresses
/// the same nodes the writes named; a first (bootstrap) load assigns
/// them in deterministic path order.
pub(super) fn restore_ids(parsed: &[ParsedFile], manifest: &VaultManifest) -> Result<IdAssignment> {
    let mut ids = IdAssignment {
        ids_by_key: BTreeMap::new(),
        next_fresh_id: manifest.next_node_id.max(1),
        short_links: ShortLinkIndex::default(),
    };
    // The recorded seed, captured before any fresh id advances the live
    // allocator head: the guard below judges restored ids against it, so a
    // sidecar id can never collide with a fresh id handed out earlier.
    let seeded_next_id = ids.next_fresh_id;
    for file in parsed {
        match restored_id(
            manifest.node_ids.get(&file.key).copied(),
            seeded_next_id,
            "node",
            &file.key,
        )? {
            Some(restored) => ids.register(&file.key, restored),
            None => {
                ids.assign_fresh(&file.key);
            }
        }
    }
    Ok(ids)
}

/// Secondary label → primary label mapping (ADR-0406 §2): a target
/// naming a secondary label resolves to that node's primary path.
fn primary_label_map(parsed: &[ParsedFile]) -> BTreeMap<String, String> {
    let mut primary_of: BTreeMap<String, String> = BTreeMap::new();
    for file in parsed {
        for label in std::iter::once(&file.label).chain(&file.doc.extra_labels) {
            primary_of
                .entry(fold_case(label))
                .or_insert_with(|| file.label.clone());
        }
    }
    primary_of
}

/// A resolved full-form target (ADR-0406 §4): the existing node, or
/// the not-yet-materialized stub — a real node with the link's label
/// and key value, ready to be filled in (Obsidian's
/// click-to-create). The invariant "fresh target = stub row" is the
/// type's two variants, never a nullable channel beside an id.
enum ResolvedTarget {
    Existing { key: String, id: i64 },
    Stub { key: String, node: Node },
}

/// Resolve a full-form link target (ADR-0406 §4): the named label
/// substitutes to the target's primary label (secondary labels map to
/// primaries), and a not-yet-materialized target is materialized as a
/// stub right here. The stub row is returned to the caller's batch,
/// not installed inline: nothing in this pass reads installed rows, so
/// the slices install whole.
fn resolve_full_target(
    keys: &BTreeMap<String, String>,
    ids: &mut IdAssignment,
    primary_of: &BTreeMap<String, String>,
    label: &str,
    key: &str,
) -> Result<ResolvedTarget> {
    let primary = primary_of
        .get(&fold_case(label))
        .cloned()
        .unwrap_or_else(|| label.to_string());
    let target_key = vault_key(&primary, key);
    match ids.ids().get(&target_key) {
        Some(&id) => Ok(ResolvedTarget::Existing {
            key: target_key,
            id,
        }),
        None => {
            let key_name = keys
                .get(&primary)
                .cloned()
                .unwrap_or_else(|| DEFAULT_KEY_PROPERTY.to_string());
            let id = ids.assign_fresh(&target_key);
            let mut stub_props = BTreeMap::new();
            stub_props.insert(key_name, Value::String(key.into()));
            Ok(ResolvedTarget::Stub {
                key: target_key,
                node: Node {
                    id,
                    labels: Arc::new(vec![primary.clone()]),
                    properties: relate_core::PropertyMap::share(stub_props),
                },
            })
        }
    }
}

/// Resolve a short-form link target (ADR-0406 §4): the unique
/// case-folded name match over the loaded keys — by construction
/// already a key in the id map. No match, or several, fails
/// closed with the authored errors naming the file.
fn resolve_short_target(ids: &IdAssignment, path: &str, name: &str) -> Result<(String, i64)> {
    // The candidate set is the owned basename index — the same
    // membership, in the id map's own key order, a scan of the map
    // would yield (every insertion maintains both tables), so the
    // unique/ambiguous verdict is the scan's, at one lookup.
    match ids.short_links.candidates(fold_case(name)) {
        Some(group) if group.len() == 1 => {
            let only = group
                .iter()
                .next()
                .expect("a one-element group holds its element");
            Ok((
                only.clone(),
                *ids.ids()
                    .get(only)
                    .expect("the id pass keyed every parsed file"),
            ))
        }
        // A present group is never empty — an entry is created only by
        // the insertion that fills it — so no-candidate is exactly the
        // absent-group case.
        None => Err(md::vault_error(format!(
            "{path}: short-form link [[{name}]] has no candidate; use the \
             full form [[Label/<key>]] or create the note"
        ))),
        Some(group) => {
            // The set already carries the id map's key order by
            // construction — the order a fresh scan would list
            // candidates in — so the ambiguous listing is a plain
            // iteration.
            let listed = group
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            Err(md::vault_error(format!(
                "{path}: short-form link [[{name}]] is ambiguous between {listed}; \
                 use the full form"
            )))
        }
    }
}

/// The short-form link candidate index: every loaded key grouped by
/// its case-folded basename, so a `[[name]]` target resolves at one
/// lookup instead of a fold over every key per link. Owned by
/// [`IdAssignment`]: seeded once from the filled id map and maintained
/// by the same method that inserts keys, so the membership always
/// equals the map's — which is what the unique/ambiguous verdict
/// reads.
#[derive(Default)]
struct ShortLinkIndex {
    by_folded_name: BTreeMap<String, std::collections::BTreeSet<String>>,
}

impl ShortLinkIndex {
    /// The grouping policy's one home: one key joins its case-folded
    /// basename's group.
    fn insert(&mut self, key: &str) {
        self.by_folded_name
            .entry(fold_case(file_name(key)))
            .or_default()
            .insert(key.to_string());
    }

    /// The case-folded basename's candidate keys, in the id map's own
    /// key order (a set of the map's keys) — the same membership and
    /// order a scan of the id map would yield. A present group is
    /// never empty: an entry is created only by the insertion that
    /// fills it, so `None` is exactly the no-candidate case.
    fn candidates(&self, folded_name: String) -> Option<&std::collections::BTreeSet<String>> {
        self.by_folded_name.get(&folded_name)
    }
}

/// Load phase 4 (ADR-0406 §4): relationships from outgoing links, with
/// unresolved full-form targets materialized as stub nodes; stable
/// relationship ids restore from the manifest's identity map. Returns
/// the rel side's assignment state — the assigned identity map and
/// the allocator's final next-free value. Stub and relationship rows
/// install in bounded slices (the codec's one `INSTALL_SLICE_ROWS`
/// ceiling: one store lock and one name cache per slice), flushed as
/// the pass resolves — nothing in the pass reads an installed row, and
/// the flush order keeps every stub ahead of the rel rows that
/// reference it. Only the returned identity map holds state
/// proportional to the vault.
pub(super) fn resolve_links_and_stubs(
    graph: &dyn relate_engine_api::GraphImport,
    parsed: &[ParsedFile],
    keys: &BTreeMap<String, String>,
    ids: &mut IdAssignment,
    rel_ids: &BTreeMap<String, i64>,
    next_rel_id: i64,
    value_encoding: u64,
) -> Result<RelAssignment> {
    let primary_of = primary_label_map(parsed);
    let mut pending = PendingSlices::new();
    let mut rels = RelAssignment::new(next_rel_id);

    // Relationships from outgoing links; unresolved targets materialize
    // stub nodes (ADR-0406 §4). Set semantics: the crate already
    // deduplicated exact entries within each list.
    for file in parsed {
        for (rel_type, entries) in &file.doc.links {
            for entry in entries {
                // Resolve the target to the resolved vault key and its
                // id; a fresh full-form target queues its stub row
                // right in the arm that produced it.
                let (target_key, end_id) = match &entry.target {
                    md::LinkTarget::Full { label, key } => {
                        match resolve_full_target(keys, ids, &primary_of, label, key)? {
                            ResolvedTarget::Existing { key, id } => (key, id),
                            ResolvedTarget::Stub { key, node } => {
                                let id = node.id;
                                pending.push_stub(node);
                                (key, id)
                            }
                        }
                    }
                    md::LinkTarget::Short(name) => resolve_short_target(ids, &file.path, name)?,
                };
                // An empty property map decodes to the empty map (the
                // codec's converter iterates nothing) — no special
                // case, same as the checkpoint side.
                let properties = props_from_cypher_json(
                    &entry.properties.clone().into_iter().collect(),
                    value_encoding,
                )
                .map_err(|e| {
                    Error::storage(format!(
                        "{path}: relationship {rel_type}: {e}",
                        path = file.path
                    ))
                })?;
                let identity = rel_identity(&file.key, rel_type, &target_key, &entry.properties);
                // The manifest invariant's rel half rides the one home
                // (`restored_id`), completed by the allocator's own
                // fresh path — the node side's twin contract.
                let id = rels.assign(rel_ids.get(&identity).copied(), next_rel_id, &identity)?;
                pending.push_rel(Relationship {
                    id,
                    start: *ids
                        .ids()
                        .get(&file.key)
                        .expect("the id pass keyed every parsed file"),
                    end: end_id,
                    rel_type: rel_type.clone(),
                    properties: relate_core::PropertyMap::share(properties),
                });
                pending.flush_when_full(graph, &file.path)?;
            }
        }
    }
    // The tail flush names the last parsed file, mirroring the node
    // batches' tail attribution.
    let tail = parsed
        .last()
        .map(|file| file.path.as_str())
        .unwrap_or_default();
    pending.flush(graph, tail)?;
    Ok(rels)
}

/// The link pass's pending install rows (load phase 4's buffering
/// protocol, owned): the stub and relationship buffers, the
/// `INSTALL_SLICE_ROWS` ceiling check, and the flush order — every
/// stub ahead of the rel rows that reference it — in one home, so the
/// resolver's loop reads one link entry → one row and the slice
/// policy is never re-spelled at a call site.
struct PendingSlices {
    stubs: Vec<Node>,
    rels: Vec<Relationship>,
}

impl PendingSlices {
    fn new() -> Self {
        Self {
            stubs: Vec::new(),
            rels: Vec::new(),
        }
    }

    /// A fresh full-form target's stub row.
    fn push_stub(&mut self, node: Node) {
        self.stubs.push(node);
    }

    /// One resolved link's relationship row.
    fn push_rel(&mut self, rel: Relationship) {
        self.rels.push(rel);
    }

    /// Flush when the rel buffer reached the ceiling: every stub was
    /// planned in the same entry iteration as a rel that references
    /// it, so a flush point never installs a rel ahead of its stub.
    /// A failing install is a graph-side validation over the slice:
    /// the flush names the entry whose push filled the buffer — the
    /// slice's tail, the same attribution style the node batches
    /// carry — so the offending vault content stays locatable.
    fn flush_when_full(
        &mut self,
        graph: &dyn relate_engine_api::GraphImport,
        through: &str,
    ) -> Result<()> {
        if self.rels.len() < INSTALL_SLICE_ROWS {
            return Ok(());
        }
        self.flush(graph, through)
    }

    /// Drain both buffers: stubs first, then rels.
    fn flush(&mut self, graph: &dyn relate_engine_api::GraphImport, through: &str) -> Result<()> {
        if !self.stubs.is_empty() {
            graph
                .load_checkpoint_nodes(&self.stubs)
                .map_err(|e| Error::storage(format!("{through}: restoring stub nodes: {e}")))?;
            self.stubs.clear();
        }
        if !self.rels.is_empty() {
            graph.load_checkpoint_rels(&self.rels).map_err(|e| {
                Error::storage(format!("{through}: restoring relationship rows: {e}"))
            })?;
            self.rels.clear();
        }
        Ok(())
    }
}
