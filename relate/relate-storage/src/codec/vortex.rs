//! Experimental Vortex snapshot codec (ADR-0665): node and relationship
//! rows in row-range Vortex shards, published through the VFS beside a JSON
//! payload manifest and the shared root envelope.
//!
//! The layout follows the ADR-0665 decision: immutable files are the
//! checkpoints, sharded by row range — column-complete struct files per
//! row window, the same shape as the Parquet codec's id-window shards
//! (ADR-0022). The rows are stored as one UTF-8 column of JSON row
//! encodings: the checkpoint carries the whole node/relationship row, and
//! the columnar property promotion is the engine-side work the ADR defers.
//! Every object is read and written through the [`Vfs`](crate::Vfs) object
//! helpers (`get_object`/`put_object`), never the host filesystem.

use relate_core::{Error, Result};
use relate_engine_api::GraphExport;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::catalog;
use super::model::{NodeRow, RelRow, SnapshotContents, SnapshotRows};
use super::SnapshotCodecKind;
use std::collections::HashSet;

use super::{assemble_loaded, content_encoding, read_codec_manifest};
use crate::snapshot::{get_object, get_object_opt, object_exists, put_object, LoadedSnapshot};
use crate::StoreTarget;

/// Rows per shard: one checkpoint shard per contiguous row window. Small
/// enough that a fold rewrites only the windows touching dirty ids.
const SHARD_ROWS: usize = 4096;
const NODE_SHARD_PREFIX: &str = "nodes-";
const REL_SHARD_PREFIX: &str = "rels-";
const SHARD_SUFFIX: &str = ".vortex";

/// The payload manifest: what the shared envelope does not carry — the
/// checkpoint LSN, the format marker, the next ids, the ontology document
/// and the shard list.
#[derive(Debug, Serialize, Deserialize)]
struct VortexMeta {
    /// The checkpoint LSN this payload holds. The load refuses a payload
    /// whose LSN disagrees with the envelope's (a missing one fails to
    /// deserialize): the two publish separately, so a disagreement is a
    /// torn publication and the store is refused, never served.
    checkpoint_lsn: u64,
    value_encoding: u64,
    next_node_id: i64,
    next_rel_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ontology: Option<String>,
    node_shards: Vec<String>,
    rel_shards: Vec<String>,
}

pub fn dump(
    target: &StoreTarget,
    cut: &crate::codec::cut::CheckpointCut<'_>,
    _rewrite: crate::codec::cut::Rewrite,
) -> Result<()> {
    let ontology = cut.ontology_document();
    let theater = cut.checkpoint_theater_for_write()?;
    let contents = SnapshotContents::from_graph(cut, cut.snapshot_ts())?;
    // The catalog rides the shared manifest; build it before the rows are
    // consumed into shards.
    let wire = catalog::to_json(&contents, &theater);

    // The live payload manifest's shard names — the generation a
    // same-LSN retry must not rewrite in place (the crash-safety rule
    // `write_shards` upholds: a checkpoint retried at the same cut LSN —
    // a crash after the envelope published, before the WAL truncation,
    // no commits since — computes the same names as the live set, and a
    // retry rewriting them in place could tear a loadable checkpoint
    // the LSN cross-check cannot see).
    let live_names: HashSet<String> = payload_meta(target)?
        .map(|meta| {
            meta.node_shards
                .iter()
                .chain(meta.rel_shards.iter())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let node_shards = write_shards(
        target,
        NODE_SHARD_PREFIX,
        cut.lsn(),
        &contents.nodes,
        NodeRow::encode,
        &live_names,
    )?;
    let rel_shards = write_shards(
        target,
        REL_SHARD_PREFIX,
        cut.lsn(),
        &contents.relationships,
        RelRow::encode,
        &live_names,
    )?;

    let meta = VortexMeta {
        checkpoint_lsn: cut.lsn(),
        value_encoding: crate::version::value_encoding::version_for_write(ontology.is_some()),
        next_node_id: contents.next_node_id,
        next_rel_id: contents.next_rel_id,
        ontology: ontology.map(str::to_owned),
        node_shards: node_shards.clone(),
        rel_shards: rel_shards.clone(),
    };
    let meta_bytes = serde_json::to_vec(&meta).map_err(|e| Error::storage(e.to_string()))?;
    // The one staged-payload publication both fixed-name payload codecs
    // share: staging write, envelope capture, rename swap, envelope
    // publish, the rollback pair.
    super::publish_staged_payload(
        target,
        &super::StagedNames {
            what: "vortex payload manifest",
            live: super::GRAPH_VORTEX_META,
            staging: super::GRAPH_VORTEX_META_STAGING,
            backup: super::GRAPH_VORTEX_META_BACKUP,
        },
        &meta_bytes,
        SnapshotCodecKind::Vortex,
        cut,
        wire,
    )?;
    // Retire orphans only after the manifest naming the new shard set is
    // published: a crash before it leaves harmless stray shards and
    // staging/backup objects, never an old manifest whose named shards
    // were already removed.
    // Post-publication garbage collection, best-effort (the rkyv
    // archive's backup-drop precedent): the checkpoint is complete and
    // loadable the moment the envelope published, so a listing or
    // removal failure here leaves harmless strays — the next
    // checkpoint's retirement removes them — never a wrongly reported
    // failed checkpoint. `?` stays on the steps completeness depends on.
    let _ = retire_orphaned_shards(target, &node_shards, &rel_shards);
    // The shared checkpoint tail (ADR-0481 §4/§5): the mirror publishes
    // after the envelope it reflects, then the dirty tracking clears.
    crate::schema_files::finish_checkpoint(target, cut, ontology)?;
    Ok(())
}

/// The one shard-object naming scheme — the codec's own policy, published
/// so the installers and probes that must predict a checkpoint's object
/// names ask the codec instead of re-deriving them. `retry` is the
/// same-LSN re-publication generation: `None` is the base name, `Some(n)`
/// appends the `r{n}` marker, so the writer's first candidate, its retry
/// loop, and the parser's grammar share one composition site.
pub fn shard_object_name(prefix: &str, index: usize, lsn: u64, retry: Option<u32>) -> String {
    match retry {
        None => format!("{prefix}{index}-{lsn}{SHARD_SUFFIX}"),
        Some(retry) => format!("{prefix}{index}-{lsn}r{retry}{SHARD_SUFFIX}"),
    }
}

/// Whether `name` is one of this codec's shard objects — the name grammar
/// the load's trust boundary, the bounded-read classifier, and the orphan
/// retirement all share. A payload manifest naming anything else (another
/// store object, a foreign path) is a plant, refused at the open: the
/// meta arrives from untrusted disk, and its lists feed reads and
/// retirement.
pub(crate) fn parse_shard_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(SHARD_SUFFIX) else {
        return false;
    };
    // `split_once` consumes the separator, so the arms carry the prefix
    // without its trailing dash (NODE_SHARD_PREFIX/REL_SHARD_PREFIX spell
    // it with one).
    let rest = match stem.split_once('-') {
        Some(("nodes", rest)) => rest,
        Some(("rels", rest)) => rest,
        _ => return false,
    };
    // `{index}-{lsn}` or a retry generation `{index}-{lsn}r{retry}`: the
    // fields are non-empty digit runs (leading zeros are a writer's
    // choice, not refused).
    let Some((index, lsn)) = rest.split_once('-') else {
        return false;
    };
    if !digit_run(index) {
        return false;
    }
    // The LSN field, or `r{retry}` beside it (the same-LSN re-publication
    // scheme's generation marker) — the one split, both halves validated.
    match lsn.split_once('r') {
        None => digit_run(lsn),
        Some((lsn, retry)) => digit_run(lsn) && digit_run(retry),
    }
}

/// The grammar's one field predicate: a non-empty run of ASCII digits.
fn digit_run(field: &str) -> bool {
    !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit())
}

/// Encode `rows` into row-range shards and publish each through the VFS;
/// returns the object names, in row order, for the payload manifest. Each
/// window's encodings live only inside its own iteration — the checkpoint
/// never stages the whole graph's encodings beside the rows themselves,
/// only one bounded window at a time.
///
/// Crash safety (the names ARE the scheme): every shard carries its
/// checkpoint's LSN, so a checkpoint never writes a name a live manifest
/// still names — a crash mid-write leaves the previous checkpoint's
/// shards intact (the store opens on them) plus harmless strays this
/// generation's retirement never reached. The manifest swap is the one
/// atomic visibility point; `retire_orphaned_shards` runs only after it.
fn write_shards<R, T>(
    target: &StoreTarget,
    prefix: &str,
    lsn: u64,
    rows: &[R],
    encode: fn(&R) -> Result<T>,
    live: &HashSet<String>,
) -> Result<Vec<String>>
where
    T: Serialize,
{
    let mut names = Vec::new();
    for (index, window) in rows.chunks(SHARD_ROWS).enumerate() {
        let mut encoded = Vec::with_capacity(window.len());
        for row in window {
            let row = encode(row)?;
            encoded.push(serde_json::to_string(&row).map_err(|e| Error::storage(e.to_string()))?);
        }
        // Crash safety (the names ARE the scheme): every shard publishes
        // under a name NO LIVE MANIFEST NAMES. A same-LSN retry computes
        // the live generation's own names, so rewriting them in place
        // could tear a loadable checkpoint; retry generations (the
        // helper's `r{n}` marker) advance until the name is fresh, the
        // envelope swap publishes the new set, and retirement removes
        // the superseded one.
        let mut retry = None;
        let mut name = shard_object_name(prefix, index, lsn, retry);
        while live.contains(&name) {
            retry = Some(retry.map_or(2, |r| r + 1));
            name = shard_object_name(prefix, index, lsn, retry);
        }
        let bytes = super::shard_codec::encode(&encoded)?;
        put_object(target, &name, &bytes)?;
        names.push(name);
    }
    Ok(names)
}

/// Delete shard objects a previous checkpoint published but this one no
/// longer names — the orphan retirement the immutable publication needs
/// when the graph shrinks between checkpoints.
fn retire_orphaned_shards(
    target: &StoreTarget,
    node_shards: &[String],
    rel_shards: &[String],
) -> Result<()> {
    let existing = target
        .vfs
        .list_dir("")
        .map_err(|e| Error::storage(e.to_string()))?;
    let mut stale = Vec::new();
    for name in existing {
        let is_shard = parse_shard_name(&name);
        // The manifest's own staging and backup strays ride the same
        // retirement: after the envelope published, neither name can be
        // one a live load reads.
        let is_meta_stray =
            name == super::GRAPH_VORTEX_META_STAGING || name == super::GRAPH_VORTEX_META_BACKUP;
        if (is_shard || is_meta_stray)
            && !node_shards.contains(&name)
            && !rel_shards.contains(&name)
        {
            stale.push(name);
        }
    }
    if !stale.is_empty() {
        target
            .vfs
            .remove_files(&stale)
            .map_err(|e| Error::storage(e.to_string()))?;
    }
    Ok(())
}

/// Load the snapshot at its checkpoint LSN: the manifest supplies the
/// catalog, theater and format marker, the payload manifest the checkpoint
/// LSN, next ids, ontology and shard list, and each shard its JSON rows.
/// The payload's LSN must match the envelope's — the two publish
/// separately, so a disagreement is a torn publication, refused here.
pub(crate) fn load_with_checkpoint_lsn<G: relate_engine_api::SnapshotGraph>(
    target: &StoreTarget,
) -> Result<LoadedSnapshot<G>> {
    let Some((manifest, marker)) = read_codec_manifest(target)? else {
        return Ok(LoadedSnapshot::empty(0));
    };
    let Some(meta) = payload_meta(target)? else {
        return Err(super::experimental::missing_payload(
            &manifest,
            super::GRAPH_VORTEX_META,
        ));
    };
    if meta.checkpoint_lsn != manifest.envelope.checkpoint_lsn {
        return Err(super::checkpoint_identity_mismatch(
            &manifest,
            "payload manifest",
            meta.checkpoint_lsn,
        ));
    }
    let encoding = content_encoding(meta.value_encoding, manifest.has_ontology)?;

    let nodes = read_shards(target, &meta.node_shards, encoding, NodeRow::decode)?;
    let relationships = read_shards(target, &meta.rel_shards, encoding, RelRow::decode)?;
    let rows = SnapshotRows {
        nodes,
        relationships,
        next_node_id: meta.next_node_id,
        next_rel_id: meta.next_rel_id,
    };
    assemble_loaded(rows, &manifest, meta.ontology, marker)
}

/// Read every named shard and decode its JSON rows into `T` — the one shard
/// read both the node and the relationship lists go through, so a change to
/// the shard wire (error context, encoding) has one home.
fn read_shards<R, T>(
    target: &StoreTarget,
    names: &[String],
    encoding: u64,
    decode: fn(R, u64) -> Result<T>,
) -> Result<Vec<T>>
where
    R: DeserializeOwned,
{
    let mut out = Vec::new();
    for name in names {
        let bytes = get_object(target, name)?;
        for row in super::shard_codec::decode(&bytes)? {
            let row: R = serde_json::from_str(&row).map_err(|e| Error::storage(e.to_string()))?;
            out.push(decode(row, encoding)?);
        }
    }
    Ok(out)
}

/// The store's evidence: its payload manifest, beside the shared envelope.
pub(crate) fn has_store_files(target: &StoreTarget) -> Result<bool> {
    object_exists(target, super::GRAPH_VORTEX_META)
}

/// The payload manifest read once — the meta object's one read-and-decode
/// policy, its name in the error context. `None` when no checkpoint has
/// published yet (a fresh store).
fn payload_meta(target: &StoreTarget) -> Result<Option<VortexMeta>> {
    let Some(bytes) = get_object_opt(target, super::GRAPH_VORTEX_META)? else {
        return Ok(None);
    };
    let meta: VortexMeta = serde_json::from_slice(&bytes)
        .map_err(|e| Error::storage(format!("{}: {e}", super::GRAPH_VORTEX_META)))?;
    // The trust boundary: the meta arrives from untrusted disk and its
    // lists feed reads and retirement — a name that is not one of this
    // codec's shard objects is a plant (another store object, a foreign
    // path), refused here so every consumer (the load, the byte
    // accounting, the next checkpoint's live-name capture) inherits the
    // validated lists.
    for name in meta.node_shards.iter().chain(meta.rel_shards.iter()) {
        if !parse_shard_name(name) {
            return Err(Error::storage(format!(
                "{} names {} which is not a vortex shard object",
                super::GRAPH_VORTEX_META,
                name
            )));
        }
    }
    Ok(Some(meta))
}

/// The snapshot's accounted bytes: the shared envelope, the payload
/// manifest, and every shard it names — through the one size-accounting
/// policy ([`crate::snapshot::sum_object_bytes`]).
pub(crate) fn snapshot_data_bytes(target: &StoreTarget) -> Result<u64> {
    let meta = payload_meta(target)?;
    let mut names: Vec<&str> = SnapshotCodecKind::Vortex.checkpoint_objects().to_vec();
    if let Some(meta) = &meta {
        names.extend(meta.node_shards.iter().map(String::as_str));
        names.extend(meta.rel_shards.iter().map(String::as_str));
    }
    crate::snapshot::sum_object_bytes(target, &names)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_shard_name, shard_object_name, VortexMeta, NODE_SHARD_PREFIX, REL_SHARD_PREFIX,
    };

    #[test]
    fn the_shard_name_grammar_admits_only_this_codec_s_names() {
        // The writer's own spellings, both generations.
        for name in [
            shard_object_name(NODE_SHARD_PREFIX, 0, 1, None),
            shard_object_name(REL_SHARD_PREFIX, 3, 9007199254740993, None),
            // A retry generation's name (the same-LSN scheme's r2 form).
            shard_object_name(NODE_SHARD_PREFIX, 0, 1, Some(2)),
        ] {
            assert!(parse_shard_name(&name), "{name} is a shard object");
        }
        // The plants and the foreigners: another store object, a foreign
        // path, a malformed field, a bare prefix.
        for name in [
            "manifest.json",
            "graph.vortex.json",
            "nodes-0.vortex",
            "nodes--1.vortex",
            "nodes-x-1.vortex",
            "nodes-0-1r.vortex",
            "nodes-0-1rx.vortex",
            "rels-0.vortex",
            "shards-0-1.vortex",
            "nodes-0-1.parquet",
            "../nodes-0-1.vortex",
        ] {
            assert!(!parse_shard_name(name), "{name} must not parse");
        }
    }

    #[test]
    fn payload_manifest_requires_its_checkpoint_lsn() {
        // A payload without the LSN (an older or truncated publication)
        // must fail closed, never parse with a defaulted LSN.
        let without_lsn = r#"{"value_encoding":2,"next_node_id":0,"next_rel_id":0,"node_shards":[],"rel_shards":[]}"#;
        assert!(serde_json::from_str::<VortexMeta>(without_lsn).is_err());

        let with_lsn = r#"{"checkpoint_lsn":7,"value_encoding":2,"next_node_id":0,"next_rel_id":0,"node_shards":[],"rel_shards":[]}"#;
        let meta: VortexMeta = serde_json::from_str(with_lsn).expect("a complete payload parses");
        assert_eq!(meta.checkpoint_lsn, 7);
    }
}
