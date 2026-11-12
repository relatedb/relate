//! `LOAD CSV`, `CALL { }` subqueries, `CALL IN TRANSACTIONS`, conditional
//! branches, and part finishing.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::{Clause, Expr, Query};
use relate_names::SmallName;

use crate::ctx::{ExecCtx, QueryCtx};
use crate::eval::eval;
use crate::graph::QueryStats;
use crate::overlay::ExecGraph;
use crate::row::Row;

mod conditional;
mod in_transactions;
mod shared;

pub(crate) use conditional::{conditional_produces_rows, exec_conditional};
pub(crate) use in_transactions::exec_call_in_transactions;
#[cfg(test)]
pub(crate) use in_transactions::transaction_error_message;

use shared::{exec_call_subquery_shared, import_invariant_body, shared_execution_plan};

use super::dispatch::{run_nested_clauses, run_nested_clauses_scoped};
use super::scope::{
    carry_scope_bindings, nested_seed_row, return_columns, star_scope_names, terminal_return_items,
    visible_keys,
};
use super::{materialize_rows, QueryResult};
use crate::csv::{field_terminator_char, record_value, split_csv_line, split_csv_records};

pub(crate) fn exec_load_csv(
    ctx: ExecCtx<'_>,
    rows: Vec<Row>,
    with_headers: bool,
    url: &Expr,
    variable: &SmallName,
    field_terminator: Option<&str>,
) -> Result<Vec<Row>> {
    let ExecCtx { graph, params, .. } = ctx;
    let delim = field_terminator_char(field_terminator)?;
    let mut out = Vec::new();
    for row in rows {
        let src = eval(graph, &row, url, params)?;
        let path = match src {
            Value::String(s) => s,
            Value::Null => continue,
            _ => return Err(Error::ty("LOAD CSV FROM requires a string URL")),
        };
        // The source boundary (`CsvReader::load`) resolves the CIP-202
        // canned fixture URL; everything else goes through the local-file
        // policy (ADR-0081).
        let text: Arc<str> = graph.csv.load(&path)?;
        let mut records = split_csv_records(&text, delim).into_iter();
        // The header record is extracted first, so a leading blank line
        // under WITH HEADERS cannot strip the header and flip map rows to
        // list rows.
        let headers: Option<Vec<String>> = if with_headers {
            records.next().map(|h| split_csv_line(h, delim))
        } else {
            None
        };
        // Blank-line policy for the data records: a blank record (an empty
        // physical line between record breaks) yields no row.
        let records = records.filter(|r| !r.is_empty());
        for record in records {
            // Ragged-row policy (docs/api/cli/spec.md): under WITH HEADERS a
            // record narrower than the header maps its missing names to
            // null — Neo4j's `LOAD CSV WITH HEADERS` semantics, a
            // deliberate behavior change from the earlier empty-string
            // padding, applied at the parse boundary by `record_value`'s
            // named rule (`zip_with_header_nulls`); extra cells are
            // dropped. List rows keep their natural width.
            let val = record_value(headers.as_deref(), split_csv_line(record, delim));
            let mut next = row.clone();
            next.insert(variable.to_string(), val);
            out.push(next);
        }
    }
    Ok(out)
}

/// `CALL { }` clause shape (CIP-141/CIP-32 + IN TRANSACTIONS), bundled to
/// keep the executors under the argument-count lint.
pub(crate) struct SubquerySpec<'a> {
    pub query: &'a Query,
    pub optional: bool,
    pub scope: &'a relate_ir::CallScope,
    pub in_transactions: Option<&'a relate_ir::InTransactions>,
}

pub(crate) fn exec_call_subquery(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    spec: SubquerySpec<'_>,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    if let Some(tx) = spec.in_transactions {
        return exec_call_in_transactions(graph, rows, spec, tx, params, qctx);
    }
    if let Some(plan) = shared_execution_plan(spec.scope, &rows)? {
        if import_invariant_body(spec.query, &plan.check()) {
            return exec_call_subquery_shared(graph, rows, spec, plan, params, qctx);
        }
    }
    let mut out = Vec::new();
    for row in rows {
        let produced = call_subquery_row(graph, &row, spec.query, spec.scope, params, qctx)?;
        if produced.is_empty() {
            out.extend(empty_body_row(row, spec.optional, spec.query, spec.scope));
        } else {
            out.extend(produced);
        }
    }
    Ok(out)
}

/// The seed row of one outer row for a `CALL (vars) { … }` body — the
/// imported bindings. An import the clause's planner scope promises must
/// survive every row-shape pass (the `UNWIND` source strip, the liveness
/// trim, the `OPTIONAL CALL` null-pad); a miss is therefore an internal
/// error, reported rather than turned into a panic.
fn import_seed(vars: &[SmallName], outer: &Row) -> Result<Row> {
    vars.iter()
        .map(|name| {
            // Row reads and the seed row's String keys bridge here.
            let value = outer.get_name(name).cloned().ok_or_else(|| {
                Error::internal(format!(
                    "planner-scope invariant: imported variable `{name}` must be in the outer row"
                ))
            })?;
            Ok((name.to_string(), value))
        })
        .collect()
}

/// The seed row one outer row gives a `CALL { … }` body — the one
/// `CallScope` → seed-row policy. The naked and `(*)` scopes hand the body
/// the outer row minus the enclosing clause engine's internal columns
/// ([`nested_seed_row`]): a parked `__ord*`/`__agg_*` slot is the enclosing
/// clause's state, never a binding the body may read as its own. A `(vars)`
/// import list names its bindings explicitly, so it needs no strip
/// ([`import_seed`]).
fn scope_seed(scope: &relate_ir::CallScope, outer: &Row) -> Result<Row> {
    match scope {
        relate_ir::CallScope::None | relate_ir::CallScope::Wildcard => Ok(nested_seed_row(outer)),
        relate_ir::CallScope::Vars(vars) => import_seed(vars, outer),
    }
}

/// The one row-merge policy of both subquery paths: the body's export
/// overlays the outer row except for the `excluded` column names, which
/// always keep the outer row's own value. Callers pass the scope's
/// imported names (per-row path) or the shared plan's check set (the same
/// names for `CALL (vars)`; every outer key for the naked form, where a
/// colliding export is provably a same-name pass-through).
fn merge_body_export(
    outer: &Row,
    columns: &[String],
    values: &[Value],
    excluded: &HashSet<String>,
) -> Row {
    let mut merged = outer.clone();
    for (col, value) in project_row(columns, values).filter(|(col, _)| !excluded.contains(col)) {
        merged.insert(col, value);
    }
    merged
}

/// The one empty-body policy: `OPTIONAL CALL` null-pads the outer row,
/// plain `CALL` drops it.
fn empty_body_row(
    row: Row,
    optional: bool,
    query: &Query,
    scope: &relate_ir::CallScope,
) -> Option<Row> {
    optional.then(|| null_pad_call_row(row, query, scope))
}

/// The names a `CALL { … }` scope imports from the outer row — the export
/// set's derivation (`call_export_columns`). A `(vars)` list names its
/// bindings; the naked scope imports nothing; the `(*)` scope imports the
/// outer row's visible names ([`visible_keys`] = the row minus the
/// enclosing clause engine's internal columns, the same set the stripped
/// seed binds). `None` (no row to derive from, e.g. a columns-only
/// question over an empty table) imports nothing — the star expansion then
/// falls back to the declared scope.
fn imported_names(scope: &relate_ir::CallScope, outer: Option<&Row>) -> HashSet<String> {
    match scope {
        // Text render: the imported-name set is the String scope domain.
        relate_ir::CallScope::Vars(vars) => vars.iter().map(|name| name.to_string()).collect(),
        relate_ir::CallScope::None => HashSet::new(),
        relate_ir::CallScope::Wildcard => outer
            .map(|row| visible_keys(row).into_iter().collect())
            .unwrap_or_default(),
    }
}

pub(crate) fn call_subquery_row(
    graph: &ExecGraph<'_>,
    outer: &Row,
    query: &Query,
    scope: &relate_ir::CallScope,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    // The seed row is the imported bindings — the one CallScope → seed
    // policy lives in [`scope_seed`].
    let seed = scope_seed(scope, outer)?;
    // The seed is the body's importing scope: every binding it carries stays
    // visible to the whole body, even across a `WITH` that does not project
    // it (`run_nested_clauses_scoped` re-attaches a dropped binding after
    // the projection, so it is never injected as a grouping key and a
    // keyless aggregation over an emptied input keeps its identity row).
    // This supersedes the `query_with_persistent_imports` rewrite: the
    // scoped clause loop preserves the same bindings without altering the
    // body's `WITH` items.
    //
    // The seed's own keys ARE the CIP-218 Rule 10 import set (the `(vars)`
    // list it was built from, or the outer row's visible names the naked and
    // `(*)` forms bind) — one derivation, not a parallel walk. A naked
    // `CALL { … }` keeps the legacy whole-row star expansion, so only a
    // scope clause has a set.
    let imported = scope
        .has_scope_clause()
        .then(|| seed.keys().cloned().collect::<HashSet<String>>());
    let result = execute_query_scoped(graph, query, params, seed, imported.as_ref(), qctx)?;
    // A part merges its rows back only when the body's FINAL stage projects
    // a statement result (`super::part_projects_rows`, read through
    // `stage_projects_rows` — the same trailing-clause classification the
    // conditional branch body uses; a schema/admin DDL command's empty
    // result must not suppress the pass-through).
    if !stage_projects_rows(&query.final_stage().parts) {
        return Ok(vec![outer.clone()]);
    }
    let excluded = imported.unwrap_or_default();
    let mut produced = Vec::new();
    for values in result.rows {
        // Exclusion is by column name: imported bindings pass through from
        // the outer row instead of being overwritten by the body's export.
        produced.push(merge_body_export(
            outer,
            &result.columns,
            &values,
            &excluded,
        ));
    }
    Ok(produced)
}

/// Zip a `QueryResult`'s columns with one row's values into `(name, value)`
/// pairs — the one projection both row-merge paths use. The engine keeps
/// exactly one value per column; a mismatch is a bug, so the widths are
/// asserted rather than silently truncated (in release `zip` would truncate
/// without the check).
fn project_row<'a>(
    columns: &'a [String],
    values: &'a [Value],
) -> impl Iterator<Item = (String, Value)> + 'a {
    assert_eq!(
        columns.len(),
        values.len(),
        "QueryResult invariant: every row has one value per column"
    );
    columns.iter().cloned().zip(values.iter().cloned())
}

/// Whether any UNION arm of a stage projects a statement result of its own
/// — the one "does this body project rows" fact. Both the conditional body's
/// `body_projects_rows` and the CALL-subquery driver read it over the
/// body's FINAL stage, like every other trailing-clause classification.
fn stage_projects_rows(parts: &[relate_ir::SingleQuery]) -> bool {
    parts
        .iter()
        .any(|part| super::part_projects_rows(&part.clauses))
}

/// The importing scope a body runs under, with the CIP-218 Rule 10 import
/// set as a property of the mode rather than a second parallel parameter:
/// only a scope clause has one, so the invalid "imports without a scope"
/// combination is unrepresentable.
#[derive(Clone, Copy)]
enum BodyScope<'a> {
    /// No importing scope: the statement's own rows ([`execute_query_seeded`]).
    Unscoped,
    /// The driving row is the body's importing scope but there is no Rule 10
    /// import exclusion: a `WHEN` branch body, and the naked `CALL { … }`
    /// form (which keeps the legacy whole-row star expansion).
    RowScope(&'a Row),
    /// A scope-clause `CALL (vars) { … }` / `CALL (*) { … }` body: the
    /// importing scope plus the Rule 10 import set its UNION arms drop
    /// before they are compared.
    Call {
        scope: &'a Row,
        imports: &'a HashSet<String>,
    },
}

impl BodyScope<'_> {
    /// The row the body's bindings stay visible from, when it has one.
    fn scope_row(&self) -> Option<&Row> {
        match self {
            BodyScope::Unscoped => None,
            BodyScope::RowScope(scope) | BodyScope::Call { scope, .. } => Some(scope),
        }
    }

    /// The Rule 10 import set, when the body has one.
    fn imports(&self) -> Option<&HashSet<String>> {
        match self {
            BodyScope::Call { imports, .. } => Some(imports),
            BodyScope::Unscoped | BodyScope::RowScope(_) => None,
        }
    }
}

pub(crate) fn execute_query_seeded(
    graph: &ExecGraph<'_>,
    query: &Query,
    params: &BTreeMap<String, Value>,
    seeds: Vec<Row>,
    qctx: QueryCtx<'_>,
) -> Result<QueryResult> {
    execute_query_seeded_in_scope(graph, query, params, seeds, qctx, BodyScope::Unscoped)
}

/// Execute `query` with `seed` as both its only input row and its
/// importing-subquery scope (`run_nested_clauses_scoped`): the seed is the
/// body's input, and every binding it carries stays visible for the whole
/// body. The one entry for scoped callers; the body-scope core below is
/// private because only the internal NEXT-stage recursion derives seeds
/// that legitimately diverge from the scope.
///
/// `imports` is the scope-clause body's CIP-218 Rule 10 import set (`None`
/// for the naked `CALL { … }` form). Each stage's UNION arms drop those
/// import columns before they are compared, since a row-derived arm result
/// carries the whole row (imports included) while the planner's agreement is
/// over the exported names.
pub(crate) fn execute_query_scoped(
    graph: &ExecGraph<'_>,
    query: &Query,
    params: &BTreeMap<String, Value>,
    seed: Row,
    imports: Option<&HashSet<String>>,
    qctx: QueryCtx<'_>,
) -> Result<QueryResult> {
    // The seed is both the input row and the importing scope; only a scope
    // clause adds the Rule 10 exclusion on top of it.
    let scope = match imports {
        Some(imports) => BodyScope::Call {
            scope: &seed,
            imports,
        },
        None => BodyScope::RowScope(&seed),
    };
    execute_query_seeded_in_scope(graph, query, params, vec![seed.clone()], qctx, scope)
}

/// How one scope-clause body's UNION arms join the fold — decided once per
/// stage from the stage's three facts (its Rule 10 import set, whether it
/// projects rows, and whether a NEXT stage consumes its output), so the fold
/// body carries no booleans.
#[derive(Clone, Copy)]
enum UnionArmPolicy<'a> {
    /// No imports to drop: compare each arm as produced (the naked
    /// `CALL { … }` and conditional callers' historical rule).
    AsIs,
    /// A scope-clause body that exports rows: drop the Rule 10 imports,
    /// then compare — the caller's merge is over exported names only.
    StripImports(&'a HashSet<String>),
    /// A scope-clause body whose result never reaches a caller: its arms
    /// are neither stripped nor compared, because their shape is arbitrary.
    Uncompared,
}

impl UnionArmPolicy<'_> {
    /// The one arm transform: strip when the policy says so, and hand the
    /// compare-vs-no-compare choice to the fold's merge (which aligns
    /// against its own accumulator).
    fn apply(self, result: QueryResult) -> (QueryResult, super::union::Align) {
        match self {
            Self::StripImports(imports) => (
                strip_import_columns(result, imports),
                super::union::Align::Compare,
            ),
            Self::AsIs => (result, super::union::Align::Compare),
            Self::Uncompared => (result, super::union::Align::Uncompared),
        }
    }
}

/// [`execute_query_scoped`]'s multi-seed core: arbitrary derived `seeds`
/// and the optionally distinct importing scope. Private to this module's
/// NEXT-stage recursion (the only caller whose seeds diverge from the
/// scope) and [`execute_query_seeded`]'s unscoped form.
fn execute_query_seeded_in_scope(
    graph: &ExecGraph<'_>,
    query: &Query,
    params: &BTreeMap<String, Value>,
    seeds: Vec<Row>,
    qctx: QueryCtx<'_>,
    scope: BodyScope<'_>,
) -> Result<QueryResult> {
    let mut fold = super::union::UnionFold::new(super::union::union_mode(&query.union_all)?);
    let mut seeds = seeds;
    let part_count = query.parts.len();
    // A scope-clause body whose parts all project no statement rows has no
    // export shape the caller sees — `call_subquery_row` short-circuits a
    // unit body to the outer row — so its UNION arms' arbitrary row-derived
    // columns are never compared. That holds only for the body's FINAL
    // stage: a `NEXT` stage seeds from these rows (`next_seeds`), which
    // requires a rectangular result, so the arms must agree there (a
    // mismatch is the typed `UNION requires same columns` error, never a
    // non-rectangular fold). The naked `CALL { … }` and conditional
    // callers keep their historical comparison.
    let projects_rows = stage_projects_rows(&query.parts);
    let imports = scope.imports();
    // One arm policy for the whole stage: a scope body that projects rows
    // strips its Rule 10 imports and compares; one whose result never
    // reaches a caller neither strips nor compares; a NEXT stage compares
    // (rectangular arms) but, matching the projection guard, does not strip
    // a unit body.
    let arm_policy = match imports {
        None => UnionArmPolicy::AsIs,
        Some(imported) if projects_rows => UnionArmPolicy::StripImports(imported),
        Some(_) if query.next.is_some() => UnionArmPolicy::AsIs,
        Some(_) => UnionArmPolicy::Uncompared,
    };
    for (part_index, part) in query.parts.iter().enumerate() {
        // The last part takes the seeds; earlier UNION parts clone them.
        let part_seeds = if part_index + 1 == part_count {
            std::mem::take(&mut seeds)
        } else {
            seeds.clone()
        };
        // ADR-0395: one nested clause-engine part pass — every UNION part
        // of this body crosses the one nested boundary
        // (`run_nested_clauses`: the budget tick, then the clause engine
        // at the explicit `None` profile site), so a wide body costs its
        // part count on the statement's budget instead of hiding its
        // width inside one invocation tick. The top-level statement
        // driver (exec/mod.rs) never enters here.
        let (rows, last) = match scope.scope_row() {
            Some(scope) => {
                run_nested_clauses_scoped(graph, &part.clauses, params, part_seeds, qctx, scope)?
            }
            None => run_nested_clauses(graph, &part.clauses, params, part_seeds, qctx)?,
        };
        let result = match last {
            Some(result) => result,
            None => super::rows_as_result(graph, &rows),
        };
        // The stage's one arm policy (computed above): strip the Rule 10
        // imports where the caller's merge is over exported names, compare
        // against the fold, or leave a discarded body's arms alone.
        let (result, align) = arm_policy.apply(result);
        fold.merge(result, align)?;
    }
    // A `Query` invariantly carries at least one part, so the fold always
    // produced a result.
    let result = fold.finish();
    if let Some(next) = query.next.as_deref() {
        // The one NEXT-stage seeding policy: a `Query`'s parts are the
        // same trailing-clause shape a `LogicalPlan`'s are. An importing
        // subquery's scope stays visible into the later stages, so the
        // seeds carry it too.
        let mut seeds =
            super::next_seeds(&result, query.parts.iter().map(|p| p.clauses.as_slice()));
        if let Some(scope) = scope.scope_row() {
            carry_scope_bindings(&mut seeds, scope);
        }
        execute_query_seeded_in_scope(graph, next, params, seeds, qctx, scope)
    } else {
        Ok(result)
    }
}

/// Drop one UNION arm's imported columns — names *and* row values — the
/// runtime half of CIP-218 Rule 10. A row-derived arm result
/// (`rows_as_result`) carries every binding of the row, the body's imports
/// among them, while the exported shape excludes them; without this the
/// fold compares unlike shapes and the caller's merge would see columns it
/// must ignore anyway. NEXT-carried names are not imports, so they stay
/// exported (CIP-213).
fn strip_import_columns(mut result: QueryResult, imported: &HashSet<String>) -> QueryResult {
    let keep: Vec<usize> = result
        .columns
        .iter()
        .enumerate()
        .filter(|(_, name)| !imported.contains(name.as_str()))
        .map(|(index, _)| index)
        .collect();
    if keep.len() == result.columns.len() {
        return result;
    }
    result.columns = keep
        .iter()
        .map(|&index| result.columns[index].clone())
        .collect();
    result.rows = result
        .rows
        .iter()
        .map(|row| keep.iter().map(|&index| row[index].clone()).collect())
        .collect();
    result
}

/// The names a `CALL { … }` body exports into the enclosing row's scope: the
/// shared [`return_columns`] set of its terminal `RETURN` — a `*` item
/// expanded from the body's declared scope via `star_scope_names`, its
/// sibling items appended after it — minus the `(vars)` imports. Both
/// runtime consumers take it as given: the `OPTIONAL CALL` null-pad
/// ([`null_pad_call_row`]) and the concluding CALL's result columns
/// ([`concluding_call_columns`]).
///
/// This is the runtime export set, not the front's admission set: the front's
/// `call_subquery_exports` threads the outer scope through the body's `WITH`s
/// and every introducing clause arm, so it is only as equal to this as
/// `star_scope_names`'s doc says the declared-scope walk is.
pub(crate) fn call_export_columns(
    query: &Query,
    scope: &relate_ir::CallScope,
    outer: Option<&Row>,
) -> Vec<String> {
    // The columns-only path's imported set (`imported_names` — the per-row
    // merge derives its own set from the seed it just built instead):
    // `None` imports nothing, `(*)` imports every visible outer name,
    // `(vars)` names its bindings. An exported name that is an import is a
    // pass-through, never a new column.
    let imported = imported_names(scope, outer);
    stage_export_columns(query.final_stage(), &imported)
}

/// One stage's exported names: the shared [`return_columns`] set of its
/// terminal `RETURN` minus the stage's CIP-218 Rule 10 imports. Empty when
/// the stage's last clause is not a `RETURN` (a `WHEN` body, `FINISH`, a
/// write) — such a stage has no projected export shape to agree on.
fn stage_export_columns(stage: &Query, imported: &HashSet<String>) -> Vec<String> {
    // A `Query` invariantly carries at least one part — the same invariant
    // the subquery fold in this file asserts.
    let part = stage
        .parts
        .first()
        .expect("a Query invariantly carries at least one part");
    let Some((clauses, items)) = terminal_return_items(part) else {
        return Vec::new();
    };
    return_columns(items, || star_scope_names(clauses))
        .into_iter()
        .filter(|name| !imported.contains(name.as_str()))
        .collect()
}

pub(crate) fn null_pad_call_row(mut row: Row, query: &Query, scope: &relate_ir::CallScope) -> Row {
    for column in call_export_columns(query, scope, Some(&row)) {
        row.insert(column, Value::Null);
    }
    row
}

/// Result columns for a part that concludes with a CALL instead of a
/// RETURN (ADR-0210): a concluding `CALL proc()` projects the rows'
/// visible keys (historical behavior); a concluding `CALL { … }`
/// subquery projects exactly the body's exported columns — its terminal
/// RETURN's output names. A unit/write-only body exports nothing and
/// the result is empty. Returns `None` when the part does not conclude
/// with a CALL (or a RETURN already produced the result). A `REPORT
/// STATUS AS <var>` column never appears here: the planner rejects
/// REPORT STATUS without a following RETURN, so the status column only
/// surfaces through that trailing RETURN.
pub(crate) fn concluding_call_columns(clauses: &[Clause], rows: &[Row]) -> Option<Vec<String>> {
    if clauses.iter().any(|c| matches!(c, Clause::Return { .. })) {
        return None;
    }
    if let Some(Clause::CallSubquery { query, scope, .. }) = clauses.last() {
        let columns = call_export_columns(query, scope, rows.first());
        // A unit/write-only body exports nothing: the statement result is
        // empty (no columns, no rows), matching the pre-ADR-0210 behavior
        // for concluding subqueries — pass-through outer rows must not
        // leak as column-less result rows. (A concluding `REPORT STATUS`
        // column never reaches here: the planner rejects REPORT STATUS
        // without a following RETURN.)
        return (!columns.is_empty()).then_some(columns);
    }
    if clauses.iter().any(|c| matches!(c, Clause::Call { .. })) {
        // Historical order: the row's own visible key order, unsorted.
        return Some(rows.first().map(visible_keys).unwrap_or_default());
    }
    None
}

/// The one concluding-CALL finisher (ADR-0210): when no clause produced a
/// result, project the concluding CALL's columns over the surviving rows;
/// otherwise fall back to an empty result. `stats` is the caller's stats
/// source — `graph.stats()` on the write path (`execute_plan_stage_seeded`),
/// `QueryStats::default()` on the read path (`execute_part_read_seeded`),
/// whose wrapper resets stats on the way out regardless.
pub(crate) fn concluding_part_result(
    clauses: &[Clause],
    rows: &mut [Row],
    last_result: Option<QueryResult>,
    stats: QueryStats,
) -> QueryResult {
    last_result
        .or_else(|| {
            concluding_call_columns(clauses, rows).map(|columns| {
                let data = materialize_rows(rows, &columns);
                QueryResult::intermediate(columns, data, stats.clone())
            })
        })
        .unwrap_or(QueryResult::intermediate(Vec::new(), Vec::new(), stats))
}

#[cfg(test)]
mod tests;
