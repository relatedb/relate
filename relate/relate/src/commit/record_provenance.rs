//! The provenance half of one commit record (ADR-0610): the optional
//! `metadata` map, the derived `changed_keys` and the record's family
//! `type` a WAL record carries while the `value-provenance` preview
//! feature is on. The one construction home for both sites that build
//! the triple — the live commit pipeline ([`derive`]) and the replicated
//! apply ([`passthrough`]) — so the gate and the empty-is-omitted
//! normalization exist once and the two cannot disagree.

use std::collections::BTreeMap;

use relate_core::{PreviewFeature, Result};
use relate_engine_api::{derive_changed_keys, ChangedKey, RecordType, RedoOp, TxChannelState};
use relate_storage::{RecordTail, WalRecord};

use crate::{Shared, Value};

/// The record's provenance half (ADR-0610) — the one declared shape the
/// whole commit pipeline carries: every field is `None` while the
/// feature is off (the zero-record-bytes guarantee for a store that
/// never opts in), a field that would be empty is `None` too (the
/// omitted form is what the record's skip-serialize fields write, so
/// the encoded bytes and the retained table never disagree), and the
/// family is spelled ([`Some`]) only on a record that carries a tail at
/// all — it is a tail field, and a commit that retains nothing writes
/// none.
#[derive(Default)]
pub(crate) struct ProvenanceFields {
    pub(super) record_type: Option<RecordType>,
    pub(super) metadata: Option<BTreeMap<String, Value>>,
    pub(super) changed_keys: Option<Vec<ChangedKey>>,
}

impl ProvenanceFields {
    /// The fields' WAL-tail borrow (ADR-0610): the one spelling of the
    /// tail both encode sites write — built from a borrow of the one
    /// declared shape, never re-declared as loose fields — with the
    /// family in its serialized form ([`RecordType::as_str`]) and every
    /// absent field the omitted form the tail itself skips.
    pub(super) fn tail(&self) -> RecordTail<'_> {
        RecordTail {
            metadata: self.metadata.as_ref(),
            changed_keys: self.changed_keys.as_deref(),
            record_type: self.record_type.as_ref().map(RecordType::as_str),
        }
    }
}

/// Derive a live commit's provenance half (ADR-0610) — the
/// `try_wal_commit` construction. The brief read guard covers the
/// GraphCell deref its two reads make (preview + the pre-state walk) and
/// releases before the caller takes the group lock — no caller of that
/// path holds the publication guard. The pre-state walk is safe without
/// the statement's pin: an element this redo deletes is live at
/// `snapshot_ts` (its visible row is never GC'd), and any concurrent
/// writer touching the same element makes the group's conflict check
/// reject the slot after the derive — a rejected record's fields never
/// reach the WAL.
///
/// The commit's type is the cell's explicit name — the typed channel's —
/// or else the provenance family's own, and `changed_keys` derive only
/// for the family's own commits (ADR-0610): another family's record is
/// its own audit surface's, never the provenance walk's. The type is
/// spelled only on a record that carries a tail at all — it is a tail
/// field, and a commit that retains nothing writes none.
pub(super) fn derive(
    shared: &Shared,
    ops: &[RedoOp],
    snapshot_ts: u64,
    tx_metadata: TxChannelState,
) -> Result<ProvenanceFields> {
    let _read = shared.read_guard()?;
    let provenance = shared
        .graph
        .preview()
        .enabled(PreviewFeature::ValueProvenance);
    let TxChannelState { record_type, map } = tx_metadata;
    let metadata = provenance.then_some(map).filter(|map| !map.is_empty());
    let changed_keys = (provenance && record_type.is_provenance())
        .then(|| derive_changed_keys(ops, &**shared.graph, snapshot_ts))
        .filter(|keys| !keys.is_empty());
    let record_type = (metadata.is_some() || changed_keys.is_some()).then_some(record_type);
    Ok(ProvenanceFields {
        record_type,
        metadata,
        changed_keys,
    })
}

/// Keep a replicated record's provenance half (ADR-0610) — the
/// `commit_replicated` construction: the record's own fields pass
/// through, derived at the source against the source's pre-state, while
/// the local opt-in is set; a replica that never opted in serializes
/// none of them (the store stays indistinguishable).
///
/// `enabled` is the caller's already-read gate: the caller holds the
/// publication **write** guard there (`begin_publication` precedes this),
/// under which the GraphCell deref the gate read makes is already
/// protected — this function takes no guard of its own, because a read
/// acquisition under the caller's write lock would re-enter the lock and
/// deadlock.
pub(super) fn passthrough(enabled: bool, record: &WalRecord) -> ProvenanceFields {
    let metadata = enabled
        .then(|| record.metadata.clone())
        .flatten()
        .filter(|map| !map.is_empty());
    let changed_keys = enabled
        .then(|| record.changed_keys.clone())
        .flatten()
        .filter(|keys| !keys.is_empty());
    // The serialized `type` string decodes into the typed family here —
    // the one decode ([`RecordType::of`]), never re-spelled per site.
    let record_type = enabled
        .then(|| record.record_type.as_deref().map(RecordType::of))
        .flatten();
    ProvenanceFields {
        record_type,
        metadata,
        changed_keys,
    }
}
