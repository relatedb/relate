//! `CALL { }` subqueries, `CALL IN TRANSACTIONS`, conditional
//! branches, and part finishing.

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{Clause, Projection, Query};
use relate_names::SmallName;

use crate::ctx::QueryCtx;
use crate::eval::is_agg_expr;
use crate::graph::QueryStats;
use crate::overlay::ExecGraph;
use crate::row::Row;

mod conditional;
mod feed;
mod in_transactions;
mod shared;

pub(crate) use conditional::{conditional_produces_rows, exec_conditional};
pub(crate) use feed::{call_subquery_feed, exec_call_subquery};
pub(crate) use in_transactions::exec_call_in_transactions;
#[cfg(test)]
pub(crate) use in_transactions::transaction_error_message;

use super::dispatch::{run_nested_clauses, run_nested_clauses_scoped};
use super::scope::{
    carry_scope_bindings, has_star_item, nested_seed_row, return_columns, star_scope_names,
    terminal_return_items, visible_keys,
};
use super::{materialize_rows, QueryResult};

/// `CALL { }` clause shape (CIP-141/CIP-32), bundled to
/// keep the executors under the argument-count lint.
pub(crate) struct SubquerySpec<'a> {
    pub query: &'a Query,
    pub optional: bool,
    pub scope: &'a relate_ir::CallScope,
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
            let value = name
                .with_str(|text| import_value(text, "imported variable", outer.get_name(name)))?;
            Ok((name.to_string(), value))
        })
        .collect()
}

/// One import's value out of the row that seeds the body — the one
/// read-and-report of the planner's import invariant: a name the clause's
/// scope promises must survive every row-shape pass (the `UNWIND` source
/// strip, the liveness trim, the `OPTIONAL CALL` null-pad), so a miss is an
/// internal error naming the import, reported rather than turned into a
/// panic.
fn import_value(name: &str, site: &str, value: Option<&Value>) -> Result<Value> {
    value.cloned().ok_or_else(|| {
        Error::internal(format!(
            "planner-scope invariant: {site} `{name}` must be in the outer row"
        ))
    })
}

/// The seed row one outer row gives a `CALL { … }` body — the one
/// `CallScope` → seed-row policy. The `(*)` scope hands the body the outer
/// row minus the enclosing clause engine's internal columns
/// ([`nested_seed_row`]): a parked `__ord*`/`__agg_*` slot is the enclosing
/// clause's state, never a binding the body may read as its own. The naked
/// scope imports only the names its leading importing `WITH` pulls in
/// ([`naked_import_row`]); a `(vars)` import list names its bindings explicitly,
/// so it needs no strip ([`import_seed`]).
fn scope_seed(
    graph: &ExecGraph<'_>,
    query: &Query,
    scope: &relate_ir::CallScope,
    outer: &Row,
) -> Result<Row> {
    match scope {
        relate_ir::CallScope::None => {
            naked_import_row(query.parts.iter(), &nested_seed_row(graph, outer))
        }
        relate_ir::CallScope::Wildcard => Ok(nested_seed_row(graph, outer)),
        relate_ir::CallScope::Vars(vars) => import_seed(vars, outer),
    }
}

/// The naked `CALL { … }` seed: exactly the names the given UNION arms'
/// leading importing `WITH`s pull in from `source` (CIP-141's legacy
/// import form — the front end's own rule,
/// [`relate_ir::SingleQuery::leading_imports`]) and nothing at all for an
/// arm without one. Seeding the whole outer row instead correlated the body
/// silently: a name the body rebinds (`MATCH (n)`) met the outer row's
/// binding and was evaluated as a pre-bound join, so counts, filters and
/// writes covered the outer row's node only (REL8-2023 issue 50).
///
/// One read for both consumers — the stage seed (every arm, over the
/// stripped outer row) and the per-arm restriction
/// ([`execute_query_seeded_in_scope`], one arm, over the stage seed), which
/// is what keeps one arm's imports out of an arm that imports none.
fn naked_import_row<'q>(
    parts: impl IntoIterator<Item = &'q relate_ir::SingleQuery>,
    source: &Row,
) -> Result<Row> {
    // `visible` is the key set of whatever row seeds this call — the stage
    // seed's caller passes the stripped outer row, the per-arm restriction
    // passes the stage seed — i.e. exactly the names an import may name at
    // that site.
    let visible: std::collections::BTreeSet<String> = source.keys().cloned().collect();
    let mut seed = Row::new();
    // The one classification of "this arm imports": the typed payload the
    // names are then read off. An arm without one contributes nothing.
    for with in parts
        .into_iter()
        .filter_map(|part| part.leading_importing_with())
    {
        for name in with.imports(&visible) {
            let value = import_value(&name, "the naked CALL import", source.get(&name))?;
            seed.insert(name, value);
        }
    }
    Ok(seed)
}

/// The import set a body run excludes from its merge: the seed's own
/// keys, only under a scope clause. `None` therefore means the naked form,
/// which is what [`call_body`] turns into the body's form; the per-row path
/// derives the set from its fresh seed, the shared fast path passes the
/// check set it already reduced at construction.
fn scope_imports(scope: &relate_ir::CallScope, seed: &Row) -> Option<HashSet<String>> {
    scope
        .has_scope_clause()
        .then(|| seed.keys().cloned().collect())
}

/// The one row-merge policy of both subquery paths: the body's export
/// overlays the outer row except for the `excluded` column names, which
/// always keep the outer row's own value. Callers pass the scope clause's
/// imported names (both the per-row path and the shared plan's check set);
/// the naked form excludes nothing — its imports are per arm and its
/// exports are never pass-throughs.
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
    graph: &ExecGraph<'_>,
    row: Row,
    optional: bool,
    query: &Query,
    scope: &relate_ir::CallScope,
) -> Option<Row> {
    optional.then(|| null_pad_call_row(graph, row, query, scope))
}

/// The names a `CALL { … }` scope imports from the outer row — the export
/// set's derivation (`call_export_columns`). A `(vars)` list names its
/// bindings; the naked scope imports nothing; the `(*)` scope imports the
/// outer row's visible names ([`visible_keys`] = the row minus the
/// enclosing clause engine's internal columns, the same set the stripped
/// seed binds). `None` (no row to derive from, e.g. a columns-only
/// question over an empty table) imports nothing — the star expansion then
/// falls back to the declared scope.
fn imported_names(
    graph: &ExecGraph<'_>,
    scope: &relate_ir::CallScope,
    outer: Option<&Row>,
) -> HashSet<String> {
    // The scope's own spelling of the imported names, derived without a
    // seed row so there is no fallible step to default: a `(vars)` list
    // names its bindings (exactly the keys [`import_seed`] would build),
    // the naked scope imports nothing (the [`scope_imports`] scope-clause
    // gate), and `(*)` imports the outer row's visible names (exactly the
    // keys [`nested_seed_row`] would bind). `None` — no row to derive
    // from, e.g. a columns-only question over an empty table — imports
    // nothing, and the star expansion falls back to the declared scope.
    match scope {
        // Text render: the imported-name set is the String scope domain.
        relate_ir::CallScope::Vars(vars) => vars.iter().map(|name| name.to_string()).collect(),
        relate_ir::CallScope::None => HashSet::new(),
        relate_ir::CallScope::Wildcard => outer
            .map(|row| visible_keys(graph, row).into_iter().collect())
            .unwrap_or_default(),
    }
}

/// Whether a `CALL { … }` body writes — any store-mutating clause
/// anywhere in it, through the one canonical per-clause classification
/// ([`super::mutates_store`]: the planner's table, with this layer's
/// `LOAD CSV`-reads and procedure-registry rulings). The `walk_query`
/// traversal descends into nested bodies, so one flat fold sees every
/// clause.
///
/// The parked form runs the body lazily, one outer row at a time, so a
/// writing body's writes would interleave with the consuming projection's
/// per-row graph reads (a `COUNT { MATCH … }` item would see the writes
/// of the rows pulled so far, not all of them); the materializing form
/// runs every body before any projection row is evaluated. A writing body
/// therefore never parks, whatever its consumer.
pub(crate) fn query_writes(query: &Query) -> bool {
    let mut writes = false;
    relate_ir::walk_query(
        query,
        &mut |clause| writes |= super::mutates_store(clause),
        &mut |_| {},
    );
    writes
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
    let seed = scope_seed(graph, query, scope, outer)?;
    // The seed is the body's importing scope: every binding it carries stays
    // visible to the whole body, even across a `WITH` that does not project
    // it (`run_nested_clauses_scoped` re-attaches a dropped binding after
    // the projection, so it is never injected as a grouping key and a
    // keyless aggregation over an emptied input keeps its identity row).
    // This supersedes the `query_with_persistent_imports` rewrite: the
    // scoped clause loop preserves the same bindings without altering the
    // body's `WITH` items.
    //
    // The seed's own keys ARE the scope clause's import set (the `(vars)`
    // list it was built from, or the `(*)` form's visible outer names) —
    // one derivation, not a parallel walk. The naked form's seed carries
    // only its arms' leading imports, so it has no exclusion set either
    // (its arms' exports are never pass-throughs).
    let imported = scope_imports(scope, &seed);
    // The one CALL-body scope construction — the shared fast path
    // (`call/feed.rs`) and the null-pad path take the same one.
    let body = call_body(imported.as_ref());
    let result = execute_query_scoped(graph, query, params, seed, body, qctx)?;
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

/// The importing scope a body runs under, with the scope clause's import
/// set as a property of the mode rather than a second parallel parameter:
/// only a scope clause has one, so the invalid "imports without a scope"
/// combination is unrepresentable.
#[derive(Clone, Copy)]
enum BodyScope<'a> {
    /// No importing scope: the statement's own rows ([`execute_query_seeded`]).
    Unscoped,
    /// The driving row is the body's importing scope but there is no import
    /// exclusion: a `WHEN` branch body (CIP-213's by-table semantics: the
    /// branch sees the whole driving row).
    RowScope(&'a Row),
    /// The naked `CALL { … }` body: each UNION arm sees only the names its
    /// own leading importing `WITH` pulls in ([`naked_import_row`]), so the arm
    /// loop restricts the seed per arm
    /// ([`execute_query_seeded_in_scope`]) instead of handing every arm
    /// the stage's union.
    NakedCall(&'a Row),
    /// A scope-clause `CALL (vars) { … }` / `CALL (*) { … }` body: the
    /// importing scope plus the import set its UNION arms drop
    /// before they are compared.
    Call {
        scope: &'a Row,
        imports: &'a HashSet<String>,
    },
}

/// Which `CALL { … }` body form runs: a scope clause is exactly the form
/// that carries an import set (`scope_imports`/`has_scope_clause`), so the
/// presence of that set *is* the form ([`call_body`]), and one construction
/// of the body's scope ([`BodyScope::for_call`]) turns it into the run.
#[derive(Clone, Copy)]
enum CallBody<'a> {
    /// The naked `CALL { … }` form: imports only its arms' leading
    /// importing `WITH` names.
    Naked,
    /// A scope clause (`CALL (…) { … }` / `CALL (*) { … }`): its imports,
    /// which the UNION arms also drop before they are compared.
    Scoped(&'a HashSet<String>),
}

/// The body form of one CALL clause, total over [`scope_imports`]'s own
/// fact: a scope clause is exactly the form that carries an import set
/// (`has_scope_clause`), so `None` can only be the naked form.
fn call_body(imports: Option<&HashSet<String>>) -> CallBody<'_> {
    match imports {
        Some(imports) => CallBody::Scoped(imports),
        None => CallBody::Naked,
    }
}

impl<'a> BodyScope<'a> {
    /// The one construction of a CALL body's scope from its form.
    fn for_call(seed: &'a Row, body: CallBody<'a>) -> Self {
        match body {
            CallBody::Scoped(imports) => BodyScope::Call {
                scope: seed,
                imports,
            },
            CallBody::Naked => BodyScope::NakedCall(seed),
        }
    }

    /// The row the body's bindings stay visible from, when it has one.
    fn scope_row(&self) -> Option<&Row> {
        match self {
            BodyScope::Unscoped => None,
            BodyScope::RowScope(scope)
            | BodyScope::NakedCall(scope)
            | BodyScope::Call { scope, .. } => Some(scope),
        }
    }

    /// The scope clause's import set, when the body has one.
    fn imports(&self) -> Option<&HashSet<String>> {
        match self {
            BodyScope::Call { imports, .. } => Some(imports),
            BodyScope::Unscoped | BodyScope::RowScope(_) | BodyScope::NakedCall(_) => None,
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
/// `body` is the clause's form ([`call_body`]); a scope clause's UNION arms
/// also drop its import columns before they are compared, since a
/// row-derived arm result carries the whole row (imports included) while the
/// planner's agreement is over the exported names.
fn execute_query_scoped(
    graph: &ExecGraph<'_>,
    query: &Query,
    params: &BTreeMap<String, Value>,
    seed: Row,
    body: CallBody<'_>,
    qctx: QueryCtx<'_>,
) -> Result<QueryResult> {
    // The seed is both the input row and the importing scope.
    let scope = BodyScope::for_call(&seed, body);
    execute_query_seeded_in_scope(graph, query, params, vec![seed.clone()], qctx, scope)
}

/// How one scope-clause body's UNION arms join the fold — decided once per
/// stage from the stage's three facts (its import set, whether it
/// projects rows, and whether a NEXT stage consumes its output), so the fold
/// body carries no booleans.
#[derive(Clone, Copy)]
enum UnionArmPolicy<'a> {
    /// No imports to drop: compare each arm as produced (the naked
    /// `CALL { … }` and conditional callers' historical rule).
    AsIs,
    /// A scope-clause body that exports rows: drop the star-excluded
    /// imports, then compare — the caller's merge is over exported names only.
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
    // strips its star-excluded imports and compares; one whose result never
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
        // A naked body's imports are per arm (issue 50): the stage seed is
        // the arms' union, so each arm keeps only the names its own leading
        // importing `WITH` pulls in and an arm without one starts empty.
        // The restricted row is both the arm's seed and its always-visible
        // scope, so a name it does not import is neither seeded nor
        // re-attached after a body `WITH`.
        let naked_part = match scope {
            BodyScope::NakedCall(seed) => Some(naked_import_row(std::iter::once(part), seed)?),
            _ => None,
        };
        // The last part takes the seeds; earlier UNION parts clone them.
        let part_seeds = match &naked_part {
            Some(row) => vec![row.clone()],
            None if part_index + 1 == part_count => std::mem::take(&mut seeds),
            None => seeds.clone(),
        };
        let part_scope = naked_part.as_ref().or_else(|| scope.scope_row());
        // ADR-0395: one nested clause-engine part pass — every UNION part
        // of this body crosses the one nested boundary
        // (`run_nested_clauses`: the budget tick, then the clause engine
        // at the explicit `None` profile site), so a wide body costs its
        // part count on the statement's budget instead of hiding its
        // width inside one invocation tick. The top-level statement
        // driver (exec/mod.rs) never enters here.
        let (rows, last) = match part_scope {
            Some(scope) => {
                run_nested_clauses_scoped(graph, &part.clauses, params, part_seeds, qctx, scope)?
            }
            None => run_nested_clauses(graph, &part.clauses, params, part_seeds, qctx)?,
        };
        let result = match last {
            Some(result) => result,
            None => super::rows_as_result(graph, &rows),
        };
        // The stage's one arm policy (computed above): strip the
        // star-excluded imports where the caller's merge is over exported names, compare
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
        // A NEXT stage is seeded from the stage's result rows, never from a
        // per-arm import restriction: the naked form's per-arm rule
        // (the arm loop's `naked_import_row`) is its first stage's, and applying it here
        // would replace the carried result with an import row.
        let next_scope = match scope {
            BodyScope::NakedCall(seed) => BodyScope::RowScope(seed),
            other => other,
        };
        execute_query_seeded_in_scope(graph, next, params, seeds, qctx, next_scope)
    } else {
        Ok(result)
    }
}

/// Drop one UNION arm's imported columns — names *and* row values — the
/// runtime half of the import rule. A row-derived arm result
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
    graph: &ExecGraph<'_>,
    query: &Query,
    scope: &relate_ir::CallScope,
    outer: Option<&Row>,
) -> Vec<String> {
    // The columns-only path's imported set (`imported_names` — the per-row
    // merge derives its own set from the seed it just built instead):
    // `None` imports nothing, `(*)` imports every visible outer name,
    // `(vars)` names its bindings. An exported name that is an import is a
    // pass-through, never a new column.
    let imported = imported_names(graph, scope, outer);
    stage_export_columns(query.final_stage(), &imported)
}

/// One stage's exported names: the shared [`return_columns`] set of its
/// terminal `RETURN` minus the stage's imports. Empty when
/// the stage's last clause is not a `RETURN` (a `WHEN` body, `FINISH`, a
/// write) — such a stage has no projected export shape to agree on.
fn stage_export_columns(stage: &Query, imported: &HashSet<String>) -> Vec<String> {
    let Some((clauses, items)) = final_export_items(stage) else {
        return Vec::new();
    };
    return_columns(items, || star_scope_names(clauses))
        .into_iter()
        .filter(|name| !imported.contains(name.as_str()))
        .collect()
}

/// A body's final-stage export projection — the final stage's first part
/// through [`terminal_return_items`] — the one derivation every
/// export-shape consumer reads: [`stage_export_columns`]'s column walk
/// here, and the IN TRANSACTIONS failed-batch row's per-row evaluation
/// (`call::in_transactions`). `None` when the stage does not conclude in
/// a `RETURN` (a `WHEN` body, `FINISH`, a write) — such a body has no
/// projected export shape to agree on.
fn final_export_items(stage: &Query) -> Option<(&[Clause], &[Projection])> {
    // A `Query` invariantly carries at least one part — the same invariant
    // the subquery fold in this file asserts.
    let part = stage
        .parts
        .first()
        .expect("a Query invariantly carries at least one part");
    terminal_return_items(part)
}

/// Whether a body's export projection is a plain per-row projection — no
/// `*` item to expand, no aggregate to fold — so a caller without the
/// body's rows (a rolled-back IN TRANSACTIONS batch) can still evaluate
/// it over an import seed.
fn plain_per_row_projection(items: &[Projection]) -> bool {
    !has_star_item(items) && !items.iter().any(|item| is_agg_expr(&item.expr))
}

fn null_pad_call_row(
    graph: &ExecGraph<'_>,
    mut row: Row,
    query: &Query,
    scope: &relate_ir::CallScope,
) -> Row {
    for column in call_export_columns(graph, query, scope, Some(&row)) {
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
fn concluding_call_columns(
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    rows: &[Row],
) -> Option<Vec<String>> {
    if clauses.iter().any(|c| matches!(c, Clause::Return { .. })) {
        return None;
    }
    if let Some(Clause::CallSubquery { query, scope, .. }) = clauses.last() {
        let columns = call_export_columns(graph, query, scope, rows.first());
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
        return Some(
            rows.first()
                .map(|row| visible_keys(graph, row))
                .unwrap_or_default(),
        );
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
    graph: &ExecGraph<'_>,
    clauses: &[Clause],
    rows: &mut [Row],
    last_result: Option<QueryResult>,
    stats: QueryStats,
) -> QueryResult {
    last_result
        .or_else(|| {
            concluding_call_columns(graph, clauses, rows).map(|columns| {
                let data = materialize_rows(rows, &columns);
                QueryResult::intermediate(columns, data, stats.clone())
            })
        })
        .unwrap_or(QueryResult::intermediate(Vec::new(), Vec::new(), stats))
}

#[cfg(test)]
mod tests;
