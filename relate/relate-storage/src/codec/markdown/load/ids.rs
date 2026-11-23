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
/// and the two link-resolution indexes (the short-form candidate index,
/// the full-form fold index). The tables cannot desync by construction:
/// every key insertion goes through [`IdAssignment::register`], which
/// maintains all of them, the id map is read through
/// [`IdAssignment::ids`], and the allocator head is read through
/// [`IdAssignment::next_fresh_id`] — [`IdAssignment::assign_fresh`] is
/// the one writer of both halves.
pub(super) struct IdAssignment {
    ids_by_key: BTreeMap<String, i64>,
    next_fresh_id: i64,
    short_links: ShortLinkIndex,
    /// fold(vault key) → the first-registered key of that fold — the
    /// full-form link pass resolves a case- or normalization-variant
    /// key spelling through it (the vault's one identity relation,
    /// ADR-0406 §2 + ADR-0647) instead of an exact match, so a variant
    /// link resolves to the existing node and never stubs a duplicate.
    by_fold: BTreeMap<String, String>,
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

    /// The one key insertion: a key enters the id map and both link
    /// indexes together, and each index's membership is derived from
    /// the map insert itself — only a first insertion indexes the key,
    /// so a re-registration can never leave a stale or duplicate group
    /// entry and the tables cannot desync.
    fn register(&mut self, key: &str, id: i64) {
        if self.ids_by_key.insert(key.to_string(), id).is_none() {
            self.short_links.insert(key);
            self.by_fold
                .entry(fold_case(key))
                .or_insert_with(|| key.to_string());
        }
    }

    /// The fold-keyed registration query the full-form arms ask — the
    /// short-form side's [`ShortLinkIndex::candidates`] contract over
    /// whole keys: the canonical key and its id when a registered key
    /// folds to this one, `None` when nothing is registered under the
    /// fold (a genuinely novel name). The hit's id is the id map's by
    /// construction — [`Self::register`] fills the fold index and the
    /// id map together — so the caller asks one question and never a
    /// follow-up lookup.
    fn registered_under_fold(&self, key: &str) -> Option<(String, i64)> {
        let canonical = self.by_fold.get(&fold_case(key))?;
        let id = *self
            .ids_by_key
            .get(canonical)
            .expect("register fills the id map and the fold index together");
        Some((canonical.clone(), id))
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
    /// The manifest's recorded next free rel id, frozen at
    /// construction — the bound every restored rel id is judged
    /// against ([`restored_id`]'s invariant), an implementation detail
    /// of the carrier, never a per-call parameter.
    seeded_rel_id: i64,
}

impl RelAssignment {
    /// The allocator, seeded with the manifest's recorded next free
    /// rel id — the derivation's one home (the `max(1)` clamp
    /// `restore_ids` applies for the node side, mirrored here), and the
    /// frozen bound every restored rel id is judged against, captured
    /// before any fresh id advances the live head.
    fn new(recorded_next_rel_id: i64) -> Self {
        let next_rel_id = recorded_next_rel_id.max(1);
        Self {
            ids: BTreeMap::new(),
            next_rel_id,
            seeded_rel_id: next_rel_id,
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

    /// One relationship's id: the id this pass already assigned the
    /// same identity — set semantics, ADR-0406 §2: two link entries
    /// resolving to one `(start, type, end, properties)` are one edge,
    /// and the shared id makes the install's later-row-wins rule
    /// collapse them — else the manifest's restored one when it holds
    /// and passes the invariant, else the next fresh id — the node
    /// side's [`IdAssignment::assign_fresh`] contract over the rel
    /// half.
    fn assign(&mut self, restored: Option<i64>, identity: &str) -> Result<i64> {
        if let Some(&assigned) = self.ids.get(identity) {
            return Ok(assigned);
        }
        let id = match restored_id(restored, self.seeded_rel_id, "relationship", identity)? {
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
        by_fold: BTreeMap::new(),
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

/// The walked-label index (ADR-0406 §2): the two distinct facts the
/// full-form target resolution consults — which label folds some node
/// carries as its **primary** label (with the canonical spelling: the
/// `Label/<key>` namespace that owns node identity), and which folds
/// are carried as **extra** labels (with the substituting primary of
/// the first file in walk order, the secondary-label mapping §2
/// defines). The two are never one map: a label that real nodes carry
/// as primary while other, earlier-sorted files carry it as extra (the
/// converted vault's `Source`, claimed by `Draft`-primary files) must
/// resolve at its own namespace, never be hijacked by the substitution
/// (REL8-2039 — every `[[Source/…]]` link in the vault silently
/// resolved into the `Draft/` namespace and the checkpoint rewrote the
/// files to match, making container rewires lossy).
///
/// `primary` is the canonicalization pass's own fold→canonical-label
/// table, handed over by [`super::parse_and_canonicalize_files`] — the
/// walked-label fact's one home, never re-derived by re-folding the
/// files here; this index adds only `secondary`, the extra-label
/// claims the canonicalization pass does not see.
struct WalkedLabelIndex<'a> {
    /// fold → the canonical spelling of a label some node carries as
    /// primary (the identity-owning namespace) — the canonicalization
    /// pass's own table, borrowed, never re-derived here.
    primary: &'a BTreeMap<String, String>,
    /// fold → the substituting primary of the first file carrying the
    /// label as an extra label (§2's secondary-label mapping).
    secondary: BTreeMap<String, String>,
}

impl<'a> WalkedLabelIndex<'a> {
    /// One pass over the canonical vault's extra labels — the primary
    /// table is the bundle's own canonicalization table, borrowed
    /// as-is; this construction claims only the secondary folds. The
    /// resolution's fixed order — primary namespace first, substitution
    /// only for a fold no node claims as primary — is what keeps the
    /// two tables from becoming one.
    fn of(canonical: &'a super::CanonicalVault) -> Self {
        let mut index = WalkedLabelIndex {
            primary: &canonical.label_folds,
            secondary: BTreeMap::new(),
        };
        for file in &canonical.files {
            for label in &file.doc.extra_labels {
                index
                    .secondary
                    .entry(fold_case(label))
                    .or_insert_with(|| file.label.clone());
            }
        }
        index
    }
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

/// One namespace resolution step — the mechanics every arm of
/// [`resolve_full_target`] shares: the target key under `namespace`,
/// resolved through the vault's one identity relation (the fold), as
/// the registered canonical key and its id when the pass already holds
/// that key (a walked file or an earlier link's stub) — `None` when
/// the name is novel. The resolution stays a pair: matched-vs-novel is
/// the caller's decision (the miss arms stub, the substitution arm
/// falls through), never a second discrimination inside the helper.
fn resolve_at(ids: &IdAssignment, namespace: &str, key: &str) -> Option<(String, i64)> {
    ids.registered_under_fold(&vault_key(namespace, key))
}

/// The resolve-or-stub step the two miss arms share: the target key
/// under `namespace` when the pass already holds it, else the §4 stub
/// materialized under that same label — the one home of the decision,
/// so neither miss arm can stub a name [`resolve_at`] can find.
fn resolve_or_stub(
    keys: &BTreeMap<String, String>,
    ids: &mut IdAssignment,
    namespace: &str,
    key: &str,
) -> ResolvedTarget {
    resolve_at(ids, namespace, key).map_or_else(
        || stub_row(keys, ids, namespace, key),
        |(key, id)| ResolvedTarget::Existing { key, id },
    )
}

/// Resolve a full-form link target (ADR-0406 §2/§4), three policies
/// over the shared steps ([`resolve_at`], [`resolve_or_stub`]): the
/// named label's **own namespace** first — `[[Label/<key>]]` names the
/// `Label/` directory, which owns node identity, so when a walked node
/// carries the label as primary (fold-matched, canonical spelling) the
/// link resolves there and never leaves it; then, only for a label no
/// node carries as primary, §2's secondary substitution to the primary
/// of the first file carrying it as extra; the miss arms —
/// own-namespace and named-label alike — materialize a stub under the
/// **named** label (§4: the stub's label comes from the wikilink),
/// never under a substituted primary, and only after [`resolve_at`]
/// proved the name novel: a repeated link to the same
/// not-yet-existing target resolves to the first stub instead of
/// stubbing a second node under the same key. The ladder always
/// answers, so the return is the target itself, no error channel. The
/// stub row is returned to the caller's batch, not installed inline:
/// nothing in this pass reads installed rows, so the slices install
/// whole.
fn resolve_full_target(
    keys: &BTreeMap<String, String>,
    ids: &mut IdAssignment,
    labels: &WalkedLabelIndex,
    label: &str,
    key: &str,
) -> ResolvedTarget {
    // The link label's one identity fold — both table lookups consult
    // it, computed once.
    let folded = fold_case(label);
    // The named label's own namespace: a walked primary label owns its
    // directory's identity, and an extra-label claim on an
    // earlier-sorted file must not redirect it.
    if let Some(named) = labels.primary.get(&folded) {
        return resolve_or_stub(keys, ids, named, key);
    }
    // A label no node carries as primary is a secondary label: §2's
    // substitution resolves it at the primary path of the first file
    // carrying it. A miss falls through — the stub belongs to the named
    // label, not the substituted one.
    if let Some((target_key, id)) = labels
        .secondary
        .get(&folded)
        .and_then(|primary| resolve_at(ids, primary, key))
    {
        return ResolvedTarget::Existing {
            key: target_key,
            id,
        };
    }
    // The named label, raw: the fold resolves case- and
    // normalization-variant spellings, and a hit returns the
    // registered canonical key — composition belongs to the stub
    // alone, where the spelling is written. A hit is the pass's own
    // earlier stub for the same wikilink; the miss is the novel name
    // §4 materializes.
    resolve_or_stub(keys, ids, label, key)
}

/// One stub row (the shared miss arm): a fresh id, the label's key
/// property mirroring the wikilink's key segment — the one construction
/// both miss paths (own-namespace and named-label) share. The
/// constructor owns both segments' canonical spelling (ADR-0647):
/// compose is idempotent over the already-canonical spellings the
/// primary arm passes, and heals a decomposed wikilink's spelling —
/// the stub's file lands at the canonical name, the same checkpoint
/// rename a decomposed filename takes.
///
/// The stub label's key property: the learned schema's record for that
/// exact spelling, else the default — the default is the
/// schema-unknown label's answer, never a silent fallback over a
/// record the vault walks. The own-namespace arm passes the walked
/// canonical spelling, which always finds the record; the named-label
/// arm's label is by construction not a walked primary (the
/// own-namespace arm owns every walked-primary fold), so no walked
/// record exists for it to skip.
fn stub_row(
    keys: &BTreeMap<String, String>,
    ids: &mut IdAssignment,
    label: &str,
    key: &str,
) -> ResolvedTarget {
    let label = md::compose(label);
    let key = md::compose(key);
    let target_key = vault_key(&label, &key);
    let key_name = keys
        .get(&label)
        .cloned()
        .unwrap_or_else(|| DEFAULT_KEY_PROPERTY.to_string());
    let id = ids.assign_fresh(&target_key);
    let mut stub_props = BTreeMap::new();
    stub_props.insert(key_name, Value::String(key.into()));
    ResolvedTarget::Stub {
        key: target_key,
        node: Node {
            id,
            labels: Arc::new(vec![label]),
            properties: relate_core::PropertyMap::share(stub_props),
        },
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
    canonical: &super::CanonicalVault,
    ids: &mut IdAssignment,
    rel_ids: &BTreeMap<String, i64>,
    next_rel_id: i64,
    value_encoding: u64,
) -> Result<RelAssignment> {
    let parsed = &canonical.files;
    let keys = &canonical.keys;
    let labels = WalkedLabelIndex::of(canonical);
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
                        match resolve_full_target(keys, ids, &labels, label, key) {
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
                let id = rels.assign(rel_ids.get(&identity).copied(), &identity)?;
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
