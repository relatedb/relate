//! `SHOW CURRENT GRAPH TYPE AS GRAPH` -- renders the current graph
//! type as a set of virtual nodes/relationships suitable for visualization
//! (mirroring `db.schema.visualization`), rather than the CIP-180 canonical
//! specification string plain `SHOW CURRENT GRAPH TYPE` reports (see
//! `graph_type::current_graph_type_specification`).
//!
//! Node element type -> `(:NodeElementType {label, properties, constraints})`
//! Non-identifying label -> `(:NodeLabel {label, constraints})`
//! Unconstrained relationship endpoint -> `(:AnyLabel {})`
//! Edge element type -> `[:RELATIONSHIP_ELEMENT_TYPE {relationshipType, properties, constraints}]`
//! Non-identifying relationship type -> `[:RELATIONSHIP_TYPE {relationshipType, constraints}]`
//! Implied label -> `[:IMPLIES]`
//!
//! Every synthesized entity gets a **negative** id -- real, store-backed
//! nodes/relationships always start at 0 and count up, so a negative id
//! unambiguously marks "no real backing". Several other parts of the engine
//! use that same signal to treat a write against one of these as a no-op
//! (`DELETE`, property `SET`/`REMOVE` -- see `clause::delete`/`clause::set`)
//! or a hard failure (label `SET`/`REMOVE`, `CREATE` off of one -- these
//! fail naturally already, since the target id can never be found in the
//! store, or are explicitly guarded in `clause::remove`), matching CIP-257's
//! "Write behavior on virtual entities" (consistent with
//! `db.schema.visualization`).
//!
//! `startNode()`/`endNode()` (`eval::function`) need to resolve a virtual
//! relationship's endpoints without a store lookup, since these entities
//! were never inserted into the store; this module registers each
//! synthesized node with the graph layer's virtual-node side table
//! (`overlay::register_virtual_node`), which `ExecGraph::node_value`
//! consults as a fallback once the primary store/overlay lookup misses.

use std::collections::{BTreeMap, HashMap, HashSet};

use indexmap::IndexMap;
use relate_model::identifier::quote_identifier_shaped;
use relate_model::CypherValue;
use relate_model::{Constraint, EntityKind};
use relate_names::SmallName;
use relate_parser::{
    render_canon, type_expr_to_canon, AnyNodeMode, GraphTypeBody, GraphTypeConstraintSpec,
    GraphTypeConstraintVerb, GraphTypeElementRef, GraphTypePropertyType,
};

use crate::catalog_meta::string_list;
use crate::graph_type::{
    alias_tables, edge_id_label_set, endpoint_identity, node_id_label_set, storage_constraint_rank,
    EndpointIdentity,
};
use crate::overlay::register_virtual_node;

/// Renders one declared property as `"name :: TYPE"` (CIP-257's `properties`
/// list entries).
///
/// Every name position in the display form goes through the parser's
/// shape-only display policy ([`quote_identifier_shaped`]): bare for a plain
/// identifier shape, backtick-quoted otherwise (with embedded backticks
/// doubled). CIP-257's worked examples use this lighter, non-canonical text
/// rendering rather than CIP-180's `canonicalize_graph_type_body` (fewer
/// backticks, `n`/`r`-renamed constraint variables, an injected `=>`) --
/// `prop1`, not `` `prop1` ``, and `` `prop-3` `` for a hyphenated name, while
/// a keyword spelling such as `when` stays bare because display text is not
/// re-parsed as a statement.
fn vis_property_text(p: &GraphTypePropertyType) -> Result<String, relate_core::Error> {
    let mut rendered = format!(
        "{} :: {}",
        p.name.with_str(quote_identifier_shaped),
        render_canon(&type_expr_to_canon(&p.value_type))
    );
    // The property string carries the default value
    // (`id :: UUID NOT NULL DEFAULT ON COMMIT uuid()`), through the one
    // canonical default-value rendering the graph-type body form uses.
    if let Some(expr) = &p.default_value {
        // The same persisted-bytes rule as every stored-spec consumer
        // (the poisoned-spec siblings): an unrenderable default is a
        // typed storage error through the family's one mapping, never
        // a panic or a raw syntax error here.
        rendered.push_str(
            &crate::graph_type::canonical_default_clause(expr)
                .map_err(crate::graph_type::persisted_derive_error)?,
        );
    }
    Ok(rendered)
}

fn vis_target_str(is_edge: bool, label: &str, identified: bool) -> String {
    let var = if is_edge { "r" } else { "n" };
    match (is_edge, identified) {
        (true, true) => format!("()-[{}:{} =>]->()", var, quote_identifier_shaped(label)),
        (true, false) => format!("()-[{}:{}]->()", var, quote_identifier_shaped(label)),
        (false, true) => format!("({}:{} =>)", var, quote_identifier_shaped(label)),
        (false, false) => format!("({}:{})", var, quote_identifier_shaped(label)),
    }
}

/// CIP-257's `REQUIRE` clause: a single property is written bare
/// (`n.prop`), more than one is parenthesized (`(n.prop1, n.prop2)`) --
/// unlike CIP-180's canonical form, which always parenthesizes.
fn vis_props_str(is_edge: bool, properties: &[SmallName]) -> String {
    let var = if is_edge { "r" } else { "n" };
    let items: Vec<String> = properties
        .iter()
        .map(|p| format!("{}.{}", var, p.with_str(quote_identifier_shaped)))
        .collect();
    if items.len() == 1 {
        items[0].clone()
    } else {
        format!("({})", items.join(", "))
    }
}

fn vis_verb_str(kind: &GraphTypeConstraintVerb, property_type: Option<&str>) -> String {
    match kind {
        GraphTypeConstraintVerb::Key => "IS KEY".to_string(),
        GraphTypeConstraintVerb::Unique => "IS UNIQUE".to_string(),
        GraphTypeConstraintVerb::Exists => "IS NOT NULL".to_string(),
        GraphTypeConstraintVerb::PropertyType => {
            format!("IS :: {}", property_type.unwrap_or_default())
        }
    }
}

/// Renders one graph-type-body `CONSTRAINT ...` element in CIP-257's
/// display form (see [`vis_property_text`]'s quoting note).
fn vis_constraint_text(
    c: &GraphTypeConstraintSpec,
    label: &SmallName,
    id_labels: &HashSet<&SmallName>,
) -> String {
    // Display rendering: the caller resolves the target once and passes
    // the label in -- the identity question and the rendering read one
    // resolution, never a re-derived or fabricated one.
    let target_str = label.with_str(|l| {
        let identified = id_labels.iter().any(|id| *id == l);
        vis_target_str(c.is_edge, l, identified)
    });
    let props_str = vis_props_str(c.is_edge, &c.properties);
    let property_type = c
        .property_type
        .as_ref()
        .map(|te| render_canon(&type_expr_to_canon(te)));
    let verb_str = vis_verb_str(&c.kind, property_type.as_deref());
    let name_part = c
        .name
        .as_ref()
        .map(|n| format!(" {}", n.with_str(quote_identifier_shaped)))
        .unwrap_or_default();
    format!("CONSTRAINT{name_part} FOR {target_str} REQUIRE {props_str} {verb_str}")
}

/// Renders an independently-created (store-only, not part of the current
/// graph type body) constraint the same way -- used for `extra_constraints`,
/// mirroring `graph_type::canonical_constraint_from_storage`: the one typed
/// storage-constraint renderer, in its display style.
fn vis_constraint_text_from_storage(c: &Constraint, identified: bool) -> String {
    crate::graph_type::render_storage_constraint(
        c,
        identified,
        &crate::graph_type::StorageConstraintStyle::DISPLAY,
    )
}

/// A registered node element type's own declared properties + the
/// undesignated constraints attached to it.
struct ElementEntry {
    properties: Vec<String>,
    constraints: Vec<String>,
}

/// A registered edge element type's own declared properties + the
/// undesignated constraints attached to it, borrowing the body's endpoint
/// refs.
struct EdgeElement<'a> {
    properties: Vec<String>,
    constraints: Vec<String>,
    source: &'a GraphTypeElementRef,
    target: &'a GraphTypeElementRef,
}

/// Allocates the next (more negative) virtual id.
fn next_id(counter: &mut i64) -> i64 {
    let id = *counter;
    *counter -= 1;
    id
}

/// Resolves (allocating if needed) the any-node id for one unconstrained
/// relationship endpoint, honoring `mode`: `Single` shares exactly one
/// any-node across the whole graph type; `Separate` (the default) shares
/// one any-node per relationship (both its own endpoints, if both are
/// unconstrained) but never across different relationships.
fn any_node_id(
    mode: AnyNodeMode,
    counter: &mut i64,
    single_any_id: &mut Option<i64>,
    this_rel_any: &mut Option<i64>,
    any_node_ids: &mut Vec<i64>,
) -> i64 {
    let slot = match mode {
        AnyNodeMode::Single => single_any_id,
        AnyNodeMode::Separate => this_rel_any,
    };
    if let Some(id) = slot {
        *id
    } else {
        let id = next_id(counter);
        *slot = Some(id);
        any_node_ids.push(id);
        id
    }
}

/// `extra_constraints` of one entity kind, in live Neo4j's graph-view
/// order: by the constraint's property list, then its kind (KEY, UNIQUE,
/// EXISTS, property type -- [`storage_constraint_rank`]), then name. A fresh
/// 2026.10.1 store renders a label's store-only constraints this way
/// whatever their creation order. The body's own constraints keep their
/// declared order -- they are the graph type, not catalog rows.
fn ordered_extras(extra_constraints: &[Constraint], kind: EntityKind) -> Vec<&Constraint> {
    let mut extras: Vec<&Constraint> = extra_constraints
        .iter()
        .filter(|c| c.entity_kind() == Ok(kind))
        .collect();
    extras.sort_by(|a, b| {
        (
            &a.properties,
            storage_constraint_rank(a.constraint_type),
            &a.name,
        )
            .cmp(&(
                &b.properties,
                storage_constraint_rank(b.constraint_type),
                &b.name,
            ))
    });
    extras
}

/// One element entry's constraint-text slot, so [`place_constraint`] can
/// place into either side's element map.
trait ConstraintsSlot {
    fn constraints_slot(&mut self) -> &mut Vec<String>;
}

impl ConstraintsSlot for ElementEntry {
    fn constraints_slot(&mut self) -> &mut Vec<String> {
        &mut self.constraints
    }
}

impl ConstraintsSlot for EdgeElement<'_> {
    fn constraints_slot(&mut self) -> &mut Vec<String> {
        &mut self.constraints
    }
}

/// One constraint text's placement, the one spelling of the identified-vs-
/// plain branch: the identified label's element-type entry keeps it (every
/// identifying label declares one, so the entry exists; if one is ever
/// absent the text is dropped); a plain label's map receives it.
fn place_constraint<V: ConstraintsSlot>(
    label: &str,
    identified: bool,
    text: String,
    elements: &mut BTreeMap<String, V>,
    plain: &mut BTreeMap<String, Vec<String>>,
) {
    if identified {
        if let Some(entry) = elements.get_mut(label) {
            entry.constraints_slot().push(text);
        }
    } else {
        plain.entry(label.to_string()).or_default().push(text);
    }
}

/// The node side's classified maps: the identifying labels' element-type
/// entries and the plain (non-identifying) labels' constraint texts.
struct NodeSide {
    element_types: BTreeMap<String, ElementEntry>,
    plain_labels: BTreeMap<String, Vec<String>>,
}

/// The edge side's: the identifying labels' element-type entries (borrowing
/// the body's endpoint refs) and the plain relationship types' texts.
struct EdgeSide<'a> {
    edge_elements: BTreeMap<String, EdgeElement<'a>>,
    plain_rel_types: BTreeMap<String, Vec<String>>,
}

/// One relationship's resolved spec: the flat record the output assembly
/// reads (the pre-split code carried this as a 7-tuple).
struct RelSpec {
    label: String,
    is_element: bool,
    rel_id: i64,
    src_id: i64,
    tgt_id: i64,
    properties: Vec<String>,
    constraints: Vec<String>,
}

/// Stage: classify the node side -- every node element type's entry, the
/// plain labels (implied labels, bare in-situ edge endpoints, constraints),
/// and each text placed through [`place_constraint`].
fn collect_node_side(
    body: &GraphTypeBody,
    extra_constraints: &[Constraint],
    node_alias_label: &HashMap<&SmallName, &SmallName>,
    node_id_labels: &HashSet<&SmallName>,
) -> Result<NodeSide, relate_core::Error> {
    let mut element_types: BTreeMap<String, ElementEntry> = BTreeMap::new();
    let mut plain_labels: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for nt in &body.node_types {
        let mut properties = Vec::with_capacity(nt.properties.len());
        for p in &nt.properties {
            properties.push(vis_property_text(p)?);
        }
        element_types.insert(
            nt.identifying_label.to_string(),
            ElementEntry {
                properties,
                constraints: Vec::new(),
            },
        );
        for implied in &nt.implied_labels {
            // CIP-266: an implied label that identifies another node
            // element type targets that type's own `NodeElementType`
            // node (it is already in `element_types` from its own
            // declaration) -- only a non-identifying implied label gets
            // a `NodeLabel` node.
            if node_id_labels.contains(implied) {
                continue;
            }
            plain_labels.entry(implied.to_string()).or_default();
        }
    }
    // A bare (non-`=>`) node endpoint reference on an edge type introduces
    // a plain label too, even with no other declaration or constraint of
    // its own.
    for et in &body.edge_types {
        for endpoint in [&et.source, &et.target] {
            if let GraphTypeElementRef::InSitu {
                implies: false,
                label,
                ..
            } = endpoint
            {
                if !node_id_labels.contains(label) {
                    plain_labels.entry(label.to_string()).or_default();
                }
            }
        }
    }
    for c in body.constraints.iter().filter(|c| !c.is_edge) {
        // The any type contributes no node-side constraint; an alias that
        // names no declared type is the poisoned-spec failure, never a
        // silent skip — the same three-state classification the endpoint
        // resolution reads, so the two cannot disagree.
        match endpoint_identity(&c.target, node_alias_label) {
            EndpointIdentity::Any => continue,
            EndpointIdentity::Unresolved => {
                return Err(relate_core::Error::storage(
                    "persisted graph type constraint reference names no declared node type",
                ));
            }
            EndpointIdentity::Identified { label, .. } => {
                let text = vis_constraint_text(c, &label, node_id_labels);
                label.with_str(|l| {
                    let identified = node_id_labels.iter().any(|id| *id == l);
                    place_constraint(l, identified, text, &mut element_types, &mut plain_labels);
                });
            }
        }
    }
    for c in ordered_extras(extra_constraints, EntityKind::Node) {
        let identified = node_id_labels.iter().any(|l| *l == c.label.as_str());
        let text = vis_constraint_text_from_storage(c, identified);
        place_constraint(
            &c.label,
            identified,
            text,
            &mut element_types,
            &mut plain_labels,
        );
    }
    Ok(NodeSide {
        element_types,
        plain_labels,
    })
}

/// Stage: classify the edge side -- every edge element type's entry and the
/// plain relationship types' texts.
fn collect_edge_side<'a>(
    body: &'a GraphTypeBody,
    extra_constraints: &[Constraint],
    edge_alias_label: &HashMap<&SmallName, &SmallName>,
    edge_id_labels: &HashSet<&SmallName>,
) -> Result<EdgeSide<'a>, relate_core::Error> {
    let mut edge_elements: BTreeMap<String, EdgeElement> = BTreeMap::new();
    for et in &body.edge_types {
        let mut properties = Vec::with_capacity(et.properties.len());
        for p in &et.properties {
            properties.push(vis_property_text(p)?);
        }
        edge_elements.insert(
            et.identifying_label.to_string(),
            EdgeElement {
                properties,
                constraints: Vec::new(),
                source: &et.source,
                target: &et.target,
            },
        );
    }
    let mut plain_rel_types: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in body.constraints.iter().filter(|c| c.is_edge) {
        // The any type contributes no edge-side constraint; an alias that
        // names no declared type is the poisoned-spec failure, never a
        // silent skip — the node side's rule, spelled the same way.
        let label = match endpoint_identity(&c.target, edge_alias_label) {
            EndpointIdentity::Any => continue,
            EndpointIdentity::Unresolved => {
                return Err(relate_core::Error::storage(
                    "persisted graph type constraint reference names no declared relationship type",
                ));
            }
            EndpointIdentity::Identified { label, .. } => label,
        };
        let text = vis_constraint_text(c, &label, edge_id_labels);
        label.with_str(|l| {
            let identified = edge_id_labels.iter().any(|id| *id == l);
            place_constraint(
                l,
                identified,
                text,
                &mut edge_elements,
                &mut plain_rel_types,
            );
        });
    }
    for c in ordered_extras(extra_constraints, EntityKind::Relationship) {
        let identified = edge_id_labels.iter().any(|l| *l == c.label.as_str());
        let text = vis_constraint_text_from_storage(c, identified);
        place_constraint(
            &c.label,
            identified,
            text,
            &mut edge_elements,
            &mut plain_rel_types,
        );
    }
    Ok(EdgeSide {
        edge_elements,
        plain_rel_types,
    })
}

/// Stage: allocate the named nodes' ids (negative, counting down) -- named
/// nodes first, because edge-endpoint resolution needs them.
fn allocate_label_ids(node_side: &NodeSide, counter: &mut i64) -> HashMap<String, i64> {
    let mut label_node_id: HashMap<String, i64> = HashMap::new();
    for label in node_side.element_types.keys() {
        label_node_id.insert(label.clone(), next_id(counter));
    }
    for label in node_side.plain_labels.keys() {
        label_node_id.insert(label.clone(), next_id(counter));
    }
    label_node_id
}

/// One classified node label's allocated id. For a body that passed
/// validation every referenced label is classified and allocated; the
/// renderer also consumes unvalidated persisted bytes
/// (`GraphTypeSpec::to_body()` from WAL/checkpoint), so a miss is a typed
/// storage error -- never a silently wrong (real, store-backed) node id.
fn classified_node_id(
    label: &str,
    label_node_id: &HashMap<String, i64>,
) -> Result<i64, relate_core::Error> {
    label_node_id.get(label).copied().ok_or_else(|| {
        relate_core::Error::storage(format!(
            "persisted graph type references node label `{label}` \
             the renderer did not classify and allocate"
        ))
    })
}

/// The endpoint resolution's per-run state: the shared any-node allocation
/// plus the label→id map -- one spelling of resolve-or-allocate, used by
/// both endpoints of every edge element type and (as any-endpoints) by the
/// plain relationship types.
struct EndpointResolver<'a> {
    node_alias_label: &'a HashMap<&'a SmallName, &'a SmallName>,
    label_node_id: &'a HashMap<String, i64>,
    mode: AnyNodeMode,
    counter: &'a mut i64,
    single_any_id: Option<i64>,
    any_node_ids: Vec<i64>,
}

impl EndpointResolver<'_> {
    /// One endpoint's node id: a labeled endpoint reads its allocated id
    /// ([`classified_node_id`]); an unconstrained endpoint allocates
    /// through [`any_node_id`], shared per `mode`.
    fn resolve(
        &mut self,
        endpoint: &GraphTypeElementRef,
        this_rel_any: &mut Option<i64>,
    ) -> Result<i64, relate_core::Error> {
        // The three-state identity keeps "the any type" distinct from a
        // dangling alias: unvalidated persisted bytes naming no declared
        // type fail typed here instead of silently matching `()`.
        match endpoint_identity(endpoint, self.node_alias_label) {
            EndpointIdentity::Any => Ok(any_node_id(
                self.mode,
                self.counter,
                &mut self.single_any_id,
                this_rel_any,
                &mut self.any_node_ids,
            )),
            EndpointIdentity::Unresolved => Err(relate_core::Error::storage(
                "persisted graph type endpoint reference names no declared node type",
            )),
            EndpointIdentity::Identified { label, .. } => {
                label.with_str(|text| classified_node_id(text, self.label_node_id))
            }
        }
    }
}

/// Stage: resolve every relationship's endpoints (allocating any-nodes as
/// needed) into the flat [`RelSpec`]s the output assembly reads.
fn resolve_rel_specs(
    edge_side: &EdgeSide<'_>,
    mode: AnyNodeMode,
    counter: &mut i64,
    label_node_id: &HashMap<String, i64>,
    node_alias_label: &HashMap<&SmallName, &SmallName>,
) -> Result<(Vec<RelSpec>, Vec<i64>), relate_core::Error> {
    let mut rel_specs: Vec<RelSpec> = Vec::new();
    let mut resolver = EndpointResolver {
        node_alias_label,
        label_node_id,
        mode,
        counter,
        single_any_id: None,
        any_node_ids: Vec::new(),
    };

    for (label, entry) in &edge_side.edge_elements {
        let rel_id = next_id(resolver.counter);
        let mut this_rel_any: Option<i64> = None;
        let src_id = resolver.resolve(entry.source, &mut this_rel_any)?;
        let tgt_id = resolver.resolve(entry.target, &mut this_rel_any)?;
        rel_specs.push(RelSpec {
            label: label.clone(),
            is_element: true,
            rel_id,
            src_id,
            tgt_id,
            properties: entry.properties.clone(),
            constraints: entry.constraints.clone(),
        });
    }
    for (label, constraints) in &edge_side.plain_rel_types {
        let rel_id = next_id(resolver.counter);
        let mut this_rel_any: Option<i64> = None;
        let src_id = resolver.resolve(&GraphTypeElementRef::Any, &mut this_rel_any)?;
        let tgt_id = resolver.resolve(&GraphTypeElementRef::Any, &mut this_rel_any)?;
        rel_specs.push(RelSpec {
            label: label.clone(),
            is_element: false,
            rel_id,
            src_id,
            tgt_id,
            properties: Vec::new(),
            constraints: constraints.clone(),
        });
    }
    Ok((rel_specs, resolver.any_node_ids))
}

/// Stage: assemble the `nodes` list -- any-nodes, then NodeElementType, then
/// NodeLabel (each alphabetical), every synthesized node registered with the
/// virtual-node side table.
fn assemble_nodes(
    node_side: &NodeSide,
    any_node_ids: &[i64],
    label_node_id: &HashMap<String, i64>,
) -> Result<Vec<CypherValue>, relate_core::Error> {
    let mut nodes: Vec<CypherValue> = Vec::with_capacity(
        any_node_ids.len() + node_side.element_types.len() + node_side.plain_labels.len(),
    );
    for id in any_node_ids {
        let v = CypherValue::Node {
            id: *id,
            labels: vec!["AnyLabel".to_string()],
            properties: IndexMap::new(),
        };
        register_virtual_node(*id, v.clone());
        nodes.push(v);
    }
    for (label, entry) in &node_side.element_types {
        let id = classified_node_id(label, label_node_id)?;
        let mut props = IndexMap::new();
        props.insert("label".to_string(), CypherValue::Str(label.clone()));
        props.insert(
            "properties".to_string(),
            string_list(entry.properties.iter().cloned()),
        );
        props.insert(
            "constraints".to_string(),
            string_list(entry.constraints.iter().cloned()),
        );
        let v = CypherValue::Node {
            id,
            labels: vec!["NodeElementType".to_string()],
            properties: props,
        };
        register_virtual_node(id, v.clone());
        nodes.push(v);
    }
    for (label, constraints) in &node_side.plain_labels {
        let id = classified_node_id(label, label_node_id)?;
        let mut props = IndexMap::new();
        props.insert("label".to_string(), CypherValue::Str(label.clone()));
        props.insert(
            "constraints".to_string(),
            string_list(constraints.iter().cloned()),
        );
        let v = CypherValue::Node {
            id,
            labels: vec!["NodeLabel".to_string()],
            properties: props,
        };
        register_virtual_node(id, v.clone());
        nodes.push(v);
    }
    Ok(nodes)
}

/// Stage: assemble the `relationships` list -- RELATIONSHIP_ELEMENT_TYPE,
/// then RELATIONSHIP_TYPE (each alphabetical, from the specs' own build
/// order), then IMPLIES.
fn assemble_relationships(
    rel_specs: &[RelSpec],
    body: &GraphTypeBody,
    label_node_id: &HashMap<String, i64>,
    counter: &mut i64,
) -> Result<Vec<CypherValue>, relate_core::Error> {
    let mut relationships: Vec<CypherValue> = Vec::with_capacity(rel_specs.len());
    for spec in rel_specs {
        let mut props = IndexMap::new();
        props.insert(
            "relationshipType".to_string(),
            CypherValue::Str(spec.label.clone()),
        );
        if spec.is_element {
            props.insert(
                "properties".to_string(),
                string_list(spec.properties.iter().cloned()),
            );
        }
        props.insert(
            "constraints".to_string(),
            string_list(spec.constraints.iter().cloned()),
        );
        let rel_type = if spec.is_element {
            "RELATIONSHIP_ELEMENT_TYPE"
        } else {
            "RELATIONSHIP_TYPE"
        };
        relationships.push(CypherValue::Relationship {
            id: spec.rel_id,
            start_id: spec.src_id,
            end_id: spec.tgt_id,
            rel_type: rel_type.to_string(),
            properties: props,
        });
    }
    for nt in &body.node_types {
        let src_id = nt
            .identifying_label
            .with_str(|text| classified_node_id(text, label_node_id))?;
        for implied in &nt.implied_labels {
            let tgt_id = implied.with_str(|text| classified_node_id(text, label_node_id))?;
            let id = next_id(counter);
            relationships.push(CypherValue::Relationship {
                id,
                start_id: src_id,
                end_id: tgt_id,
                rel_type: "IMPLIES".to_string(),
                properties: IndexMap::new(),
            });
        }
    }
    Ok(relationships)
}

/// Renders `body` (plus `extra_constraints` -- constraints in the store but
/// not part of `body` itself, from a plain `CREATE CONSTRAINT`) as CIP-257's
/// `nodes`/`relationships` column values. One stage per section: classify
/// both sides, allocate ids, resolve endpoints, assemble the two lists.
pub fn render_graph_type_as_graph(
    body: &GraphTypeBody,
    extra_constraints: &[Constraint],
    mode: AnyNodeMode,
) -> Result<(Vec<CypherValue>, Vec<CypherValue>), relate_core::Error> {
    let (node_alias_label, edge_alias_label) = alias_tables(body);
    let node_id_labels = node_id_label_set(body);
    let edge_id_labels = edge_id_label_set(body);

    let node_side = collect_node_side(body, extra_constraints, &node_alias_label, &node_id_labels)?;
    let edge_side = collect_edge_side(body, extra_constraints, &edge_alias_label, &edge_id_labels)?;

    let mut counter: i64 = -1;
    let label_node_id = allocate_label_ids(&node_side, &mut counter);
    let (rel_specs, any_node_ids) = resolve_rel_specs(
        &edge_side,
        mode,
        &mut counter,
        &label_node_id,
        &node_alias_label,
    )?;

    let nodes = assemble_nodes(&node_side, &any_node_ids, &label_node_id)?;
    let relationships = assemble_relationships(&rel_specs, body, &label_node_id, &mut counter)?;
    Ok((nodes, relationships))
}
