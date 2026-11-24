//! The graph-type schema commands (`CREATE GRAPH TYPE`, `ALTER CURRENT GRAPH
//! TYPE`): OPTIONS resolution and the per-kind policy behind it, the create
//! and set-current entry points, and the prepare/commit pipeline the
//! alteration runs. The conflict gates the pipeline runs before the first
//! catalog write live in the [`gates`] submodule. Extracted from
//! `catalog_schema`, which keeps index / constraint / database DDL.

mod gates;

use std::collections::BTreeMap;

use relate_catalog::ConstraintDef;
use relate_core::{Error, Result, Value};

use crate::overlay::ExecGraph;
use crate::redo::RedoOp;
use crate::row::Row;

use super::ddl_options::{parameter_map, ADMITTED_OPTION_KEYS};

use gates::{
    add_named_drop_targets, graph_type_delta, introduced_element_types,
    reject_added_equivalent_constraints, reject_renamed_equivalent_constraints,
    reject_same_command_constraint_replacement, reject_uncovered_independent_constraints,
};

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
    /// The kind's policy — one exhaustive match over the constraint-verb
    /// vocabulary, no absent mode: the four verbs are the OPTIONS
    /// surface's whole input vocabulary, so a verb added to the enum
    /// fails this match until it is classified. The KEY/UNIQUE pair is
    /// the model's index-backed set —
    /// [`constraint_type_of`](crate::graph_type::constraint_type_of)
    /// maps the verbs onto `relate_model::ConstraintType`, whose
    /// `is_index_backed` answers exactly those two — and the pin test
    /// at this module's foot holds the two spellings together.
    fn for_kind(kind: &relate_parser::GraphTypeConstraintVerb) -> Self {
        match kind {
            relate_parser::GraphTypeConstraintVerb::Key
            | relate_parser::GraphTypeConstraintVerb::Unique => Self::IndexBacked,
            relate_parser::GraphTypeConstraintVerb::Exists => Self::NoOptions {
                label: "property existence",
            },
            relate_parser::GraphTypeConstraintVerb::PropertyType => Self::NoOptions {
                label: "property type",
            },
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
    /// The rule-level delta of the SET (ADR-0667): the derived rules the
    /// live constraints do not already hold (`created`) and the live rules
    /// the new derivation retires (`dropped`) — what
    /// `db.ontology.apply` reports. Matched by rule identity, so the
    /// statement's mechanical purge-and-resync of unchanged rules counts
    /// as neither.
    delta: GraphTypeDelta,
    /// The live constraints under the statement's one captured snapshot:
    /// the uncovered-independent and added-equivalent gates and the delta
    /// read it inside prepare, and `db.ontology.apply`'s rename gate
    /// reads it from here — one derivation per statement, never a
    /// re-taken snapshot or a re-derived list between views.
    live: Vec<ConstraintDef>,
    /// The derived rules mapped to their catalog comparison shape: the
    /// uncovered-independent gate's coverage check reads it inside
    /// prepare, the delta counts it, and `db.ontology.apply`'s rename
    /// gate reads it from here — one mapping per statement.
    defs: Vec<ConstraintDef>,
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
    commit_current_graph_type(graph, prepared)?;
    Ok(())
}

/// The delta one `ALTER CURRENT GRAPH TYPE SET` applies (ADR-0667): the
/// derived rules the live constraints do not already hold, and the live
/// rules the new derivation retires. `db.ontology.apply`'s result row
/// reports it.
#[derive(Clone, Copy)]
pub(crate) struct GraphTypeDelta {
    pub(crate) created: usize,
    pub(crate) dropped: usize,
}

/// `db.ontology.apply`'s entry into the same pipeline (ADR-0226 §3,
/// revised by ADR-0667): prepare, the equivalent-constraint rename gate,
/// ADR-0226 §3's validate-first data scan, then the commit. Answers the
/// delta the statement applied — the procedure's result row.
pub(crate) fn set_current_graph_type_apply(
    graph: &ExecGraph<'_>,
    verb: relate_parser::AlterGraphTypeVerb,
    body: &relate_parser::GraphTypeBody,
) -> Result<GraphTypeDelta> {
    let prepared = prepare_current_graph_type(graph, verb, body)?;
    reject_renamed_equivalent_constraints(&prepared.live, &prepared.defs)?;
    super::constraint_validation::validate_derived_constraints(graph, &prepared.derived)?;
    let delta = prepared.delta;
    commit_current_graph_type(graph, prepared)?;
    Ok(delta)
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
    // One derivation of the live constraints and one mapping of the
    // derived rules to their comparison shape under the statement's one
    // snapshot: every gate below, the delta and the apply-path rename
    // gate all read these two lists — a reader that must not re-take
    // the snapshot or re-derive a list between views is
    // effective_constraints_in's own contract.
    let live = graph.effective_constraints_in(&state);
    let defs: Vec<ConstraintDef> = derived
        .iter()
        .map(|record| ConstraintDef::from_model(&record.constraint, record.entity))
        .collect();
    reject_uncovered_independent_constraints(&live, &introduced, &defs)?;
    if matches!(verb, relate_parser::AlterGraphTypeVerb::Add) {
        reject_added_equivalent_constraints(&live, verb_body, &merged)?;
    }
    reject_same_command_constraint_replacement(&before, &merged)?;
    let delta = graph_type_delta(&live, &defs);
    Ok(PreparedGraphType {
        purge,
        derived,
        // Fail-closed on a default value the verb algebra's validation
        // already rejected -- never a lossy capture.
        spec: crate::graph_type::GraphTypeSpec::from_body(&merged)
            .map_err(|e| Error::semantic(e.to_string()))?,
        delta,
        live,
        defs,
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

    #[test]
    fn the_option_policy_splits_on_the_models_index_backed_predicate() {
        // The policy's KEY/UNIQUE arms and the model's `is_index_backed`
        // set are one fact, spelled twice (the exhaustive match names
        // the verbs; the predicate owns the set) — pinned here so the
        // two spellings cannot drift.
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
