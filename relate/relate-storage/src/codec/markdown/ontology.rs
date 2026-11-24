//! The vault-committed ontology file (ADR-0649): a reserved, non-`.md`
//! document at the vault root — `ontology.yaml` or `ontology.json` —
//! that is the vault's committed schema. It is user-owned content the
//! engine reads and never writes: the walk takes `.md` files only, so
//! it is invisible to the node load, the view regeneration and the
//! checkpoint's deletion phase alike. Its declarations travel with
//! the vault through version control the way the per-machine sidecar
//! cannot.
//!
//! Two halves, by where they may land: the **learned-schema seed**
//! (the relationship-type vocabulary, the per-label key properties)
//! applies inside the load — learned facts are not in the schema text
//! mirror, so the seed cannot read as drift. The **committed
//! schema** (the document, its graph type, its extension-envelope
//! indexes) rides the [`CommittedSchema`] carrier to the facade's
//! open tail and is applied there through the replay path — the
//! mirror's open-time verification compares the checkpointed state
//! (ADR-0481 §4), so a committed change must land after it.

use std::collections::{BTreeMap, BTreeSet};

use relate_core::{Error, Result, Value as CypherValue};
use relate_engine_api::redo::IndexKindSpec;
use relate_markdown as md;

use super::manifest::LearnedSchema;
use crate::snapshot::{CommittedIndex, CommittedSchema};
use crate::vfs::Vfs;

/// The vault-committed ontology file's one naming policy: every refusal
/// of the file wraps its reason with the file's name through this
/// helper — never a re-spelled inline closure or bespoke `format!` at a
/// refusal site, so the wrapping text has one home whatever the
/// underlying failure class.
fn file_error(file: &str, e: impl std::fmt::Display) -> Error {
    md::vault_error(format!("ontology file {file}: {e}"))
}

/// The ontology file's YAML spelling, at the vault root.
pub(super) const VAULT_ONTOLOGY_YAML: &str = "ontology.yaml";
/// The ontology file's JSON spelling, at the vault root.
pub(super) const VAULT_ONTOLOGY_JSON: &str = "ontology.json";

/// The vault-committed ontology, extracted for the load: the
/// learned-schema seed plus the committed schema the open tail
/// applies.
pub(super) struct VaultOntology {
    /// Relationship types the document declares — map-form frontmatter
    /// entries under any of them are declared (ADR-0406 §2's bootstrap
    /// ambiguity, answered by the committed schema instead of a
    /// scattered bare-link declaration).
    rel_types: BTreeSet<String>,
    /// The identifying label's identity fold (ADR-0647) → its key
    /// property (`key: true`): the filename-mirroring property for
    /// that label's files, the OKF Concept ID declared rather than
    /// inferred. Keyed by the fold — the same identity relation the
    /// walked labels fold under — so [`Self::bind_keys`] meets the
    /// declarations and the walked labels at one direct lookup.
    keys: BTreeMap<String, String>,
    /// The committed half, riding the reusable carrier (ADR-0649):
    /// the document itself, its graph type, and its extension-envelope
    /// indexes — embedded, never re-declared field-by-field, so a
    /// carrier change reaches this module through the type.
    committed: CommittedSchema,
}

/// Read the vault's committed ontology, `None` when the vault carries
/// no such file. Fails closed on every shape that would make the file
/// a schema the engine cannot hold: both spellings present (an
/// ambiguous source), a document past the ontology document class's
/// 16 MiB bound (ADR-0120 §3 — the file is untrusted input), a
/// document that does not parse or validate against the pinned
/// Ontology Graph Spec (ADR-0487), or one whose graph type
/// [ADR-0226](relate_ontology)'s converter cannot express — each
/// refusal names the file.
pub(super) fn read_vault_ontology(vfs: &dyn Vfs) -> Result<Option<VaultOntology>> {
    // One mutually exclusive choice — which spelling the vault
    // carries — over the one table of spellings: none carried is no
    // committed document, one is that file, and both is the ambiguous
    // source, refused before either is read.
    let mut carried: Vec<&'static str> = Vec::new();
    for spelling in [VAULT_ONTOLOGY_YAML, VAULT_ONTOLOGY_JSON] {
        if vfs.exists(spelling)? {
            carried.push(spelling);
        }
    }
    let name = match carried.as_slice() {
        [] => return Ok(None),
        [only] => *only,
        _ => {
            return Err(md::vault_error(format!(
                "the vault carries both {VAULT_ONTOLOGY_YAML} and {VAULT_ONTOLOGY_JSON}; \
                 a vault's committed ontology is one document — keep one spelling"
            )));
        }
    };
    // The ontology document class's own bounded read (ADR-0120 §3):
    // the metadata proof (a past-bound file refused before any byte
    // is fetched), the opened-handle proof, the exact read and the
    // after-read re-proof all come from the one home — the same chain
    // the store's ontology objects read through, never a per-caller
    // re-derivation of the proof sequence. The wrap keeps the file's
    // name on the refusal: every failure names the file through
    // file_error.
    let bytes = crate::snapshot::read_bounded_vfs_path(vfs, name, &crate::snapshot::ONTOLOGY_DOC)
        .map_err(|e| file_error(name, e))?;
    let document =
        relate_ontology::parse_ontology_document(&bytes).map_err(|e| file_error(name, e))?;

    // The expressibility gate: the file claims to be the store's
    // schema, so the engine must be able to hold its graph type — the
    // same conversion `db.ontology.apply` runs (ADR-0226), proven at
    // open rather than at first apply. A type-less relationship entry
    // fails here naming the file, so every surviving entry has a type.
    let body = relate_ontology::to_graph_type_body(&document)
        .map_err(|e| file_error(name, e))?
        .body;
    let graph_type = relate_engine_api::graph_type::GraphTypeSpec::from_body(&body)
        .map_err(|e| file_error(name, e))?;

    // The vocabulary: every declared relationship type, by its Neo4j
    // type token. The expressibility gate above refused every type-less
    // entry naming the file, so a surviving entry's type is that gate's
    // proven invariant — expected here, never re-refused with a second
    // text.
    let mut rel_types = BTreeSet::new();
    for relationship in document.relationships() {
        rel_types.insert(
            relationship
                .rel_type
                .clone()
                .expect("the expressibility gate (to_graph_type_body) refused every type-less relationship entry"),
        );
    }

    // The key declarations (ADR-0649 §2), keyed by the identity fold:
    // [`keys_of`] refuses both ambiguity shapes closed, naming the
    // file — a two-key node type, and two node types whose
    // identifying labels collide under the fold.
    let keys = keys_of(&document, name)?;

    let indexes = indexes_of(&document, name)?;
    let canonical = document.to_json().map_err(|e| file_error(name, e))?;

    Ok(Some(VaultOntology {
        rel_types,
        keys,
        committed: CommittedSchema {
            document: canonical,
            graph_type,
            indexes,
        },
    }))
}

/// Extract the key declarations (ADR-0649 §2): each node type's
/// identifying label and its `key: true` property — the property's
/// addressable id is its name (the map key, never the display name,
/// ADR-0364 §5.1). The returned map is keyed by the vault's one
/// identity relation (the composed fold, ADR-0647) so the
/// declarations and the walked labels meet at one direct lookup in
/// [`VaultOntology::bind_keys`]. Both ambiguity shapes refuse closed
/// naming the file: a node type declaring two key properties, and two
/// node types whose identifying labels collide under the fold — never
/// a silent first-wins by map order.
fn keys_of(
    document: &relate_ontology::OntologyDocument,
    file: &str,
) -> Result<BTreeMap<String, String>> {
    // One table: the fold → (declared label, key property). The fold
    // keys it — the identity relation the declarations bind through —
    // and the declared label rides the value for the collision
    // refusal's message, naming both node types.
    let mut declared: BTreeMap<String, (String, String)> = BTreeMap::new();
    for node in document.nodes() {
        let Some(label) = node.identifying_label() else {
            continue;
        };
        let keys: Vec<&relate_ontology::Property> = node
            .properties
            .iter()
            .filter(|prop| prop.key == Some(true))
            .collect();
        match keys.as_slice() {
            [] => continue,
            [only] => {
                let fold = md::fold_case(label);
                if let Some((first, _)) = declared.get(&fold) {
                    return Err(file_error(
                        file,
                        format!(
                            "node types '{first}' and '{label}' declare the same identifying \
                             label under the vault's identity fold; one label, one key \
                             declaration"
                        ),
                    ));
                }
                declared.insert(fold, (label.to_string(), only.id.clone()));
            }
            _ => {
                return Err(file_error(
                    file,
                    format!(
                        "node type '{label}' declares more than one key property; a node \
                         type has one Concept ID"
                    ),
                ))
            }
        }
    }
    Ok(declared
        .into_iter()
        .map(|(fold, (_label, key))| (fold, key))
        .collect())
}

/// Extract the extension-envelope indexes (ADR-0649 §5): node entries
/// install under their labels; relationship entries are refused — the
/// engine's catalog row does not yet round-trip a relationship index,
/// and a committed file declares only what every open can hold. The
/// catalog row holds one label and one property; entries declaring
/// more of either are refused the same way, naming the file.
fn indexes_of(
    document: &relate_ontology::OntologyDocument,
    file: &str,
) -> Result<Vec<CommittedIndex>> {
    // The one file-naming wrap, partial-applied to this document.
    let fail = |what: String| file_error(file, what);
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let root = document
        .root()
        .as_object()
        .expect("the validated document's root is a mapping");
    // An absent section is an empty iteration, never an early return:
    // the relationship refusal below runs unconditionally, so a
    // committed file that declares indexes on relationships refuses
    // the same way whether or not it also declares nodes.
    for (_, node) in section(root, "nodes") {
        let Some(indexes) = declared_indexes(node) else {
            continue;
        };
        for (id, index) in indexes {
            let index = index
                .as_object()
                .expect("the validated document maps index entries to mappings");
            let name = index
                .get("name")
                .and_then(relate_ontology::Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(id)
                .to_string();
            if !seen.insert(name.clone()) {
                return Err(fail(format!(
                    "index '{name}' is declared more than once in the document"
                )));
            }
            let kind = index
                .get("type")
                .map(|value| index_kind(value).map_err(fail))
                .transpose()?
                .ok_or_else(|| fail("an index declares no type".to_string()))?;
            let labels = strings_of(index.get("labels"), "labels").map_err(fail)?;
            exactly_one(&labels, "label", &name).map_err(fail)?;
            let properties = strings_of(index.get("properties"), "properties").map_err(fail)?;
            exactly_one(&properties, "property", &name).map_err(fail)?;
            let options = options_of(index.get("options"))
                .map_err(|what| fail(format!("index '{name}': {what}")))?;
            out.push(CommittedIndex {
                name,
                detail: relate_engine_api::redo::IndexDetailSpec {
                    kind,
                    relationship: false,
                    labels,
                    properties,
                    with_properties: Vec::new(),
                },
                options,
            });
        }
    }
    // A relationship entry's index envelopes are refused as a class:
    // the catalog row the engine holds does not round-trip them.
    for (id, relationship) in section(root, "relationships") {
        if declared_indexes(relationship).is_some_and(|indexes| !indexes.is_empty()) {
            return Err(fail(format!(
                "relationship '{id}' declares indexes; relationship indexes are \
                 not held from the vault file"
            )));
        }
    }
    Ok(out)
}

/// One declared section's entries: absent is an empty iteration
/// (never an early return — a later refusal stays unconditional), and
/// a present-but-wrong shape is the validator's invariant, expected
/// here once.
fn section<'a>(
    root: &'a relate_ontology::IndexMap<String, relate_ontology::Value>,
    name: &str,
) -> impl Iterator<Item = (&'a String, &'a relate_ontology::Value)> + 'a {
    root.get(name)
        .map(|entries| {
            entries
                .as_object()
                .expect("the validated document maps a present section field to a mapping")
        })
        .into_iter()
        .flat_map(relate_ontology::IndexMap::iter)
}

/// One list field's "exactly one entry" refusal — the catalog row
/// holds one label and one property, and the two fields' refusals
/// share one shape, naming the field and the index.
fn exactly_one(list: &[String], field: &str, name: &str) -> std::result::Result<(), String> {
    if list.is_empty() {
        return Err(format!("index '{name}' declares no {field}s"));
    }
    if list.len() > 1 {
        return Err(format!(
            "index '{name}' must declare exactly one {field} \
             (multi-{field} indexes are not held from the vault file)"
        ));
    }
    Ok(())
}

/// One entry's declared index envelopes: the `extensions.indexes`
/// mapping (ADR-0649 §5) — the one navigation spelling for both entry
/// kinds, the node loop iterating it and the relationship refusal
/// reading it. The present-but-wrong shapes are the validator's
/// invariants, expected like every level of this extraction; absence
/// (`extensions`, the `indexes` key) is genuine — an entry that
/// declares none.
fn declared_indexes(
    entry: &relate_ontology::Value,
) -> Option<&relate_ontology::IndexMap<String, relate_ontology::Value>> {
    let entry = entry
        .as_object()
        .expect("the validated document maps entries to mappings");
    let extensions = entry
        .get("extensions")?
        .as_object()
        .expect("the validated document maps a present extensions field to a mapping");
    Some(
        extensions
            .get("indexes")?
            .as_object()
            .expect("the validated document maps a present indexes field to a mapping"),
    )
}

/// The index-kind classifier, one match owning both the mapping and
/// the refusal: the token vocabulary the pinned spec validates
/// (`FULLTEXT | POINT | RANGE | TEXT | VECTOR | LOOKUP`, always a
/// string) against the kinds the engine's catalog row holds. `LOOKUP`
/// is spec-valid but not held from the vault file; the non-string and
/// absent arms are the validator's invariants, kept defensive.
fn index_kind(value: &relate_ontology::Value) -> std::result::Result<IndexKindSpec, String> {
    match value {
        relate_ontology::Value::String(token) => {
            // The token→kind mapping lives once, in the engine's own
            // display-name table — this arm holds only the vault
            // file's policy over the parsed kinds.
            match IndexKindSpec::from_display_name(token) {
                Some(kind) if kind.is_lookup() => {
                    Err("the LOOKUP index kind is not held from the vault file".to_string())
                }
                Some(kind) => Ok(kind),
                None => Err(format!("index type '{token}' is not one the engine holds")),
            }
        }
        _ => Err("an index's type is not a string".to_string()),
    }
}

/// A spec list field's strings, in document order. Fails closed on a
/// non-string entry — the schema's `items: {type: string}` admits
/// none, so a non-string here is a hand edit, and the refusal names
/// the field rather than silently dropping the entry.
fn strings_of(
    value: Option<&relate_ontology::Value>,
    field: &str,
) -> std::result::Result<Vec<String>, String> {
    let Some(items) = value.and_then(relate_ontology::Value::as_array) else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{field} holds a non-string entry"))
        })
        .collect()
}

/// The options envelope's payload → the engine's value model, verbatim
/// minus the `type` discriminator. A mapping-valued option (nothing
/// the spec's index options define) fails closed naming the key.
fn options_of(
    options: Option<&relate_ontology::Value>,
) -> std::result::Result<BTreeMap<String, CypherValue>, String> {
    let mut out = BTreeMap::new();
    let Some(options) = options.and_then(relate_ontology::Value::as_object) else {
        return Ok(out);
    };
    for (key, value) in options {
        if key == "type" {
            continue;
        }
        out.insert(
            key.clone(),
            cypher_value(value).map_err(|what| format!("option '{key}' {what}"))?,
        );
    }
    Ok(out)
}

/// The ordered value model → the engine's value model, for the option
/// payload's flat scalars and lists.
fn cypher_value(value: &relate_ontology::Value) -> std::result::Result<CypherValue, String> {
    use relate_ontology::Value as Doc;
    Ok(match value {
        Doc::Null => CypherValue::Null,
        Doc::Boolean(b) => CypherValue::Bool(*b),
        Doc::Integer(i) => CypherValue::Int(*i),
        Doc::Float(f) => CypherValue::Float(*f),
        Doc::String(s) => CypherValue::String(s.as_str().into()),
        Doc::Array(items) => CypherValue::List(
            items
                .iter()
                .map(cypher_value)
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into(),
        ),
        // The spec's index options are flat scalars and lists; a
        // mapping is either a foreign extension or a hand edit, and
        // both fail closed naming the key that carried it.
        Doc::Object(_) => return Err("holds a mapping".to_string()),
    })
}

impl VaultOntology {
    /// Seed the load's vocabulary: the file's relationship types join
    /// the set the map-form classification consults, before the parse
    /// runs. The key declarations deliberately do NOT seed here —
    /// they bind after the parse, against the walked labels, through
    /// [`Self::bind_keys`].
    pub(super) fn seed_vocabulary(&self, learned: &mut LearnedSchema) {
        learned.rel_types.extend(self.rel_types.iter().cloned());
    }

    /// Bind the file's key declarations to the walked labels, through
    /// the vault's one identity relation (the case fold, composed —
    /// ADR-0647): a declaration whose label differs only in case or
    /// normalization from a label directory (`label: person` for
    /// `Person/`) binds to the walked canonical spelling, never
    /// silently missing it. Binding overrides both the sidecar's
    /// stale record and the value-matching inference — a declared key
    /// that disagrees with a filename refuses the load, exactly as
    /// ADR-0649 decision 2 records. A declaration matching no walked
    /// label under the fold is a forward declaration — the schema
    /// describes a label the vault does not carry yet — and stays
    /// inert: the keys map is keyed by walked labels only, and the
    /// vocabulary of an absent label has nothing to bind.
    ///
    /// The walked labels arrive as the canonicalization pass's own
    /// fold→canonical-label table — the declarations are keyed by the
    /// same fold ([`keys_of`]), so each declaration meets the walked
    /// spelling at one direct lookup, never a re-folded walked set.
    pub(super) fn bind_keys(
        &self,
        label_folds: &BTreeMap<String, String>,
        keys: &mut BTreeMap<String, String>,
        learned: &mut LearnedSchema,
    ) {
        // The walked table is fold-unique (canonicalize_by_fold dropped
        // the losers) and the declarations are keyed by the same fold,
        // so each declaration matches at most one walked label at a
        // direct lookup — no re-fold, and no remove: the fold-collision
        // case was refused where the declarations were built.
        for (fold, key) in &self.keys {
            let Some(walked_label) = label_folds.get(fold) else {
                continue;
            };
            keys.insert(walked_label.clone(), key.clone());
            learned.keys.insert(walked_label.clone(), key.clone());
        }
    }

    /// The committed half: carried to the facade's open tail, applied
    /// through the replay path — never inside the load, where the
    /// schema text mirror's verification would read it as drift.
    pub(super) fn into_committed(self) -> CommittedSchema {
        self.committed
    }
}
