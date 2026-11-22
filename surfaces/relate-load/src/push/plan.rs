//! The push plan (ADR-0401 §2): the scope graph grouped into entities
//! — node label groups and rel groups, their merge keys resolved, their
//! row shapes built — ready for statement generation.

use std::collections::BTreeMap;

use relate::Value;

use super::collect::ScopeGraph;
use super::keys::MergeKeys;
use super::policy::{RelMode, UnkeyedAction, UpdatePolicy};
use super::statement::{node_create_statement, node_merge_statement, rel_statement, RelWrite};
use crate::LoadError;

/// The push plan: one entry per entity group, nodes first, then rels, both
/// deterministically ordered. Dry-run renders it directly; execution
/// pre-reads, classifies, and batch-writes it.
#[derive(Debug, Clone, Default)]
pub(crate) struct PushPlan {
    pub entities: Vec<PlannedEntity>,
}

/// One entity group's plan (ADR-0401 §2). The group's rows ride its
/// [`EntityKind`] variant, so a row shape can never sit beside a kind
/// that does not take it; execution splits them into batches of
/// `batch_size`.
#[derive(Debug, Clone)]
pub(crate) struct PlannedEntity {
    pub kind: EntityKind,
    /// The merge key's properties (empty for an unkeyed `CREATE` group).
    pub merge_key: Vec<String>,
    /// Nodes skipped because their label had no merge key (rel groups:
    /// rels skipped because an endpoint was not key-addressable).
    pub skipped_unkeyed: usize,
    /// Rels skipped because an endpoint never arrived in the scope.
    pub skipped_dangling: usize,
}

/// Which entity a group is (ADR-0401 §2): its identity determines the
/// pre-read and the write statement.
#[derive(Debug, Clone)]
pub(crate) enum EntityKind {
    /// A keyed node label group: EVERY label its nodes carry, sorted —
    /// the display and the group identity — plus the set's KEYED label:
    /// the merge identity (the label the constraint/config keys). The
    /// statement merges on the keyed label alone and SETs the remaining
    /// labels onto the node it found, so secondary labels survive the
    /// push (LABS-907) AND a local label-set change updates the remote
    /// node instead of forking one with the label combination. The
    /// group is the label set, not one label: nodes sharing a keyed
    /// label but carrying different secondaries are separate groups,
    /// each with its own static statement.
    NodeLabel {
        labels: Vec<String>,
        keyed_label: String,
        policy: UpdatePolicy,
        /// The statement shape: one text for every batch of this group.
        statement: String,
        rows: Vec<KeyedNodeRow>,
    },
    /// An unkeyed label set switched to `CREATE` rows by
    /// `--unkeyed-labels create` (non-idempotent, documented).
    NodeCreate {
        labels: Vec<String>,
        policy: UpdatePolicy,
        /// The statement shape: one text for every batch of this group.
        statement: String,
        rows: Vec<CreateNodeRow>,
    },
    /// A label set skipped entirely (`--unkeyed-labels skip`, the
    /// default): counted, never written.
    NodeSkipped { labels: Vec<String> },
    /// A rel group whose every rel was skipped for an unkeyed endpoint:
    /// counted, never written. `Rel` always denotes a written,
    /// key-addressable group; every never-written bucket is modeled the
    /// same way — the node phase's `NodeSkipped`, this kind, the scope's
    /// `Dangling`.
    RelSkipped {
        rel_type: String,
        /// The start endpoint's report spelling.
        start: String,
        /// The end endpoint's report spelling.
        end: String,
    },
    /// Rels whose endpoints never arrived in the scope (ADR-0401 §8):
    /// a counted bucket, not a relationship group — its own variant so
    /// no sentinel-string sniff discriminates it.
    Dangling,
    /// A relationship group: type plus the two endpoint label groups.
    Rel {
        rel_type: String,
        start_label: String,
        start_key: Vec<String>,
        end_label: String,
        end_key: Vec<String>,
        policy: UpdatePolicy,
        /// The statement shape: one text for every batch of this group.
        statement: String,
        rows: RelRows,
    },
}

/// The display spelling of a node group's labels: `A:B`, or the
/// reserved labelless name when the group's nodes carry none.
pub(crate) fn display_labels(labels: &[String]) -> String {
    if labels.is_empty() {
        LABELLESS.to_string()
    } else {
        labels.join(":")
    }
}

impl EntityKind {
    /// The scoped update policy — `Some` exactly for the kinds that write
    /// (`NodeLabel`, `NodeCreate`, `Rel`). `None` for the counted buckets
    /// (`NodeSkipped`, `RelSkipped`, `Dangling`), which never write and so
    /// have no policy to state: a fabricated `skip` there would name a
    /// real user-selectable policy the bucket never applies (LABS-932).
    pub(crate) fn policy(&self) -> Option<UpdatePolicy> {
        match self {
            EntityKind::NodeLabel { policy, .. }
            | EntityKind::NodeCreate { policy, .. }
            | EntityKind::Rel { policy, .. } => Some(*policy),
            EntityKind::NodeSkipped { .. }
            | EntityKind::RelSkipped { .. }
            | EntityKind::Dangling => None,
        }
    }

    /// The statement a group writes with — `Some` exactly for the kinds
    /// that write, as the statement rides those variants beside their
    /// rows: a writing group without a statement cannot be built.
    pub(crate) fn statement(&self) -> Option<&str> {
        match self {
            EntityKind::NodeLabel { statement, .. }
            | EntityKind::NodeCreate { statement, .. }
            | EntityKind::Rel { statement, .. } => Some(statement),
            EntityKind::NodeSkipped { .. }
            | EntityKind::RelSkipped { .. }
            | EntityKind::Dangling => None,
        }
    }

    /// The policy of a keyed kind — the writing kinds that pre-read and
    /// classify by a key. An unkeyed `CREATE` group writes but has no
    /// identity to classify by, so it answers `None` here: the keyed
    /// decision and its policy are one fact, never a flag beside an
    /// `Option`.
    pub(crate) fn keyed_policy(&self) -> Option<UpdatePolicy> {
        match self {
            EntityKind::NodeCreate { .. } => None,
            other => other.policy(),
        }
    }

    /// Whether the kind is a relationship entity (a written group, a
    /// skipped one, or the dangling bucket) — one exhaustive answer, so
    /// a new kind must state its side.
    pub(crate) fn is_rel(&self) -> bool {
        match self {
            EntityKind::Rel { .. } | EntityKind::RelSkipped { .. } | EntityKind::Dangling => true,
            EntityKind::NodeLabel { .. }
            | EntityKind::NodeCreate { .. }
            | EntityKind::NodeSkipped { .. } => false,
        }
    }

    /// The display name of the kind: a node label set, a rel group's
    /// `(start)-[:TYPE]->(end)`, or the reserved buckets.
    pub(crate) fn display_name(&self) -> String {
        match self {
            EntityKind::NodeLabel { labels, .. } | EntityKind::NodeCreate { labels, .. } => {
                display_labels(labels)
            }
            EntityKind::NodeSkipped { labels } => {
                format!("{} (unkeyed)", display_labels(labels))
            }
            EntityKind::RelSkipped {
                rel_type,
                start,
                end,
            } => format!("({start})-[:{rel_type}]->({end}) (unkeyed)"),
            EntityKind::Dangling => DANGLING.to_string(),
            EntityKind::Rel {
                rel_type,
                start_label,
                end_label,
                ..
            } => format!("({start_label})-[:{rel_type}]->({end_label})"),
        }
    }
}

/// A keyed node row, pre-classification (ADR-0401 §2): the merge-key
/// properties the remote row is matched by, plus the payload.
#[derive(Debug, Clone)]
pub(crate) struct KeyedNodeRow {
    pub key: BTreeMap<String, Value>,
    pub props: BTreeMap<String, Value>,
}

/// An unkeyed `CREATE` node row: payload only — it has no identity to
/// match a remote row by.
#[derive(Debug, Clone)]
pub(crate) struct CreateNodeRow {
    pub props: BTreeMap<String, Value>,
}

/// A rel row written under a resolved relationship key: the key map is
/// present by construction.
#[derive(Debug, Clone)]
pub(crate) struct KeyedRelRow {
    pub start: BTreeMap<String, Value>,
    pub end: BTreeMap<String, Value>,
    pub key: BTreeMap<String, Value>,
    pub props: BTreeMap<String, Value>,
}

/// A rel row in every other mode (structural, CREATE, or the keyed mode
/// with no resolved key): matched by endpoints only, no key map.
#[derive(Debug, Clone)]
pub(crate) struct PairRelRow {
    pub start: BTreeMap<String, Value>,
    pub end: BTreeMap<String, Value>,
    pub props: BTreeMap<String, Value>,
}

/// A rel group's rows: the variant is the write mode's key decision, so
/// a group never mixes keyed and pair rows.
#[derive(Debug, Clone)]
pub(crate) enum RelRows {
    Keyed(Vec<KeyedRelRow>),
    Pair(Vec<PairRelRow>),
}

/// One row of a group, borrowed: the single level execution matches on —
/// the statement's parameter body ([`super::statement::write_row`]) and
/// the identity encoder both read it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RowRef<'a> {
    Node(&'a KeyedNodeRow),
    Create(&'a CreateNodeRow),
    Rel(&'a KeyedRelRow),
    Pair(&'a PairRelRow),
}

impl<'a> RowRef<'a> {
    /// The row's property payload (the `row.props` the statement sets).
    pub(crate) fn props(self) -> &'a BTreeMap<String, Value> {
        match self {
            RowRef::Node(row) => &row.props,
            RowRef::Create(row) => &row.props,
            RowRef::Rel(row) => &row.props,
            RowRef::Pair(row) => &row.props,
        }
    }

    /// The row's key-carrying map for one identity field, when its shape
    /// has that field.
    fn identity_map(self, field: IdentityField) -> Option<&'a BTreeMap<String, Value>> {
        match (self, field) {
            (RowRef::Node(row), IdentityField::Key) => Some(&row.key),
            (RowRef::Rel(row), IdentityField::Key) => Some(&row.key),
            (RowRef::Rel(row), IdentityField::Start) => Some(&row.start),
            (RowRef::Rel(row), IdentityField::End) => Some(&row.end),
            (RowRef::Pair(row), IdentityField::Start) => Some(&row.start),
            (RowRef::Pair(row), IdentityField::End) => Some(&row.end),
            (RowRef::Node(_), IdentityField::Start | IdentityField::End)
            | (RowRef::Pair(_), IdentityField::Key)
            | (RowRef::Create(_), _) => None,
        }
    }
}

/// Which key-carrying map of a row an identity column reads.
#[derive(Debug, Clone, Copy)]
pub(crate) enum IdentityField {
    Key,
    Start,
    End,
}

/// One run of identity columns: every property of `props`, read from one
/// row field and selected under one pre-read variable and alias prefix.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IdentityRun<'a> {
    pub field: IdentityField,
    pub var: &'static str,
    pub alias: &'static str,
    pub props: &'a [String],
}

/// A keyed group's identity-column derivation (start keys, end keys, rel
/// keys, in that order) — the ONE source of the pre-read's RETURN items,
/// its cell boundary, and the row identity encoder, so the remote side
/// and the local side cannot disagree. Unkeyed kinds have none.
#[derive(Debug, Clone)]
pub(crate) struct IdentityLayout<'a> {
    pub runs: Vec<IdentityRun<'a>>,
    /// The pre-read variable the `--updated-at` candidate columns read.
    pub candidate_var: &'static str,
}

impl IdentityLayout<'_> {
    /// The number of identity columns.
    pub(crate) fn column_count(&self) -> usize {
        self.runs.iter().map(|run| run.props.len()).sum()
    }

    /// The identity values of one row, in column order, read through the
    /// canonical keyed extraction.
    pub(crate) fn row_values<'r>(
        &self,
        entity: &str,
        row: RowRef<'r>,
    ) -> Result<Vec<&'r Value>, LoadError> {
        let mut values = Vec::with_capacity(self.column_count());
        for run in &self.runs {
            if run.props.is_empty() {
                continue;
            }
            let map = row.identity_map(run.field).ok_or_else(|| {
                LoadError::Push(format!(
                    "entity {entity}: a row shape without the {:?} identity field",
                    run.field
                ))
            })?;
            values.extend(keyed_values(entity, map, run.props)?);
        }
        Ok(values)
    }
}

impl PlannedEntity {
    /// The group's rows, borrowed, in plan order.
    pub(crate) fn rows(&self) -> Box<dyn Iterator<Item = RowRef<'_>> + '_> {
        match &self.kind {
            EntityKind::NodeLabel { rows, .. } => Box::new(rows.iter().map(RowRef::Node)),
            EntityKind::NodeCreate { rows, .. } => Box::new(rows.iter().map(RowRef::Create)),
            EntityKind::Rel {
                rows: RelRows::Keyed(rows),
                ..
            } => Box::new(rows.iter().map(RowRef::Rel)),
            EntityKind::Rel {
                rows: RelRows::Pair(rows),
                ..
            } => Box::new(rows.iter().map(RowRef::Pair)),
            EntityKind::NodeSkipped { .. }
            | EntityKind::RelSkipped { .. }
            | EntityKind::Dangling => Box::new(std::iter::empty()),
        }
    }

    /// How many rows the group writes (counted buckets: none).
    pub(crate) fn row_count(&self) -> usize {
        self.rows().count()
    }

    /// The identity-column derivation of a keyed group; `None` for the
    /// kinds that never pre-read (unkeyed `CREATE`, counted buckets).
    pub(crate) fn identity_layout(&self) -> Option<IdentityLayout<'_>> {
        let run = |field, var, alias, props| IdentityRun {
            field,
            var,
            alias,
            props,
        };
        match &self.kind {
            EntityKind::NodeLabel { .. } => Some(IdentityLayout {
                runs: vec![run(IdentityField::Key, "n", "k", &self.merge_key)],
                candidate_var: "n",
            }),
            EntityKind::Rel {
                start_key, end_key, ..
            } => Some(IdentityLayout {
                runs: vec![
                    run(IdentityField::Start, "a", "k", start_key),
                    run(IdentityField::End, "b", "e", end_key),
                    run(IdentityField::Key, "r", "r", &self.merge_key),
                ],
                candidate_var: "r",
            }),
            EntityKind::NodeCreate { .. }
            | EntityKind::NodeSkipped { .. }
            | EntityKind::RelSkipped { .. }
            | EntityKind::Dangling => None,
        }
    }
}

/// The push options the plan needs (a slice of [`super::PushOpts`], split
/// out so the builder is unit-testable without credentials).
#[derive(Debug, Clone)]
pub(crate) struct PlanOpts {
    pub nodes_update: super::policy::PolicySpec,
    pub rels_update: super::policy::PolicySpec,
    pub rel_mode: RelMode,
    pub unkeyed_labels: UnkeyedAction,
}

/// One node group's classification — the decision itself, made once at
/// the draft's creation: every later push into the group matches the
/// variant it chose, so the invalid field combinations are not
/// representable.
enum NodeDraft {
    /// A keyed label set: the keyed label, its merge key, and the rows.
    Keyed {
        label: String,
        merge_key: Vec<String>,
        rows: Vec<KeyedNodeRow>,
    },
    /// `--unkeyed-labels create`: rows without identity.
    Created { rows: Vec<CreateNodeRow> },
    /// `--unkeyed-labels skip`: the counter, never a row.
    Skipped { count: usize },
}

/// A node as a rel endpoint: the node itself (the one id-to-node table)
/// and its endpoint identity, built once here — the rel phase never
/// re-derives it per edge.
struct EndpointSlot<'a> {
    node: &'a super::collect::LocalNode,
    id: EndpointId,
}

/// A rel endpoint's group identity: keyedness is part of it, so a keyed
/// label and an unkeyed label set can never collide (a label literally
/// named `A:B` is a different endpoint than the {A, B} set) and a
/// group's mode is fixed by its key — the keyed label the MATCH binds,
/// or, for an endpoint no label of which carries a key, the label set
/// whose group never writes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EndpointId {
    /// Key-addressable: the keyed label and its merge key — the whole
    /// derivation, so the row loop, the group key and the emit all read
    /// one object.
    Keyed {
        label: String,
        merge_key: Vec<String>,
    },
    /// Not key-addressable: the label set; the report spelling derives
    /// from it.
    Unkeyed(Vec<String>),
}

impl EndpointId {
    /// The report spelling of the endpoint.
    fn display(&self) -> String {
        match self {
            EndpointId::Keyed { label, .. } => label.clone(),
            EndpointId::Unkeyed(labels) => display_labels(labels),
        }
    }
}

/// The label a per-label node policy resolves on for an unkeyed group:
/// the set's first label, else the reserved labelless name.
fn policy_label(labels: &[String]) -> String {
    labels
        .first()
        .cloned()
        .unwrap_or_else(|| LABELLESS.to_string())
}

/// Build the plan: nodes grouped by label set, rels by
/// `(type, start labels, end labels)`, then the reserved buckets.
pub(crate) fn build_plan(
    graph: &ScopeGraph,
    keys: &MergeKeys,
    opts: &PlanOpts,
) -> Result<PushPlan, LoadError> {
    let (mut entities, node_groups) = plan_nodes(graph, keys, opts)?;
    plan_rels(graph, keys, opts, &node_groups, &mut entities)?;
    Ok(PushPlan { entities })
}

/// The node phase (LABS-907): the group is the node's FULL sorted label
/// set — the statement writes every label, so secondary labels survive
/// the push. The merge key comes from the first label in the set that
/// has one; a node with no keyed label follows `--unkeyed-labels`.
/// Returns the node entities plus every node's endpoint slot for the
/// rel phase.
fn plan_nodes<'a>(
    graph: &'a ScopeGraph,
    keys: &MergeKeys,
    opts: &PlanOpts,
) -> Result<(Vec<PlannedEntity>, BTreeMap<i64, EndpointSlot<'a>>), LoadError> {
    let mut drafts: BTreeMap<Vec<String>, NodeDraft> = BTreeMap::new();
    let mut node_groups: BTreeMap<i64, EndpointSlot<'a>> = BTreeMap::new();
    for node in &graph.nodes {
        let mut labels = node.labels.clone();
        labels.sort();
        // The set's keyed label with its merge key, derived once.
        let keyed = labels.iter().find_map(|label| {
            keys.node_key(label)
                .map(|key| (label.clone(), key.to_vec()))
        });
        // The draft's variant IS the classification — chosen from the
        // same derivation every node of this label set shares.
        let draft = drafts
            .entry(labels.clone())
            .or_insert_with(|| match &keyed {
                Some((label, merge_key)) => NodeDraft::Keyed {
                    label: label.clone(),
                    merge_key: merge_key.clone(),
                    rows: Vec::new(),
                },
                None => match opts.unkeyed_labels {
                    UnkeyedAction::Skip => NodeDraft::Skipped { count: 0 },
                    UnkeyedAction::Create => NodeDraft::Created { rows: Vec::new() },
                },
            });
        // The endpoint identity, built once — the rel phase clones it
        // into group keys, never re-derives it per edge. The keyed
        // derivation is moved in, not cloned again (LABS-932).
        let id = match keyed {
            Some((label, merge_key)) => EndpointId::Keyed { label, merge_key },
            None => EndpointId::Unkeyed(labels),
        };
        node_groups.insert(node.id, EndpointSlot { node, id });
        match draft {
            NodeDraft::Keyed {
                label,
                merge_key,
                rows,
            } => {
                // The rel phase resolves endpoints through this keyed
                // label (one label with a key): the write created the
                // node carrying it, so the endpoint MATCH finds it.
                let row_key = keyed_props(label, &node.properties, merge_key)?;
                rows.push(KeyedNodeRow {
                    key: row_key,
                    props: node.properties.clone(),
                });
            }
            NodeDraft::Created { rows } => rows.push(CreateNodeRow {
                props: node.properties.clone(),
            }),
            NodeDraft::Skipped { count } => *count += 1,
        }
    }
    let mut entities: Vec<PlannedEntity> = Vec::new();
    for (labels, draft) in drafts {
        match draft {
            NodeDraft::Keyed {
                label,
                merge_key,
                rows,
            } => {
                let policy = opts.nodes_update.resolve(&label);
                // The keyed-label/secondaries partition is derived
                // inside the statement builder — one representation.
                let statement = node_merge_statement(&labels, &label, &merge_key, policy)?;
                entities.push(PlannedEntity {
                    kind: EntityKind::NodeLabel {
                        labels,
                        keyed_label: label,
                        policy,
                        statement,
                        rows,
                    },
                    merge_key,
                    skipped_unkeyed: 0,
                    skipped_dangling: 0,
                });
            }
            NodeDraft::Created { rows } => {
                let policy = opts.nodes_update.resolve(&policy_label(&labels));
                let statement = node_create_statement(&labels, policy)?;
                entities.push(PlannedEntity {
                    kind: EntityKind::NodeCreate {
                        labels,
                        policy,
                        statement,
                        rows,
                    },
                    merge_key: Vec::new(),
                    skipped_unkeyed: 0,
                    skipped_dangling: 0,
                });
            }
            NodeDraft::Skipped { count } => {
                entities.push(PlannedEntity {
                    kind: EntityKind::NodeSkipped { labels },
                    merge_key: Vec::new(),
                    skipped_unkeyed: count,
                    skipped_dangling: 0,
                });
            }
        }
    }
    Ok((entities, node_groups))
}

/// One rel group's classification — the decision itself, made once at
/// the draft's creation from the endpoint identities: every later row
/// matches the variant it chose, so the keyed facts (the pair, the
/// write mode) exist only where they are meaningful.
enum RelDraft<'k> {
    /// Both endpoints key-addressable: the two endpoint (label, merge
    /// key) pairs, the resolved write mode, and the rows. The one
    /// derivation feeds the rows' key maps, the statement and the
    /// report's merge-key column.
    Keyed {
        start: (String, Vec<String>),
        end: (String, Vec<String>),
        rows: DraftRows<'k>,
    },
    /// An endpoint is not key-addressable: the report spellings of the
    /// two endpoints and the counter of rels never written.
    Unkeyed {
        start: String,
        end: String,
        skipped: usize,
    },
}

impl<'k> RelDraft<'k> {
    /// The group's draft, derived once from its endpoint identities.
    fn new(start: &EndpointId, end: &EndpointId, write: RelWrite<'k>) -> Self {
        match (start, end) {
            (
                EndpointId::Keyed {
                    label: start_label,
                    merge_key: start_key,
                },
                EndpointId::Keyed {
                    label: end_label,
                    merge_key: end_key,
                },
            ) => RelDraft::Keyed {
                start: (start_label.clone(), start_key.clone()),
                end: (end_label.clone(), end_key.clone()),
                rows: DraftRows::new(write),
            },
            // An unkeyed endpoint on either side: counted, never
            // written. Both combinations are explicit so a third
            // identity variant is a compile error here.
            (EndpointId::Unkeyed(_), _) | (_, EndpointId::Unkeyed(_)) => RelDraft::Unkeyed {
                start: start.display(),
                end: end.display(),
                skipped: 0,
            },
        }
    }
}

/// A keyed rel group's rows with the write mode that shaped them, one
/// type: the key resolved for the group and the row variant it takes
/// cannot disagree.
enum DraftRows<'k> {
    /// `MERGE` on the type's key properties.
    OnKey(&'k [String], Vec<KeyedRelRow>),
    /// `CREATE`, or `MERGE` on the endpoint pair alone.
    Pair(RelWrite<'k>, Vec<PairRelRow>),
}

impl<'k> DraftRows<'k> {
    fn new(write: RelWrite<'k>) -> Self {
        match write {
            RelWrite::MergeOnKey(props) => DraftRows::OnKey(props, Vec::new()),
            RelWrite::Create | RelWrite::MergeOnPair => DraftRows::Pair(write, Vec::new()),
        }
    }

    /// The write mode the statement spells.
    fn write(&self) -> RelWrite<'k> {
        match self {
            DraftRows::OnKey(props, _) => RelWrite::MergeOnKey(props),
            DraftRows::Pair(write, _) => *write,
        }
    }

    fn into_rows(self) -> RelRows {
        match self {
            DraftRows::OnKey(_, rows) => RelRows::Keyed(rows),
            DraftRows::Pair(_, rows) => RelRows::Pair(rows),
        }
    }
}

/// The rel phase: group by `(type, start label set, end label set)`; a
/// rel whose endpoint is not key-addressable is counted, never written.
/// Endpoint key properties are read from the endpoint nodes themselves —
/// the remote is never asked to guess (ADR-0401 §2).
fn plan_rels(
    graph: &ScopeGraph,
    keys: &MergeKeys,
    opts: &PlanOpts,
    node_groups: &BTreeMap<i64, EndpointSlot<'_>>,
    entities: &mut Vec<PlannedEntity>,
) -> Result<(), LoadError> {
    // Keyed by the facts the group is ordered by: the type and the two
    // endpoint identities — keyedness included, so a keyed label and an
    // unkeyed label set are different groups however they spell. The
    // draft is the ONE mode decision, made when the group is created.
    let mut rel_groups: BTreeMap<(String, EndpointId, EndpointId), RelDraft<'_>> = BTreeMap::new();
    for rel in &graph.rels {
        let start = node_groups
            .get(&rel.start)
            .expect("finish_graph kept only endpointed rels");
        let end = node_groups
            .get(&rel.end)
            .expect("finish_graph kept only endpointed rels");
        let draft = rel_groups
            .entry((rel.rel_type.clone(), start.id.clone(), end.id.clone()))
            .or_insert_with(|| {
                RelDraft::new(&start.id, &end.id, rel_write(keys, opts, &rel.rel_type))
            });
        match draft {
            RelDraft::Keyed {
                start: (start_label, start_merge),
                end: (end_label, end_merge),
                rows,
            } => {
                // Both endpoints key-addressable: the key values are
                // read off the nodes the slots carry, so every miss
                // below is a missing key property.
                let start_key = keyed_props(start_label, &start.node.properties, start_merge)?;
                let end_key = keyed_props(end_label, &end.node.properties, end_merge)?;
                // Keyed (the default): merge on the resolved key when
                // one exists — a --rel-key override, else the type's
                // relationship constraint — and fall back to structural
                // when neither does.
                match rows {
                    DraftRows::OnKey(props, rows) => rows.push(KeyedRelRow {
                        start: start_key,
                        end: end_key,
                        key: keyed_props(&rel.rel_type, &rel.properties, props)?,
                        props: rel.properties.clone(),
                    }),
                    DraftRows::Pair(_, rows) => rows.push(PairRelRow {
                        start: start_key,
                        end: end_key,
                        props: rel.properties.clone(),
                    }),
                }
            }
            RelDraft::Unkeyed { skipped, .. } => *skipped += 1,
        }
    }
    for ((rel_type, _, _), draft) in rel_groups {
        match draft {
            // A Keyed group's rows are non-empty by construction (the
            // draft is created and its first row pushed in the same
            // iteration), so no empty-rows branch exists.
            RelDraft::Keyed { start, end, rows } => {
                let write = rows.write();
                // The scoped policy is resolved here, where a writing
                // kind is built — a counted group never resolves one
                // (LABS-932).
                let policy = opts.rels_update.resolve(&rel_type);
                let statement = rel_statement(
                    &rel_type,
                    (&start.0, &start.1),
                    (&end.0, &end.1),
                    write,
                    policy,
                )?;
                entities.push(PlannedEntity {
                    kind: EntityKind::Rel {
                        rel_type,
                        start_label: start.0,
                        start_key: start.1,
                        end_label: end.0,
                        end_key: end.1,
                        policy,
                        statement,
                        rows: rows.into_rows(),
                    },
                    // The keyed mode's key feeds the statement and the
                    // report's merge-key column alike.
                    merge_key: write.key().map(<[String]>::to_vec).unwrap_or_default(),
                    skipped_unkeyed: 0,
                    skipped_dangling: 0,
                });
            }
            RelDraft::Unkeyed {
                start,
                end,
                skipped,
            } => {
                entities.push(PlannedEntity {
                    kind: EntityKind::RelSkipped {
                        rel_type,
                        start,
                        end,
                    },
                    merge_key: Vec::new(),
                    skipped_unkeyed: skipped,
                    skipped_dangling: 0,
                });
            }
        }
    }
    if graph.dangling_rels > 0 {
        // Rels whose endpoints never arrived in the scope (ADR-0401 §8):
        // one reserved bucket so the report never loses them.
        entities.push(PlannedEntity {
            kind: EntityKind::Dangling,
            merge_key: Vec::new(),
            skipped_unkeyed: 0,
            skipped_dangling: graph.dangling_rels,
        });
    }
    Ok(())
}

/// The reserved report name for labelless nodes (never an identifier).
pub(crate) const LABELLESS: &str = "(no label)";

/// The reserved report name for the dangling-rels bucket.
pub(crate) const DANGLING: &str = "(dangling)";

/// How one relationship type is written under the plan's mode: the
/// keyed mode consults the type's key (override or constraint) and falls
/// back to the structural pair when none resolves; every other mode
/// resolves nothing. One derivation, made where a group is created.
fn rel_write<'a>(keys: &'a MergeKeys, opts: &PlanOpts, rel_type: &str) -> RelWrite<'a> {
    match opts.rel_mode {
        RelMode::Create => RelWrite::Create,
        RelMode::Structural => RelWrite::MergeOnPair,
        RelMode::Keyed => keys
            .rel_key(rel_type)
            .map_or(RelWrite::MergeOnPair, RelWrite::MergeOnKey),
    }
}

/// The values of one element's key properties, read in the merge key's
/// order — the one keyed extraction: [`keyed_props`] (the row's key map)
/// and the identity encoder both consume it. A missing key property
/// fails closed naming the entity and the property (a merge key the data
/// does not satisfy is a local integrity error, not a silent partial
/// push).
pub(crate) fn keyed_values<'a>(
    entity: &str,
    props: &'a BTreeMap<String, Value>,
    key: &[String],
) -> Result<Vec<&'a Value>, LoadError> {
    key.iter()
        .map(|prop| {
            props
                .get(prop)
                .ok_or_else(|| missing_key_prop(entity, prop))
        })
        .collect()
}

/// The keyed properties of one element as a map, in the merge key's order.
fn keyed_props(
    entity: &str,
    props: &BTreeMap<String, Value>,
    key: &[String],
) -> Result<BTreeMap<String, Value>, LoadError> {
    let values = keyed_values(entity, props, key)?;
    Ok(key
        .iter()
        .cloned()
        .zip(values.into_iter().cloned())
        .collect())
}

/// The merge-key-missing refusal, naming the entity and the property.
fn missing_key_prop(entity: &str, prop: &str) -> LoadError {
    LoadError::Push(format!(
        "merge-key property {prop:?} is missing on a {entity} entity in the local scope"
    ))
}

#[cfg(test)]
mod tests;
