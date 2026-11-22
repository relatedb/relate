//! The `SHOW CURRENT GRAPH TYPE` display variants, consolidated and
//! flattened (ADR-0638).
//!
//! Both variants are pure `(body, extra constraints) -> (body, extra
//! constraints)` transforms over the stored graph type: nothing is altered
//! and nothing is persisted, and the two existing renderers
//! ([`current_graph_type_specification`](super::current_graph_type_specification)
//! and `render_graph_type_as_graph`) then run on the result unchanged, so
//! the canonical sort, naming and quoting rules stay theirs. The output may
//! be a graph type that could not be `SET` (CIP-275 says so).
//!
//! - **Consolidated**: each independent property-existence / property-type
//!   constraint (CIP-186: one on a label that identifies no element type)
//!   becomes a property of an element type for its label -- a new one when
//!   the label has none. Undesignated (key / unique) constraints stay
//!   constraints.
//! - **Flattened**: each node element type shows its own rules plus
//!   everything it inherits through its implied labels, transitively: the
//!   full implied-label set, the effective properties (CIP-266's resolver,
//!   defaults included) and the undesignated constraints of every
//!   ancestor. An independent constraint on a non-identifying implied label
//!   inherits as if it were dependent, which this module realizes by
//!   running the inheritance resolvers over the consolidated view while
//!   emitting only the original element types.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use relate_core::{Error, Result};
use relate_model::{Constraint, ConstraintClassification, ConstraintType, EntityKind};
use relate_names::SmallName;
use relate_parser::type_normalize::{parse_canon_type_str, type_expr_to_canon};
use relate_parser::{
    GraphTypeBody, GraphTypeConstraintSpec, GraphTypeConstraintVerb, GraphTypeEdgeTypeSpec,
    GraphTypeElementRef, GraphTypeNodeTypeSpec, GraphTypePropertyType, GraphTypeShowVariant,
    OptionsForm, PredefinedKind, TypeExpr,
};

use super::{
    alias_tables, classify_constraint, classify_constraint_parts, constraint_category,
    constraint_type_of, effective_default_map, endpoint_identity, resolve_ref_label,
    storage_constraint_order, ConstraintCategory, EffectiveDefault, EffectiveProperties,
    EffectiveProperty, EndpointIdentity, Inheritance,
};

/// The variant's view of the stored graph type: the body and the extra
/// (store-only) constraints the renderers consume.
pub(crate) type VariantView = (GraphTypeBody, Vec<Constraint>);

/// Applies `variant` to the stored `body` and its `extras` (the
/// constraints a plain `CREATE CONSTRAINT` made, outside the body).
pub(crate) fn apply_show_variant(
    variant: GraphTypeShowVariant,
    body: &GraphTypeBody,
    extras: &[Constraint],
) -> Result<VariantView> {
    match variant {
        GraphTypeShowVariant::Consolidated => consolidate(body, extras),
        GraphTypeShowVariant::Flattened => flatten(body, extras),
    }
}

// ---------------------------------------------------------------------
// AS CONSOLIDATED
// ---------------------------------------------------------------------

/// The rules the independent constraints of one `(label, property)` add up
/// to: an optional declared type and whether the property must exist.
#[derive(Default)]
struct PropertyRules {
    declared: Option<TypeExpr>,
    not_null: bool,
}

/// One independent constraint's rule.
enum Rule {
    Exists,
    Type(TypeExpr),
}

/// One independent constraint, resolved: the entity kind, label and
/// property it constrains, and its rule. Both sources (the body's own
/// constraints and the store-only ones) are normalized to this shape.
struct Independent {
    is_edge: bool,
    label: String,
    property: String,
    rule: Rule,
}

/// The rule a constraint kind states, `None` for every kind that is not
/// an independent property rule. `property_type` is read only for a type
/// constraint -- one exhaustive step over the admitted kinds.
fn rule_of(
    kind: ConstraintType,
    property_type: impl FnOnce() -> Result<TypeExpr>,
) -> Result<Option<Rule>> {
    match constraint_category(kind) {
        ConstraintCategory::ExistsRule => Ok(Some(Rule::Exists)),
        ConstraintCategory::TypeRule => Ok(Some(Rule::Type(property_type()?))),
        ConstraintCategory::Undesignated(_) | ConstraintCategory::Structural => Ok(None),
    }
}

/// The independent rules grouped by `(is_edge, label)` then property.
type Groups = BTreeMap<(bool, String), BTreeMap<String, PropertyRules>>;

/// Adds one rule to its `(label, property)` group. `false` when the rule
/// cannot join without loss -- a second, different declared type for the
/// property -- so the caller keeps that constraint as a constraint.
fn absorb(groups: &mut Groups, independent: Independent) -> bool {
    let rules = groups
        .entry((independent.is_edge, independent.label))
        .or_default()
        .entry(independent.property)
        .or_default();
    match independent.rule {
        Rule::Exists => {
            rules.not_null = true;
            true
        }
        Rule::Type(declared) => match &rules.declared {
            None => {
                rules.declared = Some(declared);
                true
            }
            Some(existing) => type_expr_to_canon(existing) == type_expr_to_canon(&declared),
        },
    }
}

/// The element-type property the accumulated rules declare: the declared
/// type or `ANY` when only existence was required, `NOT NULL` when
/// required.
fn property_from_rules(name: &str, rules: PropertyRules) -> GraphTypePropertyType {
    let base = rules.declared.unwrap_or(TypeExpr::Predefined {
        kind: PredefinedKind::Any,
        not_null: false,
    });
    GraphTypePropertyType {
        name: SmallName::from(name),
        value_type: if rules.not_null {
            base.with_not_null()
        } else {
            base
        },
        default_value: None,
        inline_constraint: None,
    }
}

/// The independent rule one body constraint carries; `None` for every
/// constraint that stays a constraint (undesignated kinds, a dependent or
/// label-less target, a rule over a property list rather than one
/// property).
fn body_rule(
    c: &GraphTypeConstraintSpec,
    body: &GraphTypeBody,
    node_alias: &HashMap<&SmallName, &SmallName>,
    edge_alias: &HashMap<&SmallName, &SmallName>,
) -> Result<Option<Independent>> {
    let kind = constraint_type_of(&c.kind);
    let Some(rule) = rule_of(kind, || {
        c.property_type.clone().ok_or_else(|| {
            Error::internal("persisted property-type constraint carries no value type")
        })
    })?
    else {
        return Ok(None);
    };
    if c.properties.len() != 1 {
        return Ok(None);
    }
    let alias_table = if c.is_edge { edge_alias } else { node_alias };
    let label = match endpoint_identity(&c.target, alias_table) {
        EndpointIdentity::Any => return Ok(None),
        EndpointIdentity::Unresolved => {
            return Err(Error::storage(
                "persisted graph type constraint reference names no declared element type",
            ));
        }
        EndpointIdentity::Identified { label, .. } => label.to_string(),
    };
    let entity = EntityKind::from_relationship_flag(c.is_edge);
    if classify_constraint_parts(body, Some(kind), entity, &label)
        != ConstraintClassification::Independent
    {
        return Ok(None);
    }
    Ok(Some(Independent {
        is_edge: c.is_edge,
        label,
        property: c.properties[0].to_string(),
        rule,
    }))
}

/// Whether a store-only constraint targets a relationship type; a stored
/// entity kind that names neither kind is a corrupt catalog row.
fn stored_is_edge(c: &Constraint) -> Result<bool> {
    c.entity_kind()
        .map(|kind| kind.is_relationship())
        .map_err(Error::internal)
}

/// The independent rule one store-only constraint carries; `None` when it
/// stays a constraint.
fn extra_rule(c: &Constraint, body: &GraphTypeBody) -> Result<Option<Independent>> {
    let Some(rule) = rule_of(c.constraint_type, || {
        let rendered = c.property_type.as_deref().ok_or_else(|| {
            Error::internal(format!(
                "property-type constraint `{}` carries no value type",
                c.name
            ))
        })?;
        parse_canon_type_str(rendered.trim()).ok_or_else(|| {
            Error::internal(format!(
                "property-type constraint `{}` carries the unparseable type `{rendered}`",
                c.name
            ))
        })
    })?
    else {
        return Ok(None);
    };
    if c.properties.len() != 1
        || classify_constraint(body, c) != ConstraintClassification::Independent
    {
        return Ok(None);
    }
    Ok(Some(Independent {
        is_edge: stored_is_edge(c)?,
        label: c.label.clone(),
        property: c.properties[0].clone(),
        rule,
    }))
}

/// Folds every independent constraint of `items` into `groups` and returns
/// the ones that stay constraints: those that are not independent, and any
/// whose rule cannot join its property's definition without loss.
fn absorb_independent<T: Clone>(
    items: &[T],
    extract: impl Fn(&T) -> Result<Option<Independent>>,
    groups: &mut Groups,
) -> Result<Vec<T>> {
    let mut kept = Vec::with_capacity(items.len());
    for item in items {
        let absorbed = match extract(item)? {
            Some(independent) => absorb(groups, independent),
            None => false,
        };
        if !absorbed {
            kept.push(item.clone());
        }
    }
    Ok(kept)
}

/// Turns every independent constraint into an element type property (see
/// the module doc); every other constraint passes through.
fn consolidate(body: &GraphTypeBody, extras: &[Constraint]) -> Result<VariantView> {
    let (node_alias, edge_alias) = alias_tables(body);
    let mut groups = Groups::new();
    let kept_body = absorb_independent(
        &body.constraints,
        |c| body_rule(c, body, &node_alias, &edge_alias),
        &mut groups,
    )?;
    let kept_extras = absorb_independent(extras, |c| extra_rule(c, body), &mut groups)?;

    let mut out = body.clone();
    out.constraints = kept_body;
    for ((is_edge, label), properties) in groups {
        let properties: Vec<GraphTypePropertyType> = properties
            .into_iter()
            .map(|(name, rules)| property_from_rules(&name, rules))
            .collect();
        let identifying_label = SmallName::from(label.as_str());
        if is_edge {
            out.edge_types.push(GraphTypeEdgeTypeSpec {
                alias: None,
                identifying_label,
                properties,
                source: GraphTypeElementRef::Any,
                target: GraphTypeElementRef::Any,
            });
        } else {
            out.node_types.push(GraphTypeNodeTypeSpec {
                alias: None,
                identifying_label,
                implied_labels: Vec::new(),
                properties,
            });
        }
    }
    Ok((out, kept_extras))
}

// ---------------------------------------------------------------------
// AS FLATTENED
// ---------------------------------------------------------------------

/// A resolver failure over the stored graph type: stored bodies passed
/// validation on the way in, so one failing here is a corrupt store.
fn stored_body_error(error: relate_error::EngineError) -> Error {
    Error::storage(format!(
        "persisted graph type does not resolve its inheritance: {error}"
    ))
}

/// One undesignated (key / unique) node constraint.
#[derive(Clone, PartialEq)]
struct Undesignated {
    kind: GraphTypeConstraintVerb,
    properties: Vec<String>,
}

/// The verb of an undesignated constraint kind (key or unique), `None`
/// for every other kind -- the one place the vocabulary is spelled.
fn undesignated_verb(kind: ConstraintType) -> Option<GraphTypeConstraintVerb> {
    match constraint_category(kind) {
        ConstraintCategory::Undesignated(verb) => Some(verb),
        ConstraintCategory::ExistsRule
        | ConstraintCategory::TypeRule
        | ConstraintCategory::Structural => None,
    }
}

/// The node-side undesignated constraints of every label, in the body's
/// declared order, then the store-only ones in the graph view's order
/// (property list, kind rank, name).
fn undesignated_by_label(
    body: &GraphTypeBody,
    extras: &[Constraint],
) -> Result<BTreeMap<String, Vec<Undesignated>>> {
    let (node_alias, _) = alias_tables(body);
    let mut by_label: BTreeMap<String, Vec<Undesignated>> = BTreeMap::new();
    for c in body.constraints.iter().filter(|c| !c.is_edge) {
        let Some(kind) = undesignated_verb(constraint_type_of(&c.kind)) else {
            continue;
        };
        let label = resolve_ref_label(&c.target, &node_alias).ok_or_else(|| {
            Error::storage("persisted graph type constraint reference names no declared node type")
        })?;
        by_label
            .entry(label.to_string())
            .or_default()
            .push(Undesignated {
                kind,
                properties: c.properties.iter().map(|p| p.to_string()).collect(),
            });
    }
    let mut stored: Vec<(&Constraint, GraphTypeConstraintVerb)> = Vec::new();
    for c in extras {
        if let Some(kind) = undesignated_verb(c.constraint_type) {
            if !stored_is_edge(c)? {
                stored.push((c, kind));
            }
        }
    }
    stored.sort_by(|(a, _), (b, _)| storage_constraint_order(a, b));
    for (c, kind) in stored {
        by_label
            .entry(c.label.clone())
            .or_default()
            .push(Undesignated {
                kind,
                properties: c.properties.clone(),
            });
    }
    Ok(by_label)
}

/// Every label `label` implies, transitively: its implied labels and,
/// for each that identifies a node element type, that type's own. The
/// visited set bounds the walk on a degenerate body.
fn all_implied(
    by_label: &BTreeMap<String, &GraphTypeNodeTypeSpec>,
    label: &str,
) -> BTreeSet<String> {
    let mut implied = BTreeSet::new();
    let mut pending: Vec<String> = vec![label.to_string()];
    while let Some(current) = pending.pop() {
        let Some(node) = by_label.get(&current) else {
            continue;
        };
        for next in &node.implied_labels {
            let next = next.to_string();
            if implied.insert(next.clone()) {
                pending.push(next);
            }
        }
    }
    implied.remove(label);
    implied
}

/// Inlines every inherited definition into each node element type (see
/// the module doc). Edge element types, relationship endpoints and the
/// store-only constraints pass through unchanged.
fn flatten(body: &GraphTypeBody, extras: &[Constraint]) -> Result<VariantView> {
    // The resolvers read the consolidated view, so an independent
    // constraint on a non-identifying implied label inherits like a
    // dependent one; only the original element types are emitted.
    let (resolver_body, _) = consolidate(body, extras)?;
    let inheritance = Inheritance::of(&resolver_body);
    let effective =
        EffectiveProperties::resolve(&resolver_body, &inheritance).map_err(stored_body_error)?;
    let defaults =
        effective_default_map(&resolver_body, &inheritance).map_err(stored_body_error)?;
    let by_label: BTreeMap<String, &GraphTypeNodeTypeSpec> = resolver_body
        .node_types
        .iter()
        .map(|n| (n.identifying_label.to_string(), n))
        .collect();
    let undesignated = undesignated_by_label(body, extras)?;

    let mut out = body.clone();
    let mut inlined: Vec<GraphTypeConstraintSpec> = Vec::new();
    for node in &mut out.node_types {
        let label = node.identifying_label.to_string();
        inline_properties(node, effective.of(&label), defaults.get(&label));
        let ancestors = all_implied(&by_label, &label);
        node.implied_labels = ancestors
            .iter()
            .map(|l| SmallName::from(l.as_str()))
            .collect();
        inlined.extend(inherited_constraints(node, &ancestors, &undesignated));
    }
    out.constraints.extend(inlined);
    Ok((out, extras.to_vec()))
}

/// Shows `node`'s effective properties: its own declarations win (one
/// that states no default shows the default it inherits) and every
/// property it only inherits is added with its resolved type and default.
fn inline_properties(
    node: &mut GraphTypeNodeTypeSpec,
    effective: &BTreeMap<String, EffectiveProperty>,
    defaults: Option<&BTreeMap<String, EffectiveDefault>>,
) {
    let default_of = |name: &str| {
        defaults
            .and_then(|m| m.get(name))
            .map(|d| d.value.to_expr())
    };
    let own_names: BTreeSet<String> = node.properties.iter().map(|p| p.name.to_string()).collect();
    for property in &mut node.properties {
        if property.default_value.is_none() {
            property.default_value = default_of(&property.name.to_string());
        }
    }
    let mut inherited_any = false;
    for (name, resolved) in effective {
        if own_names.contains(name) {
            continue;
        }
        inherited_any = true;
        node.properties.push(GraphTypePropertyType {
            name: SmallName::from(name.as_str()),
            value_type: TypeExpr::from_canon(&resolved.canon),
            default_value: default_of(name),
            inline_constraint: None,
        });
    }
    if inherited_any {
        node.properties.sort_by_key(|p| p.name.to_string());
    }
}

/// The undesignated constraints `node` inherits from its `ancestors`,
/// unnamed and targeted at `node`'s own label; a rule `node` already has
/// (or another ancestor already contributed) is not repeated.
fn inherited_constraints(
    node: &GraphTypeNodeTypeSpec,
    ancestors: &BTreeSet<String>,
    undesignated: &BTreeMap<String, Vec<Undesignated>>,
) -> Vec<GraphTypeConstraintSpec> {
    let mut seen: Vec<&Undesignated> = undesignated
        .get(&node.identifying_label.to_string())
        .into_iter()
        .flatten()
        .collect();
    let mut inlined = Vec::new();
    for ancestor in ancestors {
        for rule in undesignated.get(ancestor).into_iter().flatten() {
            if seen.contains(&rule) {
                continue;
            }
            seen.push(rule);
            inlined.push(GraphTypeConstraintSpec {
                name: None,
                target: GraphTypeElementRef::InSitu {
                    alias: None,
                    label: node.identifying_label.clone(),
                    implies: true,
                },
                is_edge: false,
                properties: rule
                    .properties
                    .iter()
                    .map(|p| SmallName::from(p.as_str()))
                    .collect(),
                kind: rule.kind.clone(),
                property_type: None,
                options: OptionsForm::Absent,
            });
        }
    }
    inlined
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_type::tests::body_of;

    fn extra(
        name: &str,
        label: &str,
        property: &str,
        kind: ConstraintType,
        property_type: Option<&str>,
    ) -> Constraint {
        Constraint {
            name: name.to_string(),
            label: label.to_string(),
            properties: vec![property.to_string()],
            constraint_type: kind,
            property_type: property_type.map(str::to_string),
            entity_type: "NODE".to_string(),
            enforced_label: None,
        }
    }

    fn spec(body: &GraphTypeBody, extras: &[Constraint]) -> String {
        super::super::current_graph_type_specification(body, extras)
            .expect("renders")
            .replace(['`', '\n'], "")
    }

    #[test]
    fn a_second_different_type_for_one_property_stays_a_constraint() {
        let body = body_of(
            "ALTER CURRENT GRAPH TYPE SET { (:A => {}), \
             CONSTRAINT x FOR (n:D) REQUIRE n.p IS :: INTEGER, \
             CONSTRAINT y FOR (n:D) REQUIRE n.p IS :: STRING, \
             CONSTRAINT z FOR (n:D) REQUIRE n.p IS :: INTEGER }",
        );
        let (out, extras) = consolidate(&body, &[]).expect("consolidates");
        assert!(extras.is_empty());
        // The first type is the property; the restated equal one folds
        // in; only the conflicting one cannot be said as a property
        // definition, so it stays what it was -- nothing is lost.
        let rendered = spec(&out, &[]);
        assert!(rendered.contains("(:D => {p :: INTEGER})"), "{rendered}");
        assert!(
            rendered.contains("CONSTRAINT y FOR (n:D =>) REQUIRE (n.p) IS :: STRING"),
            "{rendered}"
        );
        assert!(!rendered.contains("CONSTRAINT x"), "{rendered}");
        assert!(!rendered.contains("CONSTRAINT z"), "{rendered}");
    }

    #[test]
    fn rules_that_are_not_independent_property_rules_pass_through() {
        let body = body_of(
            "ALTER CURRENT GRAPH TYPE SET { (:A => {p :: STRING}), \
             CONSTRAINT k FOR (n:D) REQUIRE n.q IS KEY, \
             CONSTRAINT FOR (n:D) REQUIRE (n.q, n.r) IS UNIQUE }",
        );
        let stored = [
            // A multi-property key stays a constraint, and so does a
            // label-existence kind.
            extra("m", "E", "x", ConstraintType::Unique, None),
            extra("l", "E", "y", ConstraintType::NodeLabelExistence, None),
        ];
        let (out, extras) = consolidate(&body, &stored).expect("consolidates");
        assert_eq!(out, body);
        assert_eq!(extras, stored);
    }

    #[test]
    fn a_property_type_that_does_not_parse_is_an_internal_error() {
        let body = body_of("ALTER CURRENT GRAPH TYPE SET { (:A => {}) }");
        for stored in [
            extra(
                "t",
                "D",
                "p",
                ConstraintType::PropertyType,
                Some("NOT A TYPE"),
            ),
            extra("t", "D", "p", ConstraintType::PropertyType, None),
        ] {
            let error = consolidate(&body, &[stored]).expect_err("a corrupt catalog row");
            assert!(matches!(error, Error::Internal(_)), "{error}");
        }
    }

    #[test]
    fn store_only_rules_consolidate_and_a_stored_key_is_inherited() {
        let body = body_of("ALTER CURRENT GRAPH TYPE SET { (:A => :D {p1 :: STRING}) }");
        let stored = [
            extra(
                "d_t",
                "D",
                "p",
                ConstraintType::PropertyType,
                Some("INTEGER"),
            ),
            extra("d_e", "D", "p", ConstraintType::Exists, None),
            extra("d_k", "D", "k", ConstraintType::Key, None),
        ];
        let (out, kept) =
            apply_show_variant(GraphTypeShowVariant::Consolidated, &body, &stored).unwrap();
        assert_eq!(
            spec(&out, &kept),
            "{  (:A => :D {p1 :: STRING}),  (:D => {p :: INTEGER NOT NULL}),  \
             CONSTRAINT d_k FOR (n:D =>) REQUIRE (n.k) IS KEY}"
        );
        let (out, kept) =
            apply_show_variant(GraphTypeShowVariant::Flattened, &body, &stored).unwrap();
        // Flattened inherits the virtual element type D's property and the
        // key, and leaves the independent constraints where they are.
        assert_eq!(kept, stored);
        let rendered = spec(&out, &kept);
        assert!(
            rendered.contains("(:A => :D {p1 :: STRING, p :: INTEGER NOT NULL})"),
            "{rendered}"
        );
        assert!(
            rendered.contains("FOR (n:A =>) REQUIRE (n.k) IS KEY"),
            "{rendered}"
        );
        assert!(rendered.contains("CONSTRAINT d_t FOR (n:D)"), "{rendered}");
    }

    #[test]
    fn the_variants_leave_the_inputs_untouched() {
        let body = body_of(
            "ALTER CURRENT GRAPH TYPE SET { (:A => :D {p1 :: STRING}), \
             CONSTRAINT FOR (n:D) REQUIRE n.p IS NOT NULL }",
        );
        let before = body.clone();
        for variant in [
            GraphTypeShowVariant::Consolidated,
            GraphTypeShowVariant::Flattened,
        ] {
            apply_show_variant(variant, &body, &[]).unwrap();
        }
        assert_eq!(body, before);
    }
}
