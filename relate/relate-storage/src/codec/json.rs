//! Relate GraphData snapshot (`graph.json` full rewrite). ADR-0050.

use relate_core::{Error, Node, Relationship, Result};
use relate_engine_api::CheckpointTheater;
#[cfg(test)]
use relate_executor::Graph;
use serde::{Deserialize, Serialize};

use super::catalog::{self, CatalogWire, JsonConstraint, JsonIndex};
use super::convert::{props_from_cypher_json, props_to_cypher_json};
use super::{Envelope, SnapshotCodecKind};
use crate::snapshot::{object_exists, put_object, LoadedSnapshot};
use crate::{storage_context, StoreTarget};

pub const GRAPH_JSON: &str = "graph.json";

#[derive(Serialize, Deserialize)]
struct GraphData {
    #[serde(
        default,
        deserialize_with = "deserialize_value_encoding",
        skip_serializing_if = "Option::is_none"
    )]
    value_encoding: Option<u64>,
    nodes: Vec<JsonNode>,
    relationships: Vec<JsonRelationship>,
    #[serde(default)]
    indexes: Vec<JsonIndex>,
    #[serde(default)]
    constraints: Vec<JsonConstraint>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::manifest::deserialize_checkpoint_theater"
    )]
    theater: Option<CheckpointTheater>,
    next_node_id: i64,
    next_rel_id: i64,
    #[serde(default)]
    graph_type: Option<String>,
    #[serde(default)]
    read_only: bool,
    /// The inline ontology document (ADR-0120 §6). Written only at
    /// `value_encoding` 3; absent means the store holds no ontology.
    /// Opaque to storage: kept verbatim, validated elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ontology: Option<String>,
}

fn deserialize_value_encoding<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u64::deserialize(deserializer).map(Some)
}

#[derive(Serialize, Deserialize)]
struct JsonNode {
    id: i64,
    labels: Vec<String>,
    properties: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
struct JsonRelationship {
    id: i64,
    #[serde(rename = "type")]
    rel_type: String,
    start_node: i64,
    end_node: i64,
    properties: serde_json::Map<String, serde_json::Value>,
}

pub fn dump(
    target: &StoreTarget,
    graph: &dyn relate_engine_api::GraphImport,
    checkpoint_lsn: u64,
    snapshot_ts: u64,
    ontology: Option<&str>,
) -> Result<()> {
    let theater = graph.checkpoint_theater_for_write()?;
    let body = dump_to_vec(graph, snapshot_ts, theater, ontology)?;
    put_object(target, GRAPH_JSON, &body)?;
    let envelope = Envelope {
        codec: SnapshotCodecKind::Json,
        checkpoint_lsn,
        commit_ts: snapshot_ts,
        query_engine: relate_engine_api::MEMORY_QUERY_ENGINE.to_owned(),
        write_engine: relate_engine_api::LOCAL_WRITE_ENGINE.to_owned(),
    };
    let env_bytes =
        serde_json::to_vec_pretty(&envelope).map_err(|e| Error::storage(e.to_string()))?;
    put_object(target, crate::snapshot::MANIFEST_JSON, &env_bytes)?;
    // The shared checkpoint tail (ADR-0481 §4/§5): the mirror publishes
    // after the envelope it reflects, then the dirty tracking clears.
    crate::schema_files::finish_checkpoint(target, graph, ontology)?;
    Ok(())
}

fn dump_to_vec(
    graph: &dyn relate_engine_api::GraphImport,
    snapshot_ts: u64,
    theater: CheckpointTheater,
    ontology: Option<&str>,
) -> Result<Vec<u8>> {
    let data = graph_to_data(graph, snapshot_ts, theater, ontology)?;
    serde_json::to_vec(&data).map_err(|e| Error::storage(e.to_string()))
}

fn graph_to_data(
    graph: &dyn relate_engine_api::GraphImport,
    snapshot_ts: u64,
    theater: CheckpointTheater,
    ontology: Option<&str>,
) -> Result<GraphData> {
    let contents = super::model::SnapshotContents::from_graph(graph, snapshot_ts)?;
    // The graph type rides the one wire carrier (the catalog) rather
    // than being re-specified from `contents` beside it.
    let CatalogWire {
        indexes,
        constraints,
        graph_type,
    } = catalog::to_json(&contents, &theater);
    let mut nodes = Vec::new();
    for n in contents.nodes {
        let id = n.id;
        nodes.push(JsonNode {
            id,
            labels: std::sync::Arc::unwrap_or_clone(n.labels),
            properties: props_to_cypher_json(&n.properties)
                .map_err(|e| storage_context(e, format_args!("node {id}")))?,
        });
    }
    let mut relationships = Vec::new();
    for r in contents.relationships {
        let id = r.id;
        relationships.push(JsonRelationship {
            id,
            rel_type: r.rel_type,
            start_node: r.start,
            end_node: r.end,
            properties: props_to_cypher_json(&r.properties)
                .map_err(|e| storage_context(e, format_args!("relationship {id}")))?,
        });
    }
    Ok(GraphData {
        // The format follows the content (ADR-0120 §8): encoding 3 when the
        // inline ontology is present, 2 otherwise.
        value_encoding: Some(crate::version::value_encoding::version_for_write(
            ontology.is_some(),
        )),
        nodes,
        relationships,
        indexes,
        constraints,
        theater: Some(theater),
        next_node_id: contents.next_node_id,
        next_rel_id: contents.next_rel_id,
        graph_type,
        read_only: false,
        ontology: ontology.map(str::to_owned),
    })
}

#[cfg(test)]
pub fn load(target: &StoreTarget) -> Result<Graph> {
    Ok(load_with_checkpoint_lsn::<Graph>(target)?.graph)
}

/// Load the snapshot and the checkpoint LSN from the one envelope read,
/// reporting the decoded `value_encoding` marker (ADR-0127 Decision 2) and
/// the inline ontology document (ADR-0120 §6; `None` when the store holds
/// none). `graph.json` is read exactly once. The LSN is 0 when no manifest
/// exists yet.
pub(crate) fn load_with_checkpoint_lsn<G: relate_engine_api::SnapshotGraph>(
    target: &StoreTarget,
) -> Result<LoadedSnapshot<G>> {
    use crate::version::{value_encoding, SnapshotMarker, Subsystem};
    let (envelope_commit_ts, checkpoint_lsn) = envelope_meta(target)?;
    if !object_exists(target, GRAPH_JSON)? {
        return Ok(LoadedSnapshot::empty(checkpoint_lsn));
    }
    let bytes = crate::snapshot::get_object(target, GRAPH_JSON)?;
    let text = String::from_utf8(bytes).map_err(|e| Error::storage(e.to_string()))?;
    if text.trim().is_empty() {
        return Ok(LoadedSnapshot::empty(checkpoint_lsn));
    }
    let data: GraphData = serde_json::from_str(&text).map_err(|e| {
        let error = Error::storage(format!("failed to parse graph JSON: {e}"));
        // A legacy-store failure names its version, probed on the error
        // path only from bytes already in hand (ADR-0127 Decision 7).
        match probe_value_encoding(&text) {
            Some(version) => crate::version::annotate_legacy_failure(
                error,
                SnapshotMarker::Legacy(Subsystem::ValueEncoding, version),
            ),
            None => error,
        }
    })?;
    // The version marker gates everything else (ADR-0114 Decision 2); the
    // registry's per-subsystem gate normalizes the absent marker to
    // encoding 1, once, and rejects an unreadable store with the one
    // unsupported-version error (ADR-0127 Decision 2). The gate classifies
    // against the content the body carries (ADR-0120 §8), and the legacy
    // subsystem is named at the gate.
    let has_ontology = data.ontology.is_some();
    let (encoding, marker) = value_encoding::gate(data.value_encoding, has_ontology)?;
    // A legacy-store failure names the version where the loader knows it
    // (ADR-0127 Decision 4); a current store keeps its error verbatim.
    let (graph, ontology) = graph_from_data::<G>(data, encoding, envelope_commit_ts)
        .map_err(|error| crate::version::annotate_legacy_failure(error, marker))?;
    Ok(LoadedSnapshot {
        graph,
        checkpoint_lsn,
        ontology,
        marker,
        deferred: None,
    })
}

/// Error-path-only marker probe (ADR-0127 Decision 7): when the full
/// `GraphData` decode fails, the store's version is still named from the
/// bytes already in hand. Never called on the success path, so a
/// current-format open never parses `graph.json` twice.
#[derive(Deserialize)]
struct ValueEncodingProbe {
    #[serde(default)]
    value_encoding: Option<u64>,
}

fn probe_value_encoding(text: &str) -> Option<u64> {
    let probe: ValueEncodingProbe = serde_json::from_str(text).ok()?;
    // The probe classifies without the content flag: an unreadable marker
    // is unreadable either way, and a content-mismatched readable marker is
    // still worth naming on this error path.
    match crate::version::value_encoding::classify(probe.value_encoding, false) {
        crate::version::Classification::Legacy(version) => Some(version),
        _ => None,
    }
}

pub(crate) fn envelope_meta(target: &StoreTarget) -> Result<(Option<u64>, u64)> {
    let Some(env) = crate::codec::read_envelope(target)? else {
        return Ok((None, 0));
    };
    env.envelope_meta()
}

/// Decode into the in-memory graph, returning the inline ontology document
/// with it (ADR-0120 §6). `value_encoding` is the marker the caller
/// already classified and normalized (absent → 1) via the registry.
fn graph_from_data<G: relate_engine_api::SnapshotGraph>(
    data: GraphData,
    value_encoding: u64,
    envelope_commit_ts: Option<u64>,
) -> Result<(G, Option<String>)> {
    let ontology = data.ontology;
    if data.read_only {
        return Err(Error::storage("read-only snapshot not supported"));
    }
    // Rows decode with the gated encoding, then the shared rebuild tail
    // (`SnapshotRows::into_graph`) installs them — the same tail every
    // codec that decodes into contents uses. Id checks and the next-id
    // guard live in that tail.
    let mut nodes = Vec::with_capacity(data.nodes.len());
    for n in data.nodes {
        let properties = props_from_cypher_json(&n.properties, value_encoding)
            .map_err(|e| storage_context(e, format_args!("node {}", n.id)))?;
        nodes.push(Node {
            id: n.id,
            labels: std::sync::Arc::new(n.labels),
            properties: relate_core::PropertyMap::share(properties),
        });
    }
    let mut relationships = Vec::with_capacity(data.relationships.len());
    for r in data.relationships {
        let properties = props_from_cypher_json(&r.properties, value_encoding)
            .map_err(|e| storage_context(e, format_args!("relationship {}", r.id)))?;
        relationships.push(Relationship {
            id: r.id,
            start: r.start_node,
            end: r.end_node,
            rel_type: r.rel_type,
            properties: relate_core::PropertyMap::share(properties),
        });
    }
    let rows = super::model::SnapshotRows {
        nodes,
        relationships,
        next_node_id: data.next_node_id,
        next_rel_id: data.next_rel_id,
    };
    let graph = rows.into_graph::<G>(
        CatalogWire {
            indexes: data.indexes,
            constraints: data.constraints,
            graph_type: data.graph_type,
        },
        envelope_commit_ts,
        data.theater,
    )?;
    Ok((graph, ontology))
}

pub fn snapshot_data_bytes(target: &StoreTarget) -> Result<u64> {
    // The one size-accounting policy over this codec's objects (see
    // `snapshot::sum_object_bytes`).
    crate::snapshot::sum_object_bytes(target, &[crate::snapshot::MANIFEST_JSON, GRAPH_JSON])
}

#[cfg(test)]
#[path = "json/tests.rs"]
mod decoding_tests;
