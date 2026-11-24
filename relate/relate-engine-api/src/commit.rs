//! One durable commit. The JSON field names match the historical log record,
//! so a file written before this type moved still decodes.
//!
//! ADR-0610 adds three optional provenance-tail fields — `metadata` (the
//! transaction's merged metadata map), `changed_keys` (the commit's
//! distinct property-key changes) and `type` (the record's family,
//! [`RecordType`]) — written only while the `value-provenance` preview
//! feature is on. All skip-serialize on the WAL record: a record without
//! them encodes byte-identically to the pre-feature form, and an older
//! reader decodes a record with them unchanged (serde ignores the
//! unknown keys), so the fields are skip-compatible in both directions.
//! The retained form's family is typed ([`RecordType`]), an absent
//! `type` field decoding as the provenance family's own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use relate_core::{Error, Result, Value};

use crate::export::GraphExport;
use crate::redo::RedoOp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRecord {
    pub lsn: u64,
    pub commit_ts: u64,
    pub ops: Vec<RedoOp>,
    /// The transaction's merged metadata map (ADR-0610): the input
    /// channels' per-transaction map, tagged-value encoded like the redo
    /// property maps. `None` while the feature is off or the map is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
    /// The commit's distinct property-key changes (ADR-0610), derived once
    /// at commit by [`derive_changed_keys`] — only for the provenance
    /// family's own commits (another type's record derives none). `None`
    /// while the feature is off or the commit changed no property keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_keys: Option<Vec<ChangedKey>>,
    /// The record's family type (ADR-0610): `"provenance"` — the family's
    /// own, the value every untyped channel hard-sets — or the typed
    /// channel's kebab-case name. Spelled only on a record that carries a
    /// tail at all; a record without the field decodes as the provenance
    /// family's ([`RecordType::default`]), so pre-typed records
    /// and old family parts read as provenance.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub record_type: Option<String>,
}

/// The record's serialized shape, spelled once beside the type whose field
/// names it mirrors. The WAL's encode-once path (ADR-0558) writes the header
/// by hand around an already-serialized `ops` array; these are the
/// spellings it consults, and the test below pins them to what serde emits.
impl CommitRecord {
    /// The opening brace and the `lsn` key.
    pub const LSN_KEY: &'static str = "{\"lsn\":";
    /// The separator and the `commit_ts` key.
    pub const COMMIT_TS_KEY: &'static str = ",\"commit_ts\":";
    /// The separator and the `ops` key; the array follows.
    pub const OPS_KEY: &'static str = ",\"ops\":";
    /// The separator and the `metadata` key; the map follows. Written only
    /// by the encode-once tail, after the `ops` array (ADR-0610).
    pub const METADATA_KEY: &'static str = ",\"metadata\":";
    /// The separator and the `changed_keys` key; the array follows.
    /// Written only by the encode-once tail, after `metadata` (ADR-0610).
    pub const CHANGED_KEYS_KEY: &'static str = ",\"changed_keys\":";
    /// The separator and the `type` key; the string follows. Written only
    /// by the encode-once tail, after `changed_keys` (ADR-0610).
    pub const TYPE_KEY: &'static str = ",\"type\":";
    /// Decimal digits of `u64::MAX`: the widest `lsn` or `commit_ts`.
    pub const U64_DECIMAL_MAX: usize = 20;
    /// The widest header the record can carry: the three key spellings and
    /// two maximum-width unsigned decimals.
    pub const HEADER_MAX: usize = Self::LSN_KEY.len()
        + Self::COMMIT_TS_KEY.len()
        + Self::OPS_KEY.len()
        + 2 * Self::U64_DECIMAL_MAX;
}

/// One property-key change a commit made (ADR-0610): the entity kind, the
/// element's id, and the property key. Sorting ([`Ord`], the derived field
/// order) is the record's canonical `(kind, id, key)` order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChangedKey {
    pub kind: ChangedKeyKind,
    pub id: i64,
    pub key: String,
}

/// Which element kind a [`ChangedKey`] belongs to. The declaration order is
/// the sort order (`node` before `rel`, the ADR's canonical order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangedKeyKind {
    Node,
    Rel,
}

/// The retained form of one [`CommitRecord`] (ADR-0610): the record minus
/// its ops — the ops stay in the WAL, and the retained-record table and the
/// checkpoint's `transactions/` objects carry only what the audit surface
/// reads (`lsn`, `commit_ts`, the optional `metadata`, `changed_keys` and
/// `record_type`). ADR-0610's `db.transactions` and provenance queries
/// read this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub lsn: u64,
    pub commit_ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_keys: Option<Vec<ChangedKey>>,
    /// The record's family (ADR-0610), typed once: the stored name, or
    /// the provenance family's own when the field is absent — a
    /// tail-less legacy record and an old family part both read as
    /// provenance, which is what every record written before typed
    /// records was. The serialized spelling is [`RecordType::as_str`].
    #[serde(default, rename = "type")]
    pub record_type: RecordType,
}

/// The provenance family's own record type (ADR-0610): `"provenance"`, the
/// name every untyped metadata channel hard-sets, the default a record
/// without the field decodes as, and the one reserved name a typed seed
/// may not claim ([`validate_record_type`]).
pub const PROVENANCE_RECORD_TYPE: &str = "provenance";

/// The record's family (ADR-0610), modelled once: the provenance
/// family's own — the default a record without the `type` field decodes
/// as ([`Self::default`]), the family every untyped channel seeds — or
/// the typed channel's name. The `type` string is the serialized form
/// ([`Self::as_str`]/[`Self::of`]); the in-memory shape is the variant,
/// so `is_provenance` is a variant test and no consumer re-derives the
/// absent-field rule. A [`RecordType::Typed`] name is
/// [`validate_record_type`]-validated where a channel applies it, never
/// by the type itself (a decoded record's stored name is trusted as the
/// bytes' own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordType {
    /// The provenance family's own record.
    Provenance,
    /// Another family's record, named by the typed channel.
    Typed(String),
}

impl Default for RecordType {
    /// The family a record without the `type` field decodes as — the
    /// one home of the absent-field rule: the provenance family's own,
    /// which is what every record written before typed records was.
    fn default() -> Self {
        Self::Provenance
    }
}

impl RecordType {
    /// The family a stored `type` string names — the serialized form's
    /// one decode, consulted by the serde impls: the provenance
    /// family's own name is [`Self::Provenance`] and any other name is
    /// [`Self::Typed`].
    pub fn of(name: &str) -> Self {
        if name == PROVENANCE_RECORD_TYPE {
            Self::Provenance
        } else {
            Self::Typed(name.to_owned())
        }
    }

    /// The serialized `type` spelling — the provenance family's own name
    /// or the typed channel's.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Provenance => PROVENANCE_RECORD_TYPE,
            Self::Typed(name) => name,
        }
    }

    /// Whether this is the provenance family's own record — the one
    /// filter the provenance queries' join applies.
    pub fn is_provenance(&self) -> bool {
        matches!(self, Self::Provenance)
    }
}

impl Serialize for RecordType {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RecordType {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(Self::of(&String::deserialize(deserializer)?))
    }
}

impl TransactionRecord {
    /// The record's family (ADR-0610), typed once: the stored name, or
    /// the provenance family's own when the field was absent — a
    /// tail-less legacy record and an old family part both read as
    /// provenance, which is what every record written before typed
    /// records was. The serialized spelling is
    /// [`RecordType::as_str`].
    pub fn record_type(&self) -> &RecordType {
        &self.record_type
    }

    /// Whether this record belongs to the provenance family (ADR-0610) —
    /// the one filter the provenance queries' join applies: another
    /// family's commit retained a record, but its property writes join no
    /// `provenance()`/`history()` event.
    pub fn is_provenance(&self) -> bool {
        self.record_type.is_provenance()
    }

    /// Whether this record retains (ADR-0610): a typed record of any
    /// family, or one carrying provenance fields — the one predicate every
    /// retention site consults. Retention retains every typed record; a
    /// provenance record with neither field — the only form a store that
    /// never opted in produces — joins no table.
    pub fn is_retained(&self) -> bool {
        match self.record_type {
            RecordType::Typed(_) => true,
            RecordType::Provenance => self.metadata.is_some() || self.changed_keys.is_some(),
        }
    }
}

impl CommitRecord {
    /// The retained-record projection of this record (ADR-0610) — the one
    /// conversion, so the WAL record and the retained form cannot drift.
    /// The serialized `type` string decodes into the typed family here
    /// ([`RecordType::of`]); the tail-less form's is the absent-field
    /// default ([`RecordType::default`]).
    pub fn retained(&self) -> TransactionRecord {
        TransactionRecord {
            lsn: self.lsn,
            commit_ts: self.commit_ts,
            metadata: self.metadata.clone(),
            changed_keys: self.changed_keys.clone(),
            record_type: self
                .record_type
                .as_deref()
                .map(RecordType::of)
                .unwrap_or_default(),
        }
    }
}

/// The ADR-0086 per-object envelope for one transaction's metadata map
/// (ADR-0610): 16 MiB, the same figure as the ontology document's envelope,
/// checked at the channel (before commit) rather than at the WAL — the
/// record the map rides is already bounded by the per-record WAL payload
/// cap, so this bound keeps one metadata object from crowding out the redo.
pub const MAX_TX_METADATA_BYTES: usize = 16 * 1024 * 1024;

/// The per-transaction channel state every metadata channel writes
/// (ADR-0610): the merged metadata map beside the record family the last
/// channel call set. The family is the provenance one
/// ([`RecordType::Provenance`]) until the typed channel names another —
/// the default every untyped channel hard-sets. One cell on the
/// connection (the facade) and the statement views (the executor); taken
/// whole at each transaction boundary, so the record's family and map
/// leave together and a rollback discards both.
#[derive(Debug, Default, Clone)]
pub struct TxChannelState {
    /// The record's family (ADR-0610): [`RecordType::Provenance`] is the
    /// default every untyped channel hard-sets; a typed channel's
    /// [`validate_record_type`]-validated name is
    /// [`RecordType::Typed`].
    pub record_type: RecordType,
    /// The merged metadata map — the canonical [`merge_tx_metadata`]'s
    /// target (later keys win, the ADR-0086 envelope).
    pub map: BTreeMap<String, Value>,
}

/// Validate a typed channel's record-type name (ADR-0610): a non-empty
/// kebab-case name — lowercase ASCII letters and digits in segments joined
/// by single hyphens — and never the provenance family's own reserved
/// name, which the untyped channels seed and no typed seed may claim. The
/// rejection is the value-domain error (`InvalidArgumentValue`), never a
/// silent coercion to the default.
pub fn validate_record_type(record_type: &str) -> Result<()> {
    let kebab = !record_type.is_empty()
        && record_type.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if !kebab {
        return Err(Error::invalid_argument_value(format!(
            "record type '{record_type}' must be a non-empty kebab-case name"
        )));
    }
    if record_type == PROVENANCE_RECORD_TYPE {
        return Err(Error::invalid_argument_value(
            "record type 'provenance' is reserved: the metadata channels seed provenance \
             records, so a typed seed must name another type",
        ));
    }
    Ok(())
}

/// Merge `incoming` into a transaction's metadata map (ADR-0610): later keys
/// win, and the merged map must stay within the per-object envelope above —
/// the one merge every channel (`tx.setMetaData`, a Bolt BEGIN's
/// `tx_metadata`, the embedded `set_tx_metadata` and its typed twin) funnels
/// through, so the bound is checked at the channel, before commit. The merge
/// is atomic: a refused map leaves `target` unchanged.
pub fn merge_tx_metadata(
    target: &mut BTreeMap<String, Value>,
    incoming: BTreeMap<String, Value>,
) -> Result<()> {
    if incoming.is_empty() {
        return Ok(());
    }
    let mut merged = target.clone();
    merged.extend(incoming);
    let bytes = serde_json::to_vec(&merged)
        .map_err(|e| Error::storage(format!("transaction metadata serialization failed: {e}")))?;
    if bytes.len() > MAX_TX_METADATA_BYTES {
        return Err(Error::storage(format!(
            "transaction metadata of {} bytes exceeds the 16 MiB limit",
            bytes.len()
        )));
    }
    *target = merged;
    Ok(())
}

/// Derive a commit's distinct property-key changes (ADR-0610) — the one
/// canonical helper, run once at commit from the redo and the pre-state:
/// creates contribute every key of the initial property map, `Set`/`Remove`
/// ops the named key, and a delete every key the element carried at the
/// statement's `snapshot_ts` (read from `pre_state`; the conflict checks
/// guarantee the element was not concurrently modified, so the state at
/// admission is the state the statement saw). Labels and every catalog,
/// pull-identity and store-level op contribute nothing — changed keys are
/// property keys. The result is distinct and sorted by `(kind, id, key)`.
pub fn derive_changed_keys(
    ops: &[RedoOp],
    pre_state: &dyn GraphExport,
    snapshot_ts: u64,
) -> Vec<ChangedKey> {
    fn push(keys: &mut Vec<ChangedKey>, kind: ChangedKeyKind, id: i64, key: &str) {
        keys.push(ChangedKey {
            kind,
            id,
            key: key.to_owned(),
        });
    }
    let mut keys = Vec::new();
    for op in ops {
        match op {
            RedoOp::CreateNode { id, properties, .. } => {
                for key in properties.keys() {
                    push(&mut keys, ChangedKeyKind::Node, *id, key);
                }
            }
            RedoOp::CreateRel { id, properties, .. } => {
                for key in properties.keys() {
                    push(&mut keys, ChangedKeyKind::Rel, *id, key);
                }
            }
            RedoOp::DeleteNode { id } => {
                if let Some(node) = pre_state.node_at(*id, snapshot_ts) {
                    for key in node.properties.keys() {
                        push(&mut keys, ChangedKeyKind::Node, *id, key);
                    }
                }
            }
            RedoOp::DeleteRel { id } => {
                if let Some(rel) = pre_state.rel_at(*id, snapshot_ts) {
                    for key in rel.properties.keys() {
                        push(&mut keys, ChangedKeyKind::Rel, *id, key);
                    }
                }
            }
            RedoOp::SetNodeProp { id, key, .. } | RedoOp::RemoveNodeProp { id, key } => {
                push(&mut keys, ChangedKeyKind::Node, *id, key);
            }
            RedoOp::SetRelProp { id, key, .. } | RedoOp::RemoveRelProp { id, key } => {
                push(&mut keys, ChangedKeyKind::Rel, *id, key);
            }
            // Labels are not property keys; a delete names no catalog name;
            // and store metadata (indexes, constraints, graph types, pull
            // identities, databases, ontology documents) changes no property.
            RedoOp::AddLabels { .. }
            | RedoOp::RemoveLabels { .. }
            | RedoOp::CreateIndex { .. }
            | RedoOp::DropIndex { .. }
            | RedoOp::CreateConstraint { .. }
            | RedoOp::DropConstraint { .. }
            | RedoOp::CatalogIndexDetail { .. }
            | RedoOp::CatalogDerivedConstraint { .. }
            | RedoOp::PutDatabase { .. }
            | RedoOp::DropDatabase { .. }
            | RedoOp::PutGraphType { .. }
            | RedoOp::SetCurrentGraphType { .. }
            | RedoOp::PullIdentityPut { .. }
            | RedoOp::OntologySet { .. }
            | RedoOp::OntologyDrop { .. } => {}
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_spellings_match_the_serialized_field_names() {
        let record = CommitRecord {
            lsn: u64::MAX,
            commit_ts: u64::MAX,
            ops: Vec::new(),
            metadata: None,
            changed_keys: None,
            record_type: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        let header = format!(
            "{}{}{}{}{}",
            CommitRecord::LSN_KEY,
            u64::MAX,
            CommitRecord::COMMIT_TS_KEY,
            u64::MAX,
            CommitRecord::OPS_KEY
        );
        assert_eq!(json, format!("{header}[]}}"));
        assert_eq!(header.len(), CommitRecord::HEADER_MAX);
        assert_eq!(u64::MAX.to_string().len(), CommitRecord::U64_DECIMAL_MAX);
    }

    #[test]
    fn provenance_fields_roundtrip_and_skip_to_yesterdays_bytes() {
        // Without the fields the record is byte-identical to the
        // pre-feature form: the zero-cost guarantee while the feature is
        // off (ADR-0610).
        let bare = CommitRecord {
            lsn: 1,
            commit_ts: 2,
            ops: Vec::new(),
            metadata: None,
            changed_keys: None,
            record_type: None,
        };
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            "{\"lsn\":1,\"commit_ts\":2,\"ops\":[]}"
        );
        // With them, the fields follow the ops array in the declaration
        // order the encode-once tail writes (metadata, changed_keys, type).
        let mut metadata = BTreeMap::new();
        metadata.insert("by".to_string(), Value::String("app".into()));
        let provenance = CommitRecord {
            lsn: 1,
            commit_ts: 2,
            ops: Vec::new(),
            metadata: Some(metadata),
            changed_keys: Some(vec![ChangedKey {
                kind: ChangedKeyKind::Node,
                id: 7,
                key: "name".to_string(),
            }]),
            record_type: Some(PROVENANCE_RECORD_TYPE.to_string()),
        };
        let json = serde_json::to_string(&provenance).unwrap();
        assert_eq!(
            json,
            "{\"lsn\":1,\"commit_ts\":2,\"ops\":[]\
             ,\"metadata\":{\"by\":{\"t\":\"string\",\"v\":\"app\"}}\
             ,\"changed_keys\":[{\"kind\":\"node\",\"id\":7,\"key\":\"name\"}]\
             ,\"type\":\"provenance\"}"
        );
        // And the retained projection keeps exactly the tail half.
        let retained = provenance.retained();
        assert!(retained.is_retained() && retained.is_provenance());
        assert_eq!(retained.changed_keys, provenance.changed_keys);
        assert_eq!(retained.record_type.as_str(), "provenance");
        assert!(!bare.retained().is_retained());
        // A record the old binary wrote (no fields, not even the type)
        // still decodes — and reads as the provenance family's own.
        let old: CommitRecord =
            serde_json::from_str("{\"lsn\":1,\"commit_ts\":2,\"ops\":[]}").unwrap();
        assert!(old.metadata.is_none() && old.changed_keys.is_none() && old.record_type.is_none());
        assert_eq!(old.retained().record_type(), &RecordType::Provenance);
        assert!(!old.retained().is_retained());
        // A pre-typed tail-bearing record (an earlier feature build wrote
        // it) and an old family part without the field both read as
        // provenance records.
        let legacy_tail: TransactionRecord = serde_json::from_str(
            "{\"lsn\":1,\"commit_ts\":2,\
              \"metadata\":{\"by\":{\"t\":\"string\",\"v\":\"app\"}}}",
        )
        .unwrap();
        assert!(legacy_tail.is_provenance() && legacy_tail.is_retained());
        assert_eq!(legacy_tail.record_type(), &RecordType::Provenance);
        // A record of another type decodes with its type intact.
        let typed: TransactionRecord =
            serde_json::from_str("{\"lsn\":3,\"commit_ts\":4,\"type\":\"audit-trail\"}").unwrap();
        assert_eq!(typed.record_type().as_str(), "audit-trail");
        assert!(!typed.is_provenance());
        assert!(typed.is_retained(), "retention retains every typed record");
    }

    #[test]
    fn the_typed_name_is_kebab_case_and_provenance_is_reserved() {
        for name in [
            "",
            "-",
            "audit trail",
            "Audit",
            "audit_trail",
            "a--b",
            "-a",
            "a-",
        ] {
            let error = validate_record_type(name)
                .expect_err(&format!("`{name}` is not a kebab-case name"));
            assert!(
                matches!(&error, Error::Type(detail)
                    if detail == "InvalidArgumentValue: record type '{name}' must be a non-empty kebab-case name"),
                "`{name}`: {error:?}"
            );
        }
        let reserved = validate_record_type(PROVENANCE_RECORD_TYPE)
            .expect_err("the provenance family's own name is reserved");
        assert!(
            reserved
                .to_string()
                .contains("InvalidArgumentValue: record type 'provenance' is reserved"),
            "{reserved}"
        );
        for name in ["audit-trail", "cdc", "nightly-run-2"] {
            validate_record_type(name).unwrap_or_else(|e| panic!("`{name}` is valid: {e}"));
        }
    }
}
