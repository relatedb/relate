//! Expression evaluation: the `eval` dispatcher and the `eval/`
//! submodule wiring.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, MapProjectionItem};
use relate_names::{SmallName, Token};

use crate::ctx::EvalCtx;
// The pattern-expression arms (PatternPred, PatternComp, ShortestPath)
// and the subquery arms below them delegate UP to engines: the walk
// engine at `crate::eval_walk` (a pattern-walk client like the exec
// walkers, hoisted out of this tree so `eval` stays the expression core
// strictly below `pattern_walk`, importing no walk module) and `exec`
// (the long-established engine-delegation edge). The cross-engine
// pattern policy that walker reads lives at the shared tier
// (`crate::pattern_walk`, LABS-811) — one public path per item, no
// re-export here.
use crate::eval_walk::{collect_pattern_rows, pattern_exists};
use crate::exec::{eval_shortest, ShortestWant};
use crate::overlay::ExecGraph;
use crate::row::Row;

mod aggregate;
mod arithmetic;
mod collection;
mod expr;
mod fold;
mod function;
mod items;
mod labels;
mod membership_index;
mod numeric;
mod predicates;
mod quantifier;
mod regex_cache;
mod string;
mod temporal;
#[cfg(test)]
mod tests;
mod typematch;
mod vector;

pub(crate) use aggregate::{
    fold_aggregate, needs_numbers, rewrite_horizontal_aggregates, FoldInput, Percentile,
    PercentileSample, StreamFold, SumAcc,
};
pub(crate) use fold::{extreme_offer, fold_extreme, fold_quantifier, membership, Extreme};
pub(crate) use items::{into_items, Items};
pub(crate) use labels::{
    dynamic_label_names, dynamic_property_key, label_expr_matches, resolve_write_labels,
};
// The expression walkers live in `relate-ir` next to `Expr::children`
// so planner and executor share one traversal.
pub(crate) use relate_ir::{
    expr_vars, is_agg_expr, key_of, label_expr_vars, orient, var_key_side, var_prop,
};

pub(crate) use expr::{
    binary, binary_pure, cmp_bool, eval_case_with, in_op, in_rhs, ordering_operator,
};
use expr::{eval_case, prop, reject_deleted_access, slice, subscript, unary};
pub(crate) use function::is_nondeterministic;
use function::{entity_backing_map, function};
pub(crate) use function::{lazy_range, RangeValues};
use predicates::comprehension_where_holds;
#[cfg(test)]
pub(crate) use predicates::take_candidate_passes;
pub(crate) use predicates::{
    candidate_passes, filter_rows_where, inline_predicates_ok, predicate_holds, where_clause_holds,
};
use quantifier::{eval_all_reduce, eval_quantifier, eval_reduce, list_argument};
pub(crate) use quantifier::{AllReduceFold, AllReduceStep};
use string::is_normalized;
pub(crate) use typematch::value_matches_type;

/// The boolean-or-null reading shared by every requiring site: a boolean
/// yields its truthiness, null yields the unknown, anything else is an
/// `InvalidArgumentType` type error naming the requiring context. This is
/// the *whole* policy of the list-quantifier WHERE predicate
/// (`quantifier.rs`); the boolean operators layer the deprecated
/// list/path conversion on top through [`bool_operand`] (ADR-0458 §7).
pub(crate) fn bool_or_null(v: &Value, ctx: &str) -> Result<Option<bool>> {
    match v {
        Value::Bool(b) => Ok(Some(*b)),
        Value::Null => Ok(None),
        // ADR-0511: the non-boolean arm is the typed
        // `InvalidArgumentType` variant, so this site and the planner's
        // boolean-requiring checks classify as the TypeError class Neo4j
        // surfaces for them (LABS-400, superseding ADR-0003's SyntaxError
        // pin for this family). The one `requires_boolean` constructor
        // keeps the wording identical across those sites.
        _ => Err(Error::requires_boolean(ctx)),
    }
}

/// The one boolean-operand policy of the boolean operators (`expr.rs`'s
/// NOT/AND/OR/XOR) and, through [`predicate_value_holds`], the predicate
/// positions (MATCH/WHERE, WITH/FILTER, HAVING, the GPM route filter, the
/// pattern engines' per-trial WHERE, the comprehension WHEREs):
/// [`bool_or_null`] plus the deprecated list/path conversion ADR-0458 §7 —
/// an empty list is false, a non-empty one (and any path, which always
/// holds at least one node) is true (ReturnAcceptance "RETURN true AND
/// list"). Every other requiring site reads [`bool_or_null`] directly, so
/// a list or path can never stand in for a quantifier predicate.
pub(crate) fn bool_operand(v: &Value, ctx: &str) -> Result<Option<bool>> {
    match v {
        Value::List(values) => Ok(Some(!values.is_empty())),
        Value::Path(_) => Ok(Some(true)),
        _ => bool_or_null(v, ctx),
    }
}

/// Whether an evaluated boolean-context predicate holds: the same runtime
/// list/path conversion the boolean operators apply (an empty list is false,
/// a non-empty list and any path true), a null never holds, and any other
/// non-boolean is the typed `InvalidArgumentType` a statically known one is
/// rejected with. The MATCH/WHERE, WITH/FILTER, HAVING, GPM route-filter,
/// pattern-engine and comprehension predicate positions read this one
/// helper; the list-quantifier's inner WHERE keeps [`bool_or_null`].
/// Named apart from [`predicate_holds`] (the expression-level form).
pub(crate) fn predicate_value_holds(v: &Value) -> Result<bool> {
    Ok(bool_operand(v, "predicate")? == Some(true))
}

/// An operand of a container access (`Subscript`/`Property`/
/// `DynamicProperty`/`Slice`): a plain variable is borrowed from the row —
/// evaluating it would deep-clone the whole bound container, and the access
/// only needs the element — while any other expression is evaluated and
/// owned. `Cow` carries the borrowed-or-owned value.
fn eval_operand<'a>(
    graph: &ExecGraph<'a>,
    row: &'a Row,
    expr: &Expr,
    params: &BTreeMap<String, Value>,
) -> Result<std::borrow::Cow<'a, Value>> {
    if let Expr::Variable(name) = expr {
        // Free read bridge: the String-keyed row schema is probed through
        // the name's text (the inline arm decodes on the stack).
        if let Some(v) = row.get_name(name) {
            return Ok(std::borrow::Cow::Borrowed(v));
        }
    }
    let v = eval(graph, row, expr, params)?;
    Ok(std::borrow::Cow::Owned(v))
}

/// The summed logical bytes of a row set — the row-materialization metric
/// (ADR-0344 metric fields): each binding counts its key's string bytes
/// (ADR-0042 string size) plus its value's [`Value::logical_bytes`]. The
/// one canonical spelling of "deep size of these rows", beside the `Row`
/// type it measures; the PROFILE dispatch loop is the caller. A row's
/// map header itself is not counted — only what the row carries, so a
/// column rename shows up as the value's size, not a constant offset.
pub(crate) fn rows_logical_bytes(rows: &[Row]) -> u64 {
    rows.iter()
        .flat_map(|row| row.iter())
        .map(|(key, value)| relate_core::str_logical_bytes(key) + value.logical_bytes())
        .sum()
}

/// The one call-shape test: `expr` is a call to `name` (case-insensitive)
/// with exactly `arity` arguments, returning them. Every statement-shape
/// recognizer (the seek shapes, the id-conversion reader, the label-set
/// group keys of ADR-0589) consults it instead of re-spelling the
/// name/arity conjunction.
pub(crate) fn named_call<'a>(expr: &'a Expr, name: &str, arity: usize) -> Option<&'a [Expr]> {
    match expr {
        Expr::Function {
            name: actual, args, ..
        } if actual.with_str(|text| text.eq_ignore_ascii_case(name)) && args.len() == arity => {
            Some(args)
        }
        _ => None,
    }
}

/// Evaluate a scalar expression against a row.
pub fn eval(
    graph: &ExecGraph<'_>,
    row: &Row,
    expr: &Expr,
    params: &BTreeMap<String, Value>,
) -> Result<Value> {
    let ctx = EvalCtx { graph, row, params };
    match expr {
        Expr::RuntimeValue(value) => Ok(value.clone()),
        Expr::Null => Ok(Value::Null),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Int(i) => Ok(Value::Int(*i)),
        Expr::Float(f) => Ok(Value::Float(*f)),
        Expr::String(s) => Ok(Value::String(s.as_str().into())),
        // ADR-0308: the facade rejects a planned statement whose parameter
        // references are absent before dispatch, so this fires only for
        // expressions the plan walk cannot see (catalog commands lower
        // theirs at run time) — with the same typed error, never `null`.
        Expr::Parameter(n) => params
            .get(n)
            .cloned()
            .ok_or_else(|| Error::parameters_missing([n.as_str()])),
        // ADR-0335: the plan-time binding walks reject an undefined
        // variable before execution, so a row miss here is either a
        // planner/executor binding bug or an expression the plan walk
        // cannot see (run-time-lowered catalog predicates) — never `null`.
        Expr::Variable(n) => row
            .get_name(n)
            .cloned()
            .ok_or_else(|| n.with_str(Error::undefined_variable)),
        Expr::Star => Err(Error::semantic("* is not a scalar expression")),
        Expr::Property { expr, key, key_id } => {
            // Variable operands are read by reference (see Subscript): a
            // property read must not clone the whole map it reads from.
            let v = eval_operand(graph, row, expr, params)?;
            reject_deleted_access(graph, &v)?;
            prop(graph, &v, &(key.clone(), *key_id))
        }
        Expr::DynamicProperty { expr, key } => {
            let v = eval_operand(graph, row, expr, params)?;
            if v.is_null() {
                return Ok(Value::Null);
            }
            reject_deleted_access(graph, &v)?;
            let key = dynamic_property_key(eval(graph, row, key, params)?)?;
            prop(graph, &v, &(SmallName::from_name(&key), Token::Unresolved))
        }
        Expr::LabelPredicate {
            expr,
            label_expr,
            strict,
        } => {
            let v = eval(graph, row, expr, params)?;
            Ok(match v {
                Value::Node(n) => Value::Bool(label_expr_matches(
                    graph, row, params, label_expr, &n.labels, false,
                )?),
                Value::Relationship(r) => Value::Bool(label_expr_matches(
                    graph,
                    row,
                    params,
                    label_expr,
                    std::slice::from_ref(&r.rel_type),
                    true,
                )?),
                Value::Null => Value::Null,
                _ if *strict => {
                    return Err(Error::invalid_argument_type(
                        "IS LABELED requires a node or relationship",
                    ))
                }
                _ => Value::Bool(false),
            })
        }
        Expr::Map(entries) => {
            let mut m = BTreeMap::new();
            for (k, e) in entries {
                m.insert(k.clone(), eval(graph, row, e, params)?);
            }
            Value::checked_map(m)
        }
        Expr::List(xs) => {
            let mut out = Vec::new();
            for e in xs {
                out.push(eval(graph, row, e, params)?);
            }
            Value::checked_list(out)
        }
        Expr::Unary { op, expr } => {
            let v = eval(graph, row, expr, params)?;
            unary(*op, v)
        }
        Expr::Binary { op, left, right } => {
            let l = eval(graph, row, left, params)?;
            let r = eval(graph, row, right, params)?;
            binary(graph, *op, l, r)
        }
        Expr::Function {
            name,
            distinct,
            args,
        } => name.with_str(|text| function(graph, row, params, text, *distinct, args)),
        // 42I24 (see `function.rs`): the scalar-eval backstop for a `count(*)`
        // that reached evaluation outside an aggregation context is a
        // compile-time-class `SyntaxError` in Neo4j.
        Expr::CountStar => Err(Error::invalid_aggregation(
            "count(*) only valid in aggregation",
        )),
        Expr::Case {
            when_then,
            else_expr,
        } => eval_case(graph, row, params, when_then, else_expr),
        Expr::Subscript { expr, index } => {
            // The container is evaluated before the index (the pre-by-reference
            // order, so error precedence is unchanged), and a variable
            // operand is read by reference: evaluating it would deep-clone
            // the whole container, and the subscript only needs the element.
            let v = eval_operand(graph, row, expr, params)?;
            let i = eval(graph, row, index, params)?;
            subscript(graph, &v, i)
        }
        Expr::Slice { expr, start, end } => {
            let v = eval_operand(graph, row, expr, params)?;
            let s = match start {
                Some(e) => Some(eval(graph, row, e, params)?),
                None => None,
            };
            let e = match end {
                Some(e) => Some(eval(graph, row, e, params)?),
                None => None,
            };
            slice(&v, s, e)
        }
        // CIP-272 map comprehension: the source-shape triage and the
        // bind/filter/project loop live together in `eval_map_comp`.
        Expr::MapComp {
            key_ident,
            value_ident,
            source,
            predicate,
            key_expr,
            value_expr,
        } => eval_map_comp(
            ctx,
            key_ident.as_ref(),
            value_ident,
            source,
            predicate.as_deref(),
            key_expr,
            value_expr,
        ),
        Expr::ListComp {
            ident,
            list,
            predicate,
            projection,
        } => eval_list_comp(ctx, ident, list, predicate.as_deref(), projection),
        Expr::Quantifier {
            kind,
            ident,
            list,
            predicate,
        } => eval_quantifier(ctx, *kind, ident, list, predicate),
        Expr::Reduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
        } => eval_reduce(ctx, accumulator, initial, ident, list, expression),
        Expr::AllReduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
            predicate,
        } => eval_all_reduce(
            ctx,
            accumulator,
            initial,
            ident,
            list,
            expression,
            predicate,
        ),
        Expr::IsType {
            expr,
            type_expr,
            negated,
        } => {
            let value = eval(graph, row, expr, params)?;
            Ok(Value::Bool(
                value_matches_type(&value, type_expr) ^ *negated,
            ))
        }
        Expr::IsNormalized {
            expr,
            form,
            negated,
        } => {
            let value = eval(graph, row, expr, params)?;
            let Value::String(s) = value else {
                return Ok(Value::Null);
            };
            // ADR-0463: the normalization walk allocates a full copy that can
            // expand several-fold; charge the conservative bound first.
            crate::eval::string::charge_unicode_expansion(graph, &s)?;
            let result = is_normalized(&s, form.as_deref().unwrap_or("NFC"));
            Ok(Value::Bool(result ^ *negated))
        }
        Expr::ChronoCompare {
            left,
            op,
            right,
            negated,
        } => {
            let l = eval(graph, row, left, params)?;
            let r = eval(graph, row, right, params)?;
            Ok(temporal::chrono_compare(*op, &l, &r, *negated))
        }
        Expr::MapProjection { expr, items } => eval_map_projection(ctx, expr, items),
        Expr::DistanceMetric(_) => Err(Error::semantic(
            "distance metrics are only valid as a vector_distance/vector_norm argument",
        )),
        Expr::PatternPred { path } => {
            // LABS-193/LABS-246: existence only — the anchored probe, then
            // a limit-1 walk; never a full match collection.
            Ok(Value::Bool(pattern_exists(graph, row, path, params)?))
        }
        Expr::PatternComp {
            path,
            predicate,
            projection,
        } => eval_pattern_comp(ctx, path, predicate.as_deref(), projection),
        Expr::ShortestPath { all, path } => {
            eval_shortest(graph, row, params, *all, path, ShortestWant::Path)
        }
        Expr::ExistsSubquery { query } => crate::exec::eval_exists_query(graph, row, params, query),
        Expr::CountSubquery { query } => crate::exec::eval_count_query(graph, row, params, query),
        Expr::CollectSubquery { query } => {
            crate::exec::eval_collect_query(graph, row, params, query)
        }
    }
}

/// The one retaining-projection loop: charge the output container's slot
/// buffer, then for each bound row apply the optional WHERE (the
/// comprehension WHERE gate, [`comprehension_where_holds`]), project, and
/// grow the container's retained total with the payload. Shared by the
/// list and pattern comprehensions so the charge protocol has one
/// spelling.
fn retain_projected(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    bound: usize,
    rows: impl Iterator<Item = Row>,
    predicate: Option<&Expr>,
    projection: &Expr,
) -> Result<Value> {
    let mut retained = graph.retain_slots(bound)?;
    let mut out = Vec::with_capacity(bound);
    for r in rows {
        graph.cancel_tick()?;
        if !comprehension_where_holds(graph, &r, predicate, params)? {
            continue;
        }
        let value = eval(graph, &r, projection, params)?;
        retained.add_owned(graph, &value)?;
        out.push(value);
    }
    Value::checked_list(out)
}

/// CIP list comprehension: bind one item per source element, filter with the
/// optional WHERE, project, and build one retaining output container.
fn eval_list_comp(
    ctx: EvalCtx<'_>,
    ident: &SmallName,
    list: &Expr,
    predicate: Option<&Expr>,
    projection: &Expr,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    let Some(items) = list_argument(
        eval(graph, row, list, params)?,
        "list comprehension needs a list",
    )?
    else {
        return Ok(Value::Null);
    };
    let bound = items.len();
    let rows = into_items(items).map(move |item| {
        let mut r = row.clone();
        r.insert(ident.to_string(), item);
        r
    });
    retain_projected(graph, params, bound, rows, predicate, projection)
}

/// Pattern comprehension: collect the pattern matches, filter with the
/// optional WHERE, project, and build one retaining output container.
fn eval_pattern_comp(
    ctx: EvalCtx<'_>,
    path: &relate_ir::PathPattern,
    predicate: Option<&Expr>,
    projection: &Expr,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    let matches = collect_pattern_rows(graph, row, path, params)?;
    let bound = matches.len();
    retain_projected(
        graph,
        params,
        bound,
        matches.into_iter(),
        predicate,
        projection,
    )
}

/// CIP-272 map comprehension: the source-shape triage, owned beside the
/// loop it feeds. The evaluated source and the comprehension's binding
/// shape must agree — a MAP source with (key, value) bindings and a LIST
/// source with the item binding alone are the two productive pairings; a
/// null source is null; every other pairing is the typed shape error.
fn eval_map_comp(
    ctx: EvalCtx<'_>,
    key_ident: Option<&SmallName>,
    value_ident: &SmallName,
    source: &Expr,
    predicate: Option<&Expr>,
    key_expr: &Expr,
    value_expr: &Expr,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    match (eval(graph, row, source, params)?, key_ident) {
        (Value::Map(m), Some(k)) => retain_projected_map(
            ctx,
            Some(k),
            value_ident,
            predicate,
            key_expr,
            value_expr,
            Arc::unwrap_or_clone(m)
                .into_iter()
                .map(|(k, v)| (Some(Value::String(k.into())), v)),
        ),
        (Value::List(xs), None) => retain_projected_map(
            ctx,
            None,
            value_ident,
            predicate,
            key_expr,
            value_expr,
            into_items(xs).map(|x| (None, x)),
        ),
        (Value::Null, _) => Ok(Value::Null),
        (Value::Map(_), None) => Err(Error::ty(
            "map comprehension with a list binding needs a LIST source",
        )),
        (Value::List(_), Some(_)) => Err(Error::ty(
            "map comprehension with key/value bindings needs a MAP source",
        )),
        _ => Err(Error::ty("map comprehension needs a MAP or LIST source")),
    }
}

/// The map comprehension's retaining-projection loop — the map twin of
/// [`retain_projected`]: bind (key, value) — or just (item), with no key
/// binding, for a list source — per element, filter with the optional
/// WHERE (the comprehension WHERE gate,
/// [`comprehension_where_holds`]), and project `keyExpr : valueExpr`
/// into a map. A non-string key is a type error; duplicate keys take the
/// last write (BTreeMap insert). Generic over the bindings iterator so
/// the map-source and list-source arms each keep their concrete iterator
/// type.
fn retain_projected_map(
    ctx: EvalCtx<'_>,
    key_ident: Option<&SmallName>,
    value_ident: &SmallName,
    predicate: Option<&Expr>,
    key_expr: &Expr,
    value_expr: &Expr,
    bindings: impl ExactSizeIterator<Item = (Option<Value>, Value)>,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    // ADR-0463: a BTreeMap is not a Vec, so seed the container with the map
    // entry-storage figure (not the value-slot model), then add each key's
    // bytes and payload as it is produced. The bindings iterator is
    // exact-size, so its len is the source bound.
    let mut retained = graph.retain_bytes(crate::workspace::map_entries_bytes(bindings.len()))?;
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in bindings {
        graph.cancel_tick()?;
        let mut r = row.clone();
        if let (Some(kvar), Some(kv)) = (key_ident, k) {
            r.insert(kvar.to_string(), kv);
        }
        r.insert(value_ident.to_string(), v);
        if !comprehension_where_holds(graph, &r, predicate, params)? {
            continue;
        }
        let key_v = eval(graph, &r, key_expr, params)?;
        let key = match key_v {
            Value::String(s) => String::from(&*s),
            // CIP-272: a map key must be a non-null string. Live Neo4j
            // 2026.10.1 rejects a null key (22G03/22N01), it does not
            // drop the entry.
            Value::Null => {
                return Err(Error::ty(
                    "map comprehension key expression must not be NULL",
                ))
            }
            _ => {
                return Err(Error::ty(
                    "map comprehension key expression must be a STRING",
                ))
            }
        };
        let value_v = eval(graph, &r, value_expr, params)?;
        // The key is a new String allocation, the map entry is new node
        // storage, and the container retains the value's payload: the
        // running retained total of this one growing map is what is enforced.
        retained.add_bytes(
            graph,
            relate_core::str_logical_bytes(&key)
                .saturating_add(crate::workspace::owned_heap(&value_v)),
        )?;
        out.insert(key, value_v);
    }
    Value::checked_map(out)
}

/// A map projection `n { .k, v: expr, name, .* }`: evaluate the target,
/// then build the output map item by item. A null target is null (CIP-284),
/// a deleted entity is rejected once for the whole projection, and an
/// `.*` reads a bound entity's properties from its store entry (CIP-257)
/// — the same policies the per-item reads apply, stated here once so the
/// projection answers as one unit.
fn eval_map_projection(
    ctx: EvalCtx<'_>,
    expr: &Expr,
    items: &[MapProjectionItem],
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    let target = eval(graph, row, expr, params)?;
    // A map projection over a null entity is null (CIP-284): the
    // projection has no properties to project, and returning the
    // `meta`-only map instead made `collect(n {.*, meta: …})` over
    // an `OPTIONAL MATCH` miss collect a row instead of nothing.
    if target.is_null() {
        return Ok(Value::Null);
    }
    // The one deleted-entity admission for the whole projection: the
    // `Property` arm below reads the same target through the same
    // policy, so it must be rejected once here rather than per item —
    // otherwise `n{.*}` silently projects `{}` while `n.p` raises
    // DeletedEntityAccess (one projection, two answers).
    reject_deleted_access(graph, &target)?;
    // The projection's output is a runtime value map: its keys are
    // the value domain (String), rendered once per entry here.
    let mut map: BTreeMap<String, Value> = BTreeMap::new();
    for item in items {
        match item {
            MapProjectionItem::KeyValue { key, value } => {
                map.insert(key.to_string(), eval(graph, row, value, params)?);
            }
            MapProjectionItem::Property { key } => {
                let value = prop(graph, &target, &(key.clone(), Token::Unresolved))?;
                map.insert(key.to_string(), value);
            }
            MapProjectionItem::Variable { name } => {
                // ADR-0335: same policy as `Expr::Variable` — an
                // unbound name is a typed error, never `null`.
                let value = row
                    .get_name(name)
                    .cloned()
                    .ok_or_else(|| name.with_str(Error::undefined_variable))?;
                map.insert(name.to_string(), value);
            }
            MapProjectionItem::AllProperties => match &target {
                Value::Map(properties) => {
                    map.extend(properties.iter().map(|(k, v)| (k.clone(), v.clone())))
                }
                // A bound entity carries its labels but not its
                // properties: the store entry is the read's source
                // (CIP-257) — the one store-backed resolver
                // `properties()` and `keys()` ride (the caller's
                // earlier deleted-entity rejection is idempotent
                // with the resolver's own).
                value @ (Value::Node(_) | Value::Relationship(_)) => {
                    let properties = entity_backing_map(graph, value, "`{.*}` map projection")?;
                    map.extend(
                        properties
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone())),
                    );
                }
                _ => {}
            },
        }
    }
    Value::checked_map(map)
}
