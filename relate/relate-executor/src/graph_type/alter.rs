//! CIP-176/182/183: the `ALTER CURRENT GRAPH TYPE` per-verb body algebra.
//!
//! Each op is a pure `GraphTypeBody -> GraphTypeBody` transformation with
//! the verb's validation folded in; the executor's catalog handler
//! (`exec::graph_type_schema::alter_current_graph_type`) keeps only the
//! snapshot/redo orchestration and the derived-constraint catalog sync.
//! Per ADR-0489 these are statement-shape rejections: every validation
//! failure is `Error::syntax` (Neo4j's `SyntaxError` class, rendered under
//! the `syntax error:` prefix), unlike the pre-ADR variants whose class
//! depended on which constructor the site happened to use.

use std::collections::{HashMap, HashSet};

use relate_core::{Error, PreviewFeatures, Result};
use relate_names::SmallName;
use relate_parser::{
    AlterGraphTypeVerb, GraphTypeBody, GraphTypeConstraintSpec, GraphTypeConstraintVerb,
    GraphTypeElementRef, GraphTypeNodeTypeSpec,
};

use super::{
    alias_tables, constraint_rank, edge_type_matches_exactly, is_bare_edge_type_ref,
    is_bare_node_type_ref, node_type_matches_exactly, resolve_ref_label, validate_graph_type_body,
    GraphTypeScope,
};

/// Applies one `ALTER CURRENT GRAPH TYPE` verb's body to the current body,
/// returning the resulting body. Pure body algebra: no catalog access, no
/// redo. `preview` is the store's `RELATE_PREVIEW`-resolved set
/// (ADR-0371): the merged-body validation threads it, so the graph-type
/// trio's stored-state gates (an `ADD` whose identifying label a stored
/// node element type already implies) fail closed with the same authored
/// error statement admission produces.
pub fn apply_verb(
    verb: AlterGraphTypeVerb,
    current: GraphTypeBody,
    body: &GraphTypeBody,
    preview: PreviewFeatures,
) -> Result<GraphTypeBody> {
    match verb {
        AlterGraphTypeVerb::Set => apply_set(body, preview),
        AlterGraphTypeVerb::Add => apply_add(current, body, preview),
        AlterGraphTypeVerb::Alter => apply_alter(current, body, preview),
        AlterGraphTypeVerb::Drop => apply_drop(current, body),
    }
}

/// `SET`: the new body replaces the current one wholesale (CIP-176), after
/// passing the CIP-178 "shall" rules.
pub fn apply_set(body: &GraphTypeBody, preview: PreviewFeatures) -> Result<GraphTypeBody> {
    validate_graph_type_body(body, preview, GraphTypeScope::Alteration, false)
        .map_err(|e| Error::syntax(e.to_string()))?;
    Ok(body.clone())
}

/// `ADD`: extends the current body with the new element types and
/// constraints, then validates the union.
///
/// Two rules make the union Neo4j-shaped:
///
/// - a constraint's `FOR` reference binds within the statement's *own* body:
///   a bare `(p)` that names no element type added by this `ADD` is
///   "graph type element referenced by 'p' not found", even when a stored
///   element type carries that alias (an alias is statement-local);
/// - re-adding a constraint the stored body already declares is a no-op, not
///   a duplicate — the incoming declaration replaces the identical stored one
///   before validation (Neo4j's `ADD` is idempotent for an existing rule).
pub fn apply_add(
    mut current: GraphTypeBody,
    body: &GraphTypeBody,
    preview: PreviewFeatures,
) -> Result<GraphTypeBody> {
    for constraint in &body.constraints {
        if let GraphTypeElementRef::AliasRef(alias) = &constraint.target {
            let declared = if constraint.is_edge {
                body.edge_types
                    .iter()
                    .any(|edge| edge.alias.as_ref() == Some(alias))
            } else {
                body.node_types
                    .iter()
                    .any(|node| node.alias.as_ref() == Some(alias))
            };
            if !declared {
                return Err(Error::syntax(format!(
                    "SchemaError: graph type element referenced by '{alias}' not found"
                )));
            }
        }
    }
    let declared_nodes: HashSet<&SmallName> = body
        .node_types
        .iter()
        .map(|node| &node.identifying_label)
        .collect();
    let declared_edges: HashSet<&SmallName> = body
        .edge_types
        .iter()
        .map(|edge| &edge.identifying_label)
        .collect();
    current.node_types.extend(body.node_types.iter().cloned());
    current.edge_types.extend(body.edge_types.iter().cloned());
    current.constraints.extend(body.constraints.iter().cloned());
    dedupe_constraints(&mut current, &declared_nodes, &declared_edges);
    validate_graph_type_body(&current, preview, GraphTypeScope::Alteration, false)
        .map_err(|e| Error::syntax(e.to_string()))?;
    Ok(current)
}

/// Drops every earlier constraint whose resolved key (entity side, label,
/// property set, kind) an incoming declaration repeats, keeping the incoming
/// one.
///
/// A re-statement is a no-op rather than a duplicate (Neo4j 22N65 otherwise)
/// when either the constraint is declared inline on an element type this
/// `ADD` itself declares -- the inline and standalone spellings are one rule
/// -- or the incoming reference is a plain (non-`=>`) reference to a label
/// the merged graph type identifies as an element type. A `=>`-qualified
/// re-statement of a *stored* element type designates it explicitly and is
/// rejected as equivalent (live corpus:
/// `GeneratedNegativeAddExamplesFromGraphTypeCIPs`).
fn dedupe_constraints(
    body: &mut GraphTypeBody,
    declared_nodes: &HashSet<&SmallName>,
    declared_edges: &HashSet<&SmallName>,
) {
    let (node_aliases, edge_aliases) = alias_tables(body);
    let node_types: HashSet<&SmallName> = body
        .node_types
        .iter()
        .map(|node| &node.identifying_label)
        .collect();
    let edge_types: HashSet<&SmallName> = body
        .edge_types
        .iter()
        .map(|edge| &edge.identifying_label)
        .collect();
    let key = |c: &GraphTypeConstraintSpec| -> Option<(bool, SmallName, Vec<SmallName>, u8)> {
        let table = if c.is_edge {
            &edge_aliases
        } else {
            &node_aliases
        };
        Some((
            c.is_edge,
            resolve_ref_label(&c.target, table)?,
            c.properties.clone(),
            constraint_rank(&c.kind),
        ))
    };
    let restates_element_type = |c: &GraphTypeConstraintSpec| -> bool {
        let Some((is_edge, label, ..)) = key(c) else {
            return false;
        };
        // An inline declaration on an element type this `ADD` itself declares
        // re-states that element type's rule, whatever reference form it
        // carries. Otherwise only a plain (non-`=>`) reference to a label the
        // merged graph type identifies as an element type re-states a rule;
        // the `=>` form on a stored element type is the 22N65 rejection.
        let declared = if is_edge {
            declared_edges.contains(&label)
        } else {
            declared_nodes.contains(&label)
        };
        if declared {
            return true;
        }
        if matches!(c.target, GraphTypeElementRef::InSitu { implies: true, .. }) {
            return false;
        }
        if is_edge {
            edge_types.contains(&label)
        } else {
            node_types.contains(&label)
        }
    };
    // Iterate newest-first: a kept re-statement suppresses the older copy of
    // the same rule it re-states (the stored inline declaration). A newer
    // constraint that does *not* re-state an element type never suppresses
    // anything, so a genuine duplicate still reaches validation.
    let mut restated: HashSet<(bool, SmallName, Vec<SmallName>, u8)> = HashSet::new();
    let mut kept: Vec<GraphTypeConstraintSpec> = Vec::with_capacity(body.constraints.len());
    for constraint in body.constraints.iter().rev() {
        if let Some(k) = key(constraint) {
            if restated.contains(&k) {
                continue;
            }
            if restates_element_type(constraint) {
                restated.insert(k);
            }
        }
        kept.push(constraint.clone());
    }
    kept.reverse();
    body.constraints = kept;
}

/// `ALTER`: replaces existing element types (matched by identifying label)
/// with their new declarations (CIP-183); constraints are not alterable.
///
/// A replaced element type whose alias disappears (or changes) leaves the
/// surviving edge endpoints and constraint targets that named the old alias
/// dangling. The alias is a statement-local name; the rule it pointed at is
/// the identifying label, so those references are rewritten in place before
/// validation -- that is what lets an `ALTER` that re-declares `(p:Person)`
/// as `(:Person)` keep the stored `(p)-[:KNOWS]->(p)` edge.
pub fn apply_alter(
    current: GraphTypeBody,
    body: &GraphTypeBody,
    preview: PreviewFeatures,
) -> Result<GraphTypeBody> {
    if !body.constraints.is_empty() {
        return Err(Error::syntax(
            "SchemaError: constraints are not supported in ALTER",
        ));
    }
    let replaced = replaced_aliases(&current, &body.node_types, &body.edge_types);
    let mut merged = current;
    upsert_element_type(
        &mut merged.node_types,
        &body.node_types,
        |node| &node.identifying_label,
        "SchemaError: node element type does not exist",
    )?;
    upsert_element_type(
        &mut merged.edge_types,
        &body.edge_types,
        |edge| &edge.identifying_label,
        "SchemaError: relationship element type does not exist",
    )?;
    rewrite_alias_refs(&mut merged, &replaced);
    validate_graph_type_body(&merged, preview, GraphTypeScope::Alteration, false)
        .map_err(|e| Error::syntax(e.to_string()))?;
    Ok(merged)
}

/// The `old alias -> identifying label` map for every element type the
/// incoming `ALTER` body re-declares under a different (or no) alias.
fn replaced_aliases(
    current: &GraphTypeBody,
    nodes: &[GraphTypeNodeTypeSpec],
    edges: &[relate_parser::GraphTypeEdgeTypeSpec],
) -> HashMap<SmallName, SmallName> {
    let mut replaced = HashMap::new();
    for node in nodes {
        if let Some(old) = current
            .node_types
            .iter()
            .find(|old| old.identifying_label == node.identifying_label)
            .and_then(|old| old.alias.as_ref())
        {
            if node.alias.as_ref() != Some(old) {
                replaced.insert(old.clone(), node.identifying_label.clone());
            }
        }
    }
    for edge in edges {
        if let Some(old) = current
            .edge_types
            .iter()
            .find(|old| old.identifying_label == edge.identifying_label)
            .and_then(|old| old.alias.as_ref())
        {
            if edge.alias.as_ref() != Some(old) {
                replaced.insert(old.clone(), edge.identifying_label.clone());
            }
        }
    }
    replaced
}

/// Rewrites every `AliasRef` naming a replaced alias into the in-situ
/// identifying label it stands for, in edge endpoints and constraint targets
/// alike. A rewrite is a pure delegation: the label is exactly what the alias
/// resolved to before the replacement.
fn rewrite_alias_refs(body: &mut GraphTypeBody, replaced: &HashMap<SmallName, SmallName>) {
    fn rewrite(reference: &mut GraphTypeElementRef, replaced: &HashMap<SmallName, SmallName>) {
        if let GraphTypeElementRef::AliasRef(alias) = reference {
            if let Some(label) = replaced.get(alias) {
                *reference = GraphTypeElementRef::InSitu {
                    alias: None,
                    label: label.clone(),
                    implies: false,
                };
            }
        }
    }
    for edge in &mut body.edge_types {
        rewrite(&mut edge.source, replaced);
        rewrite(&mut edge.target, replaced);
    }
    for constraint in &mut body.constraints {
        rewrite(&mut constraint.target, replaced);
    }
}

/// The `ALTER` upsert shared by node and edge element types: every incoming
/// spec must name an existing identifying label, then replaces it.
fn upsert_element_type<T: Clone>(
    existing: &mut Vec<T>,
    incoming: &[T],
    identifying_label: impl Fn(&T) -> &SmallName,
    missing_error: &'static str,
) -> Result<()> {
    for spec in incoming {
        let label = identifying_label(spec);
        if !existing.iter().any(|old| identifying_label(old) == label) {
            return Err(Error::syntax(missing_error));
        }
        existing.retain(|old| identifying_label(old) != label);
        existing.push(spec.clone());
    }
    Ok(())
}

/// Whether `node` is still referenced by a surviving element type — the one
/// predicate for both reference forms: an edge endpoint that names its label
/// (and whose edge type is not itself being dropped), or a CIP-266 node
/// element type that still implies it (NAGTE4: it cannot be dropped without
/// also dropping or re-declaring the child).
fn node_type_still_referenced(
    current: &GraphTypeBody,
    node: &GraphTypeNodeTypeSpec,
    dropping_edges: &HashSet<&SmallName>,
    node_names: &HashSet<&SmallName>,
    aliases: &HashMap<&SmallName, &SmallName>,
) -> bool {
    let names_label = |reference: &GraphTypeElementRef, sought: &SmallName| match reference {
        GraphTypeElementRef::Any => false,
        GraphTypeElementRef::AliasRef(alias) => {
            aliases.get(alias).is_some_and(|value| *value == sought)
        }
        GraphTypeElementRef::InSitu { label, .. } => label == sought,
    };
    let referenced_by_edge = current.edge_types.iter().any(|edge| {
        !dropping_edges.contains(&edge.identifying_label)
            && (names_label(&edge.source, &node.identifying_label)
                || names_label(&edge.target, &node.identifying_label))
    });
    let implied_by_survivor = current.node_types.iter().any(|survivor| {
        !node_names.contains(&survivor.identifying_label)
            && survivor.implied_labels.contains(&node.identifying_label)
    });
    referenced_by_edge || implied_by_survivor
}

/// `DROP`: removes the element types and constraints named in the drop body
/// (CIP-182). A dropped element type must exist and -- when given in more
/// than the bare reference form -- match the stored definition exactly; a
/// dropped node type must not still be referenced by a surviving edge type.
/// An independent constraint written outside the graph type is dropped by
/// the executor (which owns the catalog); this pure body algebra removes only
/// the body's own constraint elements, and the executor validates the names.
///
/// A dropped element type's alias may still be named by a surviving
/// constraint target (`CONSTRAINT n_constraint FOR (n)` beside
/// `(n:Label => ...)`): the alias is rewritten to its identifying label, so
/// the independent rule survives the element type exactly as Neo4j keeps it.
pub fn apply_drop(mut current: GraphTypeBody, body: &GraphTypeBody) -> Result<GraphTypeBody> {
    // A DROP body may name element types and constraints, never *define* a
    // constraint: the parser materializes an inline property constraint or a
    // trailing `REQUIRE` list on a dropped element type into `constraints`
    // (Neo4j 22NCF: "Constraint definitions are not supported in the ALTER
    // CURRENT GRAPH TYPE DROP operation.").
    if !body.constraints.is_empty() {
        return Err(Error::syntax(
            "Constraint definitions are not supported in the ALTER CURRENT GRAPH TYPE DROP \
             operation.",
        ));
    }
    let aliases: HashMap<&SmallName, &SmallName> = current
        .node_types
        .iter()
        .filter_map(|node| {
            node.alias
                .as_ref()
                .map(|alias| (alias, &node.identifying_label))
        })
        .collect();
    let dropping_edges: HashSet<&SmallName> = body
        .edge_types
        .iter()
        .map(|edge| &edge.identifying_label)
        .collect();
    let node_names: HashSet<&SmallName> = body
        .node_types
        .iter()
        .map(|n| &n.identifying_label)
        .collect();
    for node in &body.node_types {
        let Some(existing) = current
            .node_types
            .iter()
            .find(|old| old.identifying_label == node.identifying_label)
        else {
            return Err(Error::syntax(
                "SchemaError: node element type does not exist",
            ));
        };
        if !is_bare_node_type_ref(node) && !node_type_matches_exactly(node, existing) {
            return Err(Error::syntax(
                "GraphTypeError: node element type specified incorrectly",
            ));
        }
        if node_type_still_referenced(&current, node, &dropping_edges, &node_names, &aliases) {
            return Err(Error::syntax(
                "GraphTypeError: node element type is still referenced and cannot be dropped",
            ));
        }
    }
    for edge in &body.edge_types {
        let Some(existing) = current
            .edge_types
            .iter()
            .find(|old| old.identifying_label == edge.identifying_label)
        else {
            return Err(Error::syntax(
                "SchemaError: relationship element type does not exist",
            ));
        };
        if !is_bare_edge_type_ref(edge) && !edge_type_matches_exactly(edge, existing, &aliases) {
            return Err(Error::syntax(
                "GraphTypeError: edge element type specified incorrectly",
            ));
        }
    }
    // The aliases the dropped element types carried, mapped to the labels
    // their surviving references must keep resolving to.
    let mut dropped_aliases: HashMap<SmallName, SmallName> = HashMap::new();
    for node in &body.node_types {
        if let Some(alias) = current
            .node_types
            .iter()
            .find(|old| old.identifying_label == node.identifying_label)
            .and_then(|old| old.alias.clone())
        {
            dropped_aliases.insert(alias, node.identifying_label.clone());
        }
    }
    for edge in &body.edge_types {
        if let Some(alias) = current
            .edge_types
            .iter()
            .find(|old| old.identifying_label == edge.identifying_label)
            .and_then(|old| old.alias.clone())
        {
            dropped_aliases.insert(alias, edge.identifying_label.clone());
        }
    }
    let edge_names: HashSet<&SmallName> = body
        .edge_types
        .iter()
        .map(|e| &e.identifying_label)
        .collect();
    let constraint_names: HashSet<&SmallName> = body.drop_constraint_names.iter().collect();
    // A dropped element type owns the *dependent* constraints its inline
    // properties and trailing `REQUIRE` lists declared (property type /
    // existence: no name, targeting the owner with `=>`); they leave with it.
    // An inline KEY/UNIQUE is undesignated and survives its element type
    // (Neo4j keeps the rule when the type is dropped), unless the DROP body
    // names it -- and an unnamed rule is named by its generated hash.
    let owned_by_dropped = |constraint: &GraphTypeConstraintSpec| -> bool {
        if constraint.name.is_some()
            || !matches!(
                constraint.kind,
                GraphTypeConstraintVerb::Exists | GraphTypeConstraintVerb::PropertyType
            )
        {
            return false;
        }
        match &constraint.target {
            GraphTypeElementRef::InSitu {
                implies: true,
                label,
                ..
            } => {
                if constraint.is_edge {
                    edge_names.contains(label)
                } else {
                    node_names.contains(label)
                }
            }
            _ => false,
        }
    };
    // An unnamed rule is dropped by its generated name: resolve its target
    // label against the pre-drop alias tables and compare the hash the sync
    // stores it under. Computed before the element-type retain: resolving
    // against the post-drop tables would lose the alias of an element type
    // dropped in this same command, silently keeping the rule.
    let dropped_by_generated_name: Vec<bool> = {
        let (node_alias_labels, edge_alias_labels) = alias_tables(&current);
        current
            .constraints
            .iter()
            .map(|constraint| {
                if constraint.name.is_some() {
                    return false;
                }
                let table = if constraint.is_edge {
                    &edge_alias_labels
                } else {
                    &node_alias_labels
                };
                resolve_ref_label(&constraint.target, table).is_some_and(|label| {
                    label.with_str(|text| {
                        constraint_names.iter().any(|name| {
                            *name
                                == super::canonicalize::auto_constraint_name(constraint, text)
                                    .as_str()
                        })
                    })
                })
            })
            .collect()
    };
    current
        .node_types
        .retain(|n| !node_names.contains(&n.identifying_label));
    current
        .edge_types
        .retain(|e| !edge_names.contains(&e.identifying_label));
    let mut kept = Vec::with_capacity(current.constraints.len());
    for (index, constraint) in current.constraints.drain(..).enumerate() {
        let named_drop = constraint
            .name
            .as_ref()
            .is_some_and(|n| constraint_names.contains(n));
        if named_drop || owned_by_dropped(&constraint) || dropped_by_generated_name[index] {
            continue;
        }
        kept.push(constraint);
    }
    current.constraints = kept;
    rewrite_alias_refs(&mut current, &dropped_aliases);
    Ok(current)
}
