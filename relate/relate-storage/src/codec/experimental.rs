//! The experimental codec's shared machinery (ADR-0537): the checkpoint
//! image it publishes and reads, and the load tail its decoder hands off
//! to. One module cfg — the whole file compiles with the experimental
//! codec and not otherwise.

use relate_core::{Error, Result};

use super::catalog;
use super::model::SnapshotRows;
use super::{Envelope, SnapshotCodecKind};
use crate::snapshot::LoadedSnapshot;
use crate::StoreTarget;

/// The experimental codec's checkpoint image: the wire envelope plus the
/// format marker, the checkpoint theater and the catalog. The theater must
/// ride the image (not the codec payload) — the engine's marker discipline
/// and its forgery tests read it there, exactly as the Parquet manifest
/// carries it; the envelope and the catalog ride their own wire carriers,
/// flattened in, so the field sets are defined once.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CodecManifest {
    pub(crate) format_version: u32,
    #[serde(flatten)]
    pub(crate) envelope: Envelope,
    #[serde(
        default,
        deserialize_with = "crate::manifest::deserialize_checkpoint_theater",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) theater: Option<relate_engine_api::CheckpointTheater>,
    #[serde(flatten)]
    pub(crate) catalog: catalog::CatalogWire,
    /// Whether the store selects an ontology document — the content flag
    /// the format marker classifies against (ADR-0120 §8: the format
    /// follows the content), exactly as the Parquet manifest's
    /// `ontology_path` presence is its flag.
    #[serde(default)]
    pub(crate) has_ontology: bool,
}

/// Publish the experimental codec's manifest — the envelope, the format
/// marker (`version::manifest::version_for_write`) and the theater.
pub(crate) fn write_codec_manifest(
    target: &StoreTarget,
    codec: SnapshotCodecKind,
    checkpoint_lsn: u64,
    commit_ts: u64,
    theater: Option<relate_engine_api::CheckpointTheater>,
    wire: catalog::CatalogWire,
    ontology: Option<&str>,
) -> Result<()> {
    let manifest = CodecManifest {
        // An experimental codec never writes wide parts: ADR-0534's
        // encoding belongs to the Parquet codec's parts.
        format_version: crate::version::manifest::version_for_write(ontology.is_some(), false),
        envelope: Envelope {
            codec,
            checkpoint_lsn,
            commit_ts,
            query_engine: relate_engine_api::MEMORY_QUERY_ENGINE.to_owned(),
            write_engine: relate_engine_api::LOCAL_WRITE_ENGINE.to_owned(),
        },
        theater,
        catalog: wire,
        has_ontology: ontology.is_some(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| Error::storage(e.to_string()))?;
    crate::snapshot::put_object(target, crate::snapshot::MANIFEST_JSON, &bytes)
}

/// Read it back, running the one format gate (ADR-0114 Decision 2 /
/// ADR-0127 Decision 2) before anything else inspects the manifest: the
/// marker classifies against the manifest's own ontology flag, the same
/// content fact the writer derived the version from.
pub(crate) fn read_codec_manifest(
    target: &StoreTarget,
) -> Result<Option<(CodecManifest, crate::version::SnapshotMarker)>> {
    // One read: the canonical optional getter owns the absence policy.
    let Some(bytes) = crate::snapshot::get_object_opt(target, crate::snapshot::MANIFEST_JSON)?
    else {
        return Ok(None);
    };
    let manifest: CodecManifest =
        serde_json::from_slice(&bytes).map_err(|e| Error::storage(e.to_string()))?;
    let marker = crate::version::manifest::gate(manifest.format_version, manifest.has_ontology)?;
    Ok(Some((manifest, marker)))
}

/// The manifest-implies-payload invariant every experimental loader
/// enforces: `read_codec_manifest` returning `Some` is proof a checkpoint
/// was published, so the payload that checkpoint names must exist — an
/// absent one is a damaged store, never a fresh one (serving an empty
/// graph would let the next checkpoint publish that emptiness over the
/// real data).
pub(crate) fn missing_payload(manifest: &CodecManifest, what: &str) -> Error {
    Error::storage(format!(
        "experimental codec store is damaged: {} names checkpoint lsn {} but its {what} is missing",
        crate::snapshot::MANIFEST_JSON,
        manifest.envelope.checkpoint_lsn,
    ))
}

/// The payload/manifest checkpoint-identity policy every experimental
/// loader enforces: the payload names the checkpoint it was published
/// with, and a disagreement with the manifest is a torn publication (the
/// two halves publish separately) — the store is refused, never served.
/// The codec named in the message is the manifest envelope's own typed
/// fact, the one resolution already matched against this loader.
pub(crate) fn checkpoint_identity_mismatch(
    manifest: &CodecManifest,
    what: &str,
    payload_lsn: u64,
) -> Error {
    Error::storage(format!(
        "{} store is damaged: the {what} holds checkpoint lsn {payload_lsn} but {} names {}",
        manifest.envelope.codec,
        crate::snapshot::MANIFEST_JSON,
        manifest.envelope.checkpoint_lsn,
    ))
}

/// The payload content gate every experimental loader enforces: the
/// payload's own value-encoding marker is gated and the gated version is
/// the decode encoding (the JSON codec's rule). The payload's marker
/// itself is deliberately discarded — the store-level marker the load
/// carries stays the manifest's (the format gate), exactly as before.
pub(crate) fn content_encoding(payload_encoding: u64, has_ontology: bool) -> Result<u64> {
    let (encoding, _payload_marker) =
        crate::version::value_encoding::gate(Some(payload_encoding), has_ontology)?;
    Ok(encoding)
}

/// Put a store object back the way a failed publication found it: a
/// previous object is rewritten, an object that did not exist before is
/// removed if the failed publication managed to create one, and nothing
/// is done when there is nothing to undo.
pub(crate) fn restore_object(
    target: &StoreTarget,
    name: &str,
    previous: Option<Vec<u8>>,
) -> Result<()> {
    match previous {
        Some(bytes) => crate::snapshot::put_object(target, name, &bytes),
        None if crate::snapshot::object_exists(target, name)? => target.vfs.remove_file(name),
        None => Ok(()),
    }
}

/// Fold a rollback failure into the publication error the caller sees: a
/// restore that itself failed leaves a store the load must refuse, so it
/// is never swallowed. The message is rewritten through
/// [`crate::map_message`], so the error keeps its variant and its one
/// display prefix instead of nesting a second one.
pub(crate) fn publication_failure(error: Error, rollback: Result<()>) -> Error {
    match rollback {
        Ok(()) => error,
        Err(rollback) => crate::map_message(error, |message| {
            format!("{message} (and the rollback failed: {rollback})")
        }),
    }
}

/// The experimental codec's shared load tail: rebuild the graph from the
/// decoded rows (the shared `SnapshotRows::into_graph`), floor its commit
/// ts from the manifest envelope, and assemble the [`LoadedSnapshot`] —
/// everything after a codec's own row decode. The manifest's ontology
/// flag and the decoded payload's document are cross-checked here: the
/// content fact is written to both carriers, so a store where they
/// disagree is a corrupt or forged image and fails closed.
pub(crate) fn assemble_loaded<G: relate_engine_api::SnapshotGraph>(
    rows: SnapshotRows,
    manifest: &CodecManifest,
    ontology: Option<String>,
    marker: crate::version::SnapshotMarker,
) -> Result<LoadedSnapshot<G>> {
    if manifest.has_ontology != ontology.is_some() {
        return Err(Error::storage(format!(
            "manifest {} the ontology document the payload {}",
            if manifest.has_ontology {
                "names"
            } else {
                "does not name"
            },
            if ontology.is_some() {
                "carries"
            } else {
                "does not carry"
            },
        )));
    }
    let (envelope_commit_ts, checkpoint_lsn) = manifest.envelope.envelope_meta()?;
    let graph = rows.into_graph(
        manifest.catalog.clone(),
        envelope_commit_ts,
        manifest.theater.clone(),
    )?;
    Ok(LoadedSnapshot {
        graph,
        checkpoint_lsn,
        ontology,
        marker,
        deferred: None,
    })
}
