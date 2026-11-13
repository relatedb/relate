//! The graph-type schema commands (`CREATE GRAPH TYPE`, `ALTER CURRENT GRAPH
//! TYPE`): OPTIONS resolution and the per-kind policy behind it, the create and
//! set-current entry points, and the conflict gates the alteration pipeline runs
//! before the first catalog write. Extracted from `catalog_schema`, which keeps
//! index / constraint / database DDL.

use std::collections::BTreeMap;
use std::str::FromStr;

use relate_catalog::{rule_disposition, ConstraintDef, RuleDisposition};
use relate_core::{Error, Result, Value};
use relate_error::NestedErrorClass;
use relate_model::ConstraintType;

use crate::overlay::ExecGraph;
use crate::redo::RedoOp;
use crate::row::Row;

use super::ddl_options::{parameter_map, ADMITTED_OPTION_KEYS};

/// The graph-type body's non-MAP `OPTIONS` rejection, raised by both
/// resolvers so the two spellings cannot drift to different text.
const OPTIONS_NOT_A_MAP: &str =
    "Could not create graph type constraint with options: expected a map value.";

/// Resolve and validate every constraint's `OPTIONS` in an
/// `ALTER` body, in both spellings: a literal map's value expressions are
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
/// so a stored body never carries one.
fn resolve_alteration_options(
    graph: &ExecGraph<'_>,
    body: &mut relate_parser::GraphTypeBody,
    params: &BTreeMap<String, Value>,
) -> Result<()> {
    for constraint in &mut body.constraints {
        if matches!(constraint.options, relate_parser::OptionsForm::Absent) {
            continue;
        }
        // The graph-type body is not the statement's own front-end OPTIONS
        // surface: the literal spelling keeps the same argument-validator
        // class the parameter one has (live corpus).
        let policy = GraphTypeOptionsPolicy::for_kind(&constraint.kind);
        let key_error = |key: &str| policy.key_error(key);
        let options = crate::exec::ddl_options::resolve_options(
            graph,
            &constraint.options,
            params,
            &crate::exec::ddl_options::OptionKeyPolicy {
                admitted: policy.admitted(),
                literal: &key_error,
                parameter: &key_error,
            },
            OPTIONS_NOT_A_MAP,
        )?;
        policy.validate(&options)?;
        constraint.options = literal_options_form(&options);
    }
    Ok(())
}

/// The persisted `OPTIONS` form: a resolved options map rendered back to the
/// five-literal set, everything richer as the canonical `?`
/// (`OptionValueModel::NonLiteral`).
fn literal_options_form(options: &BTreeMap<String, Value>) -> relate_parser::OptionsForm {
    relate_parser::OptionsForm::Literal(
        options
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    relate_parser::Expr::from(&crate::graph_type::OptionValueModel::from_value(
                        value,
                    )),
                )
            })
            .collect(),
    )
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
        let map = parameter_map(params, &parameter, OPTIONS_NOT_A_MAP)?;
        constraint.options = literal_options_form(map);
    }
    Ok(())
}

/// The graph-type constraint OPTIONS policy: one variant per policy, built
/// from the constraint kind. The shared OPTIONS resolver reads [`admitted`]
/// and [`key_error`]; [`validate`] then checks the resolved values. An
/// index-backed constraint admits only `indexConfig`, and only an empty map;
/// an existence / property-type constraint admits no options at all (Neo4j
/// 22N81). Empty options are accepted by both, matching the
/// `CREATE CONSTRAINT` rule.
///
/// [`admitted`]: GraphTypeOptionsPolicy::admitted
/// [`key_error`]: GraphTypeOptionsPolicy::key_error
/// [`validate`]: GraphTypeOptionsPolicy::validate
enum GraphTypeOptionsPolicy {
    /// KEY / UNIQUE: the index-backed kinds share the schema DDL resolvers'
    /// admitted key and pinned rejection text.
    IndexBacked,
    /// EXISTS / PROPERTY TYPE: no options, rejected with the kind's noun.
    NoOptions { label: &'static str },
}

impl GraphTypeOptionsPolicy {
    /// The kind's policy — one variant per kind, no absent mode. The
    /// index-backed split is the model's predicate
    /// ([`constraint_type_of`](crate::graph_type::constraint_type_of)), not a
    /// KEY/UNIQUE list restated here.
    fn for_kind(kind: &relate_parser::GraphTypeConstraintVerb) -> Self {
        if crate::graph_type::constraint_type_of(kind).is_index_backed() {
            return Self::IndexBacked;
        }
        match kind {
            relate_parser::GraphTypeConstraintVerb::Exists => Self::NoOptions {
                label: "property existence",
            },
            relate_parser::GraphTypeConstraintVerb::PropertyType => Self::NoOptions {
                label: "property type",
            },
            // `constraint_type_of` marks exactly KEY/UNIQUE index-backed, so
            // the test above returned for them.
            relate_parser::GraphTypeConstraintVerb::Key
            | relate_parser::GraphTypeConstraintVerb::Unique => Self::IndexBacked,
        }
    }

    /// The keys the kind admits.
    fn admitted(&self) -> &'static [&'static str] {
        match self {
            Self::IndexBacked => ADMITTED_OPTION_KEYS,
            Self::NoOptions { .. } => &[],
        }
    }

    /// The one foreign-key rejection (both OPTIONS spellings raise it). An
    /// index-backed kind shares the schema DDL resolvers' pinned text.
    fn key_error(&self, key: &str) -> Error {
        match self {
            Self::IndexBacked => {
                Error::argument_error(crate::exec::ddl_options::rejected_option_key_message(key))
            }
            Self::NoOptions { label } => Error::argument_error(format!(
                "Invalid input: 'OPTIONS' is not supported in graph type {label} constraints."
            )),
        }
    }

    /// The value policy: an index-backed kind's `indexConfig` must be an
    /// empty map (Neo4j's "Expected nothing"). The key shape already passed.
    fn validate(&self, options: &BTreeMap<String, Value>) -> Result<()> {
        match self {
            Self::IndexBacked => {
                let Some((value, config)) =
                    crate::exec::ddl_options::index_config_or(options, &expected_nothing)?
                else {
                    return Ok(());
                };
                if config.is_empty() {
                    return Ok(());
                }
                Err(expected_nothing(value))
            }
            Self::NoOptions { .. } => Ok(()),
        }
    }
}

/// The one authored rejection for an `indexConfig` value: a non-map value and
/// a non-empty map both raise it, naming the value the navigation found.
fn expected_nothing(value: &Value) -> Error {
    Error::argument_error(format!(
        "Invalid input '{value}' for index config. Expected nothing."
    ))
}

pub(crate) fn catalog_create_graph_type(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateGraphTypeClause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    let mut parsed_body = c.body.clone();
    resolve_declaration_options(&mut parsed_body, params)?;
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
            resolve_alteration_options(graph, &mut body, params)?;
            body
        }
    };
    set_current_graph_type(graph, c.verb, &body)?;
    Ok(rows)
}

/// The prepared half of the `ALTER CURRENT GRAPH TYPE` pipeline: everything
/// computed from the captured schema snapshot -- the purge decision, the
/// verb algebra against the current body, the conflict gates, and the
/// derived constraints and captured spec -- before the first catalog write.
struct PreparedGraphType {
    purge: Vec<String>,
    derived: Vec<crate::graph_type::DerivedConstraint>,
    spec: crate::graph_type::GraphTypeSpec,
}

/// The plain DDL `ALTER CURRENT GRAPH TYPE` pipeline tail: prepare, then
/// commit with no data scan before the first catalog write (the DDL's
/// writes go through the ordinary mid-transaction validation).
fn set_current_graph_type(
    graph: &ExecGraph<'_>,
    verb: relate_parser::AlterGraphTypeVerb,
    body: &relate_parser::GraphTypeBody,
) -> Result<()> {
    let prepared = prepare_current_graph_type(graph, verb, body)?;
    commit_current_graph_type(graph, prepared)
}

/// `db.ontology.apply`'s entry into the same pipeline, with ADR-0226 §3's
/// validate-first data scan spliced between derivation and the first
/// catalog write.
pub(crate) fn set_current_graph_type_validate_first(
    graph: &ExecGraph<'_>,
    verb: relate_parser::AlterGraphTypeVerb,
    body: &relate_parser::GraphTypeBody,
) -> Result<()> {
    let prepared = prepare_current_graph_type(graph, verb, body)?;
    super::constraint_validation::validate_derived_constraints(graph, &prepared.derived)?;
    commit_current_graph_type(graph, prepared)
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
/// `GraphTypeAndConstraintCommandCombinations`).
///
/// Every key in `introduced` is absent from the previous body by construction
/// ([`introduced_element_types`] is a set difference), so a matching stored
/// rule was independent under that body: the introduced-set membership alone
/// proves the rule is not graph-type-managed yet.
fn reject_uncovered_independent_constraints(
    graph: &ExecGraph<'_>,
    state: &crate::graph::ShowSchema,
    introduced: &std::collections::HashSet<ElementTypeKey>,
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
        let entity = relate_model::EntityKind::from_relationship_flag(constraint.relationship);
        if !introduced.contains(&(entity, constraint.label.clone())) {
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
                if entity.is_relationship() {
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
    let existing = graph.effective_constraints_in(state);
    for derived in crate::graph_type::derive_constraints_typed(statement)? {
        if crate::graph_type::label_identifies_type(
            after,
            derived.entity.is_relationship(),
            &derived.constraint.label,
        ) {
            continue;
        }
        let def = ConstraintDef::from_model(&derived.constraint, derived.entity);
        if let RuleDisposition::Equivalent(existing) =
            rule_disposition(existing.iter(), &def.schema())
        {
            return Err(Error::syntax(format!(
                "{}: an equivalent constraint already exists: '{}'",
                NestedErrorClass::EquivalentConstraintAlreadyExists.as_str(),
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

impl IndexBackedDecl {
    /// The same `(entity, label, properties)` subject — one backing index.
    fn same_subject(&self, other: &Self) -> bool {
        self.relationship == other.relationship
            && self.label == other.label
            && self.properties == other.properties
    }

    /// The same rule restated: subject, kind and name.
    fn same_rule(&self, other: &Self) -> bool {
        self.same_subject(other) && self.kind == other.kind && self.name == other.name
    }
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
        .filter(|c| crate::graph_type::constraint_type_of(&c.kind).is_index_backed())
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
        // The first dropped declaration sharing the subject: a replacement
        // unless it is the same rule. A declaration survives only as the
        // *same* rule; a same-name declaration of another kind still drops
        // the old rule.
        let Some(dropped) = old
            .iter()
            .find(|dropped| created.same_subject(dropped) && !created.same_rule(dropped))
        else {
            continue;
        };
        if !new.iter().any(|candidate| candidate.same_rule(dropped)) {
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
    Ok(())
}

/// The element-type identity: `(entity kind, identifying label)`. Text
/// render: the label is String.
type ElementTypeKey = (relate_model::EntityKind, String);

/// The element-type identity set a body declares: one key per node and edge
/// type.
fn element_type_keys(
    body: &relate_parser::GraphTypeBody,
) -> std::collections::HashSet<ElementTypeKey> {
    body.node_types
        .iter()
        .map(|node| {
            (
                relate_model::EntityKind::Node,
                node.identifying_label.to_string(),
            )
        })
        .chain(body.edge_types.iter().map(|edge| {
            (
                relate_model::EntityKind::Relationship,
                edge.identifying_label.to_string(),
            )
        }))
        .collect()
}

/// The element-type identities the new body introduces relative to the
/// previous one -- the subjects an `ALTER` newly makes an element type.
fn introduced_element_types(
    before: &relate_parser::GraphTypeBody,
    after: &relate_parser::GraphTypeBody,
) -> std::collections::HashSet<ElementTypeKey> {
    element_type_keys(after)
        .difference(&element_type_keys(before))
        .cloned()
        .collect()
}

/// The prepare step shared by both entry points: purge decision, verb
/// algebra against the current body, the conflict gates, and the derived
/// constraints plus captured spec.
fn prepare_current_graph_type(
    graph: &ExecGraph<'_>,
    verb: relate_parser::AlterGraphTypeVerb,
    verb_body: &relate_parser::GraphTypeBody,
) -> Result<PreparedGraphType> {
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
        add_named_drop_targets(graph, &state, &current, verb_body, &mut purge)?;
    }
    // The store's preview set threads into the verb algebra (ADR-0371):
    // the stored-state half of the graph-type trio's gates -- an `ADD`
    // whose identifying label a stored node element type already
    // implies, a bare `ANY` a merged body introduces -- fails closed
    // here with the same authored error statement admission produces.
    let before = current.clone();
    let merged = crate::graph_type::apply_verb(verb, current, verb_body, graph.preview())?;
    let introduced = introduced_element_types(&before, &merged);
    // Body came through the verb algebra's validation; derivation is total
    // over a validated body, so propagate rather than assume.
    let derived = crate::graph_type::derive_constraints_typed(&merged)?;
    reject_uncovered_independent_constraints(graph, &state, &introduced, &derived)?;
    if matches!(verb, relate_parser::AlterGraphTypeVerb::Add) {
        reject_added_equivalent_constraints(graph, &state, verb_body, &merged)?;
    }
    reject_same_command_constraint_replacement(&before, &merged)?;
    Ok(PreparedGraphType {
        purge,
        derived,
        // Fail-closed on a default value the verb algebra's validation
        // already rejected -- never a lossy capture.
        spec: crate::graph_type::GraphTypeSpec::from_body(&merged)
            .map_err(|e| Error::semantic(e.to_string()))?,
    })
}

/// The commit step shared by both entry points: derived-constraint sync and
/// the `SetCurrentGraphType` catalog redo.
fn commit_current_graph_type(graph: &ExecGraph<'_>, prepared: PreparedGraphType) -> Result<()> {
    super::ddl::sync_derived_constraints(graph, prepared.purge, prepared.derived)?;
    graph.push_catalog_redo(RedoOp::SetCurrentGraphType {
        body: prepared.spec,
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

    #[test]
    fn the_option_policy_splits_on_the_models_index_backed_predicate() {
        // One fact, one owner: the policy never re-spells the KEY/UNIQUE set
        // the model's `is_index_backed` names.
        use relate_parser::GraphTypeConstraintVerb;
        for verb in [
            GraphTypeConstraintVerb::Key,
            GraphTypeConstraintVerb::Unique,
            GraphTypeConstraintVerb::Exists,
            GraphTypeConstraintVerb::PropertyType,
        ] {
            assert_eq!(
                matches!(
                    GraphTypeOptionsPolicy::for_kind(&verb),
                    GraphTypeOptionsPolicy::IndexBacked
                ),
                crate::graph_type::constraint_type_of(&verb).is_index_backed(),
                "{verb:?}"
            );
        }
    }
}
