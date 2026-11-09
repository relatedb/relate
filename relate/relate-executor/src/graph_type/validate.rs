use super::*;

use relate_core::{preview_feature_disabled_text, PreviewFeature, PreviewFeatures};
use relate_model::{ConstraintClassification, EntityKind};
use relate_names::SmallName;
use relate_parser::CanonType;

/// Looks up a node/edge type alias's identifying label. Used both by
/// validation (reference resolution) and by derivation/canonicalization
/// (resolving an edge endpoint / constraint target to its label).
pub fn alias_tables(
    body: &GraphTypeBody,
) -> (
    HashMap<&SmallName, &SmallName>,
    HashMap<&SmallName, &SmallName>,
) {
    let node_alias_label: HashMap<&SmallName, &SmallName> = body
        .node_types
        .iter()
        .filter_map(|nt| nt.alias.as_ref().map(|a| (a, &nt.identifying_label)))
        .collect();
    let edge_alias_label: HashMap<&SmallName, &SmallName> = body
        .edge_types
        .iter()
        .filter_map(|et| et.alias.as_ref().map(|a| (a, &et.identifying_label)))
        .collect();
    (node_alias_label, edge_alias_label)
}

/// Resolves an `<node/edge type reference>` to the label it names, `None`
/// for the any type. An `AliasRef` is resolved through `alias_table`; an
/// `InSitu` reference already carries its label directly.
pub fn resolve_ref_label(
    r: &GraphTypeElementRef,
    alias_table: &HashMap<&SmallName, &SmallName>,
) -> Option<SmallName> {
    match r {
        GraphTypeElementRef::Any => None,
        GraphTypeElementRef::AliasRef(a) => alias_table.get(a).map(|s| (*s).clone()),
        GraphTypeElementRef::InSitu { label, .. } => Some(label.clone()),
    }
}

/// Registers `a` as denoting `(is_edge, label)` in the shared element-type-
/// alias namespace (Rule 3/4), or confirms it already does. Only errors
/// when `a` is already bound to a genuinely different `(is_edge, label)`
/// pair -- a repeated in-situ reference restating its own identity (CS[8]:
/// `(st:Site => ...)` then `FOR (st:Site =>)`) is not a conflict.
pub(super) fn declare_alias<'a>(
    aliases: &mut HashMap<&'a SmallName, (bool, &'a SmallName)>,
    a: &'a SmallName,
    is_edge: bool,
    label: &'a SmallName,
) -> Result<(), EngineError> {
    match aliases.get(a) {
        Some(&(existing_is_edge, existing_label))
            if existing_is_edge != is_edge || existing_label != label =>
        {
            Err(syn("duplicate element type alias"))
        }
        _ => {
            aliases.insert(a, (is_edge, label));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------
// CIP-178: validation (CREATE GRAPH TYPE only)
// ---------------------------------------------------------------------

/// The executor-side ADR-0371 gate: the core's canonical
/// `PreviewFeatures::require`, carried through the `EngineError`
/// boundary with the same authored text -- the one gate the graph-type
/// layer's stored-state half consults (its in-body half lives at
/// statement admission). The text is the core's own (the `Semantic`
/// detail `require`'s only failure carries); the `EngineError` wrapper
/// supplies the class, so the core stays the single spelling.
fn gate(preview: PreviewFeatures, feature: PreviewFeature) -> Result<(), EngineError> {
    preview
        .require(feature)
        .map_err(|_| syn(preview_feature_disabled_text(feature)))
}

/// Validates a `GraphTypeBody` against the CIP-178 "shall" rules that need
/// whole-body context (alias/label uniqueness, reference resolution, the
/// constraint-only rules 1/2/13-16/22/23), plus the CIP graph-type trio's
/// semantic rules: CIP-283's optional (`prop :: ANY`) properties,
/// CIP-262's `DEFAULT ON COMMIT` shape/storable/conformance and
/// inheritance-interaction rules, and CIP-266's inheritance rules (cycle
/// freedom, restriction-only re-specification, instantiable diamond
/// intersections). Called by `CREATE GRAPH TYPE`'s clause handler and the
/// `ALTER CURRENT GRAPH TYPE` verb algebra -- see `GraphTypeBody`'s doc
/// comment.
///
/// `preview` is the store's `RELATE_PREVIEW`-resolved set (ADR-0371):
/// the stored-state half of the trio's gates fails closed here with the
/// same authored error statement admission produces for the in-body
/// half (an `ADD` whose identifying label a stored node element type
/// already implies).
///
/// `canonicalizer_input` marks CIP-180's `AS` spelling, whose own corpus
/// pins the canonical *rendering* rather than a valid declaration. It
/// exempts exactly two rules, each because a CIP-180 scenario exercises the
/// renderer with a body a declaration would reject: Rule 9's ineffectual
/// single-label edge type (`{ ()-[:REL =>]->() }`) and the
/// one-index-backing-per-subject clash that scenario [8] creates on purpose
/// (`REQUIRE p.a IS UNIQUE` beside `REQUIRE p.a IS KEY`, to pin the type
/// rank). Every other body rule still applies to both spellings.
pub fn validate_graph_type_body(
    body: &GraphTypeBody,
    preview: PreviewFeatures,
    scope: GraphTypeScope,
    canonicalizer_input: bool,
) -> Result<(), EngineError> {
    // Rule 5/6: duplicate identifying label (node/edge types each have
    // their own label namespace).
    let mut node_labels: HashSet<&SmallName> = HashSet::new();
    for nt in &body.node_types {
        if !node_labels.insert(&nt.identifying_label) {
            return Err(syn("duplicate identifying label"));
        }
    }
    let mut edge_labels: HashSet<&SmallName> = HashSet::new();
    for et in &body.edge_types {
        if !edge_labels.insert(&et.identifying_label) {
            return Err(syn("duplicate identifying label"));
        }
    }

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
    // (CIP-178 CS[9]/[11]). `=>`-qualified references (a pure local rename of
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

    // Rule 7 (own implied set): a node type's identifying label must not
    // recur in its own implied label set.
    for nt in &body.node_types {
        if nt.implied_labels.contains(&nt.identifying_label) {
            return Err(syn("identifying label repeated in implied label set"));
        }
    }
    // CIP-266 (replacing CIP-178 Rule 7's GTSB half): an implied label
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

    // Rule 8: a single-label (no implied labels) node type with an empty
    // property set is ineffectual.
    for nt in &body.node_types {
        if nt.implied_labels.is_empty() && nt.properties.is_empty() {
            return Err(syn("single-label node type with empty property set"));
        }
    }

    // PT Rule 3a/3b: duplicate property names; CIP-283's optional
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
        validate_inheritance_properties(body, &inheritance)?;
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
        let defaults = effective_default_map(body, &inheritance)?;
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

    let (node_alias_label, edge_alias_label) = alias_tables(body);
    let node_id_labels: HashSet<&SmallName> = body
        .node_types
        .iter()
        .map(|n| &n.identifying_label)
        .collect();
    let edge_id_labels: HashSet<&SmallName> = body
        .edge_types
        .iter()
        .map(|n| &n.identifying_label)
        .collect();

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
                    if !node_id_labels.contains(label) {
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
                    if !node_alias_label.contains_key(a) {
                        return Err(syn("node type alias reference to an undeclared alias"));
                    }
                }
                _ => {}
            }
        }
    }

    // Rule 9: an ineffectual single-label edge type (empty props, any
    // source, any destination). CIP-180's canonicalizer input is exactly
    // this shape (see the function's `canonicalizer_input`).
    if !canonicalizer_input {
        for et in &body.edge_types {
            if et.properties.is_empty()
                && matches!(et.source, GraphTypeElementRef::Any)
                && matches!(et.target, GraphTypeElementRef::Any)
            {
                return Err(syn("single-label edge type is ineffectual"));
            }
        }
    }

    // Constraint target resolution + Rules 1/15/16 (Rule 1/2/13/16-of-CS/
    // 22/23 are already enforced at parse time; see
    // `Parser::parse_graph_type_constraint_spec`).
    for c in &body.constraints {
        let (alias_table, id_labels) = if c.is_edge {
            (&edge_alias_label, &edge_id_labels)
        } else {
            (&node_alias_label, &node_id_labels)
        };
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

    // Rule 14: duplicate constraint key -- same (entity, resolved label,
    // properties, kind).
    let resolve = |c: &GraphTypeConstraintSpec| -> Option<SmallName> {
        let table = if c.is_edge {
            &edge_alias_label
        } else {
            &node_alias_label
        };
        resolve_ref_label(&c.target, table)
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

/// Whether any element type's properties carry a `DEFAULT ON COMMIT`
/// clause (CIP-262).
pub(crate) fn body_has_default_values(body: &GraphTypeBody) -> bool {
    body.node_types
        .iter()
        .flat_map(|element| element.properties.iter())
        .chain(
            body.edge_types
                .iter()
                .flat_map(|element| element.properties.iter()),
        )
        .any(|p| p.default_value.is_some())
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
    let by_label: HashMap<&SmallName, &GraphTypeNodeTypeSpec> = body
        .node_types
        .iter()
        .map(|n| (&n.identifying_label, n))
        .collect();
    for (label, node) in &by_label {
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

/// PT Rule 3a (duplicate property names) / 3b (a bare `ANY` value type,
/// without `NOT NULL`, is not storable) / the `LIST<...>` inner-type
/// restrictions (CIP-85, shared with property type constraints -- no
/// union, no nested list, no nullable inner type; unlike a standalone
/// property type constraint, a top-level `NOT NULL` on the property itself
/// is fine here, so this doesn't call `validate_property_type` wholesale).
/// CIP-283: a bare `ANY` value type is allowed behind the
/// `graph-type-opt-props` preview gate. CIP-262: a `DEFAULT ON COMMIT`
/// value must be a literal or an allowed type constructor, storable on a
/// property, and conforming to the property's declared value type.
pub(super) fn validate_property_set(
    props: &[relate_parser::GraphTypePropertyType],
    preview: PreviewFeatures,
) -> Result<(), EngineError> {
    let mut seen: HashSet<&SmallName> = HashSet::new();
    for p in props {
        if !seen.insert(&p.name) {
            return Err(syn("duplicate property name"));
        }
        if let Some(expr) = &p.default_value {
            gate(preview, PreviewFeature::GraphTypeDefaultValues)?;
            let default = DefaultValue::from_expr(expr)?;
            if !default.is_storable() {
                return Err(syn(format!(
                    "Invalid input {} for default property value: the value cannot be \
                     stored on a property",
                    default.render()
                )));
            }
            if !default.conforms_to(&p.value_type)? {
                return Err(syn(format!(
                    "Invalid input {} for default property value. Expected to be {}",
                    default.render(),
                    render_canon(&type_expr_to_canon(&p.value_type))
                )));
            }
        }
        let canon = type_expr_to_canon(&p.value_type);
        validate_storable_property_type(&canon, preview)?;
    }
    Ok(())
}

/// The property types an element type may declare: the storable scalar and
/// temporal kinds (plus `ANY NOT NULL`, the optional-property forms, and
/// `LIST<...>`/`VECTOR<...>` shapes), matching the value types a property
/// can actually hold. `NODE`/`RELATIONSHIP`/`PATH`/`MAP`/`NOTHING`/`NULL`
/// are rejected here just as CIP-85 rejects them in a standalone property
/// type constraint, and a `VECTOR` must name both coordinate type and
/// dimension; the list-inner rules are [`validate_list_element`]'s. A union
/// member may not carry `NOT NULL` (Neo4j rejects `STRING | INTEGER NOT
/// NULL`), which is the shape `validate_property_type` also rejects --
/// this function differs from it only in allowing the element type's own
/// top-level `NOT NULL`.
///
/// CIP-283's optional-property rule lives in the `Predefined` arm, so this
/// one match answers "is this kind storable?" at every recursion level: a
/// bare `ANY` nested in a union is gated exactly like a top-level one.
///
/// [`validate_list_element`]: relate_parser::validate_list_element
fn validate_storable_property_type(
    canon: &relate_parser::type_normalize::CanonType,
    preview: PreviewFeatures,
) -> Result<(), EngineError> {
    match &canon.kind {
        CanonKind::Predefined(kind) => {
            if matches!(kind, PredefinedKind::Any) {
                // CIP-283: a bare `ANY` value type is the optional property,
                // storable only behind `graph-type-opt-props`. The rejection
                // text (rather than the preview-error text) stays: a
                // statement is gated at admission, so only an
                // ontology-converted body reaches here ungated.
                if !canon.not_null && !preview.enabled(PreviewFeature::GraphTypeOptProps) {
                    return Err(syn("non-storable value type"));
                }
                return Ok(());
            }
            if matches!(
                kind,
                PredefinedKind::Nothing
                    | PredefinedKind::Null
                    | PredefinedKind::Node
                    | PredefinedKind::Relationship
                    | PredefinedKind::Path
                    | PredefinedKind::Map
            ) {
                return Err(syn(format!(
                    "Invalid property type `{}`.",
                    render_canon(canon)
                )));
            }
            Ok(())
        }
        CanonKind::List(elem) => relate_parser::validate_list_element(canon, elem).map_err(syn),
        CanonKind::Vector { coord, dimension } => {
            if coord.is_none() || dimension.is_none() {
                return Err(syn(format!(
                    "Invalid property type `{}`. Property type constraints for vectors need \
                     to define both coordinate type and dimension.",
                    render_canon(canon)
                )));
            }
            Ok(())
        }
        CanonKind::Union(members) => {
            // Nullability is uniform across a closed union by grammar
            // (CIP-86 "Not Null Uses", enforced at parse): `INT NOT NULL |
            // FLOAT NOT NULL` is a valid element-type property type, while
            // the mixed spelling is already a parse error.
            for member in members {
                validate_storable_property_type(member, preview)?;
            }
            Ok(())
        }
    }
}

pub(super) fn constraint_rank(kind: &GraphTypeConstraintVerb) -> u8 {
    match kind {
        GraphTypeConstraintVerb::Key => 1,
        GraphTypeConstraintVerb::Unique => 2,
        GraphTypeConstraintVerb::Exists => 3,
        GraphTypeConstraintVerb::PropertyType => 4,
    }
}

// ---------------------------------------------------------------------
// CIP-182/183: `ALTER CURRENT GRAPH TYPE DROP` element matching -- a drop
// element must specify either just the identifying label/type (matches
// regardless of the stored definition's other fields) or the *exact*
// stored definition (implied label set + property set, order-insensitive).
// ---------------------------------------------------------------------

/// Order-insensitive equality of two property-type sets: same names, and
/// for each name the same rendered canonical property type (including
/// `NOT NULL` and, per CIP-262, the `DEFAULT ON COMMIT` value -- a DROP
/// must restate a default it stored, FE4).
pub(crate) fn property_sets_equal(
    a: &[relate_parser::GraphTypePropertyType],
    b: &[relate_parser::GraphTypePropertyType],
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // An unrenderable restated default cannot match the stored one: the
    // drop is not the exact restatement FE4 demands (the typed
    // "node element type specified incorrectly" error the exact-match
    // check raises), never a renderer panic.
    let render = |p: &relate_parser::GraphTypePropertyType| -> Option<(String, String)> {
        Some((p.name.to_string(), super::canonical_property_type(p).ok()?))
    };
    let av: Option<Vec<(String, String)>> = a.iter().map(render).collect();
    let bv: Option<Vec<(String, String)>> = b.iter().map(render).collect();
    let (Some(mut av), Some(mut bv)) = (av, bv) else {
        // An unrenderable restatement cannot match the stored one.
        return false;
    };
    av.sort();
    bv.sort();
    av == bv
}

/// Whether a DROP node type spec is the minimal "single-label form"
/// (CIP-182: `(:Label =>)`, no implied labels, no properties) -- which
/// matches an existing node type by identifying label alone, regardless of
/// its other fields.
pub fn is_bare_node_type_ref(nt: &GraphTypeNodeTypeSpec) -> bool {
    nt.implied_labels.is_empty() && nt.properties.is_empty()
}

/// Whether a DROP node type spec exactly matches an existing node type's
/// full definition (implied label set + property set).
pub fn node_type_matches_exactly(
    spec: &GraphTypeNodeTypeSpec,
    existing: &GraphTypeNodeTypeSpec,
) -> bool {
    let mut sl: Vec<String> = spec.implied_labels.iter().map(|s| s.to_string()).collect();
    let mut el: Vec<String> = existing
        .implied_labels
        .iter()
        .map(|s| s.to_string())
        .collect();
    sl.sort();
    el.sort();
    sl == el && property_sets_equal(&spec.properties, &existing.properties)
}

/// Whether a DROP edge type spec is the minimal "anonymous-endpoint form"
/// (CIP-182: `()-[:Label =>]->()`, both endpoints the any type, no
/// properties) -- which matches an existing edge type by identifying label
/// alone, regardless of its endpoints/properties.
pub fn is_bare_edge_type_ref(et: &relate_parser::GraphTypeEdgeTypeSpec) -> bool {
    matches!(et.source, GraphTypeElementRef::Any)
        && matches!(et.target, GraphTypeElementRef::Any)
        && et.properties.is_empty()
}

/// Whether a DROP edge type spec exactly matches an existing edge type's
/// full definition (source/target resolved labels + property set).
/// `alias_table` resolves any `AliasRef` endpoints against the graph type
/// they were both declared/stored in.
pub fn edge_type_matches_exactly(
    spec: &relate_parser::GraphTypeEdgeTypeSpec,
    existing: &relate_parser::GraphTypeEdgeTypeSpec,
    alias_table: &HashMap<&SmallName, &SmallName>,
) -> bool {
    endpoint_matches(&spec.source, &existing.source, alias_table)
        && endpoint_matches(&spec.target, &existing.target, alias_table)
        && property_sets_equal(&spec.properties, &existing.properties)
}

/// An endpoint's exact-match rule: the resolved labels agree, and a DROP spec
/// that *identifies* the endpoint (`(x:Node =>)`, or an alias reference to a
/// declared node type) must match a stored reference that identifies it too.
/// The stored side may identify where the spec does not -- the stored alias
/// form `(n)-[:REL =>]->()` is dropped by `(:Node)-[:REL =>]->()` -- but the
/// reverse is the "different to the one specified in the query" rejection
/// (a stored plain `(:Node)` is not the identifying `(:Node =>)` endpoint).
fn endpoint_matches(
    spec: &GraphTypeElementRef,
    existing: &GraphTypeElementRef,
    alias_table: &HashMap<&SmallName, &SmallName>,
) -> bool {
    match (
        endpoint_identity(spec, alias_table),
        endpoint_identity(existing, alias_table),
    ) {
        (EndpointIdentity::Any, EndpointIdentity::Any) => true,
        (
            EndpointIdentity::Identified {
                label: spec_label,
                identifying: spec_identifying,
            },
            EndpointIdentity::Identified {
                label: existing_label,
                identifying: existing_identifying,
            },
        ) => spec_label == existing_label && (!spec_identifying || existing_identifying),
        // The any type against an identified endpoint is a mismatch, and a
        // reference that resolves to no declared type is never the wildcard.
        _ => false,
    }
}

/// An endpoint's identity, keeping "the any type" distinct from a reference
/// that names no declared type: collapsing both into `None` made a DROP spec
/// whose alias does not resolve match the `()` wildcard.
pub(crate) enum EndpointIdentity {
    /// The any type (`()`).
    Any,
    /// An alias reference no declared node/edge type binds. Matching rejects
    /// the reference either way, and the rejecting caller names the spec
    /// itself, so the alias needs no payload here.
    Unresolved,
    /// A resolved label plus whether the reference *identifies* it
    /// (`(x:Node =>)`, or an alias reference to a declared node type) rather
    /// than merely constraining it (`(:Node)`).
    Identified { label: SmallName, identifying: bool },
}

pub(crate) fn endpoint_identity(
    reference: &GraphTypeElementRef,
    alias_table: &HashMap<&SmallName, &SmallName>,
) -> EndpointIdentity {
    match reference {
        GraphTypeElementRef::Any => EndpointIdentity::Any,
        GraphTypeElementRef::AliasRef(_) => match resolve_ref_label(reference, alias_table) {
            Some(label) => EndpointIdentity::Identified {
                label,
                identifying: true,
            },
            None => EndpointIdentity::Unresolved,
        },
        GraphTypeElementRef::InSitu { label, implies, .. } => EndpointIdentity::Identified {
            label: label.clone(),
            identifying: *implies,
        },
    }
}

// ---------------------------------------------------------------------
// CIP-186: constraint classification (undesignated/dependent/independent)
// and the CREATE/DROP CONSTRAINT integration built on it.
// ---------------------------------------------------------------------

/// Whether `label` is the identifying label of a node type (`is_edge`
/// `false`) or edge type (`is_edge` true) declared by `body` -- CIP-186's
/// "identifies a node/edge type" test, used both to classify a stored
/// constraint (dependent vs independent) and to reject a `CREATE
/// CONSTRAINT` that would create a dependent one.
pub fn label_identifies_type(body: &GraphTypeBody, is_edge: bool, label: &str) -> bool {
    if is_edge {
        body.edge_types
            .iter()
            .any(|et| et.identifying_label == label)
    } else {
        body.node_types
            .iter()
            .any(|nt| nt.identifying_label == label)
    }
}

/// CIP-186's three-way constraint classification:
/// [`ConstraintClassification::Undesignated`] for a uniqueness/key
/// constraint (always, regardless of graph type -- `IS UNIQUE`/`IS KEY`
/// never depend on the current graph type),
/// [`ConstraintClassification::Dependent`] for a graph-type-derived
/// label/endpoint-label constraint (CIP-187's
/// `labelExistence`/`sourceLabel`/`targetLabel` kinds) or for an
/// existence/property-type constraint whose label/relationship-type
/// currently identifies a node/edge type,
/// [`ConstraintClassification::Independent`] for every other constraint.
pub fn classify_constraint(
    body: &GraphTypeBody,
    c: &relate_model::Constraint,
) -> ConstraintClassification {
    // A stored `entity_type` other than "RELATIONSHIP" classifies as a
    // node, matching the historical `== "RELATIONSHIP"` string test.
    let entity = EntityKind::from_display_name(&c.entity_type).unwrap_or(EntityKind::Node);
    classify_constraint_parts(body, Some(c.constraint_type), entity, &c.label)
}

/// The primitives form of [`classify_constraint`]: classifies from the
/// parsed kind (`None` for a stored catalog tag that no longer parses --
/// it keeps the historical label-identifies-type probe), the entity kind,
/// and the label, so a stored `ConstraintDef` row is classified without
/// building a [`relate_model::Constraint`].
pub fn classify_constraint_parts(
    body: &GraphTypeBody,
    kind: Option<ConstraintType>,
    entity: EntityKind,
    label: &str,
) -> ConstraintClassification {
    match kind {
        // `IS UNIQUE`/`IS KEY` never depend on the current graph type.
        Some(ConstraintType::Unique | ConstraintType::Key) => {
            ConstraintClassification::Undesignated
        }
        // CIP-187's graph-type-derived label/endpoint-label kinds.
        Some(
            ConstraintType::LabelExistence
            | ConstraintType::SourceLabel
            | ConstraintType::TargetLabel,
        ) => ConstraintClassification::Dependent,
        Some(ConstraintType::Exists | ConstraintType::PropertyType) | None => {
            if label_identifies_type(body, entity.is_relationship(), label) {
                ConstraintClassification::Dependent
            } else {
                ConstraintClassification::Independent
            }
        }
        // The standalone CIP-177 label-existence forms are independent by
        // definition: no graph type manages them (parser/schema.rs).
        Some(
            ConstraintType::NodeLabelExistence
            | ConstraintType::SourceLabelExistence
            | ConstraintType::TargetLabelExistence,
        ) => ConstraintClassification::Independent,
    }
}
