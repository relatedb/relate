//! Schema-mutating catalog commands (indexes, constraints, databases,
//! graph types) extracted from `execute_catalog`. Each returns the
//! statement's final rows.

use std::collections::BTreeMap;
use std::str::FromStr;

use relate_catalog::{rule_disposition, ConstraintDef, RuleDisposition};
use relate_core::{Error, Result, Value};
use relate_model::ConstraintType;

use crate::overlay::ExecGraph;
use crate::redo::RedoOp;
use crate::row::Row;

use super::catalog::schema_name_value;
use super::ddl::{index_conflict_gate, skip_existing, ExistingRule, Gate, IndexFacts};
use super::ddl_options::{
    parameter_map, resolve_index_options, validate_constraint_options, ADMITTED_OPTION_KEYS,
    KEY_INDEX_CONFIG,
};

pub(crate) fn catalog_create_index(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateIndexClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<Row>> {
    let name = match &c.name {
        Some(name) => schema_name_value(name, params)?,
        None => IndexFacts::generated_name(c),
    };
    // The OPTIONS shape resolves before the conflict gate, mirroring the
    // constraint twin (ADR-0489): Neo4j rejects the statement's shape at
    // compile time, so an `IF NOT EXISTS`/equivalent-rule skip can never
    // mask an invalid property count or OPTIONS map.
    let options = resolve_index_options(graph, c, params, cypher_version)?;
    // The conflict family is one named gate (shared with the legacy IR fast
    // path); the detail/create tail stays here.
    let facts = IndexFacts::from_clause(c, name.clone());
    if matches!(index_conflict_gate(graph, &facts)?, Gate::Skip(_)) {
        return Ok(rows);
    }

    // Catalog records own String text (ADR-0509: catalog-facing storage
    // stays string-keyed); the parsed names render once, at this feed.
    let label = c
        .labels
        .first()
        .map(|label| label.to_string())
        .unwrap_or_default();
    let joined = c
        .properties
        .iter()
        .map(|property| property.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let property = crate::redo::legacy_property_encoding(&joined, c.kind.into(), &name);
    graph.push_catalog_redo(RedoOp::CatalogIndexDetail {
        name: name.clone(),
        detail: facts.to_detail(),
        options: crate::redo::props_json(&options),
    });
    graph.create_named_index(name, label, property)?;
    Ok(rows)
}

pub(crate) fn catalog_drop_index(
    graph: &ExecGraph<'_>,
    c: &relate_parser::DropIndexClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    let name = schema_name_value(&c.name, params)?;
    // The drop policy (constraint-owned rejection, `IF EXISTS`, the
    // live-verified not-found message) is one place: the legacy IR path's
    // `exec_drop_index` owns it, and both spellings delegate there.
    super::ddl::exec_drop_index(graph, c.if_exists, &relate_ir::DropIndexTarget::Name(name))?;
    Ok(rows)
}

/// The catalog definition a `CREATE CONSTRAINT` clause declares, built once
/// for the execution handler and the `EXPLAIN` note: the single-property
/// shape check, the kind tag parsed (an unknown tag is a semantic error, so
/// the `CatalogDerivedConstraint` redo op can never be silently skipped),
/// the property type rendered and validated, and the name resolved — the
/// clause's own (`declared_name`) or the engine-generated form.
pub(crate) fn constraint_def(
    c: &relate_parser::CreateConstraintClause,
    declared_name: Option<String>,
) -> Result<(ConstraintDef, ConstraintType)> {
    if c.property_type.is_some() && c.properties.len() != 1 {
        return Err(Error::syntax(
            "InvalidConstraintSyntax: only single property IS TYPED constraints are supported",
        ));
    }
    // Parse the constraint kind tag once, up front: an unknown tag is a
    // semantic error (fail closed), so the `CatalogDerivedConstraint` redo
    // op can never be silently skipped after constraint creation.
    let constraint_type = ConstraintType::from_str(&c.constraint_type).map_err(Error::semantic)?;
    let property_type = match &c.property_type {
        Some(ty) => {
            let canon = relate_parser::type_expr_to_canon(ty);
            // ADR-0489: Neo4j's front-end property-type rules surface as
            // `SyntaxError` (its `SemanticError.*` constructors raise a
            // SyntaxException), so the same class applies here.
            relate_parser::validate_property_type_expr(ty)
                .map_err(|error| Error::syntax(error.to_string()))?;
            Some(relate_parser::render_canon(&canon))
        }
        None => None,
    };
    let name = declared_name.unwrap_or_else(|| {
        let joined = c
            .properties
            .iter()
            .map(|property| property.to_string())
            .collect::<Vec<_>>()
            .join("_");
        format!("constraint_{}_{}_{}", c.constraint_type, c.label, joined)
    });
    let def = ConstraintDef {
        name,
        relationship: c.entity_type == relate_parser::SchemaEntityType::Relationship,
        // Catalog-facing String feeds: every parsed name renders once.
        label: c.label.to_string(),
        properties: c
            .properties
            .iter()
            .map(|property| property.to_string())
            .collect(),
        kind: c.constraint_type.clone(),
        property_type,
        enforced_label: c.enforced_label.as_ref().map(|label| label.to_string()),
    };
    Ok((def, constraint_type))
}

pub(crate) fn catalog_create_constraint(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateConstraintClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<Row>> {
    let declared_name = match &c.name {
        Some(name) => Some(schema_name_value(name, params)?),
        None => None,
    };
    let (def, constraint_type) = constraint_def(c, declared_name)?;
    // The OPTIONS shape and the conflict policy are one named gate
    // (`constraint_create_gate`, whose first step is
    // `validate_constraint_options`); this function stays the definition
    // construction plus the creation half.
    if matches!(
        constraint_create_gate(graph, c, &def, constraint_type, params, cypher_version)?,
        Gate::Skip(_)
    ) {
        return Ok(rows);
    }
    // The theater record derives from the same def the catalog install
    // consumes (one constraint shape per statement); the core pushes it
    // between the constraint row and the backing index.
    let derived_record = def
        .enforced_label
        .is_some()
        .then(|| def.to_model(constraint_type));
    super::ddl::create_constraint_core(graph, def, derived_record)?;
    Ok(rows)
}

/// The equivalent-rule skip: a schema-equivalent existing constraint is
/// accepted by `IF NOT EXISTS` and every other form gets the duplicate-schema
/// reply. The rule that caused the skip rides out, so the `00NA0` note reads
/// the gate's own verdict. (ADR-0372's serving-pair range index is *not* a
/// skip: see the unconditional rejection in [`constraint_create_gate`].)
fn equivalent_rule_skip(
    c: &relate_parser::CreateConstraintClause,
    rule: ExistingRule,
) -> Result<Gate> {
    skip_existing(
        c.if_not_exists,
        Error::semantic("EquivalentSchemaRuleAlreadyExists"),
        rule,
    )
}

/// The `CREATE CONSTRAINT` conflict policy: the options shape (validated
/// first, like Neo4j's compile-time rules), duplicate name, graph-type
/// dependence, schema equivalence, index-backed conflict, serving-pair
/// conflict, and duplicate index name. One place for the reply texts and
/// the `IF NOT EXISTS` outcomes. The `EXPLAIN` note calls this same gate,
/// so it can never describe a statement the gate rejects.
pub(crate) fn constraint_create_gate(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateConstraintClause,
    def: &ConstraintDef,
    constraint_type: ConstraintType,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Gate> {
    validate_constraint_options(graph, c, constraint_type, params, cypher_version)?;
    // One catalog snapshot for the two views this gate reads (the current
    // graph type and the index details); the effective-constraint view
    // below resolves its own derived records from the same snapshot shape.
    let schema = graph.show_schema()?;
    if let Some(existing) = graph.named_constraint(&def.name) {
        return skip_existing(
            c.if_not_exists,
            Error::semantic("ConstraintAlreadyExists"),
            ExistingRule::Constraint(existing),
        );
    }
    let constraints = graph.effective_constraints_in(&schema);
    match rule_disposition(&constraints, &def.schema()) {
        // Two outcomes, flat: `IF NOT EXISTS` accepts the existing rule;
        // every other form gets the duplicate-schema reply. The earlier
        // `exists` coexistence exemption was unreachable — the durable
        // catalog gate (`Catalog::insert_constraint`) has always rejected an
        // equivalent schema of any kind — so the two gates now state one
        // policy. The probe runs before the graph-type-dependence rejection:
        // Neo4j treats a `CREATE CONSTRAINT IF NOT EXISTS` whose schema the
        // graph type already derives as a no-op, not a dependence error
        // (live corpus: `GraphTypeAndConstraintCommandCombinations`).
        RuleDisposition::Equivalent(rule) => {
            return equivalent_rule_skip(c, ExistingRule::Constraint(rule.clone()));
        }
        // The same subject and kind with different options (a property-type
        // constraint under another declared type, including the vector
        // forms) is a conflict, never a distinct rule — and `IF NOT EXISTS`
        // does not suppress it.
        RuleDisposition::Conflicts => return Err(Error::semantic("ConstraintConflict")),
        RuleDisposition::New => {}
    }
    if matches!(
        constraint_type,
        ConstraintType::Exists | ConstraintType::PropertyType
    ) && crate::graph_type::label_identifies_type(
        &schema.data.current_graph_type.to_body(),
        def.relationship,
        &def.label,
    ) {
        return Err(Error::execution(
            "IncompatibleGraphTypeDependence: dependent constraints are managed by the graph type",
        ));
    }
    let backed = constraint_type.is_index_backed();
    if constraints
        .iter()
        .any(|existing| def.claims_backing_subject_of(existing))
    {
        return Err(Error::semantic("ConstraintConflict"));
    }
    // ADR-0372: an existing RANGE index on the constraint's own schema is a
    // hard conflict, not an `IF NOT EXISTS` no-op -- Neo4j rejects the create
    // with "There already exists an index (...)" whether or not the flag is
    // present (live corpus: `Create*ConstraintCommandsPart3`, 22N73).
    if serving_pair_range_conflict(&schema.data.index_details, backed, def) {
        let schema_text = if def.relationship {
            format!("()-[:{} {{{}}}]-()", def.label, def.properties.join(", "))
        } else {
            format!("(:{} {{{}}})", def.label, def.properties.join(", "))
        };
        return Err(Error::semantic(format!(
            "AlreadyExists: there already exists an index {schema_text}. A constraint cannot \
             be created until the index has been dropped."
        )));
    }
    if graph.named_index(&def.name).is_some() {
        return Err(Error::semantic("DuplicatedIndexName"));
    }
    Ok(Gate::Proceed)
}

/// ADR-0372's serving-pair gate, the `CREATE CONSTRAINT` side: an
/// index-backed constraint conflicts with an existing RANGE index on the
/// same label/property — the constraint's backing (ADR-0339) would
/// duplicate it — closing the index-first order's gap (LABS-274) for the
/// serving pair. Non-serving kinds (FULLTEXT/VECTOR/POINT) coexist: they
/// serve search, not validation. The canonical
/// [`crate::redo::serving_pair_conflicts`] owns the whole match,
/// including the `backed` conjunct — this probe states that fact once,
/// through the payload. Consults the schema the gate captured.
fn serving_pair_range_conflict(
    index_details: &BTreeMap<String, crate::redo::IndexDetailSpec>,
    backed: bool,
    def: &ConstraintDef,
) -> bool {
    index_details.values().any(|detail| {
        crate::redo::serving_pair_conflicts(
            crate::redo::ServingPairIndex {
                kind: detail.kind,
                relationship: detail.relationship,
                labels: &detail.labels,
                properties: &detail.properties,
            },
            crate::redo::ServingPairConstraint {
                backed,
                relationship: def.relationship,
                label: &def.label,
                properties: &def.properties,
            },
        )
    })
}

pub(crate) fn catalog_drop_constraint(
    graph: &ExecGraph<'_>,
    c: &relate_parser::DropConstraintClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    let name = schema_name_value(&c.name, params)?;
    let state = graph.show_schema()?;
    let is_dependent = state
        .data
        .derived_constraints
        .get(&name)
        .is_some_and(|constraint| {
            crate::graph_type::classify_constraint(
                &state.data.current_graph_type.to_body(),
                constraint,
            ) == relate_model::ConstraintClassification::Dependent
        });
    if is_dependent {
        return Err(Error::execution(
            "DependentConstraintManagedIndividually: dependent constraints cannot be dropped",
        ));
    }
    if graph.named_constraint(&name).is_none() {
        if c.if_exists {
            return Ok(rows);
        }
        return Err(Error::semantic("ConstraintNotFound"));
    }
    super::ddl::exec_drop_constraint(graph, &name, false)?;
    Ok(rows)
}

pub(crate) fn catalog_alter_database(
    graph: &ExecGraph<'_>,
    c: &relate_parser::AlterDatabaseClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    let name = schema_name_value(&c.name, params)?;
    // ADR-0116 §1/§2: namespace names match case-insensitively; an
    // unknown name fails with CE's text unless `IF EXISTS` makes the
    // command a no-op.
    let Some(ns) = relate_core::Namespace::parse(&name) else {
        if c.if_exists {
            return Ok(rows);
        }
        return Err(relate_core::ce_messages::alter_unknown_database(&name));
    };
    let canonical = ns.as_str();
    let state = graph.show_schema()?;
    let composite = state.databases[&ns].composite;
    let language = c.default_language;
    if !matches!(language, 5 | 25) {
        return Err(Error::semantic(
            "InvalidArgumentValue: unsupported Cypher version",
        ));
    }
    graph.push_catalog_redo(RedoOp::PutDatabase {
        name: canonical.to_string(),
        language,
        composite,
    });
    Ok(rows)
}

/// Resolve and validate every graph-type constraint's `OPTIONS` (CIP-116
/// CR[4]) in place, in both spellings: a literal map's value expressions are
/// evaluated, and a whole-map `OPTIONS $param` is resolved (absent →
/// `ParameterMissing`, non-MAP → `ArgumentError`). The map is resolved
/// through the same [`crate::ddl_options::resolve_options`] sequence
/// `CREATE CONSTRAINT` uses (ADR-0489), with this surface's own key policy:
///
/// - an index-backed constraint (KEY/UNIQUE) admits only `indexConfig`, and
///   only an empty map;
/// - an existence / property-type constraint admits no options at all.
///
/// Resolution happens before validation/persistence, and `GraphTypeSpec`
/// capture fails closed on any constraint still carrying a parameter form,
/// so a stored body never carries one. The resolved entries are stored as
/// the persisted five-literal set, with everything richer rendering as the
/// canonical `?` (`OptionValueModel::NonLiteral`).
fn resolve_graph_type_options(
    graph: &ExecGraph<'_>,
    body: &mut relate_parser::GraphTypeBody,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
    scope: crate::graph_type::GraphTypeScope,
) -> Result<()> {
    // A declared graph type is the CIP-178/180/181 surface: its corpus pins
    // an OPTIONS map as opaque declaration text (the canonical rendering
    // keeps the original expressions), so only the whole-map parameter is
    // resolved here. The alteration form follows the `CREATE CONSTRAINT`
    // OPTIONS policy below.
    if scope == crate::graph_type::GraphTypeScope::Declaration {
        return resolve_declaration_options(body, params);
    }
    for constraint in &mut body.constraints {
        if matches!(constraint.options, relate_parser::OptionsForm::Absent) {
            continue;
        }
        // The graph-type body is not the statement's own front-end OPTIONS
        // surface: the literal spelling keeps the same argument-validator
        // class the parameter one has (live corpus).
        let key_error = |key: &str| graph_type_option_key_error(&constraint.kind, key);
        let options = crate::exec::ddl_options::resolve_options(
            graph,
            &constraint.options,
            params,
            cypher_version,
            &crate::exec::ddl_options::OptionKeyPolicy {
                admitted: graph_type_admitted_option_keys(constraint),
                literal: &key_error,
                parameter: &key_error,
            },
            "Could not create graph type constraint with options: expected a map value.",
        )?;
        validate_graph_type_option_values(constraint, &options)?;
        constraint.options = relate_parser::OptionsForm::Literal(
            options
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        relate_parser::Expr::from(
                            &crate::graph_type::OptionValueModel::from_value(value),
                        ),
                    )
                })
                .collect(),
        );
    }
    Ok(())
}

/// The declaration form's OPTIONS resolution: only a whole-map
/// `OPTIONS $param` is resolved (absent → `ParameterMissing`, non-MAP →
/// `ArgumentError`); literal entries stay as written so the CIP-180 canonical
/// form reproduces the declared text.
fn resolve_declaration_options(
    body: &mut relate_parser::GraphTypeBody,
    params: &BTreeMap<String, Value>,
) -> Result<()> {
    for constraint in &mut body.constraints {
        let Some(parameter) = constraint.options.parameter().map(str::to_owned) else {
            continue;
        };
        let map = parameter_map(
            params,
            &parameter,
            "Could not create graph type constraint with options: expected a map value.",
        )?;
        constraint.options = relate_parser::OptionsForm::Literal(
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        relate_parser::Expr::from(
                            &crate::graph_type::OptionValueModel::from_value(value),
                        ),
                    )
                })
                .collect(),
        );
    }
    Ok(())
}

/// The graph-type OPTIONS key/kind policy, handed to the shared
/// [`crate::ddl_options::resolve_options`]: an index-backed constraint
/// admits only `indexConfig`; an existence / property-type constraint admits
/// no options at all (Neo4j 22N81). Empty options are accepted by both,
/// matching the `CREATE CONSTRAINT` rule.
fn graph_type_admitted_option_keys(
    constraint: &relate_parser::GraphTypeConstraintSpec,
) -> &'static [&'static str] {
    match constraint.kind {
        relate_parser::GraphTypeConstraintVerb::Key
        | relate_parser::GraphTypeConstraintVerb::Unique => ADMITTED_OPTION_KEYS,
        relate_parser::GraphTypeConstraintVerb::Exists
        | relate_parser::GraphTypeConstraintVerb::PropertyType => &[],
    }
}

/// The one graph-type OPTIONS foreign-key rejection, per constraint kind
/// (both spellings raise it, see the caller). An index-backed kind shares
/// the schema DDL resolvers' pinned text — the one spelling of it.
fn graph_type_option_key_error(kind: &relate_parser::GraphTypeConstraintVerb, key: &str) -> Error {
    match kind {
        relate_parser::GraphTypeConstraintVerb::Key
        | relate_parser::GraphTypeConstraintVerb::Unique => {
            Error::argument_error(crate::exec::ddl_options::rejected_option_key_message(key))
        }
        relate_parser::GraphTypeConstraintVerb::Exists
        | relate_parser::GraphTypeConstraintVerb::PropertyType => Error::argument_error(format!(
            "Invalid input: 'OPTIONS' is not supported in graph type {} constraints.",
            graph_type_constraint_label(kind)
        )),
    }
}

/// The graph-type OPTIONS value policy: an index-backed constraint's
/// `indexConfig` must be an empty map (Neo4j's "Expected nothing"). The
/// key/kind shape already passed [`validate_graph_type_option_keys`].
fn validate_graph_type_option_values(
    constraint: &relate_parser::GraphTypeConstraintSpec,
    options: &BTreeMap<String, Value>,
) -> Result<()> {
    if matches!(
        constraint.kind,
        relate_parser::GraphTypeConstraintVerb::Key
            | relate_parser::GraphTypeConstraintVerb::Unique
    ) {
        if let Some(value) = options.get(KEY_INDEX_CONFIG) {
            if !matches!(value, Value::Map(map) if map.is_empty()) {
                return Err(Error::argument_error(format!(
                    "Invalid input '{value}' for index config. Expected nothing."
                )));
            }
        }
    }
    Ok(())
}

/// The authored noun one graph-type constraint kind's rejection uses.
fn graph_type_constraint_label(kind: &relate_parser::GraphTypeConstraintVerb) -> &'static str {
    match kind {
        relate_parser::GraphTypeConstraintVerb::Exists => "property existence",
        relate_parser::GraphTypeConstraintVerb::PropertyType => "property type",
        // Index-backed kinds admit `indexConfig`, so this noun never heads a
        // rejection; stated once so the match stays exhaustive.
        relate_parser::GraphTypeConstraintVerb::Key
        | relate_parser::GraphTypeConstraintVerb::Unique => "key or uniqueness",
    }
}

pub(crate) fn catalog_create_graph_type(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateGraphTypeClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<Row>> {
    let mut parsed_body = c.body.clone();
    resolve_graph_type_options(
        graph,
        &mut parsed_body,
        params,
        cypher_version,
        crate::graph_type::GraphTypeScope::Declaration,
    )?;
    // Both spellings are validated body-for-body; the `AS` spelling passes
    // the CIP-180 canonicalizer-input flag, which exempts the two
    // declaration rules its corpus deliberately violates (`via_as`).
    crate::graph_type::validate_graph_type_body(
        &parsed_body,
        graph.preview(),
        crate::graph_type::GraphTypeScope::Declaration,
        c.via_as,
    )
    .map_err(|e| Error::semantic(e.to_string()))?;
    // Parse-to-model happens exactly once, here at DDL-execution
    // time; the redo op carries the typed model, and replay stores
    // it verbatim (no format-then-parse). The capture is fail-closed
    // on a CIP-262 default value the (already-run) validator would
    // have rejected -- never lossy.
    let body = crate::graph_type::GraphTypeSpec::from_body(&parsed_body)
        .map_err(|e| Error::semantic(e.to_string()))?;
    graph.push_catalog_redo(RedoOp::PutGraphType {
        name: c.name.to_string(),
        body,
    });
    Ok(rows)
}

pub(crate) fn alter_current_graph_type(
    graph: &ExecGraph<'_>,
    c: &relate_parser::AlterCurrentGraphTypeClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<Row>> {
    // The stored model converts to the parser model once, here at
    // DDL-execution time; the per-verb body algebra lives in
    // `crate::graph_type` and its result is what gets persisted. Both
    // target forms resolve to a body first, then take the single
    // `apply_verb` path (the parser only builds `Named` for `SET`).
    let body = match &c.target {
        relate_parser::AlterCurrentGraphTypeTarget::Named(name) => {
            // ADR-0211: `ALTER CURRENT GRAPH TYPE SET GRAPH TYPE <name>`
            // promotes a declared named type to current -- the same pipeline
            // as `SET { <body> }` (validation, derived-constraint sync),
            // sourced from the catalog's stored body instead of the statement.
            // A catalog-read body never carries an unresolved `OPTIONS
            // $param` (the persisted model has no parameter form), so there
            // is nothing to resolve here.
            graph
                .show_schema()?
                .data
                .graph_types
                .get(&name.to_string())
                .ok_or_else(|| Error::semantic("GraphTypeNotFound"))?
                .to_body()
        }
        relate_parser::AlterCurrentGraphTypeTarget::Body(verb_body) => {
            // Only the statement's own body can carry `OPTIONS $param`.
            let mut body = verb_body.clone();
            resolve_graph_type_options(
                graph,
                &mut body,
                params,
                cypher_version,
                crate::graph_type::GraphTypeScope::Alteration,
            )?;
            body
        }
    };
    set_current_graph_type(graph, c.verb, &body)?;
    Ok(rows)
}

/// How the shared `ALTER CURRENT GRAPH TYPE` pipeline tail validates
/// existing data before the first catalog write. The entry point encodes
/// the decision once, so no caller passes a bare mode flag across the
/// module boundary.
enum DataValidation {
    /// The DDL path ([`set_current_graph_type`]): its writes go through
    /// the ordinary mid-transaction constraint validation.
    MidTransaction,
    /// ADR-0226 §3's validate-first data scan (`db.ontology.apply`,
    /// [`set_current_graph_type_validate_first`]): the captured snapshot
    /// is checked against every constraint the resulting graph type
    /// derives before any catalog change commits.
    ValidateFirst,
}

/// The plain DDL `ALTER CURRENT GRAPH TYPE` pipeline tail: purge decision,
/// verb algebra against the current body, derived-constraint sync, and
/// the `SetCurrentGraphType` catalog redo — all computed from the same
/// captured schema snapshot, with no data scan before the first catalog
/// write (the DDL's writes go through the ordinary mid-transaction
/// validation).
pub(crate) fn set_current_graph_type(
    graph: &ExecGraph<'_>,
    verb: relate_parser::AlterGraphTypeVerb,
    body: &relate_parser::GraphTypeBody,
) -> Result<()> {
    set_current_graph_type_tail(graph, verb, body, DataValidation::MidTransaction)
}

/// `db.ontology.apply`'s entry into the same pipeline tail
/// ([`set_current_graph_type`]), with ADR-0226 §3's validate-first data
/// scan spliced between derivation and the first catalog write.
pub(crate) fn set_current_graph_type_validate_first(
    graph: &ExecGraph<'_>,
    verb: relate_parser::AlterGraphTypeVerb,
    body: &relate_parser::GraphTypeBody,
) -> Result<()> {
    set_current_graph_type_tail(graph, verb, body, DataValidation::ValidateFirst)
}

/// Classifies every name an `ALTER CURRENT GRAPH TYPE DROP` body lists and
/// adds the independent catalog constraints it names to the purge set. The
/// body's own constraint elements are [`apply_drop`]'s to remove; a name the
/// graph type derives on its own (any dependent kind) cannot be dropped
/// individually (Neo4j 22N68); an independent `CREATE CONSTRAINT` rule may be
/// dropped together with the element type that shares its label
/// (live corpus: `AlterCurrentGraphTypeDrop`).
///
/// [`apply_drop`]: crate::graph_type::apply_drop
fn add_named_drop_targets(
    graph: &ExecGraph<'_>,
    state: &crate::graph::ShowSchema,
    current: &relate_parser::GraphTypeBody,
    body: &relate_parser::GraphTypeBody,
    purge: &mut Vec<String>,
) -> Result<()> {
    for name in &body.drop_constraint_names {
        if current
            .constraints
            .iter()
            .any(|constraint| constraint.name.as_ref() == Some(name))
        {
            continue;
        }
        if let Some(derived) = state.data.derived_constraints.get(&name.to_string()) {
            if crate::graph_type::classify_constraint(current, derived)
                == relate_model::ConstraintClassification::Dependent
            {
                return Err(Error::syntax(
                    "DependentConstraintManagedIndividually: dependent constraints cannot be \
                     dropped individually",
                ));
            }
            continue;
        }
        if name.with_str(|text| graph.named_constraint(text)).is_none() {
            return Err(Error::syntax("SchemaError: constraint does not exist"));
        }
        if !purge.iter().any(|existing| name == existing.as_str()) {
            purge.push(name.to_string());
        }
    }
    Ok(())
}

/// The kind tag of a *stored* constraint, parsed fail-closed. The engine
/// writes every tag itself, so an unparseable one is a corrupted or foreign
/// catalog row: skipping it would let that constraint escape the gate reading
/// it (fail open here, exactly the silent drop
/// [`index_backed_declarations`] refuses for an unresolved target).
fn stored_constraint_kind(constraint: &ConstraintDef) -> Result<ConstraintType> {
    ConstraintType::from_str(&constraint.kind).map_err(|reason| {
        Error::internal(format!(
            "stored constraint '{}' carries an unparseable kind tag: {reason}",
            constraint.name
        ))
    })
}

/// Rejects a graph type that introduces an element type on a label (or
/// relationship type) an *independent* property existence / property type
/// constraint occupies, unless the element type's derived constraints cover
/// that rule exactly -- in that case Neo4j lets the rule switch from
/// independent to dependent (live corpus:
/// `GraphTypeAndConstraintCommandCombinations`). An element type the previous
/// body already identified is not "new".
fn reject_uncovered_independent_constraints(
    graph: &ExecGraph<'_>,
    state: &crate::graph::ShowSchema,
    before: &relate_parser::GraphTypeBody,
    introduced: &std::collections::HashSet<(bool, String)>,
    derived: &[crate::graph_type::DerivedConstraint],
) -> Result<()> {
    if introduced.is_empty() {
        return Ok(());
    }
    for constraint in graph.effective_constraints_in(state) {
        let kind = stored_constraint_kind(&constraint)?;
        if !matches!(kind, ConstraintType::Exists | ConstraintType::PropertyType) {
            continue;
        }
        if !introduced.contains(&(constraint.relationship, constraint.label.clone())) {
            continue;
        }
        let entity = relate_model::EntityKind::from_relationship_flag(constraint.relationship);
        if crate::graph_type::classify_constraint_parts(
            before,
            Some(kind),
            entity,
            &constraint.label,
        ) != relate_model::ConstraintClassification::Independent
        {
            continue;
        }
        let covered = derived.iter().any(|derived| {
            ConstraintDef::from_model(&derived.constraint, derived.entity)
                .schema()
                .matches(constraint.schema_view())
        });
        if !covered {
            return Err(Error::syntax(format!(
                "IndependentConstraintOnLabel: the independent constraint '{}' is using the \
                 same {} `{}` as an element type",
                constraint.schema_text(),
                if constraint.relationship {
                    "relationship type"
                } else {
                    "label"
                },
                constraint.label
            )));
        }
    }
    Ok(())
}

/// Rejects an `ADD` declaration that is schema-equivalent to a committed
/// constraint on a subject the statement does not (re)declare as an element
/// type (Neo4j 22N65: an equivalent constraint already exists). When the
/// statement also declares that element type, the inline and standalone rules
/// are one rule and the re-statement is accepted.
fn reject_added_equivalent_constraints(
    graph: &ExecGraph<'_>,
    state: &crate::graph::ShowSchema,
    statement: &relate_parser::GraphTypeBody,
    after: &relate_parser::GraphTypeBody,
) -> Result<()> {
    use std::collections::HashSet;
    // Text render: the catalog-side element-type identity is String.
    let element_types: HashSet<(bool, String)> = after
        .node_types
        .iter()
        .map(|node| (false, node.identifying_label.to_string()))
        .chain(
            after
                .edge_types
                .iter()
                .map(|edge| (true, edge.identifying_label.to_string())),
        )
        .collect();
    let existing = graph.effective_constraints_in(state);
    for derived in crate::graph_type::derive_constraints_typed(statement)? {
        let key = (
            derived.entity == relate_model::EntityKind::Relationship,
            derived.constraint.label.clone(),
        );
        if element_types.contains(&key) {
            continue;
        }
        let def = ConstraintDef::from_model(&derived.constraint, derived.entity);
        if let RuleDisposition::Equivalent(existing) =
            rule_disposition(existing.iter(), &def.schema())
        {
            return Err(Error::syntax(format!(
                "EquivalentSchemaRuleAlreadyExists: an equivalent constraint already exists: \
                 '{}'",
                existing.name
            )));
        }
    }
    Ok(())
}

/// One index-backed constraint declaration resolved to its comparison shape.
/// `kind` stays the typed catalog tag, so a future verb cannot silently
/// collapse into "unique" through a wildcard arm.
struct IndexBackedDecl {
    relationship: bool,
    label: String,
    properties: Vec<String>,
    kind: relate_model::ConstraintType,
    name: Option<String>,
}

/// Every KEY/UNIQUE declaration resolved to its `(entity, label, properties)`
/// subject. The target label is resolved through the shared
/// [`resolved_constraint_label`](crate::graph_type::resolved_constraint_label),
/// so an unresolved alias is a loud internal invariant violation -- dropping
/// the declaration here would let a same-command replacement escape
/// detection.
fn index_backed_declarations(body: &relate_parser::GraphTypeBody) -> Result<Vec<IndexBackedDecl>> {
    let (node_aliases, edge_aliases) = crate::graph_type::alias_tables(body);
    body.constraints
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                relate_parser::GraphTypeConstraintVerb::Key
                    | relate_parser::GraphTypeConstraintVerb::Unique
            )
        })
        .map(|c| {
            let table = if c.is_edge {
                &edge_aliases
            } else {
                &node_aliases
            };
            Ok(IndexBackedDecl {
                relationship: c.is_edge,
                label: crate::graph_type::resolved_constraint_label(c, table)?,
                properties: c
                    .properties
                    .iter()
                    .map(|property| property.to_string())
                    .collect(),
                kind: crate::graph_type::constraint_type_of(&c.kind),
                name: c.name.as_ref().map(|name| name.to_string()),
            })
        })
        .collect()
}

/// Neo4j refuses to drop and re-create a *similar* index-backed constraint in
/// one command (the same subject under a new name or another kind), because
/// both would be backed by the same index (live corpus:
/// `AlterCurrentGraphTypeSetPart2`, 50N11 with cause 22N66).
fn reject_same_command_constraint_replacement(
    before: &relate_parser::GraphTypeBody,
    after: &relate_parser::GraphTypeBody,
) -> Result<()> {
    let old = index_backed_declarations(before)?;
    let new = index_backed_declarations(after)?;
    for created in &new {
        for dropped in &old {
            let same_subject = created.relationship == dropped.relationship
                && created.label == dropped.label
                && created.properties == dropped.properties;
            let replaced = created.kind != dropped.kind || created.name != dropped.name;
            // A declaration survives only as the *same* rule (name and kind,
            // on the same subject); a same-name declaration of another kind
            // still drops the old rule.
            let survives = new.iter().any(|candidate| {
                candidate.relationship == dropped.relationship
                    && candidate.label == dropped.label
                    && candidate.properties == dropped.properties
                    && candidate.kind == dropped.kind
                    && candidate.name == dropped.name
            });
            if same_subject && replaced && !survives {
                return Err(Error::semantic(format!(
                    "Trying to create constraint '{}' in same transaction as dropping '{}'. \
                     This is not supported because they are both backed by similar indexes. \
                     Please drop constraint in a separate transaction before creating the new \
                     one.",
                    created.name.as_deref().unwrap_or("constraint_(generated)"),
                    dropped.name.as_deref().unwrap_or("constraint_(generated)")
                )));
            }
        }
    }
    Ok(())
}

/// The `(is_edge, identifying label)` pairs the new body introduces relative
/// to the previous one -- the subjects an `ALTER` newly makes an element type.
fn introduced_element_types(
    before: &relate_parser::GraphTypeBody,
    after: &relate_parser::GraphTypeBody,
) -> std::collections::HashSet<(bool, String)> {
    // Text render: the diff's identity sets are String-keyed.
    let mut old_nodes: std::collections::HashSet<String> = before
        .node_types
        .iter()
        .map(|node| node.identifying_label.to_string())
        .collect();
    let mut old_edges: std::collections::HashSet<String> = before
        .edge_types
        .iter()
        .map(|edge| edge.identifying_label.to_string())
        .collect();
    let mut introduced = std::collections::HashSet::new();
    for node in &after.node_types {
        if !old_nodes.remove(&node.identifying_label.to_string()) {
            introduced.insert((false, node.identifying_label.to_string()));
        }
    }
    for edge in &after.edge_types {
        if !old_edges.remove(&edge.identifying_label.to_string()) {
            introduced.insert((true, edge.identifying_label.to_string()));
        }
    }
    introduced
}

fn set_current_graph_type_tail(
    graph: &ExecGraph<'_>,
    verb: relate_parser::AlterGraphTypeVerb,
    body: &relate_parser::GraphTypeBody,
    validation: DataValidation,
) -> Result<()> {
    let state = graph.show_schema()?;
    let mut purge = super::ddl::constraints_to_purge(
        graph
            .named_constraints()
            .into_iter()
            .map(|constraint| constraint.name)
            .collect(),
        &state.data.derived_constraints,
        verb,
    );
    let current = state.data.current_graph_type.to_body();
    if matches!(verb, relate_parser::AlterGraphTypeVerb::Drop) {
        add_named_drop_targets(graph, &state, &current, body, &mut purge)?;
    }
    // The store's preview set threads into the verb algebra (ADR-0371):
    // the stored-state half of the graph-type trio's gates -- an `ADD`
    // whose identifying label a stored node element type already
    // implies, a bare `ANY` a merged body introduces -- fails closed
    // here with the same authored error statement admission produces.
    let before = current.clone();
    let statement = body;
    let body = crate::graph_type::apply_verb(verb, current, body, graph.preview())?;
    let introduced = introduced_element_types(&before, &body);
    // Body came through the verb algebra's validation; derivation is total
    // over a validated body, so propagate rather than assume.
    let derived = crate::graph_type::derive_constraints_typed(&body)?;
    reject_uncovered_independent_constraints(graph, &state, &before, &introduced, &derived)?;
    if matches!(verb, relate_parser::AlterGraphTypeVerb::Add) {
        reject_added_equivalent_constraints(graph, &state, statement, &body)?;
    }
    reject_same_command_constraint_replacement(&before, &body)?;
    if matches!(validation, DataValidation::ValidateFirst) {
        super::constraint_validation::validate_derived_constraints(graph, &derived)?;
    }
    super::ddl::sync_derived_constraints(graph, purge, derived)?;
    graph.push_catalog_redo(RedoOp::SetCurrentGraphType {
        // Fail-closed on a default value the verb algebra's validation
        // already rejected -- never a lossy capture.
        body: crate::graph_type::GraphTypeSpec::from_body(&body)
            .map_err(|e| Error::semantic(e.to_string()))?,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constraint_body(query: &str) -> relate_parser::GraphTypeBody {
        let stmt = relate_parser::parse(query).expect("should parse");
        match stmt.clauses.into_iter().next().expect("one clause") {
            relate_parser::Clause::AlterCurrentGraphType(agt) => match agt.target {
                relate_parser::AlterCurrentGraphTypeTarget::Body(body) => body,
                relate_parser::AlterCurrentGraphTypeTarget::Named(name) => {
                    panic!("expected an inline body, got named type {name}")
                }
            },
            other => panic!("expected ALTER CURRENT GRAPH TYPE, got {other:?}"),
        }
    }

    #[test]
    fn an_unresolved_key_declaration_fails_loud() {
        // A deserialized body can carry an alias reference the current body
        // no longer declares. Skipping the declaration would let a
        // same-command replacement escape detection, so the unresolved
        // target is an internal invariant violation, never a silent drop.
        let body = constraint_body(
            "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n) REQUIRE n.prop IS KEY }",
        );
        let err = index_backed_declarations(&body)
            .err()
            .expect("an unresolved target must fail loud");
        assert!(
            err.to_string()
                .contains("constraint target resolves to no declared element type"),
            "{err}"
        );
    }

    #[test]
    fn an_unparseable_stored_constraint_kind_fails_loud() {
        // The stored tag is engine-written, so an unknown one is a corrupted
        // catalog row. Skipping it would let the constraint escape the
        // independent-constraint gate (fail open) -- the same silent-drop
        // hazard the unresolved-target test above pins.
        let constraint = ConstraintDef {
            name: "constraint_typo".into(),
            relationship: false,
            label: "Person".into(),
            properties: vec!["id".into()],
            kind: "uniqeu".into(),
            property_type: None,
            enforced_label: None,
        };
        let err = stored_constraint_kind(&constraint)
            .expect_err("an unparseable stored kind must fail loud");
        assert!(err.to_string().contains("unknown constraint type"), "{err}");
        assert!(err.to_string().contains("constraint_typo"), "{err}");
    }
}
