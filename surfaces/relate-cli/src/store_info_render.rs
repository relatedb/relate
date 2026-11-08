//! The one projection of a cold `store_info` read (ADR-0165) that every
//! human and machine surface renders from: the `relate info` key/value
//! table, `info --json`, and the `.info` store block. `from_store_info`
//! owns the layout × snapshot split, the unavailable policy for
//! envelope-codec stores (JSON, ADR-0165 Decision 5; Markdown, ADR-0406)
//! with their trailing notes, and the single label set, so the three
//! renderers stay dumb and cannot drift. Ungated: `.info` compiles in
//! every build, so this module does too; only the renderers exclusive
//! to the `info` command (`catalog_lines`, `to_json`) are feature-gated.

use relate_catalog::{ConstraintDef, IndexDef};
#[cfg(feature = "info")]
use relate_formatter::sanitize_error_text;
use relate_storage::{SnapshotCodecKind, StoreInfo, StoreLayout, StoreSnapshot};

/// The marker for fields a cold read cannot recover from an
/// envelope-codec store (JSON, ADR-0165 Decision 5; Markdown, ADR-0406):
/// the human readout prints the word, the JSON object emits `null`.
pub(crate) const UNAVAILABLE: &str = "unavailable";

/// Why a JSON-codec store reports envelope-only fields; selected from
/// the codec at the one construction boundary (`from_store_info`) and
/// printed after the store block on every human surface.
const NOTE_JSON_CODEC: &str = "a JSON-codec store keeps the format version, counts and the index/constraint catalog in graph.json, which a cold info read never parses (ADR-0165)";

/// Why a markdown-codec store reports envelope-only fields; selected
/// from the codec at the one construction boundary (`from_store_info`)
/// and printed after the store block on every human surface.
const NOTE_MARKDOWN_CODEC: &str = "a markdown-codec store keeps the format version, counts and the index/constraint catalog in the .cypherlite sidecar manifest, which a cold info read never uses to walk the vault (ADR-0406)";

/// Why an experimental-codec store (rkyv, fjall) reports envelope-only
/// fields; one note for both — their images differ in container but not
/// in what a cold read skips.
const NOTE_EXPERIMENTAL_CODEC: &str = "an experimental-codec store keeps the format version, counts and the index/constraint catalog in its checkpoint image and payload, which a cold info read never parses";

/// A cold `store_info` read projected into display-ready fields. The
/// snapshot half is the typed [`SnapshotInfo`]: each variant carries exactly
/// the fields that snapshot shape can report, so a field that does not
/// exist for a shape (the catalog of a JSON-codec store, anything at all
/// for a fresh store) cannot be read — and a field transpose cannot
/// compile.
pub struct InfoReport {
    layout: StoreLayout,
    superblock_version: Option<u32>,
    snapshot: SnapshotInfo,
    total_bytes: u64,
    wal_segments: u32,
}

/// The snapshot section of a cold read. A fresh store before its first
/// checkpoint (`StoreSnapshot::None`) has no snapshot fields at all; an
/// envelope-only codec store (JSON or Markdown) carries only the codec
/// envelope, so the format version, counts and the catalog do not exist
/// for it (ADR-0165 Decision 5); a
/// Parquet manifest carries the full checkpoint record.
// `counts`/`indexes`/`constraints` are read only by the `info`-gated
// renderers (`catalog_lines`, `to_json`); in other builds they are carried
// but unread.
#[cfg_attr(not(feature = "info"), allow(dead_code))]
enum SnapshotInfo {
    /// A fresh store before its first checkpoint: a state, not an error.
    Fresh,
    /// An envelope-only codec checkpoint (JSON, ADR-0050, or Markdown,
    /// ADR-0406): the envelope only, with the codec saying which and the
    /// trailing note its renderers print. `from_store_info` refuses a
    /// Parquet envelope at the construction boundary — a Parquet store
    /// carries the full manifest, never an envelope — so this variant's
    /// codec is always an envelope codec.
    Envelope {
        codec: SnapshotCodecKind,
        checkpoint_lsn: u64,
        commit_ts: u64,
        /// The codec's trailing note, selected with the codec at the one
        /// construction boundary (`from_store_info`).
        note: &'static str,
    },
    /// A Parquet checkpoint: the full manifest. The variant is the codec
    /// — a Parquet manifest never carries another codec's envelope.
    Parquet {
        /// Manifest format version (a Parquet-side concept).
        format_version: u32,
        checkpoint_lsn: u64,
        commit_ts: u64,
        /// Checkpoint-time `(node_count, rel_count)`.
        counts: (u64, u64),
        /// The checkpoint-time catalog.
        indexes: Vec<IndexDef>,
        constraints: Vec<ConstraintDef>,
    },
}

impl SnapshotInfo {
    /// The snapshot's codec, from the one variant→codec mapping: an
    /// envelope's codec is its own field, a Parquet manifest's codec is
    /// its variant, and a fresh store has none (`None`). Every renderer
    /// consults this accessor; none re-spells the mapping.
    fn codec(&self) -> Option<SnapshotCodecKind> {
        match self {
            SnapshotInfo::Fresh => None,
            SnapshotInfo::Envelope { codec, .. } => Some(*codec),
            SnapshotInfo::Parquet { .. } => Some(SnapshotCodecKind::Parquet),
        }
    }
}

/// The rows every checkpoint-carrying snapshot shape shares — codec,
/// format version (its value is the shape's own), checkpoint LSN, and
/// commit timestamp — spelled once for both arms.
fn checkpoint_rows(
    codec: SnapshotCodecKind,
    format_version: String,
    checkpoint_lsn: u64,
    commit_ts: u64,
) -> Vec<(String, String)> {
    vec![
        ("codec".to_string(), codec.as_str().to_string()),
        ("format version".to_string(), format_version),
        ("checkpoint LSN".to_string(), checkpoint_lsn.to_string()),
        ("commit timestamp".to_string(), commit_ts.to_string()),
    ]
}

impl InfoReport {
    /// The one layout × snapshot split. Every surface renders the result;
    /// none re-branches on `StoreInfo`.
    pub fn from_store_info(info: &StoreInfo) -> Self {
        let superblock_version = match info.layout {
            StoreLayout::NativeDir => None,
            StoreLayout::Bundle { superblock_version } => Some(superblock_version),
        };
        let snapshot = match &info.snapshot {
            StoreSnapshot::Parquet(manifest) => {
                let nodes: u64 = manifest.node_parts.iter().map(|p| p.rows).sum();
                let rels: u64 = manifest.rel_parts.iter().map(|p| p.rows).sum();
                SnapshotInfo::Parquet {
                    format_version: manifest.format_version,
                    checkpoint_lsn: manifest.checkpoint_lsn,
                    commit_ts: manifest.commit_ts,
                    counts: (nodes, rels),
                    indexes: manifest.indexes.clone(),
                    constraints: manifest.constraints.clone(),
                }
            }
            StoreSnapshot::Envelope {
                codec,
                checkpoint_lsn,
                commit_ts,
            } => {
                // The envelope projection's one construction boundary:
                // the codec selects the trailing note its renderers
                // print, and a Parquet envelope is the never-written
                // state — a Parquet store carries the full manifest
                // (`StoreSnapshot::Parquet`), never an envelope —
                // refused loudly here, so the illegal shape cannot be
                // constructed and every envelope codec prints its own
                // note.
                let note = match codec {
                    SnapshotCodecKind::Json => NOTE_JSON_CODEC,
                    SnapshotCodecKind::Markdown => NOTE_MARKDOWN_CODEC,
                    SnapshotCodecKind::Rkyv | SnapshotCodecKind::Fjall => {
                        NOTE_EXPERIMENTAL_CODEC
                    }
                    SnapshotCodecKind::Parquet => unreachable!(
                        "an envelope snapshot is never parquet: a parquet store carries the full manifest"
                    ),
                };
                SnapshotInfo::Envelope {
                    codec: *codec,
                    checkpoint_lsn: *checkpoint_lsn,
                    commit_ts: *commit_ts,
                    note,
                }
            }
            StoreSnapshot::None => SnapshotInfo::Fresh,
        };
        InfoReport {
            layout: info.layout,
            superblock_version,
            snapshot,
            total_bytes: info.total_bytes,
            wal_segments: info.wal_segments,
        }
    }

    /// The store-metadata block every human surface prints, in canonical
    /// order, as `(key, value)` pairs (keys carry no colon). The renderer
    /// decides the key/value separator and alignment; the wording is fixed
    /// here. An envelope-codec store ends the block with the `note` line.
    pub fn store_lines(&self) -> Vec<(String, String)> {
        let mut lines = vec![("layout".to_string(), self.layout_label().to_string())];
        if let Some(version) = self.superblock_version {
            lines.push(("superblock version".to_string(), version.to_string()));
        }
        // One dispatch over the snapshot shape: each arm pushes its fields
        // and yields the trailing note its shape needs (only an
        // envelope-only codec carries one, selected with its codec at the
        // construction boundary), so the note still sorts after the
        // totals pushed below without a second dispatch.
        let note = match &self.snapshot {
            SnapshotInfo::Fresh => {
                // A fresh store before its first checkpoint: a state, not an
                // error. Layout, size and WAL segments still report.
                lines.push((
                    "snapshot".to_string(),
                    "none yet (no checkpoint since creation)".to_string(),
                ));
                None
            }
            SnapshotInfo::Envelope {
                codec,
                checkpoint_lsn,
                commit_ts,
                note,
            } => {
                lines.extend(checkpoint_rows(
                    *codec,
                    UNAVAILABLE.to_string(),
                    *checkpoint_lsn,
                    *commit_ts,
                ));
                Some(*note)
            }
            SnapshotInfo::Parquet {
                format_version,
                checkpoint_lsn,
                commit_ts,
                ..
            } => {
                // The variant's codec from the one accessor
                // (`SnapshotInfo::codec`), never re-spelled here.
                lines.extend(checkpoint_rows(
                    self.snapshot
                        .codec()
                        .expect("a parquet snapshot's codec is parquet"),
                    format_version.to_string(),
                    *checkpoint_lsn,
                    *commit_ts,
                ));
                None
            }
        };
        lines.push(("total bytes".to_string(), self.total_bytes.to_string()));
        lines.push((
            "wal segments past checkpoint".to_string(),
            self.wal_segments.to_string(),
        ));
        if let Some(note) = note {
            lines.push(("note".to_string(), note.to_string()));
        }
        lines
    }

    /// The checkpoint-time counts and catalog lines `relate info`
    /// prints after the store block (plain display lines; catalog rows are
    /// indented). Empty for a fresh store; the unavailable markers for an
    /// envelope-codec store. `.info` does not print these — its counts and
    /// catalog come live from the open session (ADR-0165 Decision 4).
    #[cfg(feature = "info")]
    pub fn catalog_lines(&self) -> Vec<String> {
        match &self.snapshot {
            SnapshotInfo::Fresh => Vec::new(),
            SnapshotInfo::Envelope { .. } => vec![
                format!("nodes: {UNAVAILABLE}"),
                format!("relationships: {UNAVAILABLE}"),
                format!("indexes: {UNAVAILABLE}"),
                format!("constraints: {UNAVAILABLE}"),
            ],
            SnapshotInfo::Parquet {
                checkpoint_lsn: lsn,
                counts: (nodes, rels),
                indexes,
                constraints,
                ..
            } => {
                // Counts are checkpoint-time sums of `ShardPart.rows`, labeled with
                // the checkpoint LSN so staleness is explicit (ADR-0165 Decision 3).
                let mut lines = vec![
                    format!("nodes (at checkpoint LSN {lsn}): {nodes}"),
                    format!("relationships (at checkpoint LSN {lsn}): {rels}"),
                ];
                lines.push(format!("indexes: {}", indexes.len()));
                for index in indexes {
                    // The index's `:Label(property)` schema text through
                    // the catalog's one spelling (`IndexDef::index_text`):
                    // the bare property, never the legacy
                    // `{property}#{KIND}#{name}` storage encoding a
                    // constraint-backed index carries — the manifest names
                    // the schema, not the encoding.
                    lines.push(format!(
                        "  {}: {}",
                        sanitize_error_text(&index.name),
                        sanitize_error_text(&index.index_text())
                    ));
                }
                lines.push(format!("constraints: {}", constraints.len()));
                for constraint in constraints {
                    lines.push(format!(
                        "  {}: {}",
                        sanitize_error_text(&constraint.name),
                        describe_constraint(constraint)
                    ));
                }
                lines
            }
        }
    }

    /// One JSON object with a fixed key set (snake_case; `null` for
    /// unavailable), the envelope-only contract of ADR-0165 Decision 5 —
    /// one literal, so the snapshot arms cannot drift.
    #[cfg(feature = "info")]
    pub fn to_json(&self) -> serde_json::Value {
        let layout = match self.layout {
            StoreLayout::NativeDir => "native_dir",
            StoreLayout::Bundle { .. } => "bundle",
        };
        // The snapshot-derived values of the fixed key set, as named fields
        // (not a positional tuple) so a transpose cannot compile.
        let SnapshotJson {
            codec,
            format_version,
            checkpoint_lsn,
            commit_ts,
            counts,
            indexes,
            constraints,
        } = SnapshotJson::from(&self.snapshot);
        serde_json::json!({
            "layout": layout,
            "superblock_version": self.superblock_version,
            "codec": codec,
            "format_version": format_version,
            "checkpoint_lsn": checkpoint_lsn,
            "commit_ts": commit_ts,
            "total_bytes": self.total_bytes,
            "wal_segments": self.wal_segments,
            "node_count": counts.map(|(nodes, _)| nodes),
            "rel_count": counts.map(|(_, rels)| rels),
            "indexes": indexes.map(|indexes| indexes.iter().map(|index| {
                serde_json::json!({
                    "name": index.name,
                    "label": index.label,
                    "property": index.property,
                })
            }).collect::<Vec<_>>()),
            "constraints": constraints.map(|constraints| constraints.iter().map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "kind": c.kind,
                    "label": c.label,
                    "relationship": c.relationship,
                    "properties": c.properties,
                    "property_type": c.property_type,
                    "enforced_label": c.enforced_label,
                })
            }).collect::<Vec<_>>()),
        })
    }

    /// The human-table layout label.
    fn layout_label(&self) -> &'static str {
        match self.layout {
            StoreLayout::NativeDir => "native directory",
            StoreLayout::Bundle { .. } => ".rl bundle",
        }
    }
}

/// The snapshot-derived values of the fixed JSON key set: `None` where the
/// snapshot shape cannot provide the field (the JSON object emits `null`).
#[cfg(feature = "info")]
#[derive(Default)]
struct SnapshotJson<'a> {
    codec: Option<&'static str>,
    format_version: Option<u32>,
    checkpoint_lsn: Option<u64>,
    commit_ts: Option<u64>,
    counts: Option<(u64, u64)>,
    indexes: Option<&'a [IndexDef]>,
    constraints: Option<&'a [ConstraintDef]>,
}

#[cfg(feature = "info")]
impl<'a> From<&'a SnapshotInfo> for SnapshotJson<'a> {
    fn from(snapshot: &'a SnapshotInfo) -> Self {
        // The codec from the one accessor (`SnapshotInfo::codec`), never
        // re-spelled per arm: an envelope's codec is its field, a Parquet
        // manifest's is the variant.
        let codec = snapshot.codec().map(SnapshotCodecKind::as_str);
        match snapshot {
            SnapshotInfo::Fresh => SnapshotJson::default(),
            SnapshotInfo::Envelope {
                checkpoint_lsn,
                commit_ts,
                ..
            } => SnapshotJson {
                codec,
                checkpoint_lsn: Some(*checkpoint_lsn),
                commit_ts: Some(*commit_ts),
                ..SnapshotJson::default()
            },
            SnapshotInfo::Parquet {
                format_version,
                checkpoint_lsn,
                commit_ts,
                counts,
                indexes,
                constraints,
            } => SnapshotJson {
                codec,
                format_version: Some(*format_version),
                checkpoint_lsn: Some(*checkpoint_lsn),
                commit_ts: Some(*commit_ts),
                counts: Some(*counts),
                indexes: Some(indexes.as_slice()),
                constraints: Some(constraints.as_slice()),
            },
        }
    }
}

/// The constraint schema text, sanitized for the manifest line
/// ([`ConstraintDef::schema_text`] owns the spelling, kinds included).
#[cfg(feature = "info")]
fn describe_constraint(constraint: &ConstraintDef) -> String {
    sanitize_error_text(&constraint.schema_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_storage::{Manifest, SnapshotCodecKind, StoreInfo, StoreLayout, StoreSnapshot};

    fn manifest() -> Manifest {
        serde_json::from_str(
            r#"{
                "format_version": 3,
                "next_node_id": 4,
                "next_rel_id": 3,
                "checkpoint_lsn": 7,
                "commit_ts": 1759651200,
                "codec": "parquet",
                "node_parts": [{"path": "nodes/0.parquet", "id_lo": 0, "id_hi": 3, "rows": 3, "bytes": 100}],
                "rel_parts": [{"path": "relationships/0.parquet", "id_lo": 0, "id_hi": 2, "rows": 2, "bytes": 80}],
                "indexes": [{"name": "person_name", "label": "Person", "property": "name"}],
                "constraints": [{"name": "person_id_unique", "kind": "unique", "label": "Person", "properties": ["id"]}]
            }"#,
        )
        .expect("manifest fixture parses")
    }

    fn info(snapshot: StoreSnapshot) -> StoreInfo {
        StoreInfo {
            layout: StoreLayout::NativeDir,
            snapshot,
            total_bytes: 4163,
            wal_segments: 1,
        }
    }

    #[test]
    fn parquet_store_lines_carry_the_full_manifest() {
        let report = InfoReport::from_store_info(&info(StoreSnapshot::Parquet(manifest())));
        assert_eq!(
            report.store_lines(),
            vec![
                ("layout".to_string(), "native directory".to_string()),
                ("codec".to_string(), "parquet".to_string()),
                ("format version".to_string(), "3".to_string()),
                ("checkpoint LSN".to_string(), "7".to_string()),
                ("commit timestamp".to_string(), "1759651200".to_string()),
                ("total bytes".to_string(), "4163".to_string()),
                ("wal segments past checkpoint".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn bundle_store_lines_report_the_superblock_version() {
        let mut store = info(StoreSnapshot::Envelope {
            codec: SnapshotCodecKind::Json,
            checkpoint_lsn: 7,
            commit_ts: 42,
        });
        store.layout = StoreLayout::Bundle {
            superblock_version: 2,
        };
        let report = InfoReport::from_store_info(&store);
        let lines = report.store_lines();
        assert_eq!(lines[0], ("layout".to_string(), ".rl bundle".to_string()));
        assert_eq!(
            lines[1],
            ("superblock version".to_string(), "2".to_string())
        );
    }

    /// The envelope row shape, written once: the codec alone selects
    /// the trailing note and the codec row (the arm is codec-generic).
    fn assert_envelope_store_lines(codec: SnapshotCodecKind, note: &'static str) {
        let report = InfoReport::from_store_info(&info(StoreSnapshot::Envelope {
            codec,
            checkpoint_lsn: 7,
            commit_ts: 42,
        }));
        assert_eq!(
            report.store_lines(),
            vec![
                ("layout".to_string(), "native directory".to_string()),
                ("codec".to_string(), codec.as_str().to_string()),
                ("format version".to_string(), UNAVAILABLE.to_string()),
                ("checkpoint LSN".to_string(), "7".to_string()),
                ("commit timestamp".to_string(), "42".to_string()),
                ("total bytes".to_string(), "4163".to_string()),
                ("wal segments past checkpoint".to_string(), "1".to_string()),
                ("note".to_string(), note.to_string()),
            ]
        );
    }

    #[test]
    fn json_codec_store_lines_apply_the_unavailable_policy() {
        assert_envelope_store_lines(SnapshotCodecKind::Json, NOTE_JSON_CODEC);
    }

    #[test]
    fn markdown_codec_store_lines_select_the_markdown_note() {
        assert_envelope_store_lines(SnapshotCodecKind::Markdown, NOTE_MARKDOWN_CODEC);
    }

    #[test]
    fn fresh_store_lines_report_the_state_not_an_error() {
        let report = InfoReport::from_store_info(&info(StoreSnapshot::None));
        assert_eq!(
            report.store_lines(),
            vec![
                ("layout".to_string(), "native directory".to_string()),
                (
                    "snapshot".to_string(),
                    "none yet (no checkpoint since creation)".to_string()
                ),
                ("total bytes".to_string(), "4163".to_string()),
                ("wal segments past checkpoint".to_string(), "1".to_string()),
            ]
        );
    }

    #[cfg(feature = "info")]
    #[test]
    fn parquet_catalog_lines_label_counts_with_the_checkpoint_lsn() {
        let report = InfoReport::from_store_info(&info(StoreSnapshot::Parquet(manifest())));
        assert_eq!(
            report.catalog_lines(),
            vec![
                "nodes (at checkpoint LSN 7): 3".to_string(),
                "relationships (at checkpoint LSN 7): 2".to_string(),
                "indexes: 1".to_string(),
                "  person_name: :Person(name)".to_string(),
                "constraints: 1".to_string(),
                "  person_id_unique: unique node :Person(id)".to_string(),
            ]
        );
    }

    #[cfg(feature = "info")]
    #[test]
    fn json_codec_catalog_lines_are_all_unavailable() {
        let report = InfoReport::from_store_info(&info(StoreSnapshot::Envelope {
            codec: SnapshotCodecKind::Json,
            checkpoint_lsn: 7,
            commit_ts: 42,
        }));
        assert_eq!(
            report.catalog_lines(),
            vec![
                format!("nodes: {UNAVAILABLE}"),
                format!("relationships: {UNAVAILABLE}"),
                format!("indexes: {UNAVAILABLE}"),
                format!("constraints: {UNAVAILABLE}"),
            ]
        );
        assert!(
            InfoReport::from_store_info(&info(StoreSnapshot::None))
                .catalog_lines()
                .is_empty(),
            "a fresh store has no catalog lines"
        );
    }

    #[cfg(feature = "info")]
    #[test]
    fn json_object_has_a_fixed_key_set_with_nulls_for_unavailable() {
        // serde_json's default map orders keys alphabetically; the contract
        // is the fixed key SET, not the order.
        let mut keys = [
            "layout",
            "superblock_version",
            "codec",
            "format_version",
            "checkpoint_lsn",
            "commit_ts",
            "total_bytes",
            "wal_segments",
            "node_count",
            "rel_count",
            "indexes",
            "constraints",
        ];
        keys.sort_unstable();
        for snapshot in [
            StoreSnapshot::Parquet(manifest()),
            StoreSnapshot::Envelope {
                codec: SnapshotCodecKind::Json,
                checkpoint_lsn: 7,
                commit_ts: 42,
            },
            StoreSnapshot::None,
        ] {
            let report = InfoReport::from_store_info(&info(snapshot));
            let value = report.to_json();
            let object = value.as_object().expect("one JSON object");
            assert_eq!(
                object.keys().cloned().collect::<Vec<_>>(),
                keys.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
                "every snapshot shape emits the same fixed key set"
            );
        }
        let json_codec = InfoReport::from_store_info(&info(StoreSnapshot::Envelope {
            codec: SnapshotCodecKind::Json,
            checkpoint_lsn: 7,
            commit_ts: 42,
        }))
        .to_json();
        assert_eq!(json_codec["codec"], "json");
        assert_eq!(json_codec["format_version"], serde_json::Value::Null);
        assert_eq!(json_codec["node_count"], serde_json::Value::Null);
        assert_eq!(json_codec["indexes"], serde_json::Value::Null);
        let fresh = InfoReport::from_store_info(&info(StoreSnapshot::None)).to_json();
        assert_eq!(fresh["codec"], serde_json::Value::Null);
        assert_eq!(fresh["checkpoint_lsn"], serde_json::Value::Null);
        assert_eq!(fresh["total_bytes"], 4163);
        let parquet =
            InfoReport::from_store_info(&info(StoreSnapshot::Parquet(manifest()))).to_json();
        assert_eq!(parquet["codec"], "parquet");
        assert_eq!(parquet["node_count"], 3);
        assert_eq!(parquet["rel_count"], 2);
        assert_eq!(parquet["commit_ts"], 1759651200);
        assert_eq!(parquet["superblock_version"], serde_json::Value::Null);
    }

    #[cfg(feature = "info")]
    #[test]
    fn describe_constraint_renders_the_property_type_suffix() {
        let constraint: ConstraintDef = serde_json::from_str(
            r#"{"name": "c", "kind": "propertyType", "label": "Person",
                "properties": ["id"], "property_type": "INTEGER"}"#,
        )
        .expect("constraint fixture parses");
        assert_eq!(
            describe_constraint(&constraint),
            "propertyType node :Person(id) :: INTEGER"
        );
        let rel: ConstraintDef = serde_json::from_str(
            r#"{"name": "r", "kind": "exists", "label": "KNOWS",
                "relationship": true, "properties": ["since"]}"#,
        )
        .expect("constraint fixture parses");
        assert_eq!(
            describe_constraint(&rel),
            "exists relationship :KNOWS(since)"
        );
        let enforced: ConstraintDef = serde_json::from_str(
            r#"{"name": "e", "kind": "nodeLabelExistence", "label": "Person",
                "properties": [], "enforced_label": "Verified"}"#,
        )
        .expect("constraint fixture parses");
        assert_eq!(
            describe_constraint(&enforced),
            "nodeLabelExistence node :Person() => :Verified"
        );
    }

    #[cfg(feature = "info")]
    #[test]
    fn json_constraint_object_carries_the_enforced_label_field() {
        let parquet =
            InfoReport::from_store_info(&info(StoreSnapshot::Parquet(manifest()))).to_json();
        let mut keys: Vec<&str> = parquet["constraints"][0]
            .as_object()
            .expect("one constraint object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "enforced_label",
                "kind",
                "label",
                "name",
                "properties",
                "property_type",
                "relationship"
            ]
        );
        assert_eq!(
            parquet["constraints"][0]["enforced_label"],
            serde_json::Value::Null,
            "an absent enforced label maps to null like the other unavailable fields"
        );
    }
}
