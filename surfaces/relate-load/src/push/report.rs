//! The push run report (ADR-0401 §6): per-entity counts (created, updated,
//! skipped with a reason, incomparable warnings), batch/byte totals, and
//! elapsed — the data both the table rendering (CLI) and the `--json`
//! machine form read. One structure, so the two forms cannot disagree.

use std::num::NonZeroUsize;
use std::time::Duration;

use serde_json::{json, Value as Json};

use super::plan::{EntityKind, PlannedEntity};
use super::policy::UpdatePolicy;

/// One push's whole outcome.
#[derive(Debug, Clone)]
pub struct PushReport {
    /// `--dry-run`: nothing touched the remote; `created`/`updated` are
    /// the classification-free totals.
    pub dry_run: bool,
    pub entities: Vec<EntityReport>,
    /// Write statements the remote accepted.
    pub statements_sent: usize,
    /// Rows those statements carried.
    pub rows_sent: usize,
    /// Serialized request-body bytes those statements carried.
    pub bytes_sent: u64,
    pub elapsed: Duration,
}

impl PushReport {
    /// The one construction site for the report shape: the entity rows
    /// plus the run's totals (zeroed for a dry run — nothing touched the
    /// remote).
    pub(crate) fn build(
        dry_run: bool,
        entities: Vec<EntityReport>,
        totals: &Totals,
        elapsed: Duration,
    ) -> Self {
        Self {
            dry_run,
            entities,
            statements_sent: totals.statements,
            rows_sent: totals.rows,
            bytes_sent: totals.bytes,
            elapsed,
        }
    }
}

/// The sent totals accumulated across a push's write statements.
#[derive(Default)]
pub(crate) struct Totals {
    pub(crate) statements: usize,
    pub(crate) rows: usize,
    pub(crate) bytes: u64,
}

/// One entity's counts. `total` is the entity's local rows in scope;
/// created + updated + the three skipped counters + (for rels) the
/// unkeyed-endpoint and dangling counters account for it.
#[derive(Debug, Clone)]
pub struct EntityReport {
    /// The display name: a node label, a `(L)-[:T]->(L)` rel group, or a
    /// reserved bucket name.
    pub name: String,
    /// Whether the entity is a relationship group.
    pub is_rel: bool,
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    /// Nodes skipped for an unkeyed label; rels skipped because an endpoint
    /// was not key-addressable.
    pub skipped_unkeyed: usize,
    /// Existing entities untouched under policy `skip`.
    pub skipped_policy: usize,
    /// Existing entities the remote timestamp beat (ADR-0401 §4).
    pub skipped_remote_newer: usize,
    /// Rels skipped because an endpoint never arrived in the scope.
    pub skipped_dangling: usize,
    /// `--updated-at` pairs whose types could not be compared: the scoped
    /// policy applied with a warning (ADR-0401 §4).
    pub incomparable: usize,
    /// The merge key's properties (empty for unkeyed `CREATE` groups).
    pub merge_key: Vec<String>,
    pub policy: UpdatePolicy,
    /// The statement shape (empty when nothing is written).
    pub statement: String,
    /// Write batches: the number of `--batch-size` slices the entity's
    /// apply rows were sent in (the planned slices in a dry run).
    pub batches: usize,
}

/// The validated batch size — `push()`'s `batch_size >= 1` invariant
/// made a type (the public `PushOpts.batch_size` stays a `usize`; this
/// is the crate-internal carrier every batch computation consumes, so a
/// zero cannot reach `chunks`/`div_ceil` as an unexplained panic).
#[derive(Clone, Copy)]
pub(crate) struct BatchSize(NonZeroUsize);

impl BatchSize {
    /// The one construction, from the not-yet-validated field: `None`
    /// when the invariant does not hold — `push()` turns that into the
    /// typed refusal.
    pub(crate) fn new(size: usize) -> Option<Self> {
        NonZeroUsize::new(size).map(Self)
    }

    /// The batch slice length.
    pub(crate) fn get(self) -> usize {
        self.0.get()
    }

    /// The number of batches `rows` slice into (0 for none — `div_ceil`
    /// of zero rows is zero by construction).
    pub(crate) fn batch_count(self, rows: usize) -> usize {
        rows.div_ceil(self.get())
    }

    /// The batch slicing over `rows` — the one owner of the policy, so
    /// a dry run's planned count ([`Self::batch_count`]) and a run's
    /// sent slices cannot drift. Each item is `(batch number, row
    /// offset, slice)`: the 1-based batch number and the offset of its
    /// first row in the list.
    pub(crate) fn batches<T>(self, rows: &[T]) -> impl Iterator<Item = (usize, usize, &[T])> {
        rows.chunks(self.get())
            .enumerate()
            .map(move |(index, batch)| (index + 1, index * self.get(), batch))
    }
}

impl EntityReport {
    /// The report row for one planned entity, before any classification:
    /// all rows apply-or-not, totals filled, counters zeroed. `batches`
    /// counts batches actually sent — zero until a write lands (a partial
    /// push's never-attempted entities are exactly this shape).
    ///
    /// Which path owns which counter: the plan alone fills `total`, the
    /// skipped-unkeyed and skipped-dangling counters, the merge key, the
    /// policy and the statement; a dry run then fills `created` (and the
    /// planned `batches`) because it cannot tell created from updated;
    /// a real run's classification fills `created`, `updated`, the
    /// policy/remote-newer skips and `incomparable`, and `batches` as
    /// writes land. An entity no run reached keeps this shape — the
    /// zeroed counters and `batches == 0` name "never attempted".
    pub(crate) fn planned(entity: &PlannedEntity) -> Self {
        let kind = &entity.kind;
        Self {
            name: kind.display_name(),
            is_rel: matches!(
                kind,
                EntityKind::Rel { .. } | EntityKind::RelSkipped { .. } | EntityKind::Dangling
            ),
            total: entity.rows.len() + entity.skipped_unkeyed + entity.skipped_dangling,
            created: 0,
            updated: 0,
            skipped_unkeyed: entity.skipped_unkeyed,
            skipped_policy: 0,
            skipped_remote_newer: 0,
            skipped_dangling: entity.skipped_dangling,
            incomparable: 0,
            merge_key: entity.merge_key.clone(),
            policy: entity.policy,
            statement: entity.statement.clone(),
            batches: 0,
        }
    }

    /// The dry-run report row: planned counts, merge keys, statement
    /// shapes, the planned batch slices — a dry run cannot split created
    /// from updated (that is the pre-read's job), so `created` names the
    /// planned writes.
    pub(crate) fn dry_run(batch_size: BatchSize, entity: &PlannedEntity) -> Self {
        let mut planned = Self::planned(entity);
        planned.created = entity.rows.len();
        planned.batches = batch_size.batch_count(entity.rows.len());
        planned
    }
}

impl PushReport {
    /// The `--json` machine form (ADR-0401 §6): the whole report as one
    /// JSON object, `entities` in plan order.
    pub fn to_json(&self) -> Json {
        json!({
            "dryRun": self.dry_run,
            "entities": self.entities.iter().map(EntityReport::to_json).collect::<Vec<_>>(),
            "statementsSent": self.statements_sent,
            "rowsSent": self.rows_sent,
            "bytesSent": self.bytes_sent,
            "elapsedMs": self.elapsed.as_millis() as u64,
        })
    }
}

impl EntityReport {
    fn to_json(&self) -> Json {
        json!({
            "entity": self.name,
            "kind": if self.is_rel { "relationship" } else { "node" },
            "total": self.total,
            "created": self.created,
            "updated": self.updated,
            "skipped": {
                "unkeyed": self.skipped_unkeyed,
                "policy": self.skipped_policy,
                "remoteNewer": self.skipped_remote_newer,
                "dangling": self.skipped_dangling,
            },
            "incomparableWarnings": self.incomparable,
            "mergeKey": self.merge_key,
            "policy": self.policy.as_str(),
            "statement": self.statement,
            "batches": self.batches,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::plan::EntityRow;
    use std::collections::BTreeMap;

    fn placeholder_row() -> EntityRow {
        EntityRow::Node {
            key: BTreeMap::new(),
            props: BTreeMap::new(),
        }
    }

    fn planned_entity(kind: EntityKind, rows: usize) -> PlannedEntity {
        PlannedEntity {
            kind,
            policy: UpdatePolicy::Add,
            merge_key: vec!["name".to_string()],
            statement: "UNWIND $rows AS row MERGE (n) SET n += row.props".to_string(),
            rows: (0..rows).map(|_| placeholder_row()).collect(),
            skipped_unkeyed: 0,
            skipped_dangling: 0,
        }
    }

    #[test]
    fn batch_counts_slice_by_batch_size() {
        let thousand = BatchSize::new(1000).expect("test batch size is positive");
        let two = BatchSize::new(2).expect("test batch size is positive");
        assert_eq!(thousand.batch_count(0), 0);
        assert_eq!(thousand.batch_count(1), 1);
        assert_eq!(thousand.batch_count(1000), 1);
        assert_eq!(thousand.batch_count(1001), 2);
        assert_eq!(two.batch_count(5), 3);
        assert!(BatchSize::new(0).is_none());
    }

    #[test]
    fn planned_report_carries_totals_and_statement() {
        let entity = planned_entity(
            EntityKind::NodeLabel {
                labels: vec!["Person".into()],
                keyed_label: "Person".into(),
            },
            3,
        );
        let report = EntityReport::planned(&entity);
        assert_eq!(report.name, "Person");
        assert!(!report.is_rel);
        assert_eq!(report.total, 3);
        // Counters start zeroed: the dry run fills created with the
        // planned writes, the run fills it from classification. Batches
        // count what actually landed — zero until a write sends.
        assert_eq!(report.created, 0);
        assert_eq!(report.batches, 0);
        assert_eq!(report.statement, entity.statement);
    }

    /// The JSON form carries every counter (ADR-0401 §6), camel-cased for
    /// agents.
    #[test]
    fn json_form_carries_every_counter() {
        let entity = planned_entity(
            EntityKind::NodeLabel {
                labels: vec!["Person".into()],
                keyed_label: "Person".into(),
            },
            2,
        );
        let mut report = EntityReport::dry_run(
            BatchSize::new(1000).expect("test batch size is positive"),
            &entity,
        );
        report.updated = 1;
        report.created = 1;
        report.skipped_policy = 2;
        report.skipped_remote_newer = 3;
        report.incomparable = 4;
        let full = PushReport::build(
            false,
            vec![report],
            &Totals {
                statements: 5,
                rows: 6,
                bytes: 700,
            },
            Duration::from_millis(8),
        )
        .to_json();
        assert_eq!(full["entities"][0]["entity"], "Person");
        assert_eq!(full["entities"][0]["kind"], "node");
        assert_eq!(full["entities"][0]["skipped"]["remoteNewer"], 3);
        assert_eq!(full["entities"][0]["incomparableWarnings"], 4);
        assert_eq!(full["entities"][0]["mergeKey"][0], "name");
        assert_eq!(full["statementsSent"], 5);
        assert_eq!(full["bytesSent"], 700);
        assert_eq!(full["elapsedMs"], 8);
        assert_eq!(full["dryRun"], false);
        // The key sets are pinned (sorted — key order is the JSON map's
        // own), so a report field added later fails here instead of
        // being remembered.
        let mut entity_keys: Vec<&str> = full["entities"][0]
            .as_object()
            .expect("entity report is an object")
            .keys()
            .map(String::as_str)
            .collect();
        entity_keys.sort_unstable();
        assert_eq!(
            entity_keys,
            vec![
                "batches",
                "created",
                "entity",
                "incomparableWarnings",
                "kind",
                "mergeKey",
                "policy",
                "skipped",
                "statement",
                "total",
                "updated",
            ]
        );
        let mut top_keys: Vec<&str> = full
            .as_object()
            .expect("push report is an object")
            .keys()
            .map(String::as_str)
            .collect();
        top_keys.sort_unstable();
        assert_eq!(
            top_keys,
            vec![
                "bytesSent",
                "dryRun",
                "elapsedMs",
                "entities",
                "rowsSent",
                "statementsSent",
            ]
        );
    }
}
