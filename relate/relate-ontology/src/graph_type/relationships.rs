//! The relationship-pair policy half of the ontology→graph-type
//! mapping (ADR-0668): the types the document declares once compile to
//! edge type specs; a type declared across several endpoint pairs
//! defines no graph type at all. Split from the converter's body beside
//! the constraint compiler in [`super::constraints`] and the
//! token/property compiler in [`super::properties`].

use std::collections::HashMap;

use indexmap::IndexMap;
use relate_names::SmallName;
use relate_parser::{
    GraphTypeConstraintSpec, GraphTypeEdgeTypeSpec, GraphTypeElementRef, GraphTypePropertyType,
};

use super::constraints::owner_constraints;
use super::properties::property_decls;
use super::{OntologyError, Owner};
use crate::model::{OntologyDocument, RelationshipTarget, RelationshipType};

/// The compiled payload of one declaration (ADR-0668): the entry's
/// property declarations and constraint specs. A payload of its own, so
/// declaration identity — what an identical restatement must match — is
/// one structural comparison, and a field added to the payload joins
/// the identity by construction.
#[derive(PartialEq)]
struct PairPayload {
    properties: Vec<GraphTypePropertyType>,
    constraints: Vec<GraphTypeConstraintSpec>,
}

/// One resolved endpoint pair's identity (ADR-0668): the relationship
/// type and the two endpoint labels — the pair an entry declares, the
/// restatement policy's key and the per-type declaration count's unit.
/// The sides carry the document model's own `from`/`to` names, the one
/// vocabulary for the two ends everywhere in this module.
#[derive(Clone, Copy, PartialEq)]
struct PairKey<'a> {
    rel_type: &'a str,
    from: &'a str,
    to: &'a str,
}

/// The document's compiled relationship half (ADR-0668): the edge type
/// specs of the types the document declares once, and their constraint
/// specs — named fields, the node half's mirror.
pub(super) struct RelationshipHalf {
    pub(super) edge_types: Vec<GraphTypeEdgeTypeSpec>,
    pub(super) edge_constraints: Vec<GraphTypeConstraintSpec>,
}

/// The document's relationship half (ADR-0668): the edge type specs of
/// the types the document declares once. A relationship type declared
/// across several endpoint pairs defines no graph type at all — no
/// spec, no endpoint, property or constraint rows — so its
/// relationships stay writable under any labels while the type still
/// seeds the load's vocabulary. An entry restating an earlier entry's
/// resolved pair identically compiles once; one restating it with a
/// different payload is a conflict the mapping cannot express.
pub(super) fn relationship_specs<'a>(
    doc: &'a OntologyDocument,
    node_labels: &HashMap<&'a str, &'a str>,
) -> Result<RelationshipHalf, OntologyError> {
    // The entries resolved and grouped by their relationship type, in
    // document order — one table: a group's pairs decide the multi-pair
    // verdict, and its records feed the compilation. A type that defines
    // no graph type is never compiled, so its entries' payloads cannot
    // refuse a document whose graph type expresses nothing of them
    // (ADR-0668 §1) — through `db.ontology.apply` and the vault open
    // alike.
    let mut types: IndexMap<&'a str, Vec<ResolvedEntry<'a>>> = IndexMap::new();
    for entry in doc.relationships() {
        let record = resolve_entry(entry, node_labels)?;
        types.entry(record.pair.rel_type).or_default().push(record);
    }
    // Compile the types that define a graph type — the types with
    // exactly one distinct declared pair — one group at a time.
    let mut edge_types = Vec::new();
    let mut edge_constraints = Vec::new();
    for records in types.values() {
        let Some(compiled) = compile_declared_type(records)? else {
            continue;
        };
        edge_types.push(compiled.spec);
        edge_constraints.extend(compiled.constraints);
    }
    Ok(RelationshipHalf {
        edge_types,
        edge_constraints,
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
/// conflicting-restatement refusal when a later entry restates the
/// pair with a different declaration; the compiled declaration when the
/// type declares exactly one pair (an identical restatement compiles
/// once). The group's records arrive in document order, so the first is
/// the pair's declaration and each later record of the same pair
/// restates it.
fn compile_declared_type(
    records: &[ResolvedEntry<'_>],
) -> Result<Option<CompiledType>, OntologyError> {
    // A group is created by a push, so it is never empty.
    let (first, rest) = records
        .split_first()
        .expect("a group is created by a push, so it is never empty");
    if rest.iter().any(|record| record.pair != first.pair) {
        return Ok(None);
    }
    let payload = first.payload()?;
    for record in rest {
        let restated = record.payload()?;
        if restated != payload {
            return Err(record.owner().mapping(format!(
                "restates the endpoint pair of relationship '{}' ({} from {} to {}) with a \
                 different declaration; one endpoint pair, one declaration",
                first.entry.key, record.pair.rel_type, record.pair.from, record.pair.to
            )));
        }
    }
    Ok(Some(CompiledType {
        spec: GraphTypeEdgeTypeSpec {
            alias: Some(SmallName::from_name(first.entry.key.as_str())),
            identifying_label: SmallName::from_name(first.pair.rel_type),
            properties: payload.properties,
            source: GraphTypeElementRef::AliasRef(SmallName::from_name(first.from)),
            target: GraphTypeElementRef::AliasRef(SmallName::from_name(first.to)),
        },
        constraints: payload.constraints,
    }))
}

/// One relationship entry resolved once: the entry, its pair identity,
/// and the node keys its endpoints name — the single resolution the
/// grouping reads.
struct ResolvedEntry<'a> {
    entry: &'a RelationshipType,
    pair: PairKey<'a>,
    from: &'a str,
    to: &'a str,
}

impl<'a> ResolvedEntry<'a> {
    /// The owner this entry's payload compiles against.
    fn owner(&self) -> Owner<'a> {
        Owner::Relationship {
            entry: self.entry,
            label: &self.entry.rel_type,
        }
    }

    /// The entry's compiled declaration — its property declarations and
    /// constraint specs.
    fn payload(&self) -> Result<PairPayload, OntologyError> {
        let owner = self.owner();
        Ok(PairPayload {
            properties: property_decls(&owner)?,
            constraints: owner_constraints(&owner)?,
        })
    }
}

/// Resolve one entry: its endpoint references, its pair identity and
/// the node keys its endpoints name, in one record.
fn resolve_entry<'a>(
    entry: &'a RelationshipType,
    node_labels: &HashMap<&'a str, &'a str>,
) -> Result<ResolvedEntry<'a>, OntologyError> {
    let (from, from_label) = endpoint(&entry.from, "from", &entry.key, node_labels)?;
    let (to, to_label) = endpoint(&entry.to, "to", &entry.key, node_labels)?;
    Ok(ResolvedEntry {
        entry,
        pair: PairKey {
            rel_type: entry.rel_type.as_str(),
            from: from_label,
            to: to_label,
        },
        from,
        to,
    })
}

/// One resolved relationship endpoint: the node key its target names and
/// that node entry's identifying label, after checking that the target
/// names a node at all and that the key is one of the document's `nodes`
/// entries. `side` is the target being resolved (`from` / `to`), named in
/// the diagnostic. The key feeds the spec's endpoint reference; the label
/// is the resolved pair's half (ADR-0668 §4).
fn endpoint<'a>(
    target: &'a RelationshipTarget,
    side: &str,
    relationship: &str,
    node_labels: &HashMap<&'a str, &'a str>,
) -> Result<(&'a str, &'a str), OntologyError> {
    let node = target.node.as_deref().ok_or_else(|| {
        OntologyError::Mapping(format!(
            "relationship '{relationship}' target '{side}' has no node"
        ))
    })?;
    let label = node_labels.get(node).copied().ok_or_else(|| {
        OntologyError::Mapping(format!(
            "relationship '{relationship}' endpoint '{side}' references unknown node key \
             '{node}'"
        ))
    })?;
    Ok((node, label))
}
