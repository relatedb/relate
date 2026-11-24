//! The load path (ADR-0406 §4): parse the walked vault into the graph —
//! schema inference, frontmatter classification, stub materialization,
//! embeddings, and the fold rules.
//!
//! Extracted from `mod.rs` (the no-1k-line rule, LABS-311); the
//! identity-resolution cluster (stable-id restoration, target
//! resolution, the link pass) sits beside it in [`ids`], mirroring
//! how `tests.rs` does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use relate_core::{Error, Node, Result, Value};
use relate_markdown as md;
use serde_json::Value as Json;

use super::super::catalog;
use super::super::convert::props_from_cypher_json;
use super::super::model::INSTALL_SLICE_ROWS;
use super::journal;
use super::manifest::{read_manifest, write_manifest, LearnedSchema, VaultManifest};
use super::ontology;
use super::vault::{vault_stats, walk_vault, VaultFile, WalkedPath};
use super::MarkdownBodyProperties;
use super::VAULT_LABEL;
use crate::codec::SnapshotCodecKind;
use crate::snapshot::{DeferredOpenAction, LoadedSnapshot};
use crate::StoreTarget;

mod ids;

use ids::{resolve_links_and_stubs, restore_ids, IdAssignment, RelAssignment};

/// One parsed node file plus its vault identity. The key's segments
/// are carried structured — `label` and `name` — set once where the
/// key is built, so no consumer re-splits the composite. The root-file
/// fact stays on the walked entry (the derivation site consults it
/// once, where the label is derived); it is not re-carried here.
struct ParsedFile {
    path: String,
    key: String,
    label: String,
    /// The key's name segment (the filename without `.md`).
    name: String,
    doc: md::NodeDoc,
}

/// One file's frontmatter, split and parsed once (ADR-0406 §6): every
/// pass — link evidence, undeclared-map-link rejection, the tolerance
/// verdict, and the node parse — consults this; none re-parses
/// the yaml.
struct Frontmatter<'a> {
    file: &'a VaultFile,
    body: &'a str,
    /// The frontmatter's three real states, modeled once — no pair of
    /// independent fields can misstate them.
    root: FrontmatterRoot,
}

/// The frontmatter's three states (ADR-0406 §6).
enum FrontmatterRoot {
    /// No frontmatter block: the file is body-only.
    Absent,
    /// The block parsed: every pass consults this one value.
    Parsed(Json),
    /// Present but unparseable — real Obsidian vaults carry `@handle`
    /// values, Templater placeholders, malformed mappings. The file
    /// loads body-only and its raw text is preserved verbatim at
    /// checkpoint: the raw text is the user's, never silently
    /// rewritten.
    Raw,
}

impl FrontmatterRoot {
    /// The parsed mapping when the block parsed — the one parse every
    /// pass (link evidence, the node parse) consults.
    fn parsed(&self) -> Option<&Json> {
        match self {
            FrontmatterRoot::Parsed(root) => Some(root),
            FrontmatterRoot::Absent | FrontmatterRoot::Raw => None,
        }
    }
}

fn parse_frontmatter(file: &VaultFile) -> Result<Frontmatter<'_>> {
    let (yaml, body) = split_doc(file)?;
    let root = match yaml {
        None => FrontmatterRoot::Absent,
        // The parse failure is the tolerance verdict itself, not an
        // error to surface: ADR-0406 §6 records the file as raw.
        Some(yaml) => match md::parse_yaml_value(yaml) {
            Ok(root) => FrontmatterRoot::Parsed(root),
            Err(_) => FrontmatterRoot::Raw,
        },
    };
    Ok(Frontmatter { file, body, root })
}

/// Per-key link evidence across the whole vault, computed in one scan
/// of the parsed frontmatter (ADR-0406 §2): which keys some file
/// declares with a bare wikilink — the only shape evidence the
/// vocabulary ever accepts — and which (file, key) sites hold map-form
/// link entries. The vocabulary extension consults the first, the
/// undeclared-map rejection the pair: one traversal, one home for the
/// classification evidence.
struct LinkEvidence {
    /// Keys some file declares with a bare wikilink.
    bare: BTreeSet<String>,
    /// (file, key) sites holding map-form link entries, in walk order.
    map_form_sites: Vec<(String, String)>,
}

/// The one evidence scan (ADR-0406 §2): for every frontmatter key
/// holding a list, whether any entry is a bare wikilink (the
/// declaration) and whether any is a map-form link entry.
fn link_evidence(frontmatter: &[Frontmatter<'_>]) -> LinkEvidence {
    let mut bare = BTreeSet::new();
    let mut map_form_sites = Vec::new();
    for fm in frontmatter {
        // Absent or unparseable frontmatter is no evidence; the load
        // path records the raw case (per-file warning, ADR-0406 §6).
        let FrontmatterRoot::Parsed(Json::Object(map)) = &fm.root else {
            continue;
        };
        for (key, value) in map {
            let Some(items) = value.as_array() else {
                continue;
            };
            // One bare wikilink entry is the declaration (the error
            // text for the ambiguous case says "use a bare link");
            // map-only lists never self-evidence (ADR-0406 §2).
            if items.iter().any(|item| {
                item.as_str()
                    .is_some_and(|raw| md::parse_link(raw).is_some())
            }) {
                bare.insert(key.clone());
            }
            // A map entry whose `target` parses as a wikilink is
            // link-shaped — the undeclared-map rejection's input.
            if items.iter().any(|item| {
                matches!(item, Json::Object(map) if map
                    .get(md::KEY_TARGET)
                    .and_then(Json::as_str)
                    .and_then(md::parse_link)
                    .is_some())
            }) {
                map_form_sites.push((fm.file.walked.path.clone(), key.clone()));
            }
        }
    }
    LinkEvidence {
        bare,
        map_form_sites,
    }
}

impl LinkEvidence {
    /// A map-form entry (or any non-bare shape) under a key the
    /// vocabulary does not know is the one bootstrap ambiguity that
    /// fails closed (ADR-0406 §2: declare it or use a bare link
    /// somewhere) — at the first site, in walk order.
    fn reject_undeclared_map_links(&self, vocabulary: &BTreeSet<String>) -> Result<()> {
        for (path, key) in &self.map_form_sites {
            if vocabulary.contains(key) {
                continue;
            }
            return Err(md::vault_error(format!(
                "{path}: '{key}' holds map-form link entries but no file declares the \
                 relationship type with a bare wikilink; declare it or use a bare link"
            )));
        }
        Ok(())
    }
}

fn split_doc(file: &VaultFile) -> Result<(Option<&str>, &str)> {
    let text = std::str::from_utf8(&file.bytes)
        .map_err(|e| md::vault_error(format!("{} is not UTF-8: {e}", file.walked.path)))?;
    Ok(md::split_frontmatter(text))
}

/// Infer the per-label key property (ADR-0406 §2) — through the one
/// inference both sides of the codec consult (`super::infer_key_for_load`):
/// a load-side entry carries its own file name, so a candidate's value
/// must sanitize to that very name (the file is the identity); the
/// fallback key name is `name` with the value taken from the path.
fn infer_keys(parsed: &[ParsedFile]) -> BTreeMap<String, String> {
    let mut by_label: BTreeMap<&str, Vec<&ParsedFile>> = BTreeMap::new();
    for file in parsed {
        by_label.entry(&file.label).or_default().push(file);
    }
    let mut keys = BTreeMap::new();
    for (label, files) in by_label {
        let properties: Vec<&BTreeMap<String, Json>> =
            files.iter().map(|file| &file.doc.properties).collect();
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        keys.insert(
            label.to_string(),
            super::infer_key_for_load(&properties, &names),
        );
    }
    keys
}

/// The one identity derivation (ADR-0406 §2, ADR-0647): split the
/// disk-derived stem into the (label, name) pair — a root file keys
/// under the implicit label, its disk path stays the bare
/// root-relative path — compose both segments to the canonical NFC
/// spelling (a filename a normalizing copy tool left decomposed still
/// resolves; the key property, vault key and checkpoint plan all carry
/// the one canonical form, so the next checkpoint renames the file to
/// it), refuse the reserved-label collision on the codec's one
/// identity relation, and join the composite vault key, never
/// re-split.
fn vault_identity(walked: &WalkedPath) -> Result<(String, String, String)> {
    let (label, name) = if walked.root_file() {
        (VAULT_LABEL.to_string(), md::compose(walked.stem()))
    } else {
        let (label, name) = walked.stem().split_once('/').ok_or_else(|| {
            md::vault_error(format!(
                "node file outside a label directory: {}",
                walked.path
            ))
        })?;
        (md::compose(label), md::compose(name))
    };
    // The reserved-label collision at the one site where
    // `(label, root_file())` is derived, asked of the codec's one
    // root-label home ([`super::is_root_label`] — the same fold every
    // other consumer asks): a label directory whose name folds to the
    // implicit root label (`vault/`, `VAULT/`) collides
    // case-insensitively with the root files' `:Vault` namespace and
    // fails closed here.
    if !walked.root_file() && super::is_root_label(&label) {
        return Err(md::vault_error(format!(
            "{}: label directory '{label}' collides with the reserved implicit root \
             label '{}'; rename the label or the file location",
            walked.path, VAULT_LABEL
        )));
    }
    let key = super::vault_key(&label, &name);
    Ok((label, name, key))
}

/// Load phase 1's output — the canonical vault: the fold-canonicalized
/// files plus the two schema facts its later phases consult, the
/// per-label key property map and the canonicalization pass's
/// fold→canonical-label table (the walked-label fact's one home, which
/// the link pass's primary-label resolution consults instead of
/// re-deriving it).
struct CanonicalVault {
    files: Vec<ParsedFile>,
    keys: BTreeMap<String, String>,
    label_folds: BTreeMap<String, String>,
}

/// Load phase 1 (ADR-0406 §2/§6): parse every file once — the
/// frontmatter's tri-state deciding the node parse vs the raw-file
/// tolerance — infer the per-label key properties, and canonicalize
/// case-folded identities (the loser of a case-only duplicate drops,
/// its fold recorded as the load's warning). `bodies` is the resolved
/// body-property configuration (ADR-0460): each file's label picks its
/// ordered list. The vault-committed ontology (ADR-0649), when the
/// vault carries one, seeds its relationship vocabulary into the
/// classification before the parse runs and binds its key declarations
/// against the fold-canonical walked labels after it — the whole
/// committed feature lives inside this phase, never as branches
/// flanking it in the orchestrator. Returns the canonical vault
/// ([`CanonicalVault`]); `learned` accumulates the raw-file and fold
/// records.
fn parse_and_canonicalize_files(
    frontmatter: &[Frontmatter<'_>],
    learned: &mut LearnedSchema,
    bodies: &MarkdownBodyProperties,
    committed: Option<&ontology::VaultOntology>,
) -> Result<CanonicalVault> {
    // The classification vocabulary: the learned schema's set extended
    // by this open's bare-link evidence (drift regenerates the schema)
    // — and seeded first by the committed ontology's declared
    // relationship types (map-form entries under a declared type are
    // declared, ADR-0649 §2), so the file's declarations feed the same
    // fact every other source feeds.
    if let Some(ontology) = committed {
        ontology.seed_vocabulary(learned);
    }
    let evidence = link_evidence(frontmatter);
    let mut vocabulary = learned.rel_types.clone();
    vocabulary.extend(evidence.bare.iter().cloned());
    evidence.reject_undeclared_map_links(&vocabulary)?;

    // `frontmatter` alone carries the rows — each `Frontmatter` holds
    // its own `file` reference (one carrier, no positional pairing to
    // keep in step).
    let mut parsed = Vec::with_capacity(frontmatter.len());
    for fm in frontmatter {
        let file = fm.file;
        let (label, name, vault_key) = vault_identity(&file.walked)?;
        // The frontmatter's tri-state, matched once (ADR-0406 §6):
        // parsed or absent feeds the node parse; present-but-unparseable
        // is the raw-file tolerance — a body-only load and the file
        // preserved verbatim at checkpoint, never a silent rewrite.
        // The label's body list resolves once, outside the tri-state —
        // both arms consult the same selection.
        let body_list = bodies.resolve(&label);
        let doc = match &fm.root {
            FrontmatterRoot::Parsed(_) | FrontmatterRoot::Absent => {
                // The already-parsed root feeds the node parse — the
                // classification pass's one parse, never a second.
                md::parse_node_doc_root(
                    &label,
                    &vault_key,
                    fm.root.parsed().cloned(),
                    fm.body,
                    &vocabulary,
                    body_list,
                )
                .map_err(|e| Error::storage(format!("{path}: {e}", path = file.walked.path)))?
            }
            FrontmatterRoot::Raw => {
                learned.raw_files.insert(file.walked.path.clone());
                // The tolerance's one construction (the checkpoint's
                // unmodified check derives its expectation from it).
                super::raw_node_doc(&label, fm.body, body_list)?
            }
        };
        parsed.push(ParsedFile {
            path: file.walked.path.clone(),
            key: vault_key,
            label,
            name,
            doc,
        });
    }

    // The per-label key property: learned schema first, inference for
    // labels it does not know (drift), default `name` from the path.
    let mut keys = learned.keys.clone();
    for (label, inferred) in infer_keys(&parsed) {
        keys.entry(label).or_insert(inferred);
    }

    let (parsed, label_folds) = canonicalize_by_fold(parsed, learned);

    // The committed key declarations bind after the parse, against the
    // fold-canonical walked labels, through the vault's one identity
    // relation (ADR-0649 §2): a declaration whose label differs only
    // in case or normalization from a label directory binds to the
    // walked canonical spelling, overriding both the sidecar's stale
    // record and the inference; a forward declaration — a label the
    // vault does not carry yet — stays inert. The declarations are
    // fold-keyed and so is the canonicalization table: one direct
    // lookup per declaration, no walked-set re-derivation.
    if let Some(ontology) = committed {
        ontology.bind_keys(&label_folds, &mut keys, learned);
    }
    Ok(CanonicalVault {
        files: parsed,
        keys,
        label_folds,
    })
}

/// The canonicalization pass (ADR-0406 §2), one home: the canonical
/// label and the canonical key are both the first spelling in path
/// order, and a case-only duplicate row is dropped right here — its
/// fold recorded as the load's warning. Later passes never re-split or
/// re-derive the identity: the structured (label, name) fields are the
/// composite key's one spelling, and the fold→canonical-label table
/// built along the way is returned with the files — the walked-label
/// fact's one home, consulted by the link pass's primary-label
/// resolution instead of being re-derived there.
fn canonicalize_by_fold(
    parsed: Vec<ParsedFile>,
    learned: &mut LearnedSchema,
) -> (Vec<ParsedFile>, BTreeMap<String, String>) {
    // First occurrence in path order decides everything: the canonical
    // label per fold (the table this pass returns — its payload is
    // read downstream), and the surviving key — a key fold already
    // seen drops its file right here, the fold recorded as the load's
    // warning. No per-fold canonical-key payload: nothing downstream
    // reads one, first occurrence is the whole decision.
    let mut label_by_fold: BTreeMap<String, String> = BTreeMap::new();
    let mut seen_key_folds: BTreeSet<(String, String)> = BTreeSet::new();
    let mut canonical: Vec<ParsedFile> = Vec::with_capacity(parsed.len());
    for mut file in parsed {
        let label_fold = md::fold_case(&file.label);
        let canonical_label = label_by_fold
            .entry(label_fold.clone())
            .or_insert_with(|| file.label.clone());
        if !seen_key_folds.insert((label_fold, md::fold_case(&file.key))) {
            learned.folded_keys.insert(file.path.clone());
            continue;
        }
        file.label = canonical_label.clone();
        file.key = super::vault_key(&file.label, &file.name);
        canonical.push(file);
    }
    (canonical, label_by_fold)
}

/// One parsed file's node row (load phase 3's row policy, named): the
/// key property mirroring the filename, the property decode, and the
/// label merge/sort/dedup — everything a row is, separate from the
/// slice/install orchestration that carries rows in bounded batches.
fn node_row(
    file: &ParsedFile,
    keys: &BTreeMap<String, String>,
    ids_by_key: &BTreeMap<String, i64>,
    value_encoding: u64,
) -> Result<Node> {
    let key_name = keys
        .get(&file.label)
        .expect("the inference keyed every parsed label");
    let name_path = file.name.clone();
    let mut properties = file.doc.properties.clone();
    // The key property mirrors its filename: the file is the
    // identity (OKF Concept ID). A present value must be a string
    // that sanitizes to its own filename (case-insensitively); a
    // missing one takes the filename. Either way the inserted
    // value is the filename's canonical spelling — one insert
    // below, spelled once.
    match properties.get(key_name) {
        None => {}
        Some(Json::String(value)) => {
            let sanitized = md::sanitize_name_path(value)
                .map_err(|e| Error::storage(format!("{path}: {e}", path = file.path)))?;
            // The same case-insensitivity the identity model uses
            // everywhere (Unicode-aware `fold_case`, never ASCII).
            if md::fold_case(&sanitized) != md::fold_case(&name_path) {
                return Err(md::vault_error(format!(
                    "{}: key property '{key_name}' value '{value}' does not match \
                 its filename '{name_path}'",
                    file.path
                )));
            }
        }
        Some(other) => {
            return Err(md::vault_error(format!(
                "{}: key property '{key_name}' must be a string, found {other}",
                file.path
            )))
        }
    }
    properties.insert(key_name.clone(), Json::String(name_path));
    let decoded = props_from_cypher_json(&properties, value_encoding)
        .map_err(|e| Error::storage(format!("{path}: {e}", path = file.path)))?;
    let mut labels = vec![file.label.clone()];
    labels.extend(file.doc.extra_labels.iter().cloned());
    labels.sort();
    labels.dedup();
    Ok(Node {
        id: *ids_by_key
            .get(&file.key)
            .expect("the id pass keyed every parsed file"),
        labels: Arc::new(labels),
        properties: relate_core::PropertyMap::share(decoded),
    })
}

/// Load phase 3: the nodes themselves — the key property mirroring the
/// filename, properties decoded through the store's value encoding.
/// Rows install in bounded slices through the graph's batch entry (the
/// codec's one `INSTALL_SLICE_ROWS` ceiling: one store lock, one catalog
/// update and one name cache per slice, the shape the slice API
/// documents for concurrent load workers) instead of one per-row call.
/// Returns how many nodes loaded (the commit-timestamp floor's input).
fn load_nodes(
    graph: &dyn relate_engine_api::GraphImport,
    parsed: &[ParsedFile],
    keys: &BTreeMap<String, String>,
    ids_by_key: &BTreeMap<String, i64>,
    value_encoding: u64,
) -> Result<usize> {
    for files in parsed.chunks(INSTALL_SLICE_ROWS) {
        // The slice's rows are built before any install: a file that
        // fails validation or decode fails the slice before a single
        // row of it lands, exactly as the per-row install did.
        let mut batch = Vec::with_capacity(files.len());
        for file in files {
            batch.push(node_row(file, keys, ids_by_key, value_encoding)?);
        }
        // An install failure is a graph-side validation over the slice
        // (row ids, not any one file's content), so it names the
        // slice's path range — never a single member file.
        let first = files
            .first()
            .expect("chunks over parsed files are never empty")
            .path
            .as_str();
        let last = files
            .last()
            .expect("chunks over parsed files are never empty")
            .path
            .as_str();
        graph
            .load_checkpoint_nodes(&batch)
            .map_err(|e| Error::storage(format!("restoring the nodes of {first}..{last}: {e}")))?;
    }
    Ok(parsed.len())
}

/// Load phase 5 (ADR-0406 §3): embeddings back onto their nodes —
/// derived state, keyed `"<vault path>#<property>"` in the sidecar (a
/// bootstrap sidecar's map is empty, so a sidecar-less store has
/// nothing to restore).
fn restore_embeddings(
    graph: &dyn relate_engine_api::GraphImport,
    manifest: &VaultManifest,
    ids_by_key: &BTreeMap<String, i64>,
    value_encoding: u64,
) -> Result<()> {
    // One pass per embedding key, not grouped per node: the sidecar's
    // map is the engine-authored index (`<vault path>#<property>`), so
    // a node with several indexed properties is restored by several
    // cheap lookups rather than an inversion of the whole map. This
    // pass deliberately stays per-row where the rest of the load
    // batches: a node can carry several embedding keys, and each
    // restore reads the node's current properties and re-installs them
    // with the embedding added — batching would read stale properties
    // for every key after the first. The map is bounded by the
    // vector-indexed properties the store declares, not by the vault.
    for (embedding_key, value) in &manifest.embeddings {
        let Some((vault_key, property)) = embedding_key.split_once('#') else {
            return Err(md::vault_error(format!(
                "malformed embedding key '{embedding_key}'"
            )));
        };
        let Some(&id) = ids_by_key.get(vault_key) else {
            // The one tolerated skip: an embedding whose node file
            // is gone — externally deleted while the engine was
            // closed (ADR-0406 §5) — has nothing to restore onto,
            // so the entry drops with its node. Every other
            // malformed embedding shape (the key split, the value's
            // decode below) fails closed like the rest of the load.
            continue;
        };
        // The embeddings map is engine-authored sidecar data: a value
        // that fails to decode is corruption, not tolerance — every
        // other decode failure on this load path already fails closed.
        let decoded = props_from_cypher_json([(&property.to_string(), value)], value_encoding)
            .map_err(|e| md::vault_error(format!("restoring embedding '{embedding_key}': {e}")))?;
        // `ids_by_key` only ever holds ids this load placed in the
        // graph — a miss here is a broken invariant, not a tolerance
        // case (the one tolerated skip, the externally deleted node,
        // exited at the lookup above).
        let node = graph
            .node_at(id, u64::MAX)
            .expect("every embedding key maps to a loaded node");
        let mut props: BTreeMap<String, Value> = node
            .properties
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in decoded {
            props.insert(k, v);
        }
        graph
            .load_checkpoint_node(Node {
                id,
                labels: node.labels.clone(),
                properties: relate_core::PropertyMap::share(props),
            })
            .map_err(|e| md::vault_error(format!("restoring embedding: {e}")))?;
    }
    Ok(())
}

/// The load's resolved schema-and-configuration half, recorded into the
/// sidecar expectation together (ADR-0406 §2, ADR-0460): the regenerated
/// learned schema and the body-property configuration in the form the
/// sidecar records — the **pre-materialization** form, so an unchanged
/// vault's `bodies` compares equal (a pre-0460 sidecar stays untouched)
/// and unconfigured stores keep omitting the field. The materialized
/// view the phases parsed with stays a local of the open walk.
struct LoadSchema {
    learned: LearnedSchema,
    recorded: MarkdownBodyProperties,
}

/// Load phase 6 (ADR-0406 §2/§5): record this load's expectation —
/// the shared fields applied once over the sidecar this load read (or
/// the bootstrap skeleton when the store has none), then the
/// publication decision below.
fn record_expectation(
    target: &StoreTarget,
    manifest: &VaultManifest,
    had_sidecar: bool,
    files: &[VaultFile],
    ids: IdAssignment,
    rels: RelAssignment,
    schema: LoadSchema,
) -> Result<Option<DeferredOpenAction>> {
    // Record this load's expectation (ADR-0406 §2/§5) — one step, the
    // shared fields applied once: the base is the sidecar this load
    // read (or the bootstrap skeleton the caller resolved when the
    // store has none), updated with what the load now knows — the
    // walked vault's stats, the ids it assigned or restored, the
    // learned schema, and the next free ids.
    // A bootstrap publishes immediately: hidden engine state under
    // `.relate/`, so the explicit schema — not a re-derivation —
    // governs every later load and checkpoint; the vault files stay
    // untouched. A drifted manifest is ADR-0406 §5's edit-while-closed
    // re-baseline, and publishing it is DEFERRED — the writable open's
    // dirty-WAL guard must first prove no WAL deltas are pending (a
    // read-only open never publishes at all); publishing here would
    // let pending deltas replay over an externally-edited base, the
    // exact corruption the guard refuses.
    let stats = vault_stats(files);
    let mut expectation = manifest.clone();
    expectation.files = stats;
    // The allocator's final next-free id, read before the id map's
    // by-value handoff consumes the assignment below.
    let next_fresh_id = ids.next_fresh_id();
    let next_fresh_rel_id = rels.next_rel_id();
    expectation.node_ids = ids.into_ids_by_key();
    expectation.rel_ids = rels.into_ids();
    expectation.learned = schema.learned;
    // The body-property configuration the sidecar records (ADR-0460) is
    // the pre-materialization form: an explicit open-time config lands
    // verbatim — the vault is self-describing from this load on — and a
    // sidecar-carried one is carried forward unchanged, byte-equal, so
    // the gate below never republishes an untouched vault.
    expectation.bodies = schema.recorded;
    expectation.next_node_id = next_fresh_id;
    expectation.next_rel_id = next_fresh_rel_id;
    Ok(match had_sidecar {
        false => {
            write_manifest(&*target.vfs(), &expectation)?;
            None
        }
        // An unchanged vault keeps the sidecar it has: the expectation
        // is this manifest plus exactly the overwrites above, so one
        // whole-value comparison is the drift decision — every
        // overwritten field participates by construction (a future
        // eighth overwrite joins the gate for free), and every field
        // left untouched compares equal because it was cloned. The
        // bodies stay in the raw recording form, so a pre-0460 sidecar
        // compares equal and is never rewritten for that field alone.
        true if &expectation == manifest => None,
        // The re-baseline's publication is deferred: `finish_open`'s
        // dirty-WAL guard must first prove no WAL deltas are pending.
        true => Some(DeferredOpenAction::new(move |target| {
            write_manifest(&*target.vfs(), &expectation)
        })),
    })
}

pub(crate) fn load_with_checkpoint_lsn<G: relate_engine_api::SnapshotGraph>(
    target: &StoreTarget,
) -> Result<LoadedSnapshot<G>> {
    journal::recover(target)?;
    let sidecar = read_manifest(target)?;
    // The sidecar is resolved exactly once, here: every phase below
    // consults the same reference — the bootstrap skeleton when the
    // store has none — so "does this open have a sidecar" is never
    // re-asked as a nullable downstream. Only the publication decision
    // (phase 6) still needs the presence fact, passed as one bool.
    let bootstrap = VaultManifest {
        codec: Some(SnapshotCodecKind::Markdown),
        value_encoding: Some(crate::version::value_encoding::version_for_write(false)),
        ..VaultManifest::default()
    };
    let manifest = sidecar.as_ref().unwrap_or(&bootstrap);
    // The same verdict the JSON codec gives its read-only marker: a
    // sidecar claiming read-only is not a loadable snapshot.
    if manifest.read_only {
        return Err(md::vault_error("read-only snapshot not supported"));
    }
    let files = walk_vault(&*target.vfs())?;
    // One frontmatter parse per file, shared by every pass below.
    let frontmatter: Vec<Frontmatter<'_>> = files
        .iter()
        .map(parse_frontmatter)
        .collect::<Result<Vec<_>>>()?;

    // The vault-committed ontology (ADR-0649): a reserved non-`.md`
    // document at the vault root whose declarations — the relationship
    // vocabulary, the per-label key properties — travel with the vault
    // through version control. Read before the learned schema forms,
    // so its declarations seed the same fact the sidecar and the bare
    // link evidence feed; absent or unexpressible documents refused
    // above never reach the load.
    let vault_ontology = ontology::read_vault_ontology(&*target.vfs())?;

    let has_ontology = manifest.ontology.is_some();
    // A store without a sidecar was never written by an older engine, so
    // its absent marker classifies as the current encoding, not legacy 1
    // (the bootstrap skeleton carries it); a present sidecar with no
    // marker gets the same current-encoding default.
    let marker_value =
        manifest
            .value_encoding
            .or(Some(crate::version::value_encoding::version_for_write(
                has_ontology,
            )));
    let (value_encoding, marker) =
        crate::version::value_encoding::gate(marker_value, has_ontology)?;

    // The learned schema regenerates with this open's evidence; the
    // phases below read and extend it. The vault-committed ontology's
    // declarations seed and bind inside the parse phase (ADR-0649),
    // against the same facts every other source feeds. The
    // body-property configuration resolves exactly once, beside it,
    // through the one resolution both sides of the codec consult
    // (ADR-0460).
    let mut learned = manifest.learned.clone();
    let bodies = super::resolve_bodies(target, &manifest.bodies)?;
    let recorded = super::recorded_bodies(target, &manifest.bodies);
    let canonical =
        parse_and_canonicalize_files(&frontmatter, &mut learned, &bodies, vault_ontology.as_ref())?;

    let mut ids = restore_ids(&canonical.files, manifest)?;
    let graph = G::default();
    let loaded = load_nodes(
        &graph,
        &canonical.files,
        &canonical.keys,
        ids.ids(),
        value_encoding,
    )?;
    let rel_ids = manifest.rel_ids.clone();
    let rels = resolve_links_and_stubs(
        &graph,
        &canonical,
        &mut ids,
        &rel_ids,
        manifest.next_rel_id,
        value_encoding,
    )?;
    // The allocator's final values: the node side threaded through
    // `restore_ids` and the stub materializer's assignments, the rel
    // side through the link plan above.
    graph.set_next_node_id(ids.next_fresh_id());
    graph.set_next_rel_id(rels.next_rel_id());

    restore_embeddings(&graph, manifest, ids.ids(), value_encoding)?;

    // The load-tail commit-timestamp policy has the one home the
    // adjacent call owns: the recorded value is set unconditionally (0
    // leaves the fresh graph's 0, which the floor below then raises
    // exactly like the classic codecs), never re-spelled here.
    graph.set_commit_ts(manifest.commit_ts);
    // The load-tail commit-timestamp floor, shared with the JSON codec
    // (the one home of the policy, `snapshot::ensure_commit_ts`).
    crate::snapshot::ensure_commit_ts(&graph, loaded);

    catalog::restore(&graph, manifest.catalog.clone(), manifest.theater.clone())?;

    let deferred = record_expectation(
        target,
        manifest,
        sidecar.is_some(),
        &files,
        ids,
        rels,
        LoadSchema { learned, recorded },
    )?;
    Ok(LoadedSnapshot {
        graph,
        checkpoint_lsn: manifest.checkpoint_lsn,
        // The CHECKPOINTED document, not the vault file's: the schema
        // text mirror's open-time verification compares the
        // checkpointed state (ADR-0481 §4), so the committed
        // document rides the `committed` carrier and the facade's
        // open tail swaps it in after the verification — never
        // through this field, where a file edit would read as drift.
        ontology: manifest.ontology.clone(),
        marker,
        deferred,
        committed: vault_ontology.map(|ontology| ontology.into_committed()),
    })
}

#[cfg(test)]
mod tests;
