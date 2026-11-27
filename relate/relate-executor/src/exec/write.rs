//! `SET`/`REMOVE`/`DELETE` write clauses.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{is_virtual_id, Error, NodeId, RelId, Result, Value};
use relate_ir::{Expr, LabelsFunctionOp, RemoveItem, SetItem};
use relate_names::SmallName;

use crate::eval::{dynamic_label_names, dynamic_property_key, eval, resolve_write_labels};
use crate::graph::EntityId;
use crate::overlay::ExecGraph;
use crate::row::Row;

use crate::props::{admit_props, prepare_property_value, NullPolicy};

/// CIP-241: SET resolves every RHS value, dynamic key, label list, and
/// target against the *pre-clause* state first, then applies the
/// assignments in order. A dynamic target must not observe an earlier
/// assignment in the same SET clause (verified on Neo4j 2026.09.0:
/// `SET n['x']=n.y, n['y']=n.x` on {x:0, y:1} yields x=1, y=0).
///
/// The resolve and apply halves are separate (and `pub(crate)`) so a
/// composing arm can patch between them — the removes' own seam
/// (`ResolvedRemove` + `resolve_removes` + `apply_resolved_removes`);
/// the merge `ON CREATE` arm is the one composer (ADR-0695).
pub(crate) fn apply_sets(
    graph: &ExecGraph<'_>,
    row: &mut Row,
    items: &[SetItem],
    params: &BTreeMap<String, Value>,
) -> Result<()> {
    let resolved = resolve_sets(graph, row, items, params)?;
    apply_resolved_sets(graph, resolved)
}

/// One resolved write item's node-only entity-kind dispatch, spelled once
/// for the two label arms of [`apply_resolved_sets`] and
/// [`apply_resolved_removes`]: a live node runs its verb on the moved
/// payload; a relationship target fails with the one shared non-entity
/// write-target rejection ([`non_entity_target`], the label arms' static
/// class — the label verbs are node-only by contract).
///
/// The CIP-257 virtual-node guard is deliberately this mover's own, not a
/// rule of [`resolve_entity`]: labeling needs a *live* store entry, so a
/// virtual node (negative id — `is_virtual_id`, "Write behavior on virtual
/// entities") fails with "Unable to load NODE" here, while a property
/// write to one stays the store no-op the property verbs define. A
/// tombstoned (deleted-but-bound) node keeps its non-negative id, takes
/// the verb arm, and no-ops inside the store write.
fn apply_to_node<T>(
    entity: EntityId,
    err: &'static str,
    payload: T,
    on_node: impl FnOnce(NodeId, T),
) -> Result<()> {
    match entity {
        EntityId::Node(id) if is_virtual_id(id) => Err(Error::execution("Unable to load NODE")),
        EntityId::Node(id) => {
            on_node(id, payload);
            Ok(())
        }
        // A relationship target cannot carry labels: the shape's static
        // class, the same one a non-entity value earned before the
        // rejection moved to resolution time.
        EntityId::Rel(_) => Err(non_entity_target(err, TargetReject::Static)),
    }
}

/// The one non-entity write-target rejection, shared by both dispatch
/// helpers so the property arms (`SET x.p`, `REMOVE x.p`) and the label arms
/// (`SET x:Label`, `REMOVE x:Label`) cannot answer different classes.
///
/// The reference engine splits by target shape: a *general target
/// expression* (`SET (CASE … END).p`) is a compile-time SyntaxError-class
/// type mismatch (live-verified 2026.09; the corpus pins it in
/// `Set.feature` "An assignment target expression that is an integer is a
/// semantic error"), while a *variable-rooted* target — a bare variable,
/// `n.k`, or a container-index chain (`SET n[x][y]`) whose inner index
/// evaluates to a non-entity — is the runtime 22G03-family type error
/// (CIP-131 [15]/[16]). The shape is decided at resolution
/// ([`TargetReject::classify`], read by [`resolve_entity`]), where the
/// target expression is still visible; the apply step carries only the
/// resolved entity.
fn non_entity_target(err: &'static str, reject: TargetReject) -> Error {
    match reject {
        TargetReject::Runtime => Error::ty(err),
        TargetReject::Static => Error::syntax(err),
    }
}

/// How a property-shaped write target's non-entity rejection is classed
/// (REL8-1813). Live splits the two shapes: a *general target expression*
/// (`SET (CASE … END).p`) is the pinned compile-time SyntaxError-class
/// mismatch (`Set.feature` "An assignment target expression that is an
/// integer is a semantic error"), while a *variable-rooted* target — a
/// bare variable, `n.k`, or a container-index chain like `n[x][y]` whose
/// inner index evaluates to a non-entity — is the runtime 22G03-family
/// type error. The shape is visible only at resolution, so
/// [`resolve_entity`] raises the rejection there and the apply step
/// receives a typed entity.
#[derive(Clone, Copy)]
pub(crate) enum TargetReject {
    /// A general target expression: the pinned SyntaxError class.
    Static,
    /// A variable-rooted target: the runtime type error live reports.
    Runtime,
}

impl TargetReject {
    /// The class a non-entity rejection of this target shape carries.
    fn classify(target: &Expr) -> Self {
        // The front end's one root-variable walk decides the shape: a
        // target rooted in a variable (`n`, `n.k`, `n[x][y]`) is the
        // runtime shape, a general expression (`(CASE … END).p`,
        // `(1).p`) the pinned static one.
        if relate_ir::root_variable(target).is_some() {
            TargetReject::Runtime
        } else {
            TargetReject::Static
        }
    }
}

/// The resolved form of one SET item — the seam a composing arm (the
/// merge `ON CREATE` arm, ADR-0695) patches between the halves, the same
/// shape the removes expose (`ResolvedRemove` + `resolve_removes` +
/// `apply_resolved_removes`).
pub(crate) enum ResolvedSet {
    /// `SET n = <map>` -- full property replacement.
    ReplaceAllProps(EntityId, BTreeMap<String, Value>),
    /// `SET n.k = v` / `SET n[expr] = v` with the key pre-resolved.
    Prop(EntityId, String, Value),
    /// `SET n += <map>`.
    MergeMap(EntityId, BTreeMap<String, Value>),
    /// `SET LABEL(n) = ...` (CIP-162 dynamic labels).
    LabelsFunction(EntityId, Vec<String>, LabelsFunctionOp),
}

/// Resolve a write item's target expression to the entity id it names,
/// raising the non-entity rejection *here*, where the target's shape is
/// still visible — so the class is decided once, by the shape
/// ([`TargetReject`]), and the apply step carries a typed entity and no
/// policy. A null target yields `None` (Cypher's null-tolerant write: the
/// payload is dropped), and a non-entity value fails with the caller's
/// message under the caller's class.
fn resolve_entity(
    graph: &ExecGraph<'_>,
    row: &Row,
    entity: &Expr,
    err: &'static str,
    reject: TargetReject,
    params: &BTreeMap<String, Value>,
) -> Result<Option<EntityId>> {
    match eval(graph, row, entity, params)? {
        Value::Null => Ok(None),
        Value::Node(n) => Ok(Some(EntityId::Node(n.id))),
        Value::Relationship(r) => Ok(Some(EntityId::Rel(r.id))),
        _ => Err(non_entity_target(err, reject)),
    }
}

/// Resolve one write item's `n.k` / `n[expr]` property target against the
/// pre-clause state (CIP-241). The entity is evaluated first; a null
/// entity yields no target (`None`) and the key expression is never
/// evaluated for it, so a null target cannot raise the dynamic-key type
/// error. The key is the static name for `n.k`, the validated runtime
/// value for `n[expr]`. Any other target shape fails with the caller's
/// own expected-property error. Shared by SET and REMOVE so both clauses
/// observe identical target semantics.
fn resolve_property_target(
    graph: &ExecGraph<'_>,
    row: &Row,
    target: &Expr,
    err: &'static str,
    params: &BTreeMap<String, Value>,
) -> Result<Option<(EntityId, String)>> {
    /// How a property target's key resolves: the static name (`n.k`) or
    /// the runtime expression (`n[expr]`).
    enum KeySource<'a> {
        Static(&'a SmallName),
        Dynamic(&'a Expr),
    }
    let (expr, key) = match target {
        Expr::Property { expr, key, .. } => (expr, KeySource::Static(key)),
        Expr::DynamicProperty { expr, key } => (expr, KeySource::Dynamic(key)),
        _ => return Err(Error::ty(err)),
    };
    let Some(entity) = resolve_entity(
        graph,
        row,
        expr,
        err,
        TargetReject::classify(target),
        params,
    )?
    else {
        return Ok(None);
    };
    let key = match key {
        // The property key renders once, at the write it names.
        KeySource::Static(key) => key.to_string(),
        KeySource::Dynamic(key) => dynamic_property_key(eval(graph, row, key, params)?)?,
    };
    Ok(Some((entity, key)))
}

/// An entity value's current properties: refreshed from the graph when the
/// entity is still live there, otherwise the snapshot the value carries.
/// `None` for non-entity values.
fn current_properties(graph: &ExecGraph<'_>, value: &Value) -> Option<BTreeMap<String, Value>> {
    match value {
        Value::Node(n) => Some(match graph.node(n.id) {
            // The live node is freshly built, so its property map is uniquely
            // owned and moves out. A deleted node's snapshot map is shared.
            // `SET n = map` still wants the Cypher map type.
            Some(live) => Arc::unwrap_or_clone(live.properties).into(),
            None => (*n.properties).clone().into(),
        }),
        Value::Relationship(r) => Some(match graph.rel(r.id) {
            Some(live) => Arc::unwrap_or_clone(live.properties).into(),
            None => (*r.properties).clone().into(),
        }),
        _ => None,
    }
}

/// The two SET map forms: `=` replaces every property, `+=` merges. The
/// form owns everything the arms differ by — the entry null policy
/// ([`NullPolicy`]), what a null *source* coerces to, and the expected-map
/// error text — one table ([`SetMapForm::rules`]), so the three can never
/// disagree.
#[derive(Clone, Copy)]
enum SetMapForm {
    Replace,
    Merge,
}

/// What one [`SetMapForm`] decides, read from the one table
/// ([`SetMapForm::rules`]).
struct SetMapRules {
    /// The null-*entry* policy of the admission walk: `=` stores no null;
    /// `+=` keeps it — it records the key's removal (`merge_properties_*`
    /// runs it through the set verb).
    nulls: NullPolicy,
    /// Whether a null *source* coerces to the empty map: `SET n = null`
    /// clears; `+=` fails a null source like every other non-map source.
    null_source_clears: bool,
    /// The expected-map error text of the form.
    expected_map_error: &'static str,
}

impl SetMapForm {
    /// The one match over the form vocabulary.
    fn rules(self) -> SetMapRules {
        match self {
            SetMapForm::Replace => SetMapRules {
                nulls: NullPolicy::Drop,
                null_source_clears: true,
                expected_map_error: "SET all properties requires a map",
            },
            SetMapForm::Merge => SetMapRules {
                nulls: NullPolicy::Keep,
                null_source_clears: false,
                expected_map_error: "+= requires a map",
            },
        }
    }
}

/// One SET map item's source coercion and admission, spelled once for the
/// `=` and `+=` arms of [`resolve_sets`]: a Cypher map is the raw admission
/// input, an entity-shaped value re-reads its current properties as the
/// map (the openCypher TCK's `ON MATCH SET r = a` copy scenarios pin this;
/// live Neo4j 2026 rejects it, so the engine keeps the TCK floor), and
/// anything else fails with the form's expected-map error. The
/// [`SetMapForm`] owns the whole null semantics — the source half via
/// [`SetMapRules::null_source_clears`], the entry half via the [`NullPolicy`]
/// [`admit_props`] applies — so `=` and `+=` differ by exactly the form.
fn set_map_source(
    graph: &ExecGraph<'_>,
    v: Value,
    form: SetMapForm,
) -> Result<BTreeMap<String, Value>> {
    let rules = form.rules();
    let not_a_map = || Error::ty(rules.expected_map_error);
    let raw = match v {
        // By value: a uniquely-owned map Arc unwraps into the raw map
        // (zero-copy) instead of deep-cloning through a borrowed Value.
        Value::Map(m) => Arc::unwrap_or_clone(m),
        Value::Null if rules.null_source_clears => BTreeMap::new(),
        Value::Null => return Err(not_a_map()),
        v => current_properties(graph, &v).ok_or_else(not_a_map)?,
    };
    admit_props(raw, rules.nulls)
}

pub(crate) fn resolve_sets(
    graph: &ExecGraph<'_>,
    row: &Row,
    items: &[SetItem],
    params: &BTreeMap<String, Value>,
) -> Result<Vec<ResolvedSet>> {
    let mut resolved = Vec::with_capacity(items.len());
    for item in items {
        match item {
            SetItem::Property { target, value } => {
                // Every property target stores the single value (validated
                // and normalized — a mixed int/float list stores as
                // floats). Null transparency is the callee's contract
                // (ADR-0339), so no per-site null guard.
                let val = prepare_property_value(eval(graph, row, value, params)?)?;
                let resolved_target =
                    resolve_property_target(graph, row, target, "SET expected property", params)?;
                if let Some((entity, key)) = resolved_target {
                    resolved.push(ResolvedSet::Prop(entity, key, val));
                }
            }
            SetItem::ReplaceAll { target, value } => {
                // `SET n = <map>` (the front minted the variant where the
                // parser keeps the fact): a map replaces every property,
                // null clears them, any other entity-shaped value re-reads
                // its current properties as the replacement map.
                let val = eval(graph, row, value, params)?;
                let map = set_map_source(graph, val, SetMapForm::Replace)?;
                let Some(entity) = resolve_entity(
                    graph,
                    row,
                    target,
                    "SET map target must be a node or relationship",
                    TargetReject::Static,
                    params,
                )?
                else {
                    continue;
                };
                resolved.push(ResolvedSet::ReplaceAllProps(entity, map));
            }
            SetItem::MergeMap { target, value } => {
                let val = eval(graph, row, value, params)?;
                // `+=`: a null entry survives admission and records the
                // key's removal (see [`SetMapRules::nulls`]).
                let map = set_map_source(graph, val, SetMapForm::Merge)?;
                let Some(entity) = resolve_entity(
                    graph,
                    row,
                    target,
                    "+= target must be a node or relationship",
                    TargetReject::Static,
                    params,
                )?
                else {
                    continue;
                };
                resolved.push(ResolvedSet::MergeMap(entity, map));
            }
            SetItem::Labels { target, labels } => {
                let labels = resolve_write_labels(graph, row, params, labels)?;
                // `SET n :Label` is exactly LabelsFunction(Extend) — one
                // apply arm instead of two (round-3 review).
                let Some(entity) = resolve_entity(
                    graph,
                    row,
                    target,
                    "SET labels() requires a node",
                    TargetReject::Static,
                    params,
                )?
                else {
                    continue;
                };
                resolved.push(ResolvedSet::LabelsFunction(
                    entity,
                    labels,
                    LabelsFunctionOp::Extend,
                ));
            }
            SetItem::LabelsFunction { target, value, op } => {
                let labels = dynamic_label_names(graph, row, params, value)?;
                let Some(entity) = resolve_entity(
                    graph,
                    row,
                    target,
                    "SET labels() requires a node",
                    TargetReject::Static,
                    params,
                )?
                else {
                    continue;
                };
                resolved.push(ResolvedSet::LabelsFunction(entity, labels, *op));
            }
        }
    }
    Ok(resolved)
}

pub(crate) fn apply_resolved_sets(graph: &ExecGraph<'_>, resolved: Vec<ResolvedSet>) -> Result<()> {
    for item in resolved {
        match item {
            ResolvedSet::ReplaceAllProps(entity, map) => graph.replace_properties(entity, map),
            ResolvedSet::Prop(entity, key, val) => graph.set_property(entity, &key, val),
            ResolvedSet::MergeMap(entity, map) => graph.merge_properties(entity, &map),
            ResolvedSet::LabelsFunction(entity, labels, op) => apply_to_node(
                entity,
                "SET labels() requires a node",
                (labels, op),
                |id, (labels, op)| match op {
                    LabelsFunctionOp::Extend => graph.add_labels(id, &labels),
                    LabelsFunctionOp::Remove => graph.remove_labels(id, &labels),
                    LabelsFunctionOp::Overwrite => graph.set_labels(id, &labels),
                },
            )?,
        }
    }
    Ok(())
}

pub(crate) enum ResolvedRemove {
    /// `REMOVE n.k` / `REMOVE n[expr]`, the entity already resolved.
    Property(EntityId, String),
    Labels(EntityId, Vec<String>),
}

pub(crate) fn resolve_removes(
    graph: &ExecGraph<'_>,
    row: &Row,
    items: &[RemoveItem],
    params: &BTreeMap<String, Value>,
) -> Result<Vec<ResolvedRemove>> {
    let mut resolved = Vec::with_capacity(items.len());
    for item in items {
        match item {
            RemoveItem::Property { target } => {
                let resolved_target = resolve_property_target(
                    graph,
                    row,
                    target,
                    "REMOVE expected property",
                    params,
                )?;
                if let Some((entity, key)) = resolved_target {
                    resolved.push(ResolvedRemove::Property(entity, key));
                }
            }
            RemoveItem::Labels { target, labels } => {
                let labels = resolve_write_labels(graph, row, params, labels)?;
                let Some(entity) = resolve_entity(
                    graph,
                    row,
                    target,
                    "REMOVE labels requires a node",
                    TargetReject::Static,
                    params,
                )?
                else {
                    continue;
                };
                resolved.push(ResolvedRemove::Labels(entity, labels));
            }
        }
    }
    Ok(resolved)
}

pub(crate) fn apply_resolved_removes(
    graph: &ExecGraph<'_>,
    resolved: Vec<ResolvedRemove>,
) -> Result<()> {
    for item in resolved {
        match item {
            ResolvedRemove::Property(entity, key) => graph.set_property(entity, &key, Value::Null),
            ResolvedRemove::Labels(entity, labels) => apply_to_node(
                entity,
                "REMOVE labels requires a node",
                labels,
                |id, labels| graph.remove_labels(id, &labels),
            )?,
        }
    }
    Ok(())
}

/// The DELETE clause's one delete set (LABS-819): every relationship the
/// clause's resolved values name leaves the graph before any node is
/// admitted, so a node's connectivity is judged by the admitting
/// [`crate::overlay::ExecGraph::delete_node`]'s `DeleteConnectedNode`
/// check — the one [`crate::graph::delete_connected_node_error`]
/// spelling the primitive's `Graph::delete_node` and the bulk preflight
/// also return — against exactly the relationships *outside* the set:
/// the same policy (and error) as a single-node `DELETE n`, with no
/// path-specific skip. The set spans the whole clause's row space, the
/// model the bulk delete path already preflights against.
pub(crate) fn delete_values(graph: &ExecGraph<'_>, values: &[Value], detach: bool) -> Result<()> {
    let mut rels: Vec<RelId> = Vec::new();
    let mut nodes: Vec<NodeId> = Vec::new();
    for value in values {
        collect_delete(value, &mut rels, &mut nodes)?;
    }
    for id in rels {
        graph.delete_rel(id);
    }
    for id in nodes {
        graph.delete_node(id, detach)?;
    }
    Ok(())
}

/// Fold one resolved DELETE value into the clause's relationship and node
/// sets: the runtime half of the one stated DELETE-target rule
/// (`relate-front`'s `check_delete_expr` doc) — this fold is the
/// single authority on what a *resolved* DELETE value is. Nodes,
/// relationships, paths (both halves), and lists recurse; null is
/// Cypher's no-op; anything else fails with the clause's type error —
/// before anything is deleted, keeping a malformed tail atomic.
fn collect_delete(value: &Value, rels: &mut Vec<RelId>, nodes: &mut Vec<NodeId>) -> Result<()> {
    match value {
        Value::Node(n) => nodes.push(n.id),
        Value::Relationship(r) => rels.push(r.id),
        Value::Path(p) => {
            rels.extend(p.rels.iter().map(|r| r.id));
            nodes.extend(p.nodes.iter().map(|n| n.id));
        }
        Value::Null => {}
        Value::List(xs) => {
            for x in xs.iter() {
                collect_delete(x, rels, nodes)?;
            }
        }
        _ => return Err(Error::ty("DELETE requires nodes, relationships, or paths")),
    }
    Ok(())
}
