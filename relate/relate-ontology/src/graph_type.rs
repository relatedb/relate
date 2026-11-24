//! ADR-0226 §2: the ontology → graph-type mapping behind
//! `db.ontology.apply`, re-based on main's document shapes by ADR-0364
//! §5, on the published 1.1.0 shapes by ADR-0487 §3, revised by
//! ADR-0667, with the declaration rules of ADR-0668.
//!
//! [`to_graph_type_body`] compiles a validated [`OntologyDocument`] into
//! a CIP-176 [`GraphTypeBody`] — in the canonical form the parser's own
//! rewriters produce: element types carry no aliases, edge endpoints are
//! label references, and constraints are top-level definitions
//! referencing the element by identifying label — together with the
//! conversion's skip reports ([`GraphTypeMapping::skipped`]): the
//! descriptive constructs with no graph-type equivalent, each as
//! `"<what> on <where>"`. The body feeds `db.ontology.apply`'s
//! `apply_verb` / `derive_constraints_typed` pipeline and persists with
//! `GraphTypeSpec::from_body` (both in `relate-executor`, the only layer
//! that can see the data; ADR-0226 §3).
//!
//! The split follows the spec's enforcement table. **Enforceable** —
//! compiled into the body:
//!
//! - Node entries: one node type spec per entry with the identifying
//!   label (`labels.identifier`, the one form — validation desugars the
//!   `label` shorthand) and the implied labels; the local id never
//!   rides the body (label references resolve without aliases). One
//!   label, one node type (ADR-0668): a node type's identity is its
//!   identifying label, so a second entry declaring a declared label is
//!   a duplicate the mapping refuses by name.
//! - Property `type` tokens map through
//!   [`Neo4jType::mapping`](element::Neo4jType::mapping), the one
//!   exhaustive classification of the closed 39-token enum: the
//!   predefined scalars and temporal kinds map to the CIP-176 kind of
//!   the same name; `UUID` maps to CIP-246 `UUID`; the narrow numerics
//!   `FLOAT32` and `INTEGER8`/`INTEGER16`/`INTEGER32` widen to
//!   `FLOAT`/`INTEGER` for scalars and lists — the property type
//!   constraint still holds for narrower values — and keep their exact
//!   coordinate kinds inside `VECTOR<…>`; `LIST<X>` becomes
//!   `LIST<X NOT NULL>` (stored collections cannot hold nulls, so the
//!   inner type is a synonym, not a strengthening); the six `VECTOR<…>`
//!   tokens map to their coordinate kind with the property's
//!   `dimension`.
//! - A required property — `must_exist: true`, or listed by an `EXISTS`
//!   constraint — declares its type `NOT NULL`; a required property
//!   whose type the body cannot express (`ANY`, `LIST<ANY>`, no type at
//!   all, a `VECTOR<…>` without a dimension) still declares **`ANY NOT
//!   NULL`**, the graph-type existence alias. An optional property of
//!   such a type declares nothing, and either way the conversion
//!   reports it as skipped in the mapper's exact formats.
//! - `unique: true` / `key: true` → single-property unique / key
//!   constraint definitions named `<property's name>_<element's
//!   id>_<uniq|key>` (`key` subsumes `unique`); `KEY`/`UNIQUE` constraint
//!   objects → constraint definitions of the same kind over the listed
//!   properties' resolved names, composite and sorted by name when the
//!   list has more than one entry, keeping the object's `name` when
//!   present. `PROPERTY_TYPE` objects fail loudly: the body cannot
//!   express a property-type assertion and dropping it would
//!   under-enforce.
//! - Relationship entries → the edge type specs of the types the
//!   document declares once, both endpoints as label references: a
//!   target naming a node resolves to that node's identifying label (a
//!   label the target also carries must spell it out exactly), a
//!   label-only target is a plain label reference, and a target with
//!   neither names no node at all. A type declared across several
//!   distinct resolved endpoint pairs defines no graph type at all
//!   (ADR-0668): no spec, no endpoint/property/constraint rows, its
//!   relationships writable under any labels — the vocabulary alone
//!   seeds the load, and the conversion reports the type as skipped.
//!   An entry restating an earlier entry's resolved pair identically
//!   compiles once; one restating it with a different payload is an
//!   [`OntologyError::Mapping`] failure (ADR-0668).
//! - A node entry whose content is all unrepresentable — no property
//!   declarations, not even `ANY NOT NULL` ones, and no implied labels —
//!   has no node type spec to build: the element type is dropped with a
//!   skip report, and its constraints stay as definitions on the label.
//!
//! **Descriptive** fields are never compiled; each is reported as
//! skipped: element `aliases`, `description`, `reference`, a node's
//! optional labels, a relationship's endpoint cardinalities, and the
//! property-level `one_of`, `pattern`, `description`, `reference` and
//! `aliases`. The rest — the property, constraint and index `name`s,
//! every extension payload, the document metadata — is neither compiled
//! nor reported: the stored document is the sole source of truth, and
//! no graph is derived from it (ADR-0229 §3).
//!
//! Spec-valid constructs the graph-type body cannot express fail loudly
//! here rather than disappearing (ADR-0364 §5.4): a node with no
//! identifying label, two node entries sharing one identifying label
//! (one label, one node type, ADR-0668), a relationship entry with no
//! type, an endpoint naming no `nodes` entry or carrying a label the
//! referenced node does not have, a relationship entry restating an
//! earlier entry's resolved endpoint pair with a different declaration
//! (ADR-0668), an `EXISTS` constraint covering several properties or
//! naming an undeclared property, a `PROPERTY_TYPE` constraint, an
//! empty constraint property list, a constraint naming a property its
//! owner does not declare, an empty property name that would be
//! declared or asserted, and a non-positive vector dimension are all
//! [`OntologyError::Mapping`] errors naming the owner and construct.

mod element;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use indexmap::IndexMap;
use relate_names::SmallName;
use relate_parser::{
    GraphTypeBody, GraphTypeConstraintSpec, GraphTypeEdgeTypeSpec, GraphTypeElementRef,
    GraphTypeNodeTypeSpec, GraphTypePropertyType,
};

use crate::model::{NodeType, OntologyDocument, RelationshipTarget, RelationshipType};
use crate::OntologyError;

use element::{Element, ElementDecorations};

/// The conversion's result: the compiled CIP-176 body and the skip
/// reports — the descriptive constructs the conversion left out, each
/// as `"<what> on <where>"`, distinct and sorted.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphTypeMapping {
    /// The compiled graph-type body, in the parser's canonical form.
    pub body: GraphTypeBody,
    /// What the conversion skipped, distinct and sorted.
    pub skipped: Vec<String>,
}

/// Compile a validated ontology document into the CIP-176 graph-type
/// body `db.ontology.apply` sets on the database, together with the
/// conversion's skip reports (ADR-0226 §2, ADR-0364 §5, ADR-0487 §3,
/// ADR-0667, ADR-0668).
///
/// Relationship entries are converted first — a type-less entry or an
/// endpoint that resolves to no node is a relationship's own failure —
/// then node entries; the body's element order is each map's document
/// order.
///
/// # Errors
///
/// [`OntologyError::Mapping`] when the document is spec-valid but the
/// graph-type body cannot express it: a node entry with no identifying
/// label, two node entries sharing one identifying label (ADR-0668), a
/// relationship entry with no type, an endpoint naming no `nodes` entry
/// or carrying a label the referenced node does not have, a
/// relationship entry restating an earlier entry's resolved endpoint
/// pair with a different declaration (ADR-0668), a `PROPERTY_TYPE`
/// constraint, an `EXISTS` constraint covering several properties, a
/// constraint property the owner does not declare, an empty property
/// name that would be declared or asserted, two properties resolving to
/// one name, or a non-positive vector dimension (ADR-0364 §5.4).
pub fn to_graph_type_body(doc: &OntologyDocument) -> Result<GraphTypeMapping, OntologyError> {
    let mut skipped: Vec<String> = Vec::new();
    let mut edge_types = Vec::with_capacity(doc.relationships().len());
    let mut constraints = Vec::new();

    // The node entries by key: endpoints resolve their references
    // through this table, and every node is mapped once below.
    let mut nodes: HashMap<&str, &NodeType> = HashMap::with_capacity(doc.nodes().len());
    for entry in doc.nodes() {
        nodes.insert(entry.key.as_str(), entry);
    }

    // Relationship entries first, every endpoint resolved and grouped
    // by the entry's type (ADR-0668): a type's resolved endpoint pairs
    // decide whether it defines a graph type, and the verdict is
    // reached before any payload compiles — a skipped type's
    // declarations never refuse the document. The groups keep
    // first-declaration order, so the body's edge order is the entries'
    // document order.
    let mut by_type: IndexMap<&str, Vec<(&RelationshipType, ResolvedPair)>> =
        IndexMap::with_capacity(doc.relationships().len());
    for entry in doc.relationships() {
        let rel_type = entry.rel_type.as_deref().ok_or_else(|| {
            OntologyError::Mapping(format!(
                "Ontology relationship '{}' has no type.",
                entry.key
            ))
        })?;
        let pair = ResolvedPair {
            from: endpoint(entry.key.as_str(), "from", &entry.from, &nodes)?,
            to: endpoint(entry.key.as_str(), "to", &entry.to, &nodes)?,
        };
        by_type.entry(rel_type).or_default().push((entry, pair));
    }
    for (rel_type, records) in &by_type {
        let Some(compiled) = compile_declared_type(rel_type, records, &mut skipped)? else {
            // Several distinct resolved pairs (ADR-0668): the type
            // defines no graph type at all — no spec, no endpoint,
            // property or constraint rows — and the type-level report
            // is the type's one skip entry.
            skipped.push(format!(
                "relationship type {rel_type} is declared by multiple entries, none of which \
                 is applied"
            ));
            continue;
        };
        edge_types.push(compiled.spec);
        constraints.extend(compiled.constraints);
    }

    // Node entries: one spec per entry unless the entry's content is
    // all unrepresentable — then the element type is dropped with a
    // skip report and its constraints stay as definitions on the label.
    let mut node_types = Vec::with_capacity(doc.nodes().len());
    // The identifying label → first declaring entry key: the duplicate
    // gate and the refusal's named declarer in one table (ADR-0668).
    let mut declared_labels: HashMap<&str, &str> = HashMap::with_capacity(doc.nodes().len());
    for entry in doc.nodes() {
        let label = entry
            .identifying_label()
            .ok_or_else(|| no_identifying_label(&entry.key))?;
        // One label, one node type (ADR-0668): a node type's identity
        // is its identifying label, so a second entry declaring a
        // declared label is a duplicate the mapping refuses by name —
        // never a body the graph-type validation would refuse late and
        // generically.
        if let Some(first) = declared_labels.insert(label, entry.key.as_str()) {
            return Err(OntologyError::Mapping(format!(
                "Ontology node '{}' restates the identifying label of node '{}' ({}); one \
                 label, one node type",
                entry.key, first, label
            )));
        }
        let element = Element {
            key: entry.key.as_str(),
            is_edge: false,
            properties: &entry.properties,
            constraints: &entry.constraints,
        };
        // The one element-compilation prelude: declarations, resolution
        // tables and every element-level skip report (the optional
        // labels included, ADR-0667).
        let compiled = element.compile(ElementDecorations::from(entry), &mut skipped)?;
        let dropped = compiled.declarations.is_empty() && entry.labels.implied.is_empty();
        if dropped {
            skipped.push(format!(
                "element type {} has no representable content",
                entry.key
            ));
        } else {
            node_types.push(GraphTypeNodeTypeSpec {
                alias: None,
                identifying_label: SmallName::from_name(label),
                implied_labels: entry
                    .labels
                    .implied
                    .iter()
                    .map(|l| SmallName::from_name(l.as_str()))
                    .collect(),
                properties: compiled.declarations,
            });
        }
        // The target reference is built where the dropped decision was
        // made: a dropped element's constraints stay as definitions on
        // the label, aimed without implying a declared node type.
        constraints.extend(element.constraint_specs(
            &compiled.names,
            &compiled.required,
            label_ref(label, !dropped),
        )?);
    }

    // The mapper's one reported shape: distinct, sorted.
    skipped.sort();
    skipped.dedup();

    Ok(GraphTypeMapping {
        body: GraphTypeBody {
            node_types,
            edge_types,
            constraints,
            drop_constraint_names: Vec::new(),
        },
        skipped,
    })
}

/// One type's compiled declaration (ADR-0668): the edge type spec its
/// single declared pair builds and that pair's compiled constraints.
struct CompiledType {
    spec: GraphTypeEdgeTypeSpec,
    constraints: Vec<GraphTypeConstraintSpec>,
}

/// One type group's decision (ADR-0668): `None` when the type is
/// declared across more than one endpoint pair — it defines no graph
/// type at all, so nothing of it reaches the body; the
/// conflicting-restatement refusal when a later entry restates the pair
/// with a different declaration; the compiled declaration when the type
/// declares exactly one pair (an identical restatement compiles once).
/// The group's records arrive in document order, so the first is the
/// pair's declaration and each later record of the same pair restates
/// it — the first entry's spelling is the one the spec carries.
fn compile_declared_type<'a>(
    rel_type: &str,
    records: &[(&'a RelationshipType, ResolvedPair<'a>)],
    skipped: &mut Vec<String>,
) -> Result<Option<CompiledType>, OntologyError> {
    // A group is created by a push, so it is never empty.
    let (first, rest) = records
        .split_first()
        .expect("a group is created by a push, so it is never empty");
    if rest
        .iter()
        .any(|(_, pair)| pair.labels() != first.1.labels())
    {
        return Ok(None);
    }
    let &(first_entry, first_pair) = first;
    let declaration = compiled_payload(rel_type, first_entry, skipped)?;
    for &(entry, _) in rest {
        let restated = compiled_payload(rel_type, entry, skipped)?;
        if restated != declaration {
            return Err(OntologyError::Mapping(format!(
                "Ontology relationship '{}' restates the endpoint pair of relationship '{}' \
                 ({} from {} to {}) with a different declaration; one endpoint pair, one \
                 declaration",
                entry.key, first_entry.key, rel_type, first_pair.from.label, first_pair.to.label
            )));
        }
    }
    Ok(Some(CompiledType {
        spec: GraphTypeEdgeTypeSpec {
            alias: None,
            identifying_label: SmallName::from_name(rel_type),
            properties: declaration.properties,
            source: label_ref(first_pair.from.label, first_pair.from.implies),
            target: label_ref(first_pair.to.label, first_pair.to.implies),
        },
        constraints: declaration.constraints,
    }))
}

/// The compiled payload of one relationship declaration (ADR-0668):
/// the entry's property declarations and constraint specs — the one
/// structural comparison an identical restatement must match, so a
/// field added to the compiled declaration joins the identity by
/// construction. The entry's descriptive fields contribute their skip
/// entries on the way: neither a declaration's nor a restatement's are
/// applied, so each reports under its own id.
fn compiled_payload(
    rel_type: &str,
    entry: &RelationshipType,
    skipped: &mut Vec<String>,
) -> Result<PairPayload, OntologyError> {
    let element = Element {
        key: entry.key.as_str(),
        is_edge: true,
        properties: &entry.properties,
        constraints: &entry.constraints,
    };
    // The one element-compilation prelude (a relationship entry's
    // endpoint cardinalities report beside it — they are the entry's).
    let compiled = element.compile(ElementDecorations::from(entry), skipped)?;
    cardinality_skips(skipped, entry.key.as_str(), "from", &entry.from);
    cardinality_skips(skipped, entry.key.as_str(), "to", &entry.to);
    Ok(PairPayload {
        properties: compiled.declarations,
        constraints: element.constraint_specs(
            &compiled.names,
            &compiled.required,
            label_ref(rel_type, true),
        )?,
    })
}

/// The compiled payload of one declaration (ADR-0668): the entry's
/// property declarations and constraint specs, the unit the
/// restatement policy compares.
#[derive(PartialEq)]
struct PairPayload {
    properties: Vec<GraphTypePropertyType>,
    constraints: Vec<GraphTypeConstraintSpec>,
}

/// One resolved relationship endpoint (ADR-0668): the label the target
/// resolves to, and whether the reference names a declared element
/// type.
#[derive(Clone, Copy, PartialEq)]
struct ResolvedEndpoint<'a> {
    label: &'a str,
    implies: bool,
}

/// One relationship entry's resolved endpoint pair (ADR-0668): the two
/// resolved endpoints — the pair an entry declares, the restatement
/// policy's key and the per-type declaration count's unit.
#[derive(Clone, Copy, PartialEq)]
struct ResolvedPair<'a> {
    from: ResolvedEndpoint<'a>,
    to: ResolvedEndpoint<'a>,
}

impl ResolvedPair<'_> {
    /// The pair's identity (ADR-0668): the two endpoint labels — two
    /// spellings of one endpoint (a node reference and a label-only
    /// reference naming the same label) resolve to the same endpoint,
    /// and the reference's `implies` bit is an artifact of the
    /// spelling, not of the endpoint.
    fn labels(&self) -> (&str, &str) {
        (self.from.label, self.to.label)
    }
}

/// The skip entry one endpoint's cardinality contributes, in the
/// `cardinality on <relationship>.<end>` shape.
fn cardinality_skips(
    skips: &mut Vec<String>,
    rel_id: &str,
    end: &str,
    target: &RelationshipTarget,
) {
    let carried =
        target.count.is_some() || target.min_count.is_some() || target.max_count.is_some();
    if carried {
        skips.push(format!("cardinality on {rel_id}.{end}"));
    }
}

/// The mapping error for a node entry with no identifying label — the
/// one construction the endpoint resolution and the node loop share (a
/// spec-valid node can lack one; the converter fails it, ADR-0364
/// §5.4).
fn no_identifying_label(node_id: &str) -> OntologyError {
    OntologyError::Mapping(format!(
        "Ontology node '{node_id}' has no identifying label."
    ))
}

/// One resolved relationship endpoint: a target naming a node resolves
/// to that node's identifying label (a label the target also carries
/// must spell it exactly), a label-only target is a plain label
/// reference with no declared-type implication, and a target with
/// neither names no node at all. Whitespace-only `node` and `label`
/// values count as absent.
fn endpoint<'a>(
    rel_id: &str,
    end: &str,
    target: &'a RelationshipTarget,
    nodes: &HashMap<&'a str, &'a NodeType>,
) -> Result<ResolvedEndpoint<'a>, OntologyError> {
    fn carried(value: &Option<String>) -> Option<&str> {
        value.as_deref().filter(|value| !value.trim().is_empty())
    }
    let unknown = |node_id: &str| {
        OntologyError::Mapping(format!(
            "Ontology relationship '{rel_id}' references unknown '{end}' node '{node_id}'."
        ))
    };
    // The node reference resolves first, before the label split: a
    // target naming a node maps to that node's identifying label (an
    // explicit label must spell it exactly), and a target naming no node
    // is a plain label reference — or names nothing at all.
    match carried(&target.node) {
        Some(node_id) => {
            let node = nodes.get(node_id).ok_or_else(|| unknown(node_id))?;
            let identifier = node
                .identifying_label()
                .ok_or_else(|| no_identifying_label(node_id))?;
            if let Some(label) = carried(&target.label) {
                if label != identifier {
                    return Err(OntologyError::Mapping(format!(
                        "Ontology relationship '{rel_id}' references '{end}' node '{node_id}' \
                         with label '{label}', which the node does not have."
                    )));
                }
            }
            Ok(ResolvedEndpoint {
                label: identifier,
                implies: true,
            })
        }
        None => match carried(&target.label) {
            Some(label) => Ok(ResolvedEndpoint {
                label,
                implies: false,
            }),
            None => Err(unknown("")),
        },
    }
}

/// An endpoint or constraint label reference in the parser's canonical
/// `(:Label =>)` shape: no alias, and `implies` set when the reference
/// names a declared element type.
fn label_ref(label: &str, implies: bool) -> GraphTypeElementRef {
    GraphTypeElementRef::InSitu {
        alias: None,
        label: SmallName::from_name(label),
        implies,
    }
}
