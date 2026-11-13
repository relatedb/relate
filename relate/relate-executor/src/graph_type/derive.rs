use super::*;
use relate_names::SmallName;

use relate_core::Result;
use relate_parser::type_normalize::CanonType;

// ---------------------------------------------------------------------
// CIP-176/187: compiling a graph type into its constraint set
// ---------------------------------------------------------------------

/// Derives the flat constraint set a `GraphTypeBody` compiles into
/// (CIP-176 "Graph types and databases" table + CIP-187): one
/// `NODE_PROPERTY_TYPE`/`RELATIONSHIP_PROPERTY_TYPE` constraint per
/// declared property (plus its own property-existence constraint when the
/// declaration is `NOT NULL` — Neo4j's split), one `NODE_LABEL_EXISTENCE`
/// per node type's implied label, one
/// `RELATIONSHIP_SOURCE_LABEL`/`RELATIONSHIP_TARGET_LABEL` per labelled
/// edge endpoint, and one constraint per explicit `CONSTRAINT` element in
/// the body (including ones rewritten from an inline property or trailing
/// `REQUIRE` list -- see `GraphTypeConstraintSpec`'s doc comment). The
/// derived names are Neo4j's generated `constraint_<hash>` artifacts (see
/// [`super::name`]).
///
/// Total over persisted bytes (ADR-0388 review, LABS-386): a deserialized
/// default value whose shape the CIP-262 table never admitted (a gated
/// construct stored by an enabled follower, then replayed or
/// checkpoint-verified here) is the typed [`Error::syntax`] error
/// [`canonical_default_clause`] raises -- never a panic. The persisted
/// replay/checkpoint sites map it with [`persisted_derive_error`].
pub fn derive_constraints(body: &GraphTypeBody) -> Result<Vec<Constraint>> {
    Ok(derive_constraints_typed(body)?
        .into_iter()
        .map(|derived| derived.constraint)
        .collect())
}

/// A graph-type-derived [`Constraint`] together with the typed
/// [`EntityKind`] its `entity_type` display string encodes. Derivation
/// knows the kind from typed branch knowledge (the node- vs edge-type
/// loops, `is_edge`); carrying it alongside keeps consumers (the catalog
/// sync) from re-parsing the display string.
pub struct DerivedConstraint {
    pub constraint: Constraint,
    pub entity: EntityKind,
}

impl DerivedConstraint {
    /// One derived constraint: the `entity_type` display string renders
    /// from the typed `entity` here, the one place the dual write exists.
    #[allow(clippy::too_many_arguments)] // one argument per Constraint field
    fn new(
        entity: EntityKind,
        name: String,
        label: String,
        properties: Vec<String>,
        constraint_type: ConstraintType,
        property_type: Option<String>,
        enforced_label: Option<String>,
    ) -> Self {
        Self {
            constraint: Constraint {
                name,
                label,
                properties,
                constraint_type,
                property_type,
                entity_type: entity.display_name().to_string(),
                enforced_label,
            },
            entity,
        }
    }
}

/// The typed form of [`derive_constraints`]: each derived constraint
/// paired with the [`EntityKind`] it was derived for.
pub fn derive_constraints_typed(body: &GraphTypeBody) -> Result<Vec<DerivedConstraint>> {
    let (node_alias_label, edge_alias_label) = alias_tables(body);
    let mut out = Vec::new();

    for nt in &body.node_types {
        derive_node_type_constraints(nt, &mut out)?;
    }
    for et in &body.edge_types {
        derive_edge_type_constraints(et, &node_alias_label, &mut out)?;
    }

    for c in &body.constraints {
        derive_declared_constraint(c, &node_alias_label, &edge_alias_label, &mut out);
    }

    Ok(out)
}

/// The kind-specific half of a declared constraint: the `propertyType`
/// verb's canonical type, or a plain kind. One typed channel, so the type is
/// present exactly when the kind carries one — no `Option` that is `Some`
/// on one arm only.
enum DeclaredKind {
    /// `REQUIRE <properties> IS :: <type>`.
    PropertyType(CanonType),
    /// `REQUIRE <properties> IS UNIQUE | KEY | NOT NULL`, and a persisted
    /// `propertyType` verb whose type is missing (the validator requires
    /// one, so only a corrupt persisted body reaches that arm).
    Kind(ConstraintType),
}

/// The typed `ConstraintType` a graph-type constraint verb denotes -- the
/// persisted catalog tag. One mapping for the canonicalizer's fallback name
/// and the index-backed declaration comparison.
pub(crate) fn constraint_type_of(kind: &GraphTypeConstraintVerb) -> ConstraintType {
    match kind {
        GraphTypeConstraintVerb::Key => ConstraintType::Key,
        GraphTypeConstraintVerb::Unique => ConstraintType::Unique,
        GraphTypeConstraintVerb::Exists => ConstraintType::Exists,
        GraphTypeConstraintVerb::PropertyType => ConstraintType::PropertyType,
    }
}

impl DeclaredKind {
    fn of(c: &GraphTypeConstraintSpec) -> Self {
        match &c.kind {
            GraphTypeConstraintVerb::PropertyType => match c.property_type.as_ref() {
                Some(property_type) => Self::PropertyType(type_expr_to_canon(property_type)),
                None => Self::Kind(ConstraintType::PropertyType),
            },
            other => Self::Kind(constraint_type_of(other)),
        }
    }
}

/// The constraint one explicit `CONSTRAINT` element (or an inline property
/// / trailing `REQUIRE` list rewritten into one) derives: the verb picks
/// the constraint type, the `is_edge` flag the entity kind, and the
/// endpoint's declared label resolves the target. The third delegating
/// loop body of [`derive_constraints_typed`], next to
/// [`derive_node_type_constraints`] and [`derive_edge_type_constraints`].
fn derive_declared_constraint(
    c: &GraphTypeConstraintSpec,
    node_alias_label: &HashMap<&SmallName, &SmallName>,
    edge_alias_label: &HashMap<&SmallName, &SmallName>,
    out: &mut Vec<DerivedConstraint>,
) {
    let alias_table = if c.is_edge {
        edge_alias_label
    } else {
        node_alias_label
    };
    // The derived-catalog domain is String (the store's constraint rows):
    // the resolved label and the property list render once, here, and feed
    // both the name hash and the derived row.
    let Some(label) = resolve_ref_label(&c.target, alias_table).map(|l| l.to_string()) else {
        return;
    };
    let properties: Vec<String> = c.properties.iter().map(|p| p.to_string()).collect();
    let declared = DeclaredKind::of(c);
    let (constraint_type, property_type_canon) = match &declared {
        DeclaredKind::PropertyType(canon) => (ConstraintType::PropertyType, Some(canon)),
        DeclaredKind::Kind(kind) => (*kind, None),
    };
    let entity = EntityKind::from_relationship_flag(c.is_edge);
    let name = c
        .name
        .clone()
        .map(|n| n.to_string())
        .unwrap_or_else(|| match &declared {
            DeclaredKind::PropertyType(property_type) => {
                super::name::derived_constraint_name(&super::name::DerivedName::property_type(
                    entity,
                    &label,
                    &properties,
                    property_type,
                ))
            }
            DeclaredKind::Kind(kind) => super::name::derived_constraint_name(
                &super::name::DerivedName::props(entity, &label, &properties, *kind),
            ),
        });
    out.push(DerivedConstraint::new(
        entity,
        name,
        label,
        properties,
        constraint_type,
        property_type_canon.map(render_canon),
        None,
    ));
}

/// The constraints one edge type derives (CIP-187): one
/// `RELATIONSHIP_SOURCE_LABEL`/`RELATIONSHIP_TARGET_LABEL` per labelled
/// endpoint reference, one `RELATIONSHIP_PROPERTY_TYPE` per declared
/// property. The edge-type mirror of [`derive_node_type_constraints`].
fn derive_edge_type_constraints(
    et: &GraphTypeEdgeTypeSpec,
    node_alias_label: &HashMap<&SmallName, &SmallName>,
    out: &mut Vec<DerivedConstraint>,
) -> Result<()> {
    let entity = EntityKind::Relationship;
    // Rendered once for the whole type: every derived row and name hash
    // below consumes the same String (the derived-catalog domain).
    let rel_type = et.identifying_label.to_string();
    for (endpoint, kind) in [
        (&et.source, ConstraintType::SourceLabel),
        (&et.target, ConstraintType::TargetLabel),
    ] {
        if let Some(label) = resolve_ref_label(endpoint, node_alias_label).map(|l| l.to_string()) {
            // The endpoint side rides the typed kind, never a parallel bool.
            let name = super::name::derived_constraint_name(&super::name::DerivedName::endpoint(
                &rel_type, kind, &label,
            ));
            out.push(DerivedConstraint::new(
                entity,
                name,
                rel_type.clone(),
                Vec::new(),
                kind,
                None,
                Some(label),
            ));
        }
    }
    for p in &et.properties {
        derive_property_constraint(&rel_type, p, true, out)?;
    }
    Ok(())
}

/// The constraint set one declared property derives (CIP-187): `ANY NOT
/// NULL` collapses to the existence constraint; any other type derives the
/// property-type constraint, plus — when the type is declared `NOT NULL` —
/// the separate property-existence constraint Neo4j reports beside it.
/// One rule for node and edge type specifications alike.
fn derive_property_constraint(
    identifying_label: &str,
    p: &GraphTypePropertyType,
    is_edge: bool,
    out: &mut Vec<DerivedConstraint>,
) -> Result<()> {
    let canon = type_expr_to_canon(&p.value_type);
    let mandatory_any =
        canon.not_null && matches!(canon.kind, CanonKind::Predefined(PredefinedKind::Any));
    let entity = EntityKind::from_relationship_flag(is_edge);
    let properties = [p.name.to_string()];
    // `ANY NOT NULL` adds nothing to `ANY`: Neo4j derives only the existence
    // constraint (the default still applies at commit time), so this guard
    // skips the property-type push and leaves the shared NOT NULL tail to
    // add the existence constraint.
    if !mandatory_any {
        // The property-type constraint carries the type WITHOUT the outer
        // `NOT NULL` (that is its own existence constraint below), matching
        // Neo4j's `propertyType` column and name hash.
        let mut type_canon = canon.clone();
        type_canon.not_null = false;
        let mut property_type = render_canon(&type_canon);
        // CIP-262: the derived property-type constraint carries the default
        // value in its stored `property_type` string (SHOW CONSTRAINTS
        // renders `STRING DEFAULT ON COMMIT 'Hello'`); the post-statement
        // constraint check parses the type prefix and ignores the suffix.
        if let Some(expr) = &p.default_value {
            // Total over persisted bytes (ADR-0388 review, LABS-386): a
            // deserialized body never ran `validate_graph_type_body`, so
            // the shape invariant does not hold here -- an unrenderable
            // default is `canonical_default_clause`'s typed error, never
            // a panic. The persisted replay/checkpoint sites map it
            // with `persisted_derive_error`.
            property_type.push_str(&crate::graph_type::canonical_default_clause(expr)?);
        }
        let type_name =
            super::name::derived_constraint_name(&super::name::DerivedName::property_type(
                entity,
                identifying_label,
                &properties,
                &type_canon,
            ));
        out.push(DerivedConstraint::new(
            entity,
            type_name,
            identifying_label.to_string(),
            properties.to_vec(),
            ConstraintType::PropertyType,
            Some(property_type),
            None,
        ));
    }
    // The one NOT NULL tail: every declared `NOT NULL` — the `ANY NOT NULL`
    // collapse and a typed declaration alike — derives the separate
    // existence constraint Neo4j reports beside the type.
    if canon.not_null {
        p.name
            .with_str(|text| push_exists_constraint(entity, identifying_label, text, out));
    }
    Ok(())
}

/// The one `exists`-constraint push both `NOT NULL` arms share (the
/// `ANY NOT NULL` collapse and a typed `NOT NULL` declaration): the name is
/// the kind's generated artifact, the schema is the one property.
fn push_exists_constraint(
    entity: EntityKind,
    identifying_label: &str,
    property: &str,
    out: &mut Vec<DerivedConstraint>,
) {
    let properties = [property.to_string()];
    let name = super::name::derived_constraint_name(&super::name::DerivedName::props(
        entity,
        identifying_label,
        &properties,
        ConstraintType::Exists,
    ));
    out.push(DerivedConstraint::new(
        entity,
        name,
        identifying_label.to_string(),
        properties.to_vec(),
        ConstraintType::Exists,
        None,
        None,
    ));
}

/// The index form of [`derive_constraints`] used by every "set the current
/// graph type" path: one entry per derived constraint, keyed by its name.
/// `TheaterData::derived_constraints` and redo preflight share this shape.
pub(crate) fn derived_constraints_map(
    body: &GraphTypeBody,
) -> Result<BTreeMap<String, Constraint>> {
    Ok(derive_constraints(body)?
        .into_iter()
        .map(|constraint| (constraint.name.clone(), constraint))
        .collect())
}

fn derive_node_type_constraints(
    nt: &GraphTypeNodeTypeSpec,
    out: &mut Vec<DerivedConstraint>,
) -> Result<()> {
    let entity = EntityKind::Node;
    // Rendered once for the whole type (the derived-catalog domain).
    let label = nt.identifying_label.to_string();
    for implied in &nt.implied_labels {
        let name = implied.with_str(|text| {
            super::name::derived_constraint_name(&super::name::DerivedName::required_label(
                entity, &label, text,
            ))
        });
        out.push(DerivedConstraint::new(
            entity,
            name,
            label.clone(),
            Vec::new(),
            ConstraintType::LabelExistence,
            None,
            Some(implied.to_string()),
        ));
    }
    for p in &nt.properties {
        derive_property_constraint(&label, p, false, out)?;
    }
    Ok(())
}
