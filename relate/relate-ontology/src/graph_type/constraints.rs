//! The constraint compiler half of the ontology→graph-type mapping
//! (ADR-0226 §2): an owner's standalone graph-type constraints — the
//! shorthand flags of its properties and its constraint objects — and
//! the ONE correspondence between main's constraint kinds and CIP-176
//! constraint verbs. Split from the converter's body so the compiler is
//! addressable beside the token/property half in
//! [`super::properties`].

use relate_names::SmallName;
use relate_parser::{
    GraphTypeConstraintSpec, GraphTypeConstraintVerb, GraphTypeElementRef, OptionsForm,
};

use super::properties;
use super::{OntologyError, Owner};
use crate::model::{Constraint, ConstraintType, Property};

/// One owner's standalone graph-type constraints, in document order: the
/// shorthand flags of its properties first (in property order), then its
/// constraint objects. A constraint object restating a flag exactly
/// suppresses the flag-derived constraint, so the named object survives.
///
/// # Errors
///
/// [`OntologyError::Mapping`] for an empty-named property asserted by a
/// shorthand flag, an empty constraint property list, a `PROPERTY_TYPE`
/// constraint, or a listed property the owner does not declare
/// (ADR-0364 §5.3/§5.4).
pub(super) fn owner_constraints(
    owner: &Owner<'_>,
) -> Result<Vec<GraphTypeConstraintSpec>, OntologyError> {
    let mut out = flag_constraints(owner)?;
    out.extend(object_constraints(owner)?);
    Ok(out)
}

/// The shorthand flags of an owner's properties, in property order. A
/// flag a constraint object restates exactly (same kind, same single
/// property) is suppressed, so the named object survives.
///
/// # Errors
///
/// [`OntologyError::Mapping`] for an empty-named property asserted by a
/// shorthand flag (ADR-0364 §5.3/§5.4).
fn flag_constraints(owner: &Owner<'_>) -> Result<Vec<GraphTypeConstraintSpec>, OntologyError> {
    let mut out = Vec::new();
    for property in owner.properties() {
        // The one producer decides which flags the property asserts; the
        // empty-name guard asks it, never a re-listed field set.
        let verbs = shorthand_verbs(property);
        if property.id.is_empty() {
            if verbs.is_empty() {
                continue;
            }
            return Err(owner.mapping(
                "property '' is asserted by a shorthand flag: a graph-type property name \
                 is non-empty",
            ));
        }
        for verb in verbs {
            if !restated_by_object(&verb, &property.id, owner.constraints()) {
                out.push(constraint_spec(
                    None,
                    owner,
                    std::slice::from_ref(&property.id),
                    verb,
                ));
            }
        }
    }
    Ok(out)
}

/// One property's shorthand verbs, in emission order — the ONE home of
/// `key` subsumes `unique`, and of the rule that only a property
/// declaring nothing gets a standalone `must_exist` existence constraint
/// (a typed property's `NOT NULL` rides its declaration in
/// [`super::properties`]).
fn shorthand_verbs(property: &Property) -> Vec<GraphTypeConstraintVerb> {
    let mut verbs = Vec::new();
    if property.key == Some(true) {
        verbs.push(GraphTypeConstraintVerb::Key);
    } else if property.unique == Some(true) {
        verbs.push(GraphTypeConstraintVerb::Unique);
    }
    if properties::declares_nothing(property) && property.must_exist == Some(true) {
        verbs.push(GraphTypeConstraintVerb::Exists);
    }
    verbs
}

/// The owner's constraint objects, in document order.
///
/// # Errors
///
/// [`OntologyError::Mapping`] for an empty constraint property list, a
/// `PROPERTY_TYPE` constraint, or a listed property the owner does not
/// declare (ADR-0364 §5.3/§5.4).
fn object_constraints(owner: &Owner<'_>) -> Result<Vec<GraphTypeConstraintSpec>, OntologyError> {
    let mut out = Vec::new();
    for object in owner.constraints() {
        if object.properties.is_empty() {
            return Err(owner.mapping(format!(
                "constraint '{}' lists no properties: a graph-type constraint needs at least one",
                object.id
            )));
        }
        for property in &object.properties {
            if property.is_empty() {
                return Err(owner.mapping(format!(
                    "constraint '{}' lists the empty property name: a graph-type property name \
                     is non-empty",
                    object.id
                )));
            }
            if !owner.properties().iter().any(|p| p.id == *property) {
                return Err(owner.mapping(format!(
                    "constraint '{}' references property '{}', which {} does not declare",
                    object.id,
                    property,
                    owner.describe()
                )));
            }
        }
        let Some(verb) = object.constraint_type.graph_type_verb() else {
            return Err(owner.mapping(format!(
                "constraint '{}' is of type {}: the graph-type body cannot express a \
                 property-type constraint object",
                object.id,
                object.constraint_type.as_str()
            )));
        };
        out.push(constraint_spec(
            object.name.as_deref(),
            owner,
            &object.properties,
            verb,
        ));
    }
    Ok(out)
}

impl ConstraintType {
    /// The graph-type verb of this constraint kind — an exhaustive
    /// match, so a new kind fails the build here rather than silently
    /// mapping to nothing. `None` for `PROPERTY_TYPE`, the one variant
    /// the graph-type body cannot express.
    fn graph_type_verb(self) -> Option<GraphTypeConstraintVerb> {
        match self {
            ConstraintType::Exists => Some(GraphTypeConstraintVerb::Exists),
            ConstraintType::Key => Some(GraphTypeConstraintVerb::Key),
            ConstraintType::Unique => Some(GraphTypeConstraintVerb::Unique),
            ConstraintType::PropertyType => None,
        }
    }
}

/// Whether a constraint object restates a single-property shorthand flag
/// exactly (same kind, same property) — the object-replaces-flag rule for
/// all three shorthands (`key`, `unique`, `must_exist`). No flag maps to
/// `PROPERTY_TYPE`, so it never restates one.
fn restated_by_object(
    verb: &GraphTypeConstraintVerb,
    property: &str,
    objects: &[Constraint],
) -> bool {
    objects.iter().any(|o| {
        o.constraint_type
            .graph_type_verb()
            .is_some_and(|v| &v == verb)
            && o.properties == [property]
    })
}

/// One standalone graph-type constraint, in the target shape the parser's
/// own inline-constraint rewrite produces (`FOR (:Label =>)`) —
/// borrowing its name and properties at the boundary, so no call site
/// stages a copy.
fn constraint_spec(
    name: Option<&str>,
    owner: &Owner<'_>,
    properties: &[String],
    kind: GraphTypeConstraintVerb,
) -> GraphTypeConstraintSpec {
    GraphTypeConstraintSpec {
        name: name.map(SmallName::from_name),
        target: in_situ(owner.label()),
        is_edge: owner.is_edge(),
        properties: properties
            .iter()
            .map(|p| SmallName::from_name(p.as_str()))
            .collect(),
        kind,
        property_type: None,
        options: OptionsForm::Absent,
    }
}

/// An in-situ element reference in the `(:Label =>)` shape — the ONE
/// construction of it, shared by the converter and the test modules.
pub(super) fn in_situ(label: &str) -> GraphTypeElementRef {
    GraphTypeElementRef::InSitu {
        alias: None,
        label: SmallName::from_name(label),
        implies: true,
    }
}
