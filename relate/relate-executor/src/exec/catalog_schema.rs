//! Index, constraint, and database schema-mutating catalog commands
//! extracted from `execute_catalog`. Each returns the statement's final
//! rows. The graph-type commands live in the sibling `graph_type_schema`.

use std::collections::BTreeMap;
use std::str::FromStr;

use relate_catalog::{rule_disposition, ConstraintDef, RuleDisposition};
use relate_core::{Error, Result, Value};
use relate_error::NestedErrorClass;
use relate_model::ConstraintType;

use crate::overlay::ExecGraph;
use crate::redo::RedoOp;
use crate::row::Row;

use super::catalog::schema_name_value;
use super::ddl::{index_conflict_gate, skip_existing, ExistingRule, Gate, IndexFacts};
use super::ddl_options::{resolve_index_options, validate_constraint_options};

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
        Error::semantic(NestedErrorClass::EquivalentConstraintAlreadyExists.as_str()),
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
