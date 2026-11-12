//! `CALL` procedure execution: registered + built-in procedures,
//! fulltext and vector index query procedures. The dispatch path
//! classifies a `CALL` name once ([`divert_call`]): a specialized executor
//! (ontology, vector, fulltext) or the generic registry fallback. The
//! built-in procedure row producers live in the sibling `procedure_rows`
//! module.

use std::borrow::Cow;
use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, YieldItems};
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::exec::candidates::{materialize_candidate, resolve_search_index};
use crate::graph::Procedure;
use crate::procedure_registry::ProcSignature;
use crate::row::Row;

use super::fulltext;
use super::ontology::{exec_ontology_call, OntologyProc};
use super::procedure_rows::{builtin_procedure_rows, executable_signature, procedure_metadata};
use super::vector::{vector_index_candidates, vector_operand, VectorSearchFilter};

/// Whether a value satisfies a procedure argument type string (`STRING`,
/// `INTEGER`, `LIST<FLOAT>`, …). `ANY`, an empty type, a nullable `?`
/// suffix, and a null value match everything; the accepted type vocabulary
/// is explicit and fails closed, so an unknown (typo'd or unmodelled) type
/// name matches nothing. The precise element-type check of a list happens
/// in the procedure body itself. Used by the executor's `CALL` dispatch
/// and re-exported for `relate::Database`'s compile-time call
/// validation.
pub fn value_matches_proc_type(v: &Value, ty: &str) -> bool {
    let ty = ty.trim().trim_end_matches('?').trim().to_ascii_uppercase();
    if v.is_null() || ty.is_empty() || ty == "ANY" {
        return true;
    }
    // A signature may declare several admissible types (`BOOLEAN|MAP`, the
    // Cypher 25 and legacy forms of one argument).
    if ty.contains('|') {
        return ty
            .split('|')
            .any(|alternative| value_matches_proc_type(v, alternative));
    }
    // The accepted type vocabulary is explicit and fails closed: an unknown
    // (typo'd or unmodelled) name does not silently disable checking.
    // Generic lists (`LIST<FLOAT>`, `LIST<NODE>`, …) share the list check.
    if ty == "LIST" || ty.starts_with("LIST<") {
        return matches!(v, Value::List(_));
    }
    match ty.as_str() {
        "STRING" => matches!(v, Value::String(_)),
        "INTEGER" | "INT" => matches!(v, Value::Int(_)),
        "FLOAT" => matches!(v, Value::Float(_) | Value::Int(_)),
        "NUMBER" => matches!(v, Value::Int(_) | Value::Float(_)),
        "BOOLEAN" | "BOOL" => matches!(v, Value::Bool(_)),
        "MAP" => matches!(v, Value::Map(_)),
        "NODE" => matches!(v, Value::Node(_)),
        "RELATIONSHIP" | "REL" => matches!(v, Value::Relationship(_)),
        "PATH" => matches!(v, Value::Path(_)),
        "BYTES" => matches!(v, Value::Bytes(_)),
        "DATE" | "DATETIME" | "LOCAL DATETIME" | "LOCALDATETIME" | "TIME" | "LOCAL TIME"
        | "LOCALTIME" | "DURATION" => matches!(v, Value::Temporal(_)),
        "POINT" => matches!(v, Value::Point(_)),
        "UUID" => matches!(v, Value::Uuid(_)),
        "VECTOR" => matches!(v, Value::Vector { .. }),
        _ => false,
    }
}

pub(crate) fn call_args_match(proc_val: &Value, given: &Value) -> bool {
    // `eq_non_null` already covers the Int/Float cross-comparison with the
    // exact rule (`relate_model::canonical_key::eq_i64_f64`); a separate
    // `as f64` fallback here would reintroduce the lossy cast.
    proc_val.eq_non_null(given) || (proc_val.is_null() && given.is_null())
}

/// Static shape of a `CALL` clause (name, arguments, yields, optionality).
#[derive(Clone, Copy)]
pub(crate) struct CallSpec<'a> {
    pub name: &'a str,
    pub args: &'a [Expr],
    pub explicit: bool,
    pub yield_items: &'a YieldItems,
    pub optional: bool,
    pub where_clause: Option<&'a Expr>,
}

/// The registry-driven explicit-arity check shared by the generic `CALL`
/// path and the `db.ontology.*` executor (the divert's consumer): an
/// explicit call passes between `required` and `total` arguments. (The
/// facade's compile-time `validate_calls` already enforces the same
/// contract.)
pub fn check_explicit_arity(name: &str, required: usize, total: usize, given: usize) -> Result<()> {
    if given < required || given > total {
        return Err(Error::syntax(format!(
            "InvalidNumberOfArguments: {name} expected {total}, got {given}"
        )));
    }
    Ok(())
}

/// The passing-mode backstop shared by the generic `CALL` path and the
/// `db.ontology.*` executor (the divert's consumer): an in-query implicit
/// call — a `CALL` with a YIELD, in a clause pipeline — cannot pass
/// arguments; the procedure's arguments resolve only in a standalone unit
/// call. `required_args` is the caller's registry truth: the number of
/// declared parameters *without a default* (LABS-389: a procedure whose
/// parameters all carry defaults is callable implicitly). (The facade's
/// compile-time `validate_calls` enforces the same contract before
/// execution; this is the executor-side backstop.)
pub fn check_implicit_passing_mode(
    explicit: bool,
    required_args: usize,
    has_yield: bool,
) -> Result<()> {
    if !explicit && required_args > 0 && has_yield {
        return Err(Error::syntax(
            "InvalidArgumentPassingMode: in-query CALL cannot pass arguments implicitly",
        ));
    }
    Ok(())
}

/// The generic void-procedure contract: an OPTIONAL `CALL` with no return
/// columns over no input rows yields one unit row. Callers gate on the
/// void shape themselves.
pub(crate) fn void_optional_unit_row(rows_empty: bool, optional: bool) -> bool {
    rows_empty && optional
}

/// Row composition shared by every built-in row-producing procedure
/// executor (the generic registry path, fulltext, vector and
/// `db.ontology.get`): resolve the effective YIELD list (the caller's
/// `yield_items`, or — when empty or `*`-containing — `default_yields`),
/// then per input row and per produced column→value map, clone the input,
/// insert each yielded cell under `alias.unwrap_or(column)` (`Null` for a
/// column the producer did not emit), apply the WHERE truthy filter, and
/// pad one null row when an OPTIONAL call produced nothing for the input
/// row.
pub(crate) fn compose_rows<'a>(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    default_yields: &[&str],
    yield_items: &YieldItems,
    optional: bool,
    where_clause: Option<&Expr>,
    mut produce: impl FnMut(&Row) -> Result<Vec<Cow<'a, BTreeMap<String, Value>>>>,
) -> Result<Vec<Row>> {
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    let yields: Vec<(String, Option<String>)> = yield_items.resolved_pairs(default_yields);
    // The YIELD key rule (`alias`, else the column) resolved once; every
    // loop below reads the same keys.
    let keys: Vec<String> = yields
        .iter()
        .map(|(column, alias)| alias.clone().unwrap_or_else(|| column.clone()))
        .collect();
    let mut out = Vec::new();
    for input in rows {
        let mut produced = 0usize;
        for values in produce(&input)? {
            let mut row = input.clone();
            for ((column, _), key) in yields.iter().zip(&keys) {
                row.insert(
                    key.clone(),
                    values.get(column).cloned().unwrap_or(Value::Null),
                );
            }
            if where_clause
                .map(|predicate| eval(graph, &row, predicate, params, cypher_version))
                .transpose()?
                .is_none_or(|value| value.truthy() == Some(true))
            {
                out.push(row);
                produced += 1;
            }
        }
        if produced == 0 && optional {
            let mut row = input;
            for key in &keys {
                row.insert(key.clone(), Value::Null);
            }
            out.push(row);
        }
    }
    Ok(out)
}

/// The entity kind a `db.index.vector.query*` procedure targets — the typed
/// fact its name selected, so the dispatch is an exhaustive match instead
/// of a bool re-encoded downstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VectorEntity {
    Nodes,
    Relationships,
}

impl VectorEntity {
    fn relationship(self) -> bool {
        self == VectorEntity::Relationships
    }
}

/// The one classification step of the `CALL` dispatch path
/// ([`exec_call`]): which specialized executor owns the name — or `None`
/// for the generic registry fallback. The `db.ontology.*` procedures
/// (ADR-0121/ADR-0227) classify by the single [`OntologyProc::try_from`]
/// (their executor's exhaustive dispatch enum); the vector and fulltext
/// query procedures classify by the canonical signature registry — the
/// exact, case-sensitive registered names (CIP-230), the same resolution
/// the generic path's own built-in lookup below uses. This replaces the
/// historical ad-hoc divert schemes (a registry scan plus a
/// `starts_with` prefix plus `try_from` for the ontology divert, a
/// lowercase literal match for the vector pair, a lowercase substring
/// match for fulltext): no parallel name-matching scheme remains in the
/// dispatch path.
enum CallDivert {
    /// One of the four `db.ontology.*` procedures, already classified.
    Ontology(OntologyProc),
    /// One of the two `db.index.vector.query*` procedures, with the entity
    /// kind its name selects.
    Vector(VectorEntity),
    /// One of the two `db.index.fulltext.query*` procedures;
    /// `relationship` distinguishes `queryRelationships` from `queryNodes`.
    FulltextQuery { relationship: bool },
}

/// The diverted-call classification as a predicate, for the producer-table
/// completeness pin (test-only: no production path asks without also needing
/// the variant).
#[cfg(test)]
pub(crate) fn is_diverted_call(name: &str) -> bool {
    divert_call(name).is_some()
}

/// The one classification step: decide [`CallDivert`] for one `CALL` name,
/// once, before the generic machinery.
fn divert_call(name: &str) -> Option<CallDivert> {
    if let Ok(proc) = OntologyProc::try_from(name) {
        return Some(CallDivert::Ontology(proc));
    }
    // The registry lookup doubles as the classifier: only exact registered
    // names divert, case-sensitively (CIP-230), exactly like the generic
    // path's built-in resolution below.
    match crate::procedure_registry::builtin_procedure_signature(name)?.name {
        "db.index.vector.queryNodes" => Some(CallDivert::Vector(VectorEntity::Nodes)),
        "db.index.vector.queryRelationships" => {
            Some(CallDivert::Vector(VectorEntity::Relationships))
        }
        "db.index.fulltext.queryNodes" => Some(CallDivert::FulltextQuery {
            relationship: false,
        }),
        "db.index.fulltext.queryRelationships" => {
            Some(CallDivert::FulltextQuery { relationship: true })
        }
        _ => None,
    }
}

/// The runtime `CALL` contract for the diverted `db.ontology.*`
/// procedures — the SAME checks the generic path applies below, from the
/// ONE registry signature: the passing-mode backstop
/// (`check_implicit_passing_mode`), then the explicit arity
/// (`check_explicit_arity`). Applied at the divert site in `exec_call`
/// so the diverted executor enforces exactly the contract the generic
/// path enforces, visibly, at the same place.
pub(super) fn check_ontology_call_contract(call: &CallSpec<'_>) -> Result<()> {
    // The four names are statically registered and the classification
    // accepts exactly those four (pinned by test), so this lookup cannot
    // miss; the fail-closed arm replaces the divert's historical
    // `ProcedureNotFound` error should the table and the classification
    // ever drift apart.
    let signature = crate::procedure_registry::builtin_procedure_signature(call.name)
        .ok_or_else(|| Error::procedure_not_found(call.name))?;
    // All-default parameters are callable paren-less (LABS-389): the
    // guard keys on required args, not declared ones.
    check_implicit_passing_mode(
        call.explicit,
        signature.required_args(),
        call.yield_items.has_yield(),
    )?;
    if call.explicit {
        check_explicit_arity(
            call.name,
            signature.required_args(),
            signature.args.len(),
            call.args.len(),
        )?;
    }
    Ok(())
}

pub(crate) fn exec_call(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    call: CallSpec<'_>,
) -> Result<(Vec<Row>, bool)> {
    // ONE classification step decides divert-or-generic once. The
    // `db.ontology.*` divert runs the same runtime contract (passing
    // mode, arity — `check_ontology_call_contract`) the generic path runs
    // below, at this same site, before handing the call to its executor;
    // the vector executor takes the classified entity kind (node vs
    // relationship) with it. Everything else falls to the generic
    // registry machinery below.
    // The unit fact is the registry's: every diverted name is a registered
    // signature, so the call path reports `is_void()` rather than re-deriving
    // it per arm.
    let yields_unit = crate::exec::procedure_rows::executable_signature(call.name)
        .is_some_and(ProcSignature::is_void);
    match divert_call(call.name) {
        Some(CallDivert::Ontology(proc)) => {
            check_ontology_call_contract(&call)?;
            exec_ontology_call(ctx, rows, call, proc).map(|rows| (rows, yields_unit))
        }
        Some(CallDivert::Vector(entity)) => {
            exec_vector_query_call(ctx, rows, entity, call).map(|rows| (rows, yields_unit))
        }
        Some(CallDivert::FulltextQuery { relationship }) => {
            exec_fulltext_query_call(ctx, rows, relationship, call).map(|rows| (rows, yields_unit))
        }
        None => exec_registry_call(ctx, rows, call),
    }
}

/// Which rows one resolved call answers: the rows it already has (a
/// registered procedure's stored rows or a no-argument built-in's
/// once-produced rows), or a producing signature whose rows are a function of
/// each row's bound arguments.
enum CallRows<'a> {
    Rows(Cow<'a, [BTreeMap<String, Value>]>),
    PerRow(&'static ProcSignature),
}

/// One `CALL`'s resolved shape, from the one resolution: the metadata its
/// arguments bind against, its minimum arity, whether it yields columns, and
/// where its rows come from. The call path reads this and never re-matches
/// the target.
struct ResolvedCall<'a> {
    metadata: Cow<'a, Procedure>,
    required_args: usize,
    yields_unit: bool,
    rows: CallRows<'a>,
}

/// The generic registry path a non-diverted call takes: one resolution into a
/// [`CallTarget`], the passing-mode and arity contract, per-row argument
/// binding and type check, row matching, composition, and the void-procedure
/// unit row. One unit, so the classifier above stays a classifier.
fn exec_registry_call(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    call: CallSpec<'_>,
) -> Result<(Vec<Row>, bool)> {
    let ExecCtx {
        graph, principal, ..
    } = ctx;
    let CallSpec {
        name,
        args,
        explicit,
        yield_items,
        optional,
        where_clause,
    } = call;
    // One resolution decides registered-vs-builtin and carries everything
    // the call needs: metadata, arity, the unit fact and the row source. A
    // void call produces no rows at all, so nothing is produced to be
    // discarded.
    let registered = graph.procedure(name);
    let resolved = match &registered {
        Some(procedure) => ResolvedCall {
            yields_unit: procedure.yield_names.is_empty() && procedure.rows.is_empty(),
            metadata: Cow::Borrowed(procedure),
            required_args: procedure.arg_names.len(),
            rows: CallRows::Rows(Cow::Borrowed(procedure.rows.as_slice())),
        },
        None => {
            let signature =
                executable_signature(name).ok_or_else(|| Error::procedure_not_found(name))?;
            let rows = if signature.is_void() {
                CallRows::Rows(Cow::Borrowed(&[]))
            } else if signature.args.is_empty() {
                CallRows::Rows(Cow::Owned(builtin_procedure_rows(
                    graph,
                    signature,
                    principal,
                    &BTreeMap::new(),
                )?))
            } else {
                CallRows::PerRow(signature)
            };
            ResolvedCall {
                yields_unit: signature.is_void(),
                metadata: Cow::Owned(procedure_metadata(signature)),
                required_args: signature.required_args(),
                rows,
            }
        }
    };
    let ResolvedCall {
        metadata,
        required_args,
        yields_unit,
        rows: row_source,
    } = resolved;
    // Same rule on the unit-call path (LABS-389): required_args already
    // folds the builtin/registered split, so the guard reads it instead
    // of any-args.
    check_implicit_passing_mode(explicit, required_args, yield_items.has_yield())?;
    if explicit {
        check_explicit_arity(name, required_args, metadata.arg_names.len(), args.len())?;
    }
    if yields_unit && void_optional_unit_row(rows.is_empty(), optional) {
        return Ok((vec![Row::new()], true));
    }
    let binding = CallBinding {
        name,
        source: if explicit {
            CallArgsSource::Explicit(args)
        } else {
            CallArgsSource::Implicit
        },
        metadata: &metadata,
        required_args,
        ctx,
    };
    let mut out = Vec::new();
    // One binding pass over the driving rows: a void call binds and
    // type-checks each row's arguments too, it just produces nothing.
    for row in &rows {
        let call_args = binding.bind_row(row)?;
        if yields_unit {
            continue;
        }
        // Rows come from the resolved target: a registered procedure's are
        // fixed at registration, a no-argument built-in's were produced once
        // above, and any other built-in's are a function of this row's bound
        // arguments (a null `db.schema.visualization` options map selects
        // nothing at all; a `db.stats.retrieve` section names the section it
        // serves).
        let proc_rows: Cow<'_, [BTreeMap<String, Value>]> = match &row_source {
            CallRows::Rows(rows) => Cow::Borrowed(rows.as_ref()),
            CallRows::PerRow(signature) => Cow::Owned(builtin_procedure_rows(
                graph, signature, principal, &call_args,
            )?),
        };
        let matched: Vec<_> = proc_rows
            .iter()
            .filter(|pr| {
                metadata
                    .arg_names
                    .iter()
                    .all(|n| match (pr.get(n), call_args.get(n)) {
                        (Some(a), Some(b)) => call_args_match(a, b),
                        (None, _) => true,
                        _ => false,
                    })
            })
            .collect();
        if matched.is_empty() && metadata.yield_names.is_empty() {
            out.push(row.clone());
            continue;
        }
        let default_yields: Vec<&str> = metadata.yield_names.iter().map(String::as_str).collect();
        out.extend(compose_rows(
            ctx,
            vec![row.clone()],
            &default_yields,
            yield_items,
            optional,
            where_clause,
            // The matched rows are BORROWED, not cloned: compose_rows
            // clones only the yielded cells it inserts, so a wide
            // procedure row costs one cell per yielded column — never a
            // full-row deep clone per input row.
            |_| Ok(matched.iter().map(|pr| Cow::Borrowed(*pr)).collect()),
        )?);
    }
    Ok((if yields_unit { rows } else { out }, yields_unit))
}

/// The bound-argument source of one resolved CALL: explicit positional
/// expressions, or the implicit parameter/row-column names.
enum CallArgsSource<'a> {
    Explicit(&'a [Expr]),
    Implicit,
}

/// One resolved CALL's binding context: the facts every per-row bind needs,
/// carried once instead of as nine positional parameters.
struct CallBinding<'a> {
    name: &'a str,
    source: CallArgsSource<'a>,
    metadata: &'a Procedure,
    required_args: usize,
    ctx: ExecCtx<'a>,
}

impl CallBinding<'_> {
    /// Bind one driving row's arguments and type-check them against the
    /// resolved metadata — the one binding policy the row loop and the
    /// resolved call share, so a void procedure's arguments are validated
    /// exactly like a row-answering one's.
    fn bind_row(&self, row: &Row) -> Result<BTreeMap<String, Value>> {
        // One declared type per declared argument: `procedure_metadata` and
        // registration build the two lists together, so a shorter type list
        // is an engine bug, not a per-argument absence.
        if self.metadata.arg_types.len() != self.metadata.arg_names.len() {
            return Err(Error::internal(format!(
                "procedure '{}' declares {} arguments but {} types",
                self.name,
                self.metadata.arg_names.len(),
                self.metadata.arg_types.len()
            )));
        }
        let explicit = matches!(self.source, CallArgsSource::Explicit(_));
        let args = match self.source {
            CallArgsSource::Explicit(args) => args,
            CallArgsSource::Implicit => &[],
        };
        if !explicit {
            // Only parameters without a default are mandatory; defaulted
            // built-in parameters (e.g. `db.awaitIndexes`'s
            // `timeOutSeconds`) may be omitted.
            let missing_required = self
                .metadata
                .arg_names
                .iter()
                .take(self.required_args)
                .any(|n| !self.ctx.params.contains_key(n) && !row.contains_key(n));
            if missing_required {
                return Err(Error::syntax(format!(
                    "MissingParameter: implicit args for {}",
                    self.name
                )));
            }
        }
        let mut call_args = BTreeMap::new();
        for (index, (n, ty)) in self
            .metadata
            .arg_names
            .iter()
            .zip(self.metadata.arg_types.iter())
            .enumerate()
        {
            let Some(value) = call_arg_value(&self.ctx, row, args, explicit, index, n)? else {
                // A defaulted implicit argument may legitimately be absent.
                continue;
            };
            if !value_matches_proc_type(&value, ty) {
                return Err(Error::syntax(format!(
                    "InvalidArgumentType: {} argument `{n}` expects {ty}",
                    self.name
                )));
            }
            call_args.insert(n.clone(), value);
        }
        Ok(call_args)
    }
}

/// Resolve one declared CALL argument: its explicit positional expression
/// evaluated against the driving row, or the implicit parameter/row-column
/// value. The one per-argument resolver the generic binder and the
/// `db.ontology.*` readers share, so the policy is spelled once.
pub(crate) fn call_arg_value(
    ctx: &ExecCtx<'_>,
    row: &Row,
    args: &[Expr],
    explicit: bool,
    index: usize,
    arg_name: &str,
) -> Result<Option<Value>> {
    if explicit {
        args.get(index)
            .map(|expr| eval(ctx.graph, row, expr, ctx.params, ctx.cypher_version))
            .transpose()
    } else {
        Ok(ctx
            .params
            .get(arg_name)
            .cloned()
            .or_else(|| row.get(arg_name).cloned()))
    }
}

/// The one fulltext scan for both `db.index.fulltext.query*` procedures:
/// `relationship` selects the entity kind (relationship indexes match on
/// relationship types, node indexes on labels) and the yielded column name.
pub(crate) fn exec_fulltext_query_call(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    relationship: bool,
    call: CallSpec<'_>,
) -> Result<Vec<Row>> {
    let CallSpec {
        name,
        args,
        yield_items,
        optional,
        where_clause,
        ..
    } = call;
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    if args.len() != 2 {
        return Err(Error::syntax(format!(
            "InvalidNumberOfArguments: {name} expected 2, got {}",
            args.len()
        )));
    }
    // The entity column is the divert's classification; the score column
    // is shared by both entity variants, so the table is one choice plus
    // one literal.
    let entity_column = if relationship { "relationship" } else { "node" };
    let score_key = "score";
    let columns = [entity_column, score_key];
    compose_rows(
        ctx,
        rows,
        &columns,
        yield_items,
        optional,
        where_clause,
        |input| {
            let index_name = eval(graph, input, &args[0], params, cypher_version)?
                .as_string()
                .ok_or_else(|| Error::invalid_argument_type("indexName must be STRING"))?
                .to_string();
            let query = eval(graph, input, &args[1], params, cypher_version)?
                .as_string()
                .ok_or_else(|| Error::invalid_argument_type("query must be STRING"))?
                .to_string();
            // The shared SEARCH index resolution (the vector procedure's
            // one spelling): a missing or non-fulltext index is that
            // helper's typed error. Only the entity-kind disagreement is
            // this procedure's own — `queryNodes` on a relationship index
            // (or the reverse) has no scan to run.
            let (detail, _options) =
                resolve_search_index(graph, &index_name, crate::redo::IndexKindSpec::Fulltext)?;
            if detail.relationship != relationship {
                return Err(Error::execution(
                    "WrongIndexType: fulltext index entity type mismatch",
                ));
            }
            // The one CIP-225 scan: the Lucene-style query parser (terms
            // OR'd, `AND`/`OR`/`NOT`, `property:term` field filters) and the
            // live-fitted BM25 score, ranked best-first, no window.
            let kind = EntityKind::from_relationship_flag(detail.relationship);
            let produced =
                fulltext::search_all(graph, &detail, &query, fulltext::Analyzer::Standard)?
                    .into_iter()
                    .map(|(id, score)| {
                        Cow::Owned(BTreeMap::from([
                            (
                                entity_column.to_string(),
                                materialize_candidate(graph, kind, id),
                            ),
                            (score_key.to_string(), Value::Float(score)),
                        ]))
                    })
                    .collect();
            Ok(produced)
        },
    )
}

fn exec_vector_query_call(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    entity: VectorEntity,
    call: CallSpec<'_>,
) -> Result<Vec<Row>> {
    let relationship = entity.relationship();
    let CallSpec {
        name,
        args,
        yield_items,
        optional,
        where_clause,
        ..
    } = call;
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    if args.len() != 3 {
        return Err(Error::syntax(format!(
            "InvalidNumberOfArguments: {name} expected 3, got {}",
            args.len()
        )));
    }
    // The divert already classified the entity kind (`relationship` is
    // `queryRelationships` vs `queryNodes`); no name re-matching here.
    let entity_column = if relationship { "relationship" } else { "node" };
    compose_rows(
        ctx,
        rows,
        &[entity_column, "score"],
        yield_items,
        optional,
        where_clause,
        |input| {
            let index_name = eval(graph, input, &args[0], params, cypher_version)?
                .as_string()
                .ok_or_else(|| Error::invalid_argument_type("indexName must be STRING"))?
                .to_string();
            let limit = match eval(graph, input, &args[1], params, cypher_version)? {
                Value::Int(value) if (1..=i32::MAX as i64).contains(&value) => value as usize,
                _ => return Err(Error::ty(
                    "InvalidArgumentValue: numberOfNearestNeighbours must be a positive INTEGER",
                )),
            };
            let query = eval(graph, input, &args[2], params, cypher_version)?;
            let Some(query) = vector_operand(&query)? else {
                return Ok(Vec::new());
            };
            let (index_kind, candidates) = vector_index_candidates(
                ctx,
                &index_name,
                &query,
                0,
                limit,
                VectorSearchFilter {
                    filters: &[],
                    input,
                },
            )?;
            if index_kind.is_relationship() != relationship {
                return Err(Error::execution(
                    "WrongIndexType: vector index entity type mismatch",
                ));
            }
            let mut produced = Vec::new();
            for (entity, score) in candidates {
                // Both entity spellings map to the candidate (the
                // historical cell match keyed `"node" | "relationship"`),
                // so an explicit YIELD of either sees it.
                produced.push(BTreeMap::from([
                    ("node".to_string(), entity.clone()),
                    ("relationship".to_string(), entity.clone()),
                    ("score".to_string(), Value::Float(score)),
                ]));
            }
            Ok(produced.into_iter().map(Cow::Owned).collect())
        },
    )
}

#[cfg(test)]
mod type_vocabulary_tests {
    use super::value_matches_proc_type;
    use relate_core::Value;

    /// ADR-0463 round 6: the list spelling is exactly `LIST` or `LIST<…>`;
    /// a bare prefix match would silently accept `LISTS`/`LISTING` as any
    /// list, re-opening the fail-closed vocabulary.
    #[test]
    fn list_type_accepts_only_list_or_list_generic() {
        let list = Value::List(Vec::new().into());
        assert!(value_matches_proc_type(&list, "LIST"));
        assert!(value_matches_proc_type(&list, "LIST<FLOAT>"));
        assert!(value_matches_proc_type(&list, "list<string>"));
        assert!(!value_matches_proc_type(&list, "LISTS"));
        assert!(!value_matches_proc_type(&list, "LISTING"));
        assert!(!value_matches_proc_type(&Value::Int(1), "LISTS"));
    }
}

#[cfg(test)]
mod row_provenance_tests {
    use crate::exec::test_support::{exec_autocommit, exec_cypher};
    use crate::graph::Graph;
    use relate_core::Value;

    /// A no-argument built-in's rows are produced once for the call and
    /// answered by every driving row — two driving rows times two labels,
    /// never one production that only the first row sees.
    #[test]
    fn a_no_argument_builtin_answers_its_rows_for_every_driving_row() {
        let graph = Graph::default();
        exec_autocommit(&graph, "CREATE (:User), (:Admin)").expect("the fixture's CREATE succeeds");

        let result = exec_cypher(
            &graph,
            "UNWIND [1, 2] AS i CALL db.labels() YIELD label \
             RETURN i, label ORDER BY i, label",
        );
        let string = |value: &str| Value::String(value.into());
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Int(1), string("Admin")],
                vec![Value::Int(1), string("User")],
                vec![Value::Int(2), string("Admin")],
                vec![Value::Int(2), string("User")],
            ]
        );
    }

    /// A built-in whose producer reads its arguments produces them under
    /// every driving row's own binding: `db.stats.retrieve` is served the
    /// section each row carries, never an empty argument map.
    #[test]
    fn an_argument_taking_builtin_produces_rows_from_each_rows_bindings() {
        let graph = Graph::default();

        let result = exec_cypher(
            &graph,
            "UNWIND [1, 2] AS i WITH i, 'GRAPH COUNTS' AS wanted \
             CALL db.stats.retrieve(wanted) YIELD section RETURN i, section",
        );
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Int(1), Value::String("GRAPH COUNTS".into())],
                vec![Value::Int(2), Value::String("GRAPH COUNTS".into())],
            ]
        );
    }
}
