use super::*;

use relate_core::{preview_feature_disabled_text, PreviewFeature, PreviewFeatures};
use relate_model::{ConstraintClassification, EntityKind};
use relate_names::SmallName;

use super::validate_rules::*;

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
pub(super) fn gate(preview: PreviewFeatures, feature: PreviewFeature) -> Result<(), EngineError> {
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
    // Rule groups run in the order their errors take precedence.
    let tables = ElementTables::of(body);
    check_identifying_labels(body, &tables)?;
    check_alias_namespace(body, scope)?;
    let inheritance = check_implied_labels(body, preview)?;
    check_property_rules(body, preview, &inheritance)?;
    check_entity_qualifiers(body)?;
    check_edge_endpoints(body, &tables)?;
    if !canonicalizer_input {
        check_ineffectual_edge_types(body)?;
    }
    check_constraint_targets(body, &tables)?;
    check_constraint_arity_and_names(body)?;
    check_constraint_keys(body, &tables, canonicalizer_input)?;
    check_independent_property_types(body)
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
