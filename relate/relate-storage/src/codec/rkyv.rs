//! EXPERIMENTAL rkyv snapshot codec — measurement only, behind the
//! `rkyv-codec` feature (default off). Not a product codec until an ADR
//! promotes it; no store compatibility promise.
//!
//! One archive file per store (`graph.rkyv`), columnar struct-of-arrays:
//! id columns, a label/type dictionary with per-row dictionary ids, and
//! per-row property lists over a shared key dictionary. Loads by validated
//! `rkyv::access` (the archive is *checked*, never taken on trust) and
//! rebuilds through the shared [`SnapshotRows::into_graph`] tail.
//!
//! Property *values* reuse the JSON codec's cypher-json conversion
//! (`props_to_cypher_json` / `props_from_cypher_json`) verbatim, so
//! fidelity is identical to the JSON codec and a measurement compares
//! containers, not value encodings. The catalog rides the same wire as a
//! small JSON string — non-bulk, and it keeps the shared catalog types
//! free of an rkyv derive.

use std::sync::Arc;

use relate_core::{Error, Node, PropertyMap, Relationship, Result};
use relate_executor::Graph;
use rkyv::{Archive, Deserialize, Serialize};

use super::catalog;
use super::convert::{props_from_cypher_json, props_to_cypher_json};
use super::model::{SnapshotContents, SnapshotRows};
use super::SnapshotCodecKind;
use crate::snapshot::{object_exists, put_object, LoadedSnapshot};
use crate::{storage_context, StoreTarget};

/// A cypher-json property value, archived **flat**. Mirrors
/// `serde_json::Value`'s vocabulary exactly (including the
/// integer/unsigned/float split, so no number round-trips through a lossy
/// form), but a list or map holds the *ids* of its children in the
/// archive's shared value table instead of nesting them: the archived
/// type is not recursive, so `rkyv::access` can validate the whole
/// archive — a corrupt or crafted store file is a typed error, never
/// undefined behaviour. Children are interned before their parent, so
/// every child id is smaller than its parent's and the graph is acyclic
/// by construction (the decoder still caps nesting depth, since a
/// validated archive can be logically deep).
#[derive(Archive, Serialize, Deserialize, Debug)]
pub enum JVal {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    /// The ids of this list's items in the archive's value table.
    List(Vec<u32>),
    /// This map's `(key, value id)` entries, in the archive's value table.
    Map(Vec<(String, u32)>),
}

/// The archive's shared value table: every property value interned once,
/// children before parents, addressed by index. The one encoder side of
/// the flat representation ([`JVal`]).
#[derive(Default)]
struct ValueTable {
    values: Vec<JVal>,
}

impl ValueTable {
    /// Intern `value` and return its id. Children are interned first, so
    /// ids are topologically ordered and a crafted archive can only ever
    /// reference backwards. The nesting the reader caps at is enforced
    /// here too, so the writer cannot publish an archive the reader refuses.
    fn intern(&mut self, value: &serde_json::Value) -> Result<u32> {
        self.intern_at(value, 0)
    }

    fn intern_at(&mut self, value: &serde_json::Value, depth: usize) -> Result<u32> {
        if depth > MAX_VALUE_DEPTH {
            return Err(Error::storage(format!(
                "property value nesting exceeds {MAX_VALUE_DEPTH}; the rkyv codec cannot checkpoint it"
            )));
        }
        let archived = match value {
            serde_json::Value::Null => JVal::Null,
            serde_json::Value::Bool(b) => JVal::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    JVal::Int(i)
                } else if let Some(u) = n.as_u64() {
                    JVal::UInt(u)
                } else {
                    let f = n.as_f64().ok_or_else(|| {
                        Error::storage("snapshot number is neither int nor float")
                    })?;
                    JVal::Float(f)
                }
            }
            serde_json::Value::String(text) => JVal::Str(text.clone()),
            serde_json::Value::Array(items) => {
                let mut ids = Vec::with_capacity(items.len());
                for item in items {
                    ids.push(self.intern_at(item, depth + 1)?);
                }
                JVal::List(ids)
            }
            serde_json::Value::Object(map) => {
                let mut entries = Vec::with_capacity(map.len());
                for (key, item) in map {
                    let id = self.intern_at(item, depth + 1)?;
                    entries.push((key.clone(), id));
                }
                JVal::Map(entries)
            }
        };
        let id = checked_table_id(self.values.len(), "value table")?;
        self.values.push(archived);
        Ok(id)
    }
}

/// The deepest property nesting this codec reads *and* writes: the writer
/// refuses what the reader would refuse, so a checkpoint never produces an
/// archive its own load cannot decode.
const MAX_VALUE_DEPTH: usize = 256;

/// The decode's work budget, spent by every item the archive's ids can
/// make the decode materialize: one spend per value-table visit (our
/// encoder interns each value exactly once per occurrence, so a
/// legitimate archive's walk expands at most one entry per table slot)
/// and one per dictionary reference (each materializes an owned name).
/// A crafted (but valid) archive can share one id from many parents, so
/// the walk is counted against the archive's own counts — that is what
/// makes the decode total, not just depth-bounded.
struct WalkBudget {
    remaining: usize,
    row: i64,
}

impl WalkBudget {
    fn spend(&mut self) -> Result<()> {
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
            Error::storage(format!(
                "rkyv snapshot property values expand beyond the archive's own table at node/relationship {}",
                self.row
            ))
        })?;
        Ok(())
    }
}

/// One value id as a `serde_json::Value`, walking the flat table. The
/// depth cap and the work budget together make the walk total on any
/// validated archive.
fn value_to_json(
    values: &rkyv::vec::ArchivedVec<ArchivedJVal>,
    id: u32,
    depth: usize,
    budget: &mut WalkBudget,
) -> Result<serde_json::Value> {
    if depth > MAX_VALUE_DEPTH {
        return Err(Error::storage(format!(
            "rkyv snapshot property nesting exceeds {MAX_VALUE_DEPTH} at node/relationship {}",
            budget.row
        )));
    }
    budget.spend()?;
    let value = dict_item(values, id, "value")?;
    Ok(match value {
        ArchivedJVal::Null => serde_json::Value::Null,
        ArchivedJVal::Bool(b) => serde_json::Value::Bool(*b),
        ArchivedJVal::Int(i) => serde_json::Value::Number(i.to_native().into()),
        ArchivedJVal::UInt(u) => serde_json::Value::Number(u.to_native().into()),
        ArchivedJVal::Float(f) => serde_json::Value::Number(
            serde_json::Number::from_f64(f.to_native())
                .ok_or_else(|| Error::storage("snapshot float is not representable"))?,
        ),
        ArchivedJVal::Str(text) => serde_json::Value::String(text.as_str().to_owned()),
        ArchivedJVal::List(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| value_to_json(values, item.to_native(), depth + 1, budget))
                .collect::<Result<Vec<_>>>()?,
        ),
        ArchivedJVal::Map(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|entry| {
                    Ok((
                        entry.0.as_str().to_owned(),
                        value_to_json(values, entry.1.to_native(), depth + 1, budget)?,
                    ))
                })
                .collect::<Result<serde_json::Map<_, _>>>()?,
        ),
    })
}

/// The archived snapshot: value-encoding marker, id counters, the catalog
/// wire, the shared value table and the columnar row data. Property columns
/// hold `(key id, value id)` pairs; the values themselves live once in
/// [`Snapshot::values`].
#[derive(Archive, Serialize, Deserialize, Debug)]
pub struct Snapshot {
    value_encoding: u64,
    /// The checkpoint LSN this archive holds. The load refuses an archive
    /// whose LSN disagrees with the manifest's: the archive and the
    /// manifest publish separately, so a mismatch is a torn publication.
    checkpoint_lsn: u64,
    next_node_id: i64,
    next_rel_id: i64,
    ontology: Option<String>,
    key_dict: Vec<String>,
    label_dict: Vec<String>,
    values: Vec<JVal>,
    node_ids: Vec<i64>,
    node_label_ids: Vec<Vec<u32>>,
    node_props: Vec<Vec<(u32, u32)>>,
    rel_ids: Vec<i64>,
    rel_type_ids: Vec<u32>,
    rel_starts: Vec<i64>,
    rel_ends: Vec<i64>,
    rel_props: Vec<Vec<(u32, u32)>>,
}

/// The one table-id ceiling policy: a table's next id must fit u32, and
/// exceeding it is a typed error, never a panic.
fn checked_table_id(len: usize, what: &str) -> Result<u32> {
    u32::try_from(len).map_err(|_| Error::storage(format!("rkyv snapshot {what} exceeds u32 ids")))
}

/// A dictionary that interns strings to dense ids in first-seen order. One
/// insertion-ordered set serves both directions, so a name is stored once
/// and the `id == position` invariant needs no maintenance.
#[derive(Default)]
struct Dict {
    names: indexmap::IndexSet<String>,
}

impl Dict {
    fn intern(&mut self, name: &str) -> Result<u32> {
        // An existing name is looked up by borrow, so the checkpoint path
        // allocates only on a dictionary miss.
        if let Some(id) = self.names.get_index_of(name) {
            return checked_table_id(id, "dictionary");
        }
        // The id is proven before the insert, so a refused ceiling leaves
        // the dictionary untouched (the value table twin's rule).
        let id = checked_table_id(self.names.len(), "dictionary")?;
        self.names.insert(name.to_owned());
        Ok(id)
    }

    /// Consume the dictionary into its insertion-ordered names — the one
    /// way out, so the `id == position` invariant stays the wrapper's own.
    fn into_names(self) -> Vec<String> {
        self.names.into_iter().collect()
    }
}

/// Encode a property map as `(key id, value id)` pairs, interning each
/// value into the shared table.
fn encode_props(
    props: &PropertyMap,
    keys: &mut Dict,
    values: &mut ValueTable,
    id: i64,
) -> Result<Vec<(u32, u32)>> {
    let json = props_to_cypher_json(props)
        .map_err(|e| storage_context(e, format_args!("node/relationship {id}")))?;
    json.iter()
        .map(|(key, value)| Ok((keys.intern(key)?, values.intern(value)?)))
        .collect()
}

fn decode_props(
    entries: &rkyv::vec::ArchivedVec<
        rkyv::tuple::ArchivedTuple2<rkyv::primitive::ArchivedU32, rkyv::primitive::ArchivedU32>,
    >,
    keys: &rkyv::vec::ArchivedVec<rkyv::string::ArchivedString>,
    values: &rkyv::vec::ArchivedVec<ArchivedJVal>,
    id: i64,
    value_encoding: u64,
    budget: &mut WalkBudget,
) -> Result<Arc<PropertyMap>> {
    let mut map = serde_json::Map::new();
    for entry in entries.iter() {
        budget.spend()?;
        let key = dict_item(keys, entry.0.to_native(), "key")?;
        map.insert(
            key.as_str().to_owned(),
            value_to_json(values, entry.1.to_native(), 0, budget)?,
        );
    }
    let props = props_from_cypher_json(&map, value_encoding)
        .map_err(|e| storage_context(e, format_args!("node/relationship {id}")))?;
    Ok(PropertyMap::share(props))
}

pub fn dump(
    target: &StoreTarget,
    graph: &Graph,
    checkpoint_lsn: u64,
    snapshot_ts: u64,
    ontology: Option<&str>,
) -> Result<()> {
    let contents = SnapshotContents::from_graph(graph, snapshot_ts)?;
    let theater = graph.checkpoint_theater_for_write()?;
    let wire = catalog::to_json(&contents, &theater);
    let mut keys = Dict::default();
    let mut labels = Dict::default();
    let mut values = ValueTable::default();
    // The columns are accumulated straight into the archive struct, so the
    // pushes and the layout read by the same field names.
    let mut snapshot = Snapshot {
        value_encoding: crate::version::value_encoding::version_for_write(ontology.is_some()),
        checkpoint_lsn,
        next_node_id: contents.next_node_id,
        next_rel_id: contents.next_rel_id,
        ontology: ontology.map(str::to_owned),
        key_dict: Vec::new(),
        label_dict: Vec::new(),
        values: Vec::new(),
        node_ids: Vec::new(),
        node_label_ids: Vec::new(),
        node_props: Vec::new(),
        rel_ids: Vec::new(),
        rel_type_ids: Vec::new(),
        rel_starts: Vec::new(),
        rel_ends: Vec::new(),
        rel_props: Vec::new(),
    };
    for n in &contents.nodes {
        snapshot.node_ids.push(n.id);
        snapshot.node_label_ids.push(
            n.labels
                .iter()
                .map(|label| labels.intern(label))
                .collect::<Result<Vec<_>>>()?,
        );
        snapshot
            .node_props
            .push(encode_props(&n.properties, &mut keys, &mut values, n.id)?);
    }
    for r in &contents.relationships {
        snapshot.rel_ids.push(r.id);
        snapshot.rel_type_ids.push(labels.intern(&r.rel_type)?);
        snapshot.rel_starts.push(r.start);
        snapshot.rel_ends.push(r.end);
        snapshot
            .rel_props
            .push(encode_props(&r.properties, &mut keys, &mut values, r.id)?);
    }
    snapshot.key_dict = keys.into_names();
    snapshot.label_dict = labels.into_names();
    snapshot.values = values.values;

    // The encode is scoped so nothing the publication no longer needs
    // outlives it: the archive struct and the row models drop with the
    // block, and the uncompressed archive drops before the compressed one
    // exists — the checkpoint path's peak is the graph plus the compressed
    // archive, never several copies of it.
    let bytes = {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&snapshot)
            .map_err(|e| Error::storage(format!("rkyv snapshot encode failed: {e}")))?;
        // One lz4 block for the whole archive: the columnar layout alone
        // does not compress (v1 has no per-column encodings), so the codec
        // relies on the same class of shard compression parquet (snappy)
        // and fjall (lz4) use. Pure Rust, no C toolchain.
        // The lz4 block's size prefix is a u32: refuse an archive it
        // cannot describe rather than publishing one every later load
        // fails to read.
        if u32::try_from(bytes.len()).is_err() {
            return Err(Error::storage(format!(
                "rkyv snapshot of {} bytes exceeds the lz4 block's u32 size prefix",
                bytes.len()
            )));
        }
        lz4_flex::compress_prepend_size(&bytes)
    };
    drop(snapshot);
    drop(contents);
    // The publication is a rename swap, like the fjall codec's: the new
    // archive is written to a staging object, the live one renamed aside,
    // the staging one moved in, and only then does the manifest publish.
    // The previous archive is never read back — the rollback moves it by
    // rename — so the checkpoint path's peak stays the graph plus the
    // compressed archive, and a failed checkpoint leaves the previous
    // checkpoint loadable instead of a store every later open refuses.
    put_object(target, super::GRAPH_RKYV_STAGING, &bytes)?;
    let previous_manifest =
        crate::snapshot::get_object_opt(target, crate::snapshot::MANIFEST_JSON)?;
    swap_archive_in(target)?;
    // The manifest publishes last and carries the theater (see
    // `codec::CodecManifest`); a failure rolls the archive swap and the
    // manifest both back — neither half of a checkpoint is left
    // published alone.
    let published = super::write_codec_manifest(
        target,
        SnapshotCodecKind::Rkyv,
        checkpoint_lsn,
        snapshot_ts,
        Some(theater),
        wire,
        ontology,
    );
    if let Err(error) = published {
        let rollback = restore_archive(target).and_then(|()| {
            super::restore_object(target, crate::snapshot::MANIFEST_JSON, previous_manifest)
        });
        return Err(super::publication_failure(error, rollback));
    }
    crate::schema_files::finish_checkpoint(target, graph, ontology)?;
    // The retired backup is dropped best-effort: a cleanup failure leaves
    // residue, never a wrongly reported checkpoint.
    let _ = target.vfs.remove_file(super::GRAPH_RKYV_BACKUP);
    Ok(())
}

/// Swap the staged archive into the live name, keeping the previous
/// archive aside by rename — the one publication move. Its own failure
/// is rolled back by [`restore_archive`].
fn swap_archive_in(target: &StoreTarget) -> Result<()> {
    let had_live = object_exists(target, super::GRAPH_RKYV)?;
    if had_live {
        // A stale backup from an interrupted publication is replaced, not
        // adopted.
        if object_exists(target, super::GRAPH_RKYV_BACKUP)? {
            target.vfs.remove_file(super::GRAPH_RKYV_BACKUP)?;
        }
        target
            .vfs
            .rename(super::GRAPH_RKYV, super::GRAPH_RKYV_BACKUP)?;
    }
    if let Err(err) = target
        .vfs
        .rename(super::GRAPH_RKYV_STAGING, super::GRAPH_RKYV)
    {
        return Err(super::publication_failure(
            Error::storage(format!("rkyv archive publication failed: {err}")),
            restore_archive(target),
        ));
    }
    Ok(())
}

/// Undo a failed rkyv publication: whatever archive sits at the live name
/// is removed and the backup — the previous checkpoint's archive, kept by
/// rename rather than a byte copy — moves back.
fn restore_archive(target: &StoreTarget) -> Result<()> {
    if object_exists(target, super::GRAPH_RKYV)? {
        target.vfs.remove_file(super::GRAPH_RKYV)?;
    }
    if object_exists(target, super::GRAPH_RKYV_BACKUP)? {
        target
            .vfs
            .rename(super::GRAPH_RKYV_BACKUP, super::GRAPH_RKYV)?;
    }
    Ok(())
}

/// LZ4's per-sequence expansion ceiling: a match copies at most 255 bytes
/// per two-byte offset, so no valid single block can expand its input by
/// more than this factor. The prepended length is checked against it
/// before any allocation, so a crafted length cannot make the load
/// allocate gigabytes (the decompression-bomb bound).
const LZ4_MAX_EXPANSION: usize = 255;

/// Decompress the size-prepended lz4 block, refusing a declared length
/// beyond what the compressed bytes can possibly expand to.
fn decompress_bounded(bytes: &[u8]) -> Result<Vec<u8>> {
    let (prefix, body) = bytes
        .split_first_chunk::<4>()
        .ok_or_else(|| Error::storage("rkyv snapshot is shorter than its size prefix"))?;
    let declared = u32::from_le_bytes(*prefix) as usize;
    let ceiling = body
        .len()
        .saturating_mul(LZ4_MAX_EXPANSION)
        .saturating_add(1 << 20);
    if declared > ceiling {
        return Err(Error::storage(format!(
            "rkyv snapshot declares {declared} bytes, beyond the {ceiling}-byte ceiling its {}-byte block can expand to",
            body.len(),
        )));
    }
    lz4_flex::decompress_size_prepended(bytes)
        .map_err(|e| Error::storage(format!("rkyv snapshot decompression failed: {e}")))
}

/// Whether the store carries this codec's archive object — the evidence
/// `resolve_codec` counts.
pub(crate) fn has_store_files(target: &StoreTarget) -> Result<bool> {
    object_exists(target, super::GRAPH_RKYV)
}

/// The load path: zero-copy access (validation only) into the archive, then
/// the shared rebuild tail.
pub(crate) fn load_with_checkpoint_lsn(target: &StoreTarget) -> Result<LoadedSnapshot> {
    let Some((manifest, marker)) = super::read_codec_manifest(target)? else {
        return Ok(LoadedSnapshot::empty(0));
    };
    let Some(bytes_compressed) = crate::snapshot::get_object_opt(target, super::GRAPH_RKYV)? else {
        // The manifest names a published checkpoint, so its archive must
        // exist (the shared manifest-implies-payload policy).
        return Err(super::experimental::missing_payload(&manifest, "archive"));
    };
    // The compressed buffer is released before validation: the load
    // path's peak is the decompressed archive alone, not both copies.
    let bytes = decompress_bounded(&bytes_compressed)?;
    drop(bytes_compressed);
    // Validated access, not `access_unchecked`: a corrupt, truncated or
    // crafted store file is a typed error here, never undefined behaviour.
    // The archive's value representation is flat (no recursive type) and
    // the codec is built with rkyv's `unaligned` feature, so validation
    // and the returned reference are sound on a plain byte buffer.
    let archived = rkyv::access::<ArchivedSnapshot, rkyv::rancor::Error>(&bytes)
        .map_err(|e| Error::storage(format!("rkyv snapshot validation failed: {e}")))?;
    if archived.checkpoint_lsn.to_native() != manifest.envelope.checkpoint_lsn {
        return Err(super::checkpoint_identity_mismatch(
            &manifest,
            "archive",
            archived.checkpoint_lsn.to_native(),
        ));
    }
    let has_ontology = archived.ontology.is_some();
    // The content gate: the payload's own value-encoding marker (the
    // shared experimental policy).
    let encoding = super::content_encoding(archived.value_encoding.to_native(), has_ontology)?;
    // Decoded straight from the archived form: deserializing into an owned
    // `Snapshot` first would hold the compressed bytes, the decompressed
    // bytes and the copy at peak, for no benefit.
    let rows = decode_contents(archived, encoding)?;
    let ontology = archived
        .ontology
        .as_ref()
        .map(|document| document.as_str().to_owned());
    super::assemble_loaded(rows, &manifest, ontology, marker)
}

/// The one column-bounds policy of the decode: every column must have a
/// row for every id.
fn column<'a, T>(column: &'a [T], row: usize, what: &str) -> Result<&'a T> {
    column
        .get(row)
        .ok_or_else(|| Error::storage(format!("rkyv snapshot {what} column is short")))
}

/// The one dictionary-lookup policy of the decode: a dictionary id must
/// name an entry.
fn dict_item<'a, T>(dict: &'a [T], id: u32, what: &str) -> Result<&'a T> {
    dict.get(id as usize)
        .ok_or_else(|| Error::storage(format!("rkyv snapshot {what} id {id} out of range")))
}

fn decode_contents(snapshot: &ArchivedSnapshot, value_encoding: u64) -> Result<SnapshotRows> {
    // One budget for the whole decode, sized by the archive's own counts:
    // the value table plus every dictionary reference its columns carry
    // (labels, rel types, property keys — each materializes an owned
    // name). A legitimate archive expands at most one entry per slot and
    // walks each reference its columns declare; a crafted one that shares
    // subtrees or walks more than its columns declare runs out of budget
    // instead of expanding without bound.
    let dict_refs = snapshot
        .node_label_ids
        .iter()
        .map(|ids| ids.len())
        .chain(std::iter::once(snapshot.rel_ids.len()))
        .chain(snapshot.node_props.iter().map(|props| props.len()))
        .chain(snapshot.rel_props.iter().map(|props| props.len()))
        .try_fold(0usize, |sum, n| sum.checked_add(n))
        .ok_or_else(|| Error::storage("rkyv snapshot dictionary references overflow usize"))?;
    let mut budget = WalkBudget {
        remaining: snapshot
            .values
            .len()
            .checked_add(dict_refs)
            .ok_or_else(|| Error::storage("rkyv snapshot decode budget overflows usize"))?,
        row: 0,
    };
    let mut nodes = Vec::with_capacity(snapshot.node_ids.len());
    for (row, id) in snapshot.node_ids.iter().enumerate() {
        let id = id.to_native();
        budget.row = id;
        let label_ids = column(&snapshot.node_label_ids, row, "node label")?;
        let labels = label_ids
            .iter()
            .map(|label_id| {
                budget.spend()?;
                Ok(
                    dict_item(&snapshot.label_dict, label_id.to_native(), "label")?
                        .as_str()
                        .to_owned(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let props = column(&snapshot.node_props, row, "node property")?;
        nodes.push(Node {
            id,
            labels: Arc::new(labels),
            properties: decode_props(
                props,
                &snapshot.key_dict,
                &snapshot.values,
                id,
                value_encoding,
                &mut budget,
            )?,
        });
    }
    let mut relationships = Vec::with_capacity(snapshot.rel_ids.len());
    for (row, id) in snapshot.rel_ids.iter().enumerate() {
        let id = id.to_native();
        budget.row = id;
        let type_id = column(&snapshot.rel_type_ids, row, "rel type")?.to_native();
        budget.spend()?;
        let rel_type = dict_item(&snapshot.label_dict, type_id, "rel type")?
            .as_str()
            .to_owned();
        let props = column(&snapshot.rel_props, row, "rel property")?;
        relationships.push(Relationship {
            id,
            start: column(&snapshot.rel_starts, row, "rel start")?.to_native(),
            end: column(&snapshot.rel_ends, row, "rel end")?.to_native(),
            rel_type,
            properties: decode_props(
                props,
                &snapshot.key_dict,
                &snapshot.values,
                id,
                value_encoding,
                &mut budget,
            )?,
        });
    }
    // The catalog rides the manifest to `into_graph` (the rows carry the
    // engine's row models, the manifest the catalog wire).
    Ok(SnapshotRows {
        nodes,
        relationships,
        next_node_id: snapshot.next_node_id.to_native(),
        next_rel_id: snapshot.next_rel_id.to_native(),
    })
}

pub fn snapshot_data_bytes(target: &StoreTarget) -> Result<u64> {
    // The one size-accounting policy over this codec's objects (see
    // `snapshot::sum_object_bytes`).
    crate::snapshot::sum_object_bytes(target, &[crate::snapshot::MANIFEST_JSON, super::GRAPH_RKYV])
}
