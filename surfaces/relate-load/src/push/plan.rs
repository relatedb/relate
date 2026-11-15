//! The push plan (ADR-0401 §2): the scope graph grouped into entities
//! — node label groups and rel groups, their merge keys resolved, their
//! row shapes built — ready for statement generation.

use std::collections::BTreeMap;

use relate::Value;

use super::collect::ScopeGraph;
use super::keys::MergeKeys;
use super::policy::{RelMode, UnkeyedAction, UpdatePolicy};
use super::statement::{node_create_statement, node_merge_statement, rel_statement};
use crate::LoadError;

/// The push plan: one entry per entity group, nodes first, then rels, both
/// deterministically ordered. Dry-run renders it directly; execution
/// pre-reads, classifies, and batch-writes it.
#[derive(Debug, Clone, Default)]
pub(crate) struct PushPlan {
    pub entities: Vec<PlannedEntity>,
}

/// One entity group's planned rows (ADR-0401 §2).
#[derive(Debug, Clone)]
pub(crate) struct PlannedEntity {
    pub kind: EntityKind,
    /// The scoped update policy — `Some` exactly for the kinds that write
    /// (`NodeLabel`, `NodeCreate`, `Rel`). `None` for the counted buckets
    /// (`NodeSkipped`, `RelSkipped`, `Dangling`), which never write and so
    /// have no policy to state: a fabricated `skip` there would name a
    /// real user-selectable policy the bucket never applies (LABS-932).
    pub policy: Option<UpdatePolicy>,
    /// The merge key's properties (empty for an unkeyed `CREATE` group).
    pub merge_key: Vec<String>,
    /// The statement shape: one text for every batch of this entity. Empty
    /// for a skip-only group (nothing to write).
    pub statement: String,
    /// The group's rows, pre-classification. Execution splits them into
    /// batches of `batch_size`.
    pub rows: Vec<EntityRow>,
    /// Nodes skipped because their label had no merge key (rel groups:
    /// rels skipped because an endpoint was not key-addressable).
    pub skipped_unkeyed: usize,
    /// Rels skipped because an endpoint never arrived in the scope.
    pub skipped_dangling: usize,
}

/// Which entity a group is (ADR-0401 §2): its identity determines the
/// pre-read and the write statement.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    },
    /// An unkeyed label set switched to `CREATE` rows by
    /// `--unkeyed-labels create` (non-idempotent, documented).
    NodeCreate { labels: Vec<String> },
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
    /// Whether this kind classifies by a key — a positive list over the
    /// kinds that actually pre-read, so a new counted bucket cannot
    /// silently default into "keyed".
    pub(crate) fn is_keyed(&self) -> bool {
        matches!(self, EntityKind::NodeLabel { .. } | EntityKind::Rel { .. })
    }

    /// The display name of the kind: a node label set, a rel group's
    /// `(start)-[:TYPE]->(end)`, or the reserved buckets.
    pub(crate) fn display_name(&self) -> String {
        match self {
            EntityKind::NodeLabel { labels, .. } | EntityKind::NodeCreate { labels } => {
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

/// One planned row, pre-classification (ADR-0401 §2): the identity the
/// remote row is matched by, plus the payload.
#[derive(Debug, Clone)]
pub(crate) enum EntityRow {
    Node {
        /// Merge-key properties (empty for an unkeyed `CREATE` row).
        key: BTreeMap<String, Value>,
        props: BTreeMap<String, Value>,
    },
    /// A rel row in the keyed mode with a resolved relationship key:
    /// the key map is present by construction.
    Rel {
        start: BTreeMap<String, Value>,
        end: BTreeMap<String, Value>,
        key: BTreeMap<String, Value>,
        props: BTreeMap<String, Value>,
    },
    /// A rel row in every other mode (structural, CREATE, or the keyed
    /// mode with no resolved key): matched by endpoints only, no key map.
    RelUnkeyed {
        start: BTreeMap<String, Value>,
        end: BTreeMap<String, Value>,
        props: BTreeMap<String, Value>,
    },
}

impl EntityRow {
    /// The row's property payload (the `row.props` the statement sets).
    pub(crate) fn props(&self) -> &BTreeMap<String, Value> {
        match self {
            EntityRow::Node { props, .. }
            | EntityRow::Rel { props, .. }
            | EntityRow::RelUnkeyed { props, .. } => props,
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
        rows: Vec<EntityRow>,
    },
    /// `--unkeyed-labels create`: rows without identity.
    Created { rows: Vec<EntityRow> },
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
                rows.push(EntityRow::Node {
                    key: row_key,
                    props: node.properties.clone(),
                });
            }
            NodeDraft::Created { rows } => rows.push(EntityRow::Node {
                key: BTreeMap::new(),
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
                entities.push(PlannedEntity {
                    // The keyed-label/secondaries partition is derived
                    // inside the statement builder — one representation.
                    statement: node_merge_statement(&labels, &label, &merge_key, policy)?,
                    kind: EntityKind::NodeLabel {
                        labels,
                        keyed_label: label,
                    },
                    policy: Some(policy),
                    merge_key,
                    rows,
                    skipped_unkeyed: 0,
                    skipped_dangling: 0,
                });
            }
            NodeDraft::Created { rows } => {
                let policy = opts.nodes_update.resolve(&policy_label(&labels));
                entities.push(PlannedEntity {
                    statement: node_create_statement(&labels, policy)?,
                    kind: EntityKind::NodeCreate { labels },
                    policy: Some(policy),
                    merge_key: Vec::new(),
                    rows,
                    skipped_unkeyed: 0,
                    skipped_dangling: 0,
                });
            }
            NodeDraft::Skipped { count } => {
                entities.push(PlannedEntity {
                    statement: String::new(),
                    kind: EntityKind::NodeSkipped { labels },
                    policy: None,
                    merge_key: Vec::new(),
                    rows: Vec::new(),
                    skipped_unkeyed: count,
                    skipped_dangling: 0,
                });
            }
        }
    }
    Ok((entities, node_groups))
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
    // Keyed by the facts the group uses: the type and the two endpoint
    // identities — keyedness included, so a keyed label and an unkeyed
    // label set are different groups however they spell. The endpoint
    // ids are the ONE mode decision: the group key, the row loop and
    // the emit all consult them, never two views of the same slots.
    // The value carries only what accumulates: rows, and the counter of
    // the rels an unkeyed-keyed group never writes.
    let mut rel_groups: BTreeMap<(String, EndpointId, EndpointId), RelGroup> = BTreeMap::new();
    for rel in &graph.rels {
        let start = node_groups
            .get(&rel.start)
            .expect("finish_graph kept only endpointed rels");
        let end = node_groups
            .get(&rel.end)
            .expect("finish_graph kept only endpointed rels");
        // The group key is built from the slots' stored ids directly;
        // the row loop below then borrows those same ids, so neither is
        // cloned a second time (LABS-932).
        let group = rel_groups
            .entry((rel.rel_type.clone(), start.id.clone(), end.id.clone()))
            .or_default();
        match (&start.id, &end.id) {
            (
                EndpointId::Keyed {
                    label: start_label,
                    merge_key: start_merge,
                },
                EndpointId::Keyed {
                    label: end_label,
                    merge_key: end_merge,
                },
            ) => {
                // Both endpoints key-addressable: the key values are
                // read off the nodes the slots carry — the remote is
                // never asked to guess (ADR-0401 §2) — so every miss
                // below is a missing key property.
                let start_key = keyed_props(start_label, &start.node.properties, start_merge)?;
                let end_key = keyed_props(end_label, &end.node.properties, end_merge)?;
                // Keyed (the default): merge on the resolved key when
                // one exists — a --rel-key override, else the type's
                // relationship constraint — and fall back to structural
                // when neither does. The one derivation feeds both this
                // row's key map and the group's merge key, so the two
                // can never desync.
                let row = match resolved_rel_key(keys, opts, &rel.rel_type) {
                    Some(props) => EntityRow::Rel {
                        start: start_key,
                        end: end_key,
                        key: keyed_props(&rel.rel_type, &rel.properties, props)?,
                        props: rel.properties.clone(),
                    },
                    None => EntityRow::RelUnkeyed {
                        start: start_key,
                        end: end_key,
                        props: rel.properties.clone(),
                    },
                };
                group.rows.push(row);
            }
            // An unkeyed endpoint on either side — the same explicit
            // combinations the emit matches, so a third identity variant
            // is a compile error in both loops, never a silently
            // skipped group: counted, never written.
            (EndpointId::Unkeyed(_), _) | (_, EndpointId::Unkeyed(_)) => {
                group.skipped += 1;
            }
        }
    }
    for ((rel_type, start_id, end_id), group) in rel_groups {
        // The same single derivation the row loop used (see
        // resolved_rel_key): structural forces the pair, create writes
        // without identity, and the keyed mode's key feeds both the
        // statement and the report's merge-key column.
        let resolved_key = resolved_rel_key(keys, opts, &rel_type);
        let rel_merge_key = resolved_key.map(<[String]>::to_vec).unwrap_or_default();
        // The mode is the group key's; each arm pushes its own entity —
        // a Keyed-keyed group's rows are non-empty by construction (the
        // group is created and its first row pushed in the same
        // iteration), so no empty-rows branch exists.
        match (&start_id, &end_id) {
            (
                EndpointId::Keyed {
                    label: start_label,
                    merge_key: start_key,
                },
                EndpointId::Keyed {
                    label: end_label,
                    merge_key: end_key,
                },
            ) => {
                // The scoped policy is resolved here, where a writing
                // kind is built — a counted group never resolves one
                // (LABS-932).
                let policy = opts.rels_update.resolve(&rel_type);
                entities.push(PlannedEntity {
                    statement: rel_statement(
                        &rel_type,
                        (start_label, start_key),
                        (end_label, end_key),
                        resolved_key,
                        opts.rel_mode,
                        policy,
                    )?,
                    kind: EntityKind::Rel {
                        rel_type,
                        start_label: start_label.clone(),
                        start_key: start_key.clone(),
                        end_label: end_label.clone(),
                        end_key: end_key.clone(),
                    },
                    policy: Some(policy),
                    merge_key: rel_merge_key,
                    rows: group.rows,
                    skipped_unkeyed: 0,
                    skipped_dangling: 0,
                });
            }
            // An unkeyed endpoint on either side: counted, never
            // written — the counted kind, never the writing one. The
            // combinations are matched explicitly so a third identity
            // variant is a compile error here, not a silently skipped
            // group.
            (EndpointId::Unkeyed(_), _) | (_, EndpointId::Unkeyed(_)) => {
                entities.push(PlannedEntity {
                    statement: String::new(),
                    kind: EntityKind::RelSkipped {
                        rel_type,
                        start: start_id.display(),
                        end: end_id.display(),
                    },
                    policy: None,
                    merge_key: Vec::new(),
                    rows: Vec::new(),
                    skipped_unkeyed: group.skipped,
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
            policy: None,
            merge_key: Vec::new(),
            statement: String::new(),
            rows: Vec::new(),
            skipped_unkeyed: 0,
            skipped_dangling: graph.dangling_rels,
        });
    }
    Ok(())
}

/// What accumulates in one rel group: the rows a keyed-keyed group
/// writes, and the counter of the rels an unkeyed-keyed group never
/// writes. The mode and every keyed fact live in the group key.
#[derive(Default)]
struct RelGroup {
    rows: Vec<EntityRow>,
    skipped: usize,
}

/// The reserved report name for labelless nodes (never an identifier).
pub(crate) const LABELLESS: &str = "(no label)";

/// The reserved report name for the dangling-rels bucket.
pub(crate) const DANGLING: &str = "(dangling)";

/// The resolved key of one relationship type under the plan's mode:
/// the keyed mode consults the type's key (override or constraint),
/// every other mode resolves nothing. One derivation, consulted by the
/// row loop and the group loop alike.
fn resolved_rel_key<'a>(
    keys: &'a MergeKeys,
    opts: &PlanOpts,
    rel_type: &str,
) -> Option<&'a [String]> {
    (opts.rel_mode == RelMode::Keyed)
        .then(|| keys.rel_key(rel_type))
        .flatten()
}

/// The keyed properties of one element, read in the merge key's order; a
/// missing key property fails closed naming the entity and the property
/// (a merge key the data does not satisfy is a local integrity error, not
/// a silent partial push).
fn keyed_props(
    entity: &str,
    props: &BTreeMap<String, Value>,
    key: &[String],
) -> Result<BTreeMap<String, Value>, LoadError> {
    let mut map = BTreeMap::new();
    for prop in key {
        let value = props
            .get(prop)
            .cloned()
            .ok_or_else(|| missing_key_prop(entity, prop))?;
        map.insert(prop.clone(), value);
    }
    Ok(map)
}

/// The merge-key-missing refusal, naming the entity and the property.
fn missing_key_prop(entity: &str, prop: &str) -> LoadError {
    LoadError::Push(format!(
        "merge-key property {prop:?} is missing on a {entity} entity in the local scope"
    ))
}

#[cfg(test)]
mod tests;
