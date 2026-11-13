//! The rule groups `validate_graph_type_body` runs, in precedence order.

use super::*;

use relate_core::{PreviewFeature, PreviewFeatures};
use relate_names::SmallName;
use relate_parser::CanonType;

/// The alias→label and identifying-label tables of one body, shared by the
/// reference-resolution rule groups.
pub(super) struct ElementTables<'a> {
    node_alias_label: HashMap<&'a SmallName, &'a SmallName>,
    edge_alias_label: HashMap<&'a SmallName, &'a SmallName>,
    node_id_labels: HashSet<&'a SmallName>,
    edge_id_labels: HashSet<&'a SmallName>,
}

impl<'a> ElementTables<'a> {
    /// The alias→label table of the entity kind a constraint targets.
    fn alias_table(&self, is_edge: bool) -> &HashMap<&'a SmallName, &'a SmallName> {
        if is_edge {
            &self.edge_alias_label
        } else {
            &self.node_alias_label
        }
    }

    /// The identifying-label set of the entity kind a constraint targets.
    fn id_labels(&self, is_edge: bool) -> &HashSet<&'a SmallName> {
        if is_edge {
            &self.edge_id_labels
        } else {
            &self.node_id_labels
        }
    }

    pub(super) fn of(body: &'a GraphTypeBody) -> Self {
        let (node_alias_label, edge_alias_label) = alias_tables(body);
        Self {
            node_alias_label,
            edge_alias_label,
            node_id_labels: body
                .node_types
                .iter()
                .map(|n| &n.identifying_label)
                .collect(),
            edge_id_labels: body
                .edge_types
                .iter()
                .map(|n| &n.identifying_label)
                .collect(),
        }
    }
}

/// Rule 5/6: node and edge types each have their own identifying-label
/// namespace — a set collapsing two types of one kind is a duplicate.
pub(super) fn check_identifying_labels(
    body: &GraphTypeBody,
    tables: &ElementTables<'_>,
) -> Result<(), EngineError> {
    if tables.node_id_labels.len() != body.node_types.len()
        || tables.edge_id_labels.len() != body.edge_types.len()
    {
        return Err(syn("duplicate identifying label"));
    }
    Ok(())
}

/// Rules 3a-4c: the shared element-type-alias namespace.
pub(super) fn check_alias_namespace(
    body: &GraphTypeBody,
    scope: GraphTypeScope,
) -> Result<(), EngineError> {
    // Rule 3a/3b/4a: node type aliases and edge type aliases share one
    // element-type-alias namespace -- two node/edge type *specifications*
    // may not declare the same alias.
    let mut aliases: HashMap<&SmallName, (bool, &SmallName)> = HashMap::new();
    for nt in &body.node_types {
        if let Some(a) = &nt.alias {
            declare_alias(&mut aliases, a, false, &nt.identifying_label)?;
        }
    }
    for et in &body.edge_types {
        if let Some(a) = &et.alias {
            declare_alias(&mut aliases, a, true, &et.identifying_label)?;
        }
    }

    // Rule 3c/4c, the declaration form only: a *bare* in-situ constraint
    // reference's own alias (e.g. `CONSTRAINT FOR (p:City) REQUIRE ...`, an
    // ad-hoc reference that need not resolve to a declared type at all) must
    // not collide with an unrelated node/edge type specification's alias
    // (CIP-178). `=>`-qualified references (a pure local rename of
    // an already-fully-identified target) and plain alias references (which
    // always resolve to a declared type) are excluded. An `ALTER` body
    // deliberately skips this rule: Neo4j's verified acceptance corpus pairs
    // `(n:EN1 => {p1 :: STRING})` with `CONSTRAINT nk FOR (n:L1)` in one
    // `ADD` body, and `(n:Label => ...)` with `CONSTRAINT c5 FOR (n:A)` in
    // one `SET` body -- a constraint alias is a schema variable, a namespace
    // separate from the element-type aliases.
    if scope == GraphTypeScope::Declaration {
        for c in &body.constraints {
            if let GraphTypeElementRef::InSitu {
                alias: Some(a),
                label,
                implies: false,
            } = &c.target
            {
                if let Some(&(existing_is_edge, existing_label)) = aliases.get(a) {
                    if existing_is_edge != c.is_edge || existing_label != label {
                        return Err(syn("duplicate element type alias"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Implied labels and their (preview-gated, acyclic) inheritance (CIP-266),
/// returned for the property rules that follow.
pub(super) fn check_implied_labels(
    body: &GraphTypeBody,
    preview: PreviewFeatures,
) -> Result<Inheritance, EngineError> {
    // Rule 7 (own implied set): a node type's identifying label must not
    // recur in its own implied label set.
    for nt in &body.node_types {
        if nt.implied_labels.contains(&nt.identifying_label) {
            return Err(syn("identifying label repeated in implied label set"));
        }
    }
    // CIP-266 (replacing CIP-178's earlier implied-label rule): an implied label
    // may name another node element type's identifying label --
    // inheritance -- behind the `graph-type-implied-labels` preview gate
    // (ADR-0371). The in-body half of the gate lives at statement
    // admission; the stored-state half (an `ADD` whose identifying label
    // a stored node element type already implies) fails closed here with
    // the same authored error. Within the gate, inheritance must be
    // acyclic (NAGTE3).
    let inheritance = Inheritance::of(body);
    if !inheritance.is_empty() {
        gate(preview, PreviewFeature::GraphTypeImpliedLabels)?;
        if let Some((x, y)) = inheritance.find_cycle() {
            return Err(syn(format!(
                "The label `{x}` is not allowed as an implied label on the node element \
                 type identified by the label `{y}`: node element types cannot have \
                 circular inheritance"
            )));
        }
    }
    Ok(inheritance)
}

/// The property-set rules and the CIP-266/CIP-262 inheritance and
/// default-value rules.
pub(super) fn check_property_rules(
    body: &GraphTypeBody,
    preview: PreviewFeatures,
    inheritance: &Inheritance,
) -> Result<(), EngineError> {
    // Rule 8: a single-label (no implied labels) node type with an empty
    // property set is ineffectual.
    for nt in &body.node_types {
        if nt.implied_labels.is_empty() && nt.properties.is_empty() {
            return Err(syn("single-label node type with empty property set"));
        }
    }

    // Duplicate property names; CIP-283's optional
    // (bare `ANY`) properties and CIP-262's default values, behind their
    // preview gates.
    for nt in &body.node_types {
        validate_property_set(&nt.properties, preview)?;
    }
    for et in &body.edge_types {
        validate_property_set(&et.properties, preview)?;
    }

    // CIP-266: the inheritance property-definition rules -- a re-specified
    // property must repeat or further restrict the intersection of its
    // parents' effective definitions (diamond: parents not defining the
    // property contribute `ANY`), and an implicitly inherited property
    // must be instantiable (a `NULL`/`NOTHING` intersection is not).
    if !inheritance.is_empty() {
        validate_inheritance_properties(body, inheritance)?;
    }

    // CIP-262's interaction with CIP-266: the effective default map must
    // resolve (a property inheriting defaults from more than one implied
    // node element type is the FE8 conflict), and each effective default
    // must conform to the type the child element type declares for its
    // property (FE9: restricting the property type must not invalidate an
    // implicitly inherited default).
    // No gate here: `validate_property_set` already failed closed on
    // every default value the feature does not enable (the per-property
    // gate covers this whole block).
    if body_has_default_values(body) {
        let defaults = effective_default_map(body, inheritance)?;
        for nt in &body.node_types {
            if let Some(map) = nt.identifying_label.with_str(|text| defaults.get(text)) {
                for (property, default) in map {
                    let Some(declared) = nt.properties.iter().find(|p| p.name == property.as_str())
                    else {
                        continue;
                    };
                    if !default.value.conforms_to(&declared.value_type)? {
                        return Err(syn(format!(
                            "Invalid input {} for default property value. Expected to be \
                             {}",
                            default.value.render(),
                            render_canon(&type_expr_to_canon(&declared.value_type))
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rule 2: no REL qualifier in a node type, no NODE qualifier in an edge type.
pub(super) fn check_entity_qualifiers(body: &GraphTypeBody) -> Result<(), EngineError> {
    // Rule 2: a node type must not contain REL/RELATIONSHIP; an edge type
    // must not contain NODE (checked via each inline property constraint's
    // recorded qualifier).
    for nt in &body.node_types {
        for p in &nt.properties {
            if let Some(ic) = &p.inline_constraint {
                if ic.qualifier == Some(GraphTypeEntityQualifier::Rel) {
                    return Err(syn("REL/RELATIONSHIP keyword not allowed in node type"));
                }
            }
        }
    }
    for et in &body.edge_types {
        for p in &et.properties {
            if let Some(ic) = &p.inline_constraint {
                if ic.qualifier == Some(GraphTypeEntityQualifier::Node) {
                    return Err(syn("NODE keyword not allowed in edge type"));
                }
            }
        }
    }
    Ok(())
}

/// Rules 3/4 (edge endpoints), Rule 1.2.3/REF[5]/REF[6].
pub(super) fn check_edge_endpoints(
    body: &GraphTypeBody,
    tables: &ElementTables<'_>,
) -> Result<(), EngineError> {
    // Rule 3/4 (edge endpoints): an in-situ endpoint reference may not
    // carry an element type alias. Rule 1.2.3/REF[6]: an implies (`=>`)
    // in-situ reference must name a declared node type. REF[5]: an alias
    // reference must resolve to a declared node type alias.
    for et in &body.edge_types {
        for endpoint in [&et.source, &et.target] {
            match endpoint {
                GraphTypeElementRef::InSitu { alias: Some(_), .. } => {
                    return Err(syn(
                        "in-situ endpoint reference may not carry an element type alias",
                    ));
                }
                GraphTypeElementRef::InSitu {
                    alias: None,
                    label,
                    implies: true,
                } => {
                    if !tables.node_id_labels.contains(label) {
                        return Err(syn(
                            "implies in-situ reference must name a declared node type",
                        ));
                    }
                }
                // Kept as a nested `if` rather than a match guard: collapsing it
                // produces a guard line that different rustfmt versions wrap
                // inconsistently.
                #[allow(clippy::collapsible_match)]
                GraphTypeElementRef::AliasRef(a) => {
                    if !tables.node_alias_label.contains_key(a) {
                        return Err(syn("node type alias reference to an undeclared alias"));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// An ineffectual single-label edge type. Skipped for CIP-180's
/// canonicalizer input, which is exactly this shape.
pub(super) fn check_ineffectual_edge_types(body: &GraphTypeBody) -> Result<(), EngineError> {
    // Rule 9: an ineffectual single-label edge type (empty props, any
    // source, any destination). CIP-180's canonicalizer input is exactly
    // this shape (see the function's `canonicalizer_input`).
    for et in &body.edge_types {
        if et.properties.is_empty()
            && matches!(et.source, GraphTypeElementRef::Any)
            && matches!(et.target, GraphTypeElementRef::Any)
        {
            return Err(syn("single-label edge type is ineffectual"));
        }
    }
    Ok(())
}

/// Constraint target resolution and Rules 1/15/16.
pub(super) fn check_constraint_targets(
    body: &GraphTypeBody,
    tables: &ElementTables<'_>,
) -> Result<(), EngineError> {
    // Constraint target resolution + Rules 1/15/16 (Rule 1/2/13/16-of-CS/
    // 22/23 are already enforced at parse time; see
    // `Parser::parse_graph_type_constraint_spec`).
    for c in &body.constraints {
        let (alias_table, id_labels) = (tables.alias_table(c.is_edge), tables.id_labels(c.is_edge));
        match &c.target {
            GraphTypeElementRef::AliasRef(a) => {
                if !alias_table.contains_key(a) {
                    return Err(syn(format!(
                        "{} type alias reference to an undeclared alias",
                        if c.is_edge { "edge" } else { "node" }
                    )));
                }
            }
            GraphTypeElementRef::InSitu {
                implies: true,
                label,
                ..
            } => {
                if !id_labels.contains(label) {
                    return Err(syn("implies in-situ reference must name a declared type"));
                }
            }
            GraphTypeElementRef::InSitu {
                implies: false,
                label,
                ..
            } => {
                if matches!(
                    c.kind,
                    GraphTypeConstraintVerb::Exists | GraphTypeConstraintVerb::PropertyType
                ) && id_labels.contains(label)
                {
                    return Err(syn(
                        "independent constraint conflicts with an identifying label",
                    ));
                }
            }
            GraphTypeElementRef::Any => {}
        }
    }
    Ok(())
}

/// Single-property verbs and Rule 13 (duplicate constraint name).
pub(super) fn check_constraint_arity_and_names(body: &GraphTypeBody) -> Result<(), EngineError> {
    // A property existence / property type constraint names exactly one
    // property (Neo4j: "Constraint type 'IS NOT NULL'/'IS TYPED' does not
    // allow multiple properties"). KEY/UNIQUE stay multi-property.
    for c in &body.constraints {
        if c.properties.len() > 1 {
            if let Some(verb) = match c.kind {
                GraphTypeConstraintVerb::Exists => Some("IS NOT NULL"),
                GraphTypeConstraintVerb::PropertyType => Some("IS TYPED"),
                GraphTypeConstraintVerb::Key | GraphTypeConstraintVerb::Unique => None,
            } {
                return Err(syn(format!(
                    "Constraint type '{verb}' does not allow multiple properties"
                )));
            }
        }
    }

    // Rule 13: duplicate constraint name.
    let mut names: HashSet<&SmallName> = HashSet::new();
    for c in &body.constraints {
        if let Some(n) = &c.name {
            if !names.insert(n) {
                return Err(syn("duplicate constraint name"));
            }
        }
    }
    Ok(())
}

/// Rule 14 and the one-index-backing-per-subject clash.
pub(super) fn check_constraint_keys(
    body: &GraphTypeBody,
    tables: &ElementTables<'_>,
    canonicalizer_input: bool,
) -> Result<(), EngineError> {
    // Rule 14: duplicate constraint key -- same (entity, resolved label,
    // properties, kind).
    let resolve = |c: &GraphTypeConstraintSpec| -> Option<SmallName> {
        resolve_ref_label(&c.target, tables.alias_table(c.is_edge))
    };
    let mut seen_keys: Vec<(bool, SmallName, Vec<SmallName>, u8)> = Vec::new();
    // KEY and UNIQUE are both index-backed: two of them on one subject — even
    // of different kinds, and even when one is inline in an element type and
    // the other a standalone constraint — cannot coexist (Neo4j 22N66/22N67:
    // the second is a clash, not a distinct rule).
    let mut index_backed_subjects: Vec<(bool, SmallName, Vec<SmallName>)> = Vec::new();
    for c in &body.constraints {
        if let Some(label) = resolve(c) {
            let key = (
                c.is_edge,
                label.clone(),
                c.properties.clone(),
                constraint_rank(&c.kind),
            );
            if seen_keys.contains(&key) {
                return Err(syn("duplicate constraint key"));
            }
            seen_keys.push(key);
            // CIP-180 scenario [8] pins the KEY/UNIQUE/NOT NULL/IS :: rank
            // by putting all four kinds on one subject; a declaration cannot
            // (22N66), so the canonicalizer-input spelling skips this clash.
            if !canonicalizer_input
                && matches!(
                    c.kind,
                    GraphTypeConstraintVerb::Key | GraphTypeConstraintVerb::Unique
                )
            {
                let subject = (c.is_edge, label, c.properties.clone());
                if index_backed_subjects.contains(&subject) {
                    return Err(syn("duplicate constraint key"));
                }
                index_backed_subjects.push(subject);
            }
        }
    }
    Ok(())
}

/// CIP-181 independent property-type constraints' value types.
pub(super) fn check_independent_property_types(body: &GraphTypeBody) -> Result<(), EngineError> {
    // CIP-181 Independent constraints [18-22]: an independent property
    // type constraint's value type is subject to the same restriction as a
    // standalone property type constraint (CIP-85's `validate_property_type`
    // -- no top-level NOT NULL, no ANY/NODE/RELATIONSHIP/PATH/MAP, and the
    // usual LIST<...> inner-type restrictions), unlike a node/edge type's
    // own declared properties (`validate_property_set`), which allow a
    // top-level NOT NULL.
    for c in &body.constraints {
        if c.kind == GraphTypeConstraintVerb::PropertyType {
            if let Some(te) = &c.property_type {
                let canon = type_expr_to_canon(te);
                relate_parser::validate_property_type_expr(te).map_err(|_| {
                    syn(format!(
                        "{} type constraint has a value type that is not supported{}",
                        if c.is_edge { "Edge" } else { "Node" },
                        if canon.not_null {
                            " (non-nullable type)"
                        } else {
                            ""
                        }
                    ))
                })?;
            }
        }
    }
    Ok(())
}

/// The CIP-266 inheritance property-definition rules over the resolved
/// effective properties: a re-specified (`declared`) property must be
/// repeated exactly or further restrict the intersection of its parents'
/// effective definitions, and an implicitly inherited property must be
/// instantiable (not `NULL`/`NOTHING`).
fn validate_inheritance_properties(
    body: &GraphTypeBody,
    inheritance: &Inheritance,
) -> Result<(), EngineError> {
    let effective = EffectiveProperties::resolve(body, inheritance)?;
    for node in &body.node_types {
        let label = &node.identifying_label;
        for p in &node.properties {
            // The intersection of the parents' effective definitions,
            // a parent not defining the property contributing `ANY`
            // (CIP-266): the declared type must restrict it.
            let intersection =
                parents_effective_intersection(inheritance, &effective, label, &p.name);
            let declared = type_expr_to_canon(&p.value_type);
            if !canon_allows(&intersection, &declared) {
                return Err(syn(format!(
                    "The property definition `{} :: {}` is not allowed as it is either in \
                     conflict with or more permissive than the property definition `{} :: {}` \
                     coming from the intersection of property definitions of the implied \
                     node element types",
                    p.name,
                    render_canon(&declared),
                    p.name,
                    render_canon(&intersection)
                )));
            }
        }
        // An implicitly inherited (not re-specified) property must be
        // instantiable (NAGTE5: a `NULL` intersection is not an allowed
        // property type).
        for (property, definition) in label.with_str(|text| effective.of(text)) {
            if !definition.declared
                && matches!(
                    definition.canon.kind,
                    CanonKind::Predefined(PredefinedKind::Null | PredefinedKind::Nothing)
                )
            {
                return Err(syn(format!(
                    "The property definition `{} :: {}` coming from the intersection of \
                     property definitions from multiple implied node element types is not \
                     an allowed property type",
                    property,
                    render_canon(&definition.canon)
                )));
            }
        }
    }
    Ok(())
}

/// One child's parents' effective intersection for one property (CIP-266
/// diamond rule): every parent's effective definition -- `ANY` when the
/// parent's chain does not define the property -- intersected pointwise,
/// starting from `ANY`.
fn parents_effective_intersection(
    inheritance: &Inheritance,
    effective: &EffectiveProperties,
    label: &SmallName,
    property: &SmallName,
) -> CanonType {
    let mut intersection = CanonType::predefined(PredefinedKind::Any, false);
    // The inheritance/effective tables are String-keyed (the stored label
    // domain); both probes read through the allocation-free text bridge.
    for parent in label.with_str(|text| inheritance.parents_of(text)) {
        let parent_definition = property
            .with_str(|text| effective.property(parent, text))
            .map(|d| d.canon.clone())
            .unwrap_or_else(|| CanonType::predefined(PredefinedKind::Any, false));
        intersection = canon_intersection(&intersection, &parent_definition);
    }
    intersection
}
