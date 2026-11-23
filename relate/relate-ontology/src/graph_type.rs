//! ADR-0226 §2: the ontology → graph-type mapping behind
//! `db.ontology.apply`, re-based on main's document shapes by ADR-0364
//! §5 and on the published 1.1.0 shapes by ADR-0487 §3.
//!
//! [`to_graph_type_body`] compiles a validated [`OntologyDocument`] into a
//! CIP-176 [`GraphTypeBody`] — the same parser model the
//! `ALTER CURRENT GRAPH TYPE SET` path consumes, so `db.ontology.apply`
//! can feed it to `apply_verb` / `derive_constraints_typed` and persist it
//! with `GraphTypeSpec::from_body` (both in `relate-executor`, the
//! only layer that can see the data; ADR-0226 §3).
//!
//! The split follows the spec's enforcement table. **Enforceable** —
//! compiled into the body:
//!
//! - Node entries: one node type spec per entry with the identifying
//!   label (`labels.identifier`, the one form — validation desugars the
//!   `label` shorthand and leaves a null identifier as main allows), the
//!   local id as the element alias (endpoint and constraint references
//!   need it), implied labels from `labels.implied`, and the typed
//!   property declarations.
//! - Property `type` tokens map through [`Neo4jType::mapping`], the one
//!   exhaustive classification of main's closed 39-token enum: `ANY`,
//!   `LIST<ANY>` and an absent `type` declare nothing; the predefined
//!   scalars and temporal kinds map to the CIP-176 kind of the same name;
//!   `UUID` maps to CIP-246 `UUID`; `LIST<X>` becomes `LIST<X NOT NULL>`;
//!   the six `VECTOR<…>` tokens map to their coordinate kind with the
//!   property's `dimension`. `FLOAT32`, `INTEGER8`, `INTEGER16`,
//!   `INTEGER32` and their list forms have no plain CIP-176 counterpart
//!   and fail loudly — never a silent widening or a declare-nothing.
//! - `must_exist: true` → `NOT NULL` on the declaration, or a standalone
//!   `REQUIRE <el>.<p> IS NOT NULL` graph-type constraint when the
//!   property declares no type. `unique: true` / `key: true` →
//!   single-property unique / key constraints; `key` subsumes `unique`.
//! - Constraint objects → graph-type constraints of the same kind over the
//!   listed properties — composite when the list has more than one entry —
//!   keeping the object's `name` when present. A constraint object that
//!   restates a shorthand flag exactly (same kind, same single property)
//!   replaces the flag-derived constraint, so the object is what survives
//!   — with its name. `PROPERTY_TYPE` objects fail loudly: the body cannot
//!   express a property-type assertion and dropping it would under-enforce.
//! - Relationship entries → the edge type specs of the types the
//!   document declares once, endpoints resolved through the referenced
//!   node entries' aliases. A type declared across several endpoint
//!   pairs defines no graph type at all (ADR-0668): no spec, no
//!   endpoint/property/constraint rows, its relationships writable
//!   under any labels — the vocabulary alone seeds the load. An entry
//!   restating an earlier entry's resolved pair identically compiles
//!   once; one restating it with a different payload is an
//!   [`OntologyError::Mapping`] failure.
//!
//! **Descriptive** fields are never compiled: the property, constraint
//! and index `name`s (1.1.0 carries no node or relationship `name`),
//! `description`, node, relationship and property `aliases`,
//! `reference`, `labels.optional`, endpoint labels and counts, `one_of`,
//! `pattern`, every extension payload (`indexes`, `display`,
//! `data_source_tables`, `data_source_mappings` and `custom`), and the
//! document metadata (`schema`, `id`, `version`, `name`,
//! `description`). They remain available verbatim through
//! `db.ontology.get` — the stored document is the sole source of truth,
//! and no graph is derived from it (ADR-0229 §3).
//!
//! Spec-valid constructs the graph-type body cannot express fail loudly
//! here rather than disappearing (ADR-0364 §5.4): a node with no
//! identifying label, two node entries sharing one identifying label
//! (one label, one node type, ADR-0668 §3), a relationship target with
//! no `node`, an endpoint
//! naming a missing `nodes` key, a relationship entry restating an
//! earlier entry's resolved endpoint pair with a different declaration
//! (ADR-0668 §4), a token with no plain CIP-176
//! counterpart, a `PROPERTY_TYPE` constraint, an empty constraint
//! property list, a constraint naming a property its owner does not
//! declare, an empty property name that would be declared or
//! constrained, and a non-positive vector dimension are all
//! [`OntologyError::Mapping`] errors naming the owner and construct.

use std::collections::HashMap;

use relate_names::SmallName;
use relate_parser::{GraphTypeBody, GraphTypeConstraintSpec, GraphTypeNodeTypeSpec};

use crate::model::{Constraint, NodeType, OntologyDocument, Property, RelationshipType};
use crate::OntologyError;

mod constraints;
mod properties;
mod relationships;

use constraints::owner_constraints;
use properties::property_decls;
use relationships::relationship_specs;

/// The entry that owns the properties and constraints being compiled, for
/// named [`OntologyError::Mapping`] diagnostics.
///
/// The variant is the owner kind — the one encoding of node-vs-relationship
/// — and carries the entry plus the identifying label a constraint targets
/// (`FOR (:Label =>)`).
enum Owner<'a> {
    /// A node entry and its identifying label.
    Node { entry: &'a NodeType, label: &'a str },
    /// A relationship entry and its type, the label its constraints target.
    Relationship {
        entry: &'a RelationshipType,
        label: &'a str,
    },
}

impl<'a> Owner<'a> {
    /// `node 'N'` / `relationship 'R'`, the prefix of every mapping error.
    fn describe(&self) -> String {
        match self {
            Self::Node { entry, .. } => format!("node '{}'", entry.key),
            Self::Relationship { entry, .. } => format!("relationship '{}'", entry.key),
        }
    }

    fn mapping(&self, detail: impl std::fmt::Display) -> OntologyError {
        OntologyError::Mapping(format!("{} {detail}", self.describe()))
    }

    /// This owner's properties, in document order.
    fn properties(&self) -> &'a [Property] {
        match self {
            Self::Node { entry, .. } => &entry.properties,
            Self::Relationship { entry, .. } => &entry.properties,
        }
    }

    /// This owner's constraint objects, in document order.
    fn constraints(&self) -> &'a [Constraint] {
        match self {
            Self::Node { entry, .. } => &entry.constraints,
            Self::Relationship { entry, .. } => &entry.constraints,
        }
    }

    /// The identifying label a constraint targets (`FOR (:Label =>)`).
    fn label(&self) -> &'a str {
        match self {
            Self::Node { label, .. } | Self::Relationship { label, .. } => label,
        }
    }

    /// Whether the owner compiles to an edge type spec.
    fn is_edge(&self) -> bool {
        match self {
            Self::Node { .. } => false,
            Self::Relationship { .. } => true,
        }
    }
}

/// Compile a validated ontology document into the CIP-176 graph-type body
/// `db.ontology.apply` sets on the database (ADR-0226 §2, ADR-0364 §5,
/// ADR-0487 §3, ADR-0668 §4).
///
/// # Errors
///
/// [`OntologyError::Mapping`] when the document is spec-valid but the
/// graph-type body cannot express it: a node entry with no identifying
/// label, two node entries sharing one identifying label, a
/// relationship target with no `node`, an endpoint naming no `nodes`
/// entry, a relationship entry restating an earlier entry's resolved
/// endpoint pair with a different declaration, a property token with no
/// plain CIP-176 counterpart, a `PROPERTY_TYPE` constraint, an empty
/// constraint property list, a constraint property the owner does not
/// declare, an empty property name that would be declared or
/// constrained, or a non-positive vector dimension (ADR-0364 §5.4).
pub fn to_graph_type_body(doc: &OntologyDocument) -> Result<GraphTypeBody, OntologyError> {
    // The two halves, one function each: the node half (ADR-0668 §3's
    // one-label-one-type policy) and the relationship half (ADR-0668
    // §4's pair declarations).
    let node_half = node_specs(doc)?;
    let relationship_half = relationship_specs(doc, &node_half.node_labels)?;
    let mut constraints = node_half.constraints;
    constraints.extend(relationship_half.edge_constraints);

    Ok(GraphTypeBody {
        node_types: node_half.node_types,
        edge_types: relationship_half.edge_types,
        constraints,
        drop_constraint_names: Vec::new(),
    })
}

/// The document's compiled node half (ADR-0668 §3): the specs, the
/// constraints, and the node key → identifying label table an endpoint
/// resolves through — named fields, never a positional triple.
struct NodeHalf<'a> {
    node_types: Vec<GraphTypeNodeTypeSpec>,
    constraints: Vec<GraphTypeConstraintSpec>,
    node_labels: HashMap<&'a str, &'a str>,
}

/// The document's node half (ADR-0668 §3): one node type spec per entry,
/// its compiled constraints, and the node key → identifying label table
/// an endpoint resolves through. One label, one node type: a node type's
/// identity is its identifying label, so a second entry declaring a
/// declared label is a duplicate the mapping refuses by name — never a
/// body the graph-type validation would refuse late and generically.
/// The first-declarer table the refusal names comes from the same
/// insert that detects the duplicate.
fn node_specs(doc: &OntologyDocument) -> Result<NodeHalf<'_>, OntologyError> {
    let mut node_types = Vec::with_capacity(doc.nodes().len());
    let mut constraints = Vec::new();
    let mut node_labels: HashMap<&str, &str> = HashMap::with_capacity(doc.nodes().len());
    // The identifying label → first declaring entry key: the duplicate
    // gate and the refusal's named declarer in one table.
    let mut declared_labels: HashMap<&str, &str> = HashMap::with_capacity(doc.nodes().len());
    for entry in doc.nodes() {
        let label = entry.identifying_label().ok_or_else(|| {
            OntologyError::Mapping(format!("node '{}' has no identifying label", entry.key))
        })?;
        if let Some(first) = declared_labels.insert(label, entry.key.as_str()) {
            return Err(OntologyError::Mapping(format!(
                "node '{}' restates the identifying label of node '{}' ({}); one label, one \
                 node type",
                entry.key, first, label
            )));
        }
        node_labels.insert(entry.key.as_str(), label);
        let owner = Owner::Node { entry, label };
        // Document names enter the AST's name domain here, classified
        // once at construction (ADR-0509 §1).
        node_types.push(GraphTypeNodeTypeSpec {
            alias: Some(SmallName::from_name(entry.key.as_str())),
            identifying_label: SmallName::from_name(label),
            implied_labels: entry
                .labels
                .implied
                .iter()
                .map(|l| SmallName::from_name(l.as_str()))
                .collect(),
            properties: property_decls(&owner)?,
        });
        constraints.extend(owner_constraints(&owner)?);
    }
    Ok(NodeHalf {
        node_types,
        constraints,
        node_labels,
    })
}

#[cfg(test)]
mod tests;
