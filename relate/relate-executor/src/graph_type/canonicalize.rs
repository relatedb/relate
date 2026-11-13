use super::*;
use relate_names::SmallName;

use relate_core::{Error, Result};

// ---------------------------------------------------------------------
// CIP-180: canonical form
// ---------------------------------------------------------------------

/// Renders a `GraphTypeBody` to its CIP-180 canonical `<graph type
/// specification body>` string: component groups in order (node types,
/// edge types, node type constraints, edge type constraints), each
/// internally sorted per the CIP-180 canonical sort order, joined with
/// `", "`.
pub fn canonicalize_graph_type_body(body: &GraphTypeBody) -> Result<String> {
    let (types, node_c, edge_c, _node_id_labels, _edge_id_labels) =
        graph_type_components(body, CanonicalForm::Declared)?;
    Ok(sorted_parts(types, node_c, edge_c).join(", "))
}

type ConstraintSortKey = (u8, String, Vec<String>, u8);
type ConstraintGroups = (
    Vec<String>,
    Vec<(ConstraintSortKey, String)>,
    Vec<(ConstraintSortKey, String)>,
    // The identifying-label sets (round-15 F-15-1): graph_type_components
    // already builds them for its own constraint grouping, so it returns
    // them -- the only spelling of the builders' results is here.
    Vec<String>,
    Vec<String>,
);

/// Sorts each constraint group by its CIP-180 sort key and assembles the
/// ordered part list (type parts first, then the sorted node- and
/// edge-constraint renderings) — the shared assembly of
/// [`canonicalize_graph_type_body`] and
/// [`current_graph_type_specification`], which each join the list with
/// their own separator convention.
fn sorted_parts(
    types: Vec<String>,
    mut node_c: Vec<(ConstraintSortKey, String)>,
    mut edge_c: Vec<(ConstraintSortKey, String)>,
) -> Vec<String> {
    node_c.sort_by(|a, b| a.0.cmp(&b.0));
    edge_c.sort_by(|a, b| a.0.cmp(&b.0));
    let mut parts = types;
    parts.extend(node_c.into_iter().map(|(_, s)| s));
    parts.extend(edge_c.into_iter().map(|(_, s)| s));
    parts
}

/// The same `constraint_<type>_<label>_<properties>` fallback name
/// `derive_constraints` assigns an unnamed (undesignated-by-declaration)
/// constraint when registering it in the store -- the same Neo4j hash
/// `DerivedName` computes there, so `current_graph_type_specification`'s
/// rendering of an unnamed body constraint matches the name it is actually
/// stored under.
pub(super) fn auto_constraint_name(c: &GraphTypeConstraintSpec, label: &str) -> String {
    let entity = EntityKind::from_relationship_flag(c.is_edge);
    let kind = super::derive::constraint_type_of(&c.kind);
    // The DerivedName hash layer is spelled text; the AST property list
    // renders once, here.
    let props: Vec<String> = c.properties.iter().map(|p| p.to_string()).collect();
    match (&c.kind, &c.property_type) {
        (GraphTypeConstraintVerb::PropertyType, Some(te)) => {
            let canon = type_expr_to_canon(te);
            super::name::derived_constraint_name(&super::name::DerivedName::property_type(
                entity, label, &props, &canon,
            ))
        }
        // A persisted property-type spec without a type only reaches this
        // arm through corruption; hashing it as a plain kind mirrors
        // `derive_declared_constraint`'s fallback.
        _ => super::name::derived_constraint_name(&super::name::DerivedName::props(
            entity, label, &props, kind,
        )),
    }
}

/// Which rendering of a graph type body the shared component builder
/// produces; the two facts that vary (fallback constraint names, rendered
/// options) always vary together, so one variant names both.
#[derive(Clone, Copy)]
pub(super) enum CanonicalForm {
    /// The literal declared specification (CIP-180): unnamed constraints
    /// stay unnamed and options render.
    Declared,
    /// The observed stored graph type (CIP-184): every constraint carries
    /// its concrete auto-generated name and options are not rendered.
    Stored,
}

impl CanonicalForm {
    fn auto_name_constraints(self) -> bool {
        matches!(self, Self::Stored)
    }

    fn render_options(self) -> bool {
        matches!(self, Self::Declared)
    }
}

/// Shared by `canonicalize_graph_type_body` and
/// `current_graph_type_specification`: the node/edge type parts (already
/// fully rendered, in final order) plus the node- and edge-constraint
/// groups as `(sort key, rendered string)` pairs, *not yet sorted* --
/// `current_graph_type_specification` interleaves extra
/// independently-created constraints into these same two groups before
/// sorting each and rendering the final `specification` string (CIP-184).
///
/// `form`: the canonical form (used by
/// `canonicalize_graph_type_body` for CIP-182/183's fragment assertions)
/// renders an unnamed constraint with no name at all, matching the literal
/// declared specification; `SHOW CURRENT GRAPH TYPE` (CIP-184) instead
/// observes the *stored* graph type, where every constraint -- including
/// one declared without an explicit name -- already has a concrete,
/// auto-generated name, so `current_graph_type_specification` passes
/// [`CanonicalForm::Stored`] to render that fallback name instead of
/// omitting it.
pub(super) fn graph_type_components(
    body: &GraphTypeBody,
    form: CanonicalForm,
) -> Result<ConstraintGroups> {
    let (node_alias_label, edge_alias_label) = alias_tables(body);
    let node_id_labels = node_id_label_set(body);
    let edge_id_labels = edge_id_label_set(body);

    let mut type_parts: Vec<String> = Vec::new();

    let mut nts: Vec<&GraphTypeNodeTypeSpec> = body.node_types.iter().collect();
    nts.sort_by_key(|nt| nt.identifying_label.to_string());
    for nt in nts {
        type_parts.push(canonical_node_type(nt)?);
    }

    let mut ets: Vec<&relate_parser::GraphTypeEdgeTypeSpec> = body.edge_types.iter().collect();
    ets.sort_by_key(|et| et.identifying_label.to_string());
    for et in ets {
        type_parts.push(canonical_edge_type(et, &node_alias_label, &node_id_labels)?);
    }

    let node_c = constraint_group(body, form, false, &node_alias_label, &node_id_labels)?;
    let edge_c = constraint_group(body, form, true, &edge_alias_label, &edge_id_labels)?;

    // Deterministic set renderings (sorted label names): the callers
    // consume membership, not iteration order, but the rendering stays
    // stable for the pinned canonical texts.
    let node_id_labels = {
        let mut labels: Vec<String> = node_id_labels
            .into_iter()
            .map(|label| label.to_string())
            .collect();
        labels.sort();
        labels
    };
    let edge_id_labels = {
        let mut labels: Vec<String> = edge_id_labels
            .into_iter()
            .map(|label| label.to_string())
            .collect();
        labels.sort();
        labels
    };
    Ok((type_parts, node_c, edge_c, node_id_labels, edge_id_labels))
}

/// One side's (node or edge) constraint group, as unsorted
/// `(sort key, rendered string)` pairs. Each constraint's label resolves
/// once and feeds both the auto-generated fallback name and the sort key.
fn constraint_group(
    body: &GraphTypeBody,
    form: CanonicalForm,
    is_edge: bool,
    alias_table: &HashMap<&SmallName, &SmallName>,
    id_labels: &HashSet<&SmallName>,
) -> Result<Vec<(ConstraintSortKey, String)>> {
    body.constraints
        .iter()
        .filter(|c| c.is_edge == is_edge)
        .map(|c| {
            let label = resolved_constraint_label(c, alias_table)?;
            let fallback = form
                .auto_name_constraints()
                .then(|| auto_constraint_name(c, label.as_str()));
            Ok((
                constraint_sort_key(c, alias_table, id_labels)?,
                canonical_constraint(
                    c,
                    alias_table,
                    id_labels,
                    fallback.as_deref(),
                    form.render_options(),
                )?,
            ))
        })
        .collect()
}

pub(crate) fn node_id_label_set(body: &GraphTypeBody) -> HashSet<&SmallName> {
    body.node_types
        .iter()
        .map(|n| &n.identifying_label)
        .collect()
}

pub(crate) fn edge_id_label_set(body: &GraphTypeBody) -> HashSet<&SmallName> {
    body.edge_types
        .iter()
        .map(|n| &n.identifying_label)
        .collect()
}

/// `SHOW CURRENT GRAPH TYPE`'s `specification` column. Unlike
/// `canonicalize_graph_type_body` (CIP-180's single-line, `", "`-joined
/// form, used for CIP-182/183's `CONTAINS`-based fragment assertions),
/// this renders the CIP-184-observed pretty multi-line form (`{\n  ` +
/// parts joined by `,\n  ` + `\n}`, or the literal `{}` when empty) and
/// additionally folds in `extra_constraints` -- constraints present in the
/// store that aren't part of `body` itself (i.e. created directly via a
/// plain `CREATE CONSTRAINT`, not through `ALTER CURRENT GRAPH TYPE`).
/// Per CIP-184's constraint classification, a Key/Unique constraint is
/// always "Undesignated" and an Exists/PropertyType constraint created this
/// way is always "Independent" -- both stay top-level `CONSTRAINT` lines
/// (never folded into a node/edge type's own property set), interleaved
/// into the same sorted node-/edge-constraint groups `body`'s own
/// constraints occupy.
pub fn current_graph_type_specification(
    body: &GraphTypeBody,
    extra_constraints: &[relate_model::Constraint],
) -> Result<String> {
    // The identifying-label sets ride graph_type_components' return
    // (round-15 F-15-1): no second spelling of the builders.
    let (types, mut node_c, mut edge_c, node_id_labels, edge_id_labels) =
        graph_type_components(body, CanonicalForm::Stored)?;

    for c in extra_constraints {
        let is_edge = c.entity_type == "RELATIONSHIP";
        let id_labels = if is_edge {
            &edge_id_labels
        } else {
            &node_id_labels
        };
        let identified = id_labels.iter().any(|label| label == c.label.as_str());
        let key: ConstraintSortKey = (
            if identified { 1 } else { 2 },
            c.label.clone(),
            c.properties.clone(),
            storage_constraint_rank(c.constraint_type),
        );
        let rendered = canonical_constraint_from_storage(c, identified);
        if is_edge {
            edge_c.push((key, rendered));
        } else {
            node_c.push((key, rendered));
        }
    }

    let parts = sorted_parts(types, node_c, edge_c);

    if parts.is_empty() {
        Ok("{}".to_string())
    } else {
        Ok(format!("{{\n  {}\n}}", parts.join(",\n  ")))
    }
}

/// Renders a `relate_model::Constraint` (a raw, already-created
/// store constraint, not part of any `GraphTypeBody`) in the same CIP-180
/// canonical `CONSTRAINT` form `canonical_constraint` produces for a
/// body-native `GraphTypeConstraintSpec` -- used only by
/// `current_graph_type_specification` to render `extra_constraints`. The
/// verb, target shape and property spelling come from the one typed
/// storage-constraint renderer; this is its canonical style.
pub(super) fn canonical_constraint_from_storage(
    c: &relate_model::Constraint,
    identified: bool,
) -> String {
    render_storage_constraint(c, identified, &StorageConstraintStyle::CANONICAL)
}

/// A constraint target's resolved label. Validation rejects an alias
/// reference that resolves to no declared type, so a missing label here is
/// an invariant violation, never a rendering fallback: an empty label
/// renders as `(n:)` and hashes an empty auto-generated constraint name.
pub(crate) fn resolved_constraint_label(
    c: &GraphTypeConstraintSpec,
    alias_table: &HashMap<&SmallName, &SmallName>,
) -> Result<String> {
    resolve_ref_label(&c.target, alias_table)
        .map(|label| label.to_string())
        .ok_or_else(|| {
            Error::internal(
                "graph type constraint target resolves to no declared element type".to_string(),
            )
        })
}

/// The CIP-180 canonical constraint sort key: identified references (sort
/// label 1) before unidentified ones (sort label 2), then by resolved
/// label, then by the declared property-name list (order-sensitive, not
/// re-sorted -- this is what breaks the KEY-before-UNIQUE rank tie), then
/// by constraint-type rank.
pub(super) fn constraint_sort_key(
    c: &GraphTypeConstraintSpec,
    alias_table: &HashMap<&SmallName, &SmallName>,
    id_labels: &HashSet<&SmallName>,
) -> Result<(u8, String, Vec<String>, u8)> {
    let label = resolved_constraint_label(c, alias_table)?;
    let identified = id_labels.iter().any(|l| *l == label.as_str());
    Ok((
        if identified { 1 } else { 2 },
        label,
        c.properties.iter().map(|p| p.to_string()).collect(),
        constraint_rank(&c.kind),
    ))
}

pub(super) fn canonical_property_set(
    props: &[relate_parser::GraphTypePropertyType],
) -> Result<String> {
    let mut items: Vec<String> = props
        .iter()
        .map(canonical_property_type)
        .collect::<Result<Vec<_>>>()?;
    items.sort();
    Ok(items.join(", "))
}

/// The canonical form of one property type (CIP-180 + CIP-262):
/// `CPN :: CVT`, with ` DEFAULT ON COMMIT CVE` when a default value is
/// given -- the one rendering shared by the graph-type body form
/// ([`canonical_property_set`]), the derived property-type constraint's
/// stored `property_type` string, and the drop element-type matcher's
/// comparison key.
pub(super) fn canonical_property_type(p: &relate_parser::GraphTypePropertyType) -> Result<String> {
    let mut rendered = format!(
        "{} :: {}",
        p.name.with_str(quote_identifier_always),
        render_canon(&type_expr_to_canon(&p.value_type))
    );
    if let Some(expr) = &p.default_value {
        // The one canonical default-value rendering (the body validator
        // rejected every non-allowed shape before a body reaches here,
        // so the renderer's shape invariant holds).
        rendered.push_str(&super::canonical_default_clause(expr)?);
    }
    Ok(rendered)
}

pub(super) fn canonical_node_type(nt: &GraphTypeNodeTypeSpec) -> Result<String> {
    let mut s = format!(
        "(:{} =>",
        nt.identifying_label.with_str(quote_identifier_always)
    );
    if !nt.implied_labels.is_empty() {
        let mut labels: Vec<String> = nt
            .implied_labels
            .iter()
            .map(|l| l.with_str(quote_identifier_always))
            .collect();
        labels.sort();
        s.push_str(&format!(" :{}", labels.join("&")));
    }
    let props = canonical_property_set(&nt.properties)?;
    if !props.is_empty() {
        s.push_str(&format!(" {{{props}}}"));
    }
    s.push(')');
    Ok(s)
}

pub(super) fn canonical_node_endpoint(
    r: &GraphTypeElementRef,
    node_alias_label: &HashMap<&SmallName, &SmallName>,
    node_id_labels: &HashSet<&SmallName>,
) -> String {
    match resolve_ref_label(r, node_alias_label) {
        None => "()".to_string(),
        Some(label) => {
            if node_id_labels.contains(&label) {
                format!("(:{} =>)", label.with_str(quote_identifier_always))
            } else {
                format!("(:{})", label.with_str(quote_identifier_always))
            }
        }
    }
}

pub(super) fn canonical_edge_type(
    et: &relate_parser::GraphTypeEdgeTypeSpec,
    node_alias_label: &HashMap<&SmallName, &SmallName>,
    node_id_labels: &HashSet<&SmallName>,
) -> Result<String> {
    let source = canonical_node_endpoint(&et.source, node_alias_label, node_id_labels);
    let target = canonical_node_endpoint(&et.target, node_alias_label, node_id_labels);
    let props = canonical_property_set(&et.properties)?;
    let props_part = if props.is_empty() {
        String::new()
    } else {
        format!(" {{{props}}}")
    };
    Ok(format!(
        "{}-[:{} =>{}]->{}",
        source,
        et.identifying_label.with_str(quote_identifier_always),
        props_part,
        target
    ))
}

pub(super) fn canonical_options(options: &[(String, Expr)]) -> String {
    let mut entries: Vec<String> = options
        .iter()
        .map(|(k, v)| {
            format!(
                "{}: {}",
                quote_identifier_always(k),
                canonical_option_value(v)
            )
        })
        .collect();
    entries.sort();
    entries.join(", ")
}

pub(super) fn canonical_option_value(e: &Expr) -> String {
    match e {
        Expr::Literal(LiteralValue::String(s)) => format!("\"{}\"", s.replace('"', "\\\"")),
        Expr::Literal(LiteralValue::Integer(i)) => i.to_string(),
        Expr::Literal(LiteralValue::Float(f)) => f.to_string(),
        Expr::Literal(LiteralValue::Bool(b)) => b.to_string(),
        Expr::Literal(LiteralValue::Null) => "null".to_string(),
        _ => "?".to_string(),
    }
}

/// Renders one body constraint in the CIP-180 canonical `CONSTRAINT` form.
/// `fallback_name` is the auto-generated name to print for an unnamed
/// constraint ([`CanonicalForm::Stored`]); `render_options` prints its
/// `OPTIONS` ([`CanonicalForm::Declared`]).
pub(super) fn canonical_constraint(
    c: &GraphTypeConstraintSpec,
    alias_table: &HashMap<&SmallName, &SmallName>,
    id_labels: &HashSet<&SmallName>,
    fallback_name: Option<&str>,
    render_options: bool,
) -> Result<String> {
    let var = if c.is_edge { "r" } else { "n" };
    let label = resolved_constraint_label(c, alias_table)?;
    let identified = id_labels.iter().any(|l| *l == label.as_str());
    let target_str = if c.is_edge {
        if identified {
            format!(
                "()-[{}:{} =>]->()",
                quote_identifier_always(var),
                quote_identifier_always(&label)
            )
        } else {
            format!(
                "()-[{}:{}]->()",
                quote_identifier_always(var),
                quote_identifier_always(&label)
            )
        }
    } else if identified {
        format!(
            "({}:{} =>)",
            quote_identifier_always(var),
            quote_identifier_always(&label)
        )
    } else {
        format!(
            "({}:{})",
            quote_identifier_always(var),
            quote_identifier_always(&label)
        )
    };

    let props: Vec<String> = c
        .properties
        .iter()
        .map(|p| {
            format!(
                "{}.{}",
                quote_identifier_always(var),
                p.with_str(quote_identifier_always)
            )
        })
        .collect();
    let props_str = format!("({})", props.join(", "));

    let verb_str = match &c.kind {
        GraphTypeConstraintVerb::Key => "IS KEY".to_string(),
        GraphTypeConstraintVerb::Unique => "IS UNIQUE".to_string(),
        GraphTypeConstraintVerb::Exists => "IS NOT NULL".to_string(),
        GraphTypeConstraintVerb::PropertyType => {
            let t = c
                .property_type
                .as_ref()
                .map(|te| render_canon(&type_expr_to_canon(te)))
                .unwrap_or_default();
            format!("IS :: {t}")
        }
    };

    let name_part = c
        .name
        .as_ref()
        .map(|n| n.to_string())
        .as_deref()
        .or(fallback_name)
        .map(|n| format!(" {}", quote_identifier_always(n)))
        .unwrap_or_default();
    let options_part = match &c.options {
        // `SHOW CURRENT GRAPH TYPE` omits OPTIONS entirely -- Neo4j's
        // rendered specification carries the rule, not its declaration-time
        // index config (live corpus: the CIPs running example).
        _ if !render_options => String::new(),
        relate_parser::OptionsForm::Literal(entries) => {
            let options_str = canonical_options(entries);
            if options_str.is_empty() {
                String::new()
            } else {
                format!(" OPTIONS {{ {options_str} }}")
            }
        }
        // An unresolved whole-map parameter renders as written: the canonical
        // text never silently claims a constraint has no OPTIONS. (Every
        // stored body resolves its parameters before capture, so this arm is
        // reachable only for a pre-resolution body.)
        relate_parser::OptionsForm::Parameter(name) => format!(" OPTIONS ${name}"),
        relate_parser::OptionsForm::Absent => String::new(),
    };

    Ok(format!(
        "CONSTRAINT{name_part} FOR {target_str} REQUIRE {props_str} {verb_str}{options_part}"
    ))
}
