//! `SET`/`REMOVE`/`DELETE` write clauses.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{is_virtual_id, Error, NodeId, RelId, Result, Value};
use relate_ir::{Expr, LabelsFunctionOp, RemoveItem, SetItem};
use relate_names::SmallName;

use crate::eval::{dynamic_label_names, dynamic_property_key, eval, resolve_write_labels};
use crate::overlay::ExecGraph;
use crate::row::Row;

use crate::props::{admit_props, prepare_property_value, NullPolicy};

/// CIP-241: SET resolves every RHS value, dynamic key, label list, and
/// target against the *pre-clause* state first, then applies the
/// assignments in order. A dynamic target must not observe an earlier
/// assignment in the same SET clause (verified on Neo4j 2026.09.0:
/// `SET n['x']=n.y, n['y']=n.x` on {x:0, y:1} yields x=1, y=0).
pub(crate) fn apply_sets(
    graph: &ExecGraph<'_>,
    row: &mut Row,
    items: &[SetItem],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<()> {
    let resolved = resolve_sets(graph, row, items, params, cypher_version)?;
    apply_resolved_sets(graph, resolved)
}

/// One resolved assignment's entity-kind dispatch, spelled once for the
/// four property-shaped arms of [`apply_resolved_sets`] and
/// [`apply_resolved_removes`]: a node or relationship runs its verb on
/// the moved payload, a null target is a no-op (Cypher's null-tolerant
/// SET/REMOVE, payload dropped), anything else fails with the one shared
/// non-entity write-target rejection ([`non_entity_target`]).
fn apply_to_entity<T>(
    entity: Value,
    err: &'static str,
    payload: T,
    on_node: impl FnOnce(NodeId, T),
    on_rel: impl FnOnce(RelId, T),
) -> Result<()> {
    match entity {
        Value::Node(n) => {
            on_node(n.id, payload);
            Ok(())
        }
        Value::Relationship(r) => {
            on_rel(r.id, payload);
            Ok(())
        }
        Value::Null => Ok(()),
        _ => Err(non_entity_target(err)),
    }
}

/// One resolved write item's node-only entity-kind dispatch, spelled once
/// for the two label arms of [`apply_resolved_sets`] and
/// [`apply_resolved_removes`]: a live node runs its verb on the moved
/// payload, a null target is a no-op (payload dropped), anything else
/// fails with the one shared non-entity write-target rejection
/// ([`non_entity_target`]). A CIP-257 virtual node (negative id —
/// `is_virtual_id`, "Write behavior on virtual entities") has no store
/// entry and cannot be labeled: the arm fails with "Unable to load NODE".
/// A tombstoned (deleted-but-bound) node keeps its non-negative id, takes
/// the verb arm, and no-ops inside the store write.
fn apply_to_node<T>(
    entity: Value,
    err: &'static str,
    payload: T,
    on_node: impl FnOnce(NodeId, T),
) -> Result<()> {
    match entity {
        Value::Node(n) if is_virtual_id(n.id) => Err(Error::execution("Unable to load NODE")),
        Value::Node(n) => {
            on_node(n.id, payload);
            Ok(())
        }
        Value::Null => Ok(()),
        _ => Err(non_entity_target(err)),
    }
}

/// The one non-entity write-target rejection, shared by both dispatch
/// helpers so the property arms (`SET x.p`, `REMOVE x.p`) and the label arms
/// (`SET x:Label`, `REMOVE x:Label`) cannot answer different classes.
///
/// The reference engine splits by shape, and the write helpers run after
/// target resolution, where that shape is no longer visible: a *general
/// target expression* (`SET (CASE … END).p`) is a compile-time
/// SyntaxError-class type mismatch (live-verified 2026.09; the corpus pins
/// it in `Set.feature` "An assignment target expression that is an integer
/// is a semantic error"), while a *variable* target is the runtime
/// data-exception 22N27 (`TypeError`). The pinned shape is the general
/// expression, so both helpers answer that SyntaxError class; the variable
/// target's 22N27 divergence is unpinned and left with the deferred
/// write-target typing work.
fn non_entity_target(err: &'static str) -> Error {
    Error::syntax(err)
}

enum ResolvedSet {
    /// `SET n = <map>` -- full property replacement.
    ReplaceAllProps(Value, BTreeMap<String, Value>),
    /// `SET n.k = v` / `SET n[expr] = v` with the key pre-resolved.
    Prop(Value, String, Value),
    /// `SET n += <map>`.
    MergeMap(Value, BTreeMap<String, Value>),
    /// `SET LABEL(n) = ...` (CIP-162 dynamic labels).
    LabelsFunction(Value, Vec<String>, LabelsFunctionOp),
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
    cypher_version: Option<u32>,
) -> Result<Option<(Value, String)>> {
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
    let entity = eval(graph, row, expr, params, cypher_version)?;
    if entity.is_null() {
        return Ok(None);
    }
    let key = match key {
        // The property key renders once, at the write it names.
        KeySource::Static(key) => key.to_string(),
        KeySource::Dynamic(key) => {
            dynamic_property_key(eval(graph, row, key, params, cypher_version)?)?
        }
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

/// One SET map item's source coercion and admission, spelled once for the
/// `=` and `+=` arms of [`resolve_sets`]: a Cypher map is the raw admission
/// input, an entity-shaped value re-reads its current properties as the
/// map (the openCypher TCK's `ON MATCH SET r = a` copy scenarios pin this;
/// live Neo4j 2026 rejects it, so the engine keeps the TCK floor), and
/// anything else fails with the arm's expected-map error (derived from the
/// policy — the pair can never disagree). The [`NullPolicy`] owns the
/// whole null semantics, both halves read from it — the entry half via
/// [`admit_props`], the source half via [`NullPolicy::null_source_map`] —
/// and the raw map is admitted with that same policy, so `=` and `+=`
/// differ by exactly the typed null policy and each arm sees the admitted
/// map.
fn set_map_source(
    graph: &ExecGraph<'_>,
    v: Value,
    nulls: NullPolicy,
) -> Result<BTreeMap<String, Value>> {
    let raw = match v {
        // By value: a uniquely-owned map Arc unwraps into the raw map
        // (zero-copy) instead of deep-cloning through a borrowed Value.
        Value::Map(m) => Arc::unwrap_or_clone(m),
        // The policy owns the null-source half: `Drop` coerces a null
        // source to the empty map (`SET n = null` clears); `KeepAsRemoval`
        // treats it like every other non-map source — the arm's error.
        Value::Null => match nulls.null_source_map() {
            Some(map) => map,
            None => return Err(Error::ty(expected_map_error(nulls))),
        },
        v => match current_properties(graph, &v) {
            Some(props) => props,
            None => return Err(Error::ty(expected_map_error(nulls))),
        },
    };
    admit_props(raw, nulls)
}

/// The arm's expected-map error text — a total function of the policy, so
/// the message can never disagree with the null semantics it travels with.
/// SET passes its two policies; the match policy's reader is the pattern
/// evaluator, whose text this arm states for totality.
fn expected_map_error(nulls: NullPolicy) -> &'static str {
    match nulls {
        NullPolicy::Drop => "SET all properties requires a map",
        NullPolicy::KeepAsRemoval => "+= requires a map",
        NullPolicy::KeepAsUnmatchable => "pattern properties must be a map",
    }
}

fn resolve_sets(
    graph: &ExecGraph<'_>,
    row: &Row,
    items: &[SetItem],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<ResolvedSet>> {
    let mut resolved = Vec::with_capacity(items.len());
    for item in items {
        match item {
            SetItem::Property { target, value } => {
                // Every property target stores the single value (validated
                // and normalized — a mixed int/float list stores as
                // floats). Null transparency is the callee's contract
                // (ADR-0339), so no per-site null guard.
                let val = prepare_property_value(eval(graph, row, value, params, cypher_version)?)?;
                let resolved_target = resolve_property_target(
                    graph,
                    row,
                    target,
                    "SET expected property",
                    params,
                    cypher_version,
                )?;
                if let Some((entity, key)) = resolved_target {
                    resolved.push(ResolvedSet::Prop(entity, key, val));
                }
            }
            SetItem::ReplaceAll { target, value } => {
                // `SET n = <map>` (the front minted the variant where the
                // parser keeps the fact): a map replaces every property,
                // null clears them, any other entity-shaped value re-reads
                // its current properties as the replacement map.
                let val = eval(graph, row, value, params, cypher_version)?;
                let map = set_map_source(graph, val, NullPolicy::Drop)?;
                resolved.push(ResolvedSet::ReplaceAllProps(
                    eval(graph, row, target, params, cypher_version)?,
                    map,
                ));
            }
            SetItem::MergeMap { target, value } => {
                let val = eval(graph, row, value, params, cypher_version)?;
                // `+=`'s null policy: a null entry survives admission and
                // records the key's removal (`merge_properties_*` runs it
                // through the set verb) — the one typed difference from
                // the `=` arm's `NullPolicy::Drop` above.
                let map = set_map_source(graph, val, NullPolicy::KeepAsRemoval)?;
                resolved.push(ResolvedSet::MergeMap(
                    eval(graph, row, target, params, cypher_version)?,
                    map,
                ));
            }
            SetItem::Labels { target, labels } => {
                let labels = resolve_write_labels(graph, row, params, labels, cypher_version)?;
                // `SET n :Label` is exactly LabelsFunction(Extend) — one
                // apply arm instead of two (round-3 review).
                resolved.push(ResolvedSet::LabelsFunction(
                    eval(graph, row, target, params, cypher_version)?,
                    labels,
                    LabelsFunctionOp::Extend,
                ));
            }
            SetItem::LabelsFunction { target, value, op } => {
                let labels = dynamic_label_names(graph, row, params, value, cypher_version)?;
                resolved.push(ResolvedSet::LabelsFunction(
                    eval(graph, row, target, params, cypher_version)?,
                    labels,
                    *op,
                ));
            }
        }
    }
    Ok(resolved)
}

fn apply_resolved_sets(graph: &ExecGraph<'_>, resolved: Vec<ResolvedSet>) -> Result<()> {
    for item in resolved {
        match item {
            ResolvedSet::ReplaceAllProps(entity, map) => apply_to_entity(
                entity,
                "SET map target must be a node or relationship",
                map,
                |id, m| graph.replace_properties_node(id, m),
                |id, m| graph.replace_properties_rel(id, m),
            )?,
            ResolvedSet::Prop(entity, key, val) => apply_to_entity(
                entity,
                "SET property target invalid",
                (key, val),
                |id, (k, v)| graph.set_property_node(id, &k, v),
                |id, (k, v)| graph.set_property_rel(id, &k, v),
            )?,
            ResolvedSet::MergeMap(entity, map) => apply_to_entity(
                entity,
                "+= target must be a node or relationship",
                &map,
                |id, m| graph.merge_properties_node(id, m),
                |id, m| graph.merge_properties_rel(id, m),
            )?,
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
    Property(Value, String),
    Labels(Value, Vec<String>),
}

pub(crate) fn resolve_removes(
    graph: &ExecGraph<'_>,
    row: &Row,
    items: &[RemoveItem],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
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
                    cypher_version,
                )?;
                if let Some((entity, key)) = resolved_target {
                    resolved.push(ResolvedRemove::Property(entity, key));
                }
            }
            RemoveItem::Labels { target, labels } => {
                let labels = resolve_write_labels(graph, row, params, labels, cypher_version)?;
                resolved.push(ResolvedRemove::Labels(
                    eval(graph, row, target, params, cypher_version)?,
                    labels,
                ));
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
            ResolvedRemove::Property(entity, key) => apply_to_entity(
                entity,
                "REMOVE property target invalid",
                key,
                |id, k| graph.set_property_node(id, &k, Value::Null),
                |id, k| graph.set_property_rel(id, &k, Value::Null),
            )?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::{exec_write, exec_write_uncommitted, write_scratch, Builder};
    use crate::graph::Graph;

    /// LABS-709: the runtime half of the shared dynamic-access rule rejects
    /// an empty or NUL-containing property key resolved from a *runtime*
    /// value (the compile-time fold cannot see `n.k`).
    #[test]
    fn runtime_dynamic_property_key_rejects_empty_and_nul_names() {
        for bad_key in ["", "a\0b"] {
            let builder = Builder::new();
            builder.prop_node(
                "holder",
                &["Holder"],
                &[("k", Value::String(bad_key.into()))],
            );
            let graph = builder.done();
            let scratch = write_scratch(&graph);
            let error = match exec_write(
                &graph,
                &scratch,
                "MATCH (n:Holder) WITH n, n.k AS prop_key SET n[prop_key] = 1",
            ) {
                Err(error) => error,
                Ok(_) => panic!("an empty/NUL property key must be rejected at runtime"),
            };
            assert!(
                error.to_string().contains(
                    "invalid name. A property key name cannot be empty or contain any null-bytes"
                ),
                "unexpected error: {error}"
            );
        }
    }

    /// A legitimately runtime-computed key keeps working: only the checks'
    /// *phase* moved for constants, never the dynamic feature itself.
    #[test]
    fn runtime_computed_property_key_still_writes_the_property() {
        let builder = Builder::new();
        builder.prop_node("holder", &["Holder"], &[("k", Value::String("x".into()))]);
        let graph = builder.done();
        let result = exec_write_uncommitted(
            &graph,
            "MATCH (n:Holder) WITH n, n.k AS prop_key SET n[prop_key] = 1 RETURN n.x AS v",
        );
        assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
    }

    /// A property list holding null is not storable (Neo4j 22N39), while a
    /// plain null assignment is the removal policy's business.
    #[test]
    fn set_property_rejects_a_list_holding_null() {
        let builder = Builder::new();
        builder.anon(&["A"]);
        let graph = builder.done();
        let error = match exec_write(
            &graph,
            &write_scratch(&graph),
            "MATCH (n:A) SET n.p = [1, null]",
        ) {
            Err(error) => error,
            Ok(_) => panic!("a list holding null must not be stored as a property"),
        };
        assert!(error.to_string().contains("InvalidPropertyType"), "{error}");
    }

    /// A write target whose value is not an entity is the one shared
    /// SyntaxError-class type mismatch — the pinned general-target shape
    /// (`Set.feature` "An assignment target expression that is an integer is
    /// a semantic error") — for the property and label arms alike. Asserted
    /// on the typed variant, never on the rendered prefix.
    #[test]
    fn non_entity_write_targets_share_the_one_typed_rejection() {
        let graph = Graph::default();
        for query in [
            "SET (CASE WHEN true THEN 42 END).name = 'neo4j'",
            "UNWIND [42] AS x SET x.p = 1",
            "UNWIND [42] AS x REMOVE x.p",
            "UNWIND [42] AS x SET x:Label",
            "UNWIND [42] AS x REMOVE x:Label",
        ] {
            let error = match exec_write(&graph, &write_scratch(&graph), query) {
                Err(error) => error,
                Ok(_) => panic!("{query}: a non-entity write target must be rejected"),
            };
            assert!(
                matches!(error, Error::Syntax(_)),
                "{query}: expected the shared syntax-class rejection, got: {error}"
            );
        }
    }

    /// The runtime label rule (STRING or LIST<STRING>) names the offending
    /// input type, and list elements are checked individually.
    #[test]
    fn runtime_dynamic_labels_reject_wrong_types_and_bad_names() {
        let cases: [(Value, &str); 4] = [
            (Value::Int(1), "Expected to be STRING or LIST<STRING>."),
            (
                Value::List(vec![Value::String("A".into()), Value::Int(1)].into()),
                "Expected to be STRING or LIST<STRING>.",
            ),
            (
                Value::String("".into()),
                "invalid name. A label name cannot be empty or contain any null-bytes",
            ),
            (
                Value::String("a\0b".into()),
                "invalid name. A label name cannot be empty or contain any null-bytes",
            ),
        ];
        for (value, expected) in cases {
            let builder = Builder::new();
            builder.prop_node("holder", &["Holder"], &[("label", value)]);
            let graph = builder.done();
            let scratch = write_scratch(&graph);
            let error = match exec_write(
                &graph,
                &scratch,
                "MATCH (n:Holder) WITH n, n.label AS lbl SET n:$(lbl)",
            ) {
                Err(error) => error,
                Ok(_) => panic!("a bad runtime dynamic label must be rejected"),
            };
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error}"
            );
        }
    }

    /// DELETE of a list bound in the row reads nested lists and paths in
    /// place: every entity is deleted once and the row keeps its list.
    #[test]
    fn delete_list_of_nodes_and_paths_keeps_the_row_list() {
        let g = Graph::default();
        let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
        let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
        g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
        g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
        let r = exec_write_uncommitted(
            &g,
            "MATCH p = (:A)-->(:B), (c:C) WITH [c] + [[p], null] AS xs \
             DELETE xs RETURN size(xs) AS n",
        );
        assert_eq!(r.rows, vec![vec![Value::Int(3)]]);
        assert_eq!(r.stats.nodes_deleted, 3);
        assert_eq!(r.stats.relationships_deleted, 1);
    }

    /// LABS-819: `DELETE p` admits its path nodes through the same
    /// `DeleteConnectedNode` policy as `DELETE n` — a path node whose
    /// relationships reach outside the clause's delete set fails the
    /// statement with the *same* error instead of being silently skipped.
    #[test]
    fn delete_path_fails_like_delete_n_on_outside_relationships() {
        fn graph_a_r_b_outside_c() -> Graph {
            let builder = Builder::new();
            let a = builder.anon(&["A"]);
            let b = builder.anon(&["B"]);
            let c = builder.anon(&["C"]);
            builder.rel(a, b, "R", &[]);
            builder.rel(b, c, "OUTSIDE", &[]);
            builder.done()
        }
        let g = graph_a_r_b_outside_c();
        let path_error =
            match exec_write(&g, &write_scratch(&g), "MATCH p = (:A)-[:R]->(:B) DELETE p") {
                Err(error) => error,
                Ok(_) => panic!("DELETE p must fail on a node with outside relationships"),
            };
        // The single-node spelling of the same graph state fails with the
        // identical message: one admission policy, no path-specific skip.
        let g = graph_a_r_b_outside_c();
        let node_error = match exec_write(&g, &write_scratch(&g), "MATCH (b:B) DELETE b") {
            Err(error) => error,
            Ok(_) => panic!("DELETE b on the same graph state must fail"),
        };
        assert_eq!(path_error.to_string(), node_error.to_string());
        assert!(
            path_error.to_string().contains(
                "ConstraintVerificationFailed: DeleteConnectedNode: Cannot delete node with \
                 relationships unless DETACH"
            ),
            "unexpected error: {path_error}"
        );
    }

    /// The Delete5::[7] graph: two `:User` nodes joined by a
    /// two-relationship cycle, shared by the two delete-set tests (one
    /// deletes through a collected list of paths, the other through the
    /// multi-row path match).
    fn two_user_cycle() -> Graph {
        let builder = Builder::new();
        let a = builder.anon(&["User"]);
        let b = builder.anon(&["User"]);
        builder.rel(a, b, "R", &[]);
        builder.rel(b, a, "R", &[]);
        builder.done()
    }

    /// The Delete5::[7] shape: two paths whose cross-relationships belong
    /// to the *same clause's* delete set — the clause's relationships all
    /// leave the graph before any node is admitted, so every node is
    /// deleted (nothing silently skipped, nothing wrongly failed).
    #[test]
    fn delete_paths_covering_all_relationships_delete_every_node() {
        let r = exec_write_uncommitted(
            &two_user_cycle(),
            "MATCH p = (:User)-[r]->(:User) \
             WITH {key: collect(p)} AS pathColls \
             DELETE pathColls.key[0], pathColls.key[1]",
        );
        assert_eq!(r.stats.nodes_deleted, 2);
        assert_eq!(r.stats.relationships_deleted, 2);
    }

    /// The delete set spans the whole clause, not one row: a multi-row
    /// path match whose rows cover each other's relationships deletes
    /// everything — the row-space set model the bulk delete path already
    /// uses for its own preflight.
    #[test]
    fn delete_path_set_spans_all_rows_of_the_clause() {
        let r = exec_write_uncommitted(
            &two_user_cycle(),
            "MATCH p = (:User)-[:R]->(:User) DELETE p",
        );
        assert_eq!(r.stats.nodes_deleted, 2);
        assert_eq!(r.stats.relationships_deleted, 2);
    }

    /// A deleted-but-bound relationship is tombstoned like a node: the
    /// `r {.*}` map projection raises the same `DeletedEntityAccess` the
    /// sibling property arm (`r.since`) raises, not a silent empty map —
    /// one projection, one answer about one deleted value.
    #[test]
    fn deleted_relationship_all_properties_projection_raises_deleted_access() {
        let b = Builder::new();
        let a = b.anon(&["A"]);
        let b_node = b.anon(&["B"]);
        b.rel(a, b_node, "R", &[("since", Value::Int(2020))]);
        let g = b.done();
        let error = match exec_write(
            &g,
            &write_scratch(&g),
            "MATCH (:A)-[r:R {since: 2020}]->(:B) DELETE r RETURN r {.*} AS p",
        ) {
            Err(error) => error,
            Ok(_) => panic!("a deleted relationship projection must not project an empty map"),
        };
        assert!(error.to_string().contains("DeletedEntityAccess"), "{error}");
    }
}
