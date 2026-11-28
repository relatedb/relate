//! Expression evaluation: the `eval` dispatcher and the `eval/`
//! submodule wiring.

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, MapProjectionItem};
use relate_names::SmallName;

use crate::ctx::EvalCtx;
// The pattern-expression arms (PatternPred, ShortestPath)
// and the subquery arms below them delegate UP to engines: the walk
// engine at `crate::eval_walk` (a pattern-walk client like the exec
// walkers, hoisted out of this tree so `eval` stays the expression core
// strictly below `pattern_walk`, importing no walk module) and `exec`
// (the long-established engine-delegation edge). The cross-engine
// pattern policy that walker reads lives at the shared tier
// (`crate::pattern_walk`, LABS-811) — one public path per item, no
// re-export here.
use crate::eval_walk::pattern_exists;
use crate::exec::{eval_shortest, ShortestWant};
use crate::overlay::ExecGraph;
use crate::row::{row_bound, Row};

mod aggregate;
mod arithmetic;
pub(crate) mod bytecode;
mod collection;
mod comprehension;
mod expr;
use comprehension::{eval_list_comp, eval_map_comp};
mod fold;
mod function;
mod items;
mod labels;
mod membership_index;
mod numeric;
mod path_value;
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
pub(crate) use fold::{extreme_offer, fold_extreme, fold_quantifier, Extreme};
pub(crate) use items::{into_items, iterated, open_source, Elements};
pub(crate) use labels::{
    dynamic_label_names, dynamic_property_key, label_expr_matches, resolve_write_labels,
};
pub(crate) use membership_index::member_of;
// The expression walkers live in `relate-ir` next to `Expr::children`
// so planner and executor share one traversal.
pub(crate) use relate_ir::{
    all_vars_satisfy, expr_vars, is_agg_expr, key_of, label_expr_vars, orient, var_key_side,
    var_prop,
};

pub(crate) use expr::{
    binary, binary_pure, cmp_bool, eval_case_with, in_op, in_rhs, is_membership_scalar,
    ordering_operator, OrderingSide,
};
use expr::{eval_case, prop, reject_deleted_access, slice, subscript, unary};
/// Whether every variable `expr` reads is in `allowed`.
pub(crate) fn expr_within(expr: &Expr, allowed: &HashSet<String>) -> bool {
    all_vars_satisfy(expr, |name| allowed.contains(name))
}

/// The one stability fact every evaluate-early policy reads: no
/// nondeterministic call and no scope-bearing form
/// ([`is_row_unstable_node`] is the node classifier).
pub(crate) fn row_stable(expr: &Expr) -> bool {
    !expr.any(&mut is_row_unstable_node)
}

/// Whether `expr` may evaluate on `row` as a seek recognizer's bound
/// side: every variable it reads is already bound in `row`
/// ([`all_vars_satisfy`] against the row), and the expression is
/// [`row_stable`] — a nondeterministic call differs per row, and a
/// scope-bearing form such as a subquery reads its own scope. The seek
/// recognizers evaluate the bound side against the *entry* row, before
/// the pattern's own variable and any later pattern's variables are
/// bound; a free-variable eval there does not answer null the way an
/// unbound property read does — a subquery runs its body with the name
/// free (`EXISTS` matches anything, `COUNT` counts the whole side) and
/// hands the recognizer a wrong constant to serve as the seek. A not-ready
/// bound declines the seek; the residual re-applies the predicate per
/// candidate row, which is the same answer, correctly reached.
pub(crate) fn bound_ready(expr: &Expr, row: &Row) -> bool {
    all_vars_satisfy(expr, row_bound(row)) && row_stable(expr)
}

/// The seek recognizers' one bound-side evaluation: [`bound_ready`] first,
/// then the eval — the single site that pairs the proof with the eval, so
/// a recognizer cannot evaluate a bound the proof has not cleared (the
/// wrong-constant-seek regression this pairing exists to prevent has no
/// compile-time signal — only this one spelling to review). A not-ready
/// bound, or an eval that errors or yields nothing, answers `None`: the
/// recognizer declines, and the residual re-applies the predicate per
/// candidate row.
pub(crate) fn eval_bound(
    expr: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Option<Value> {
    if !bound_ready(expr, row) {
        return None;
    }
    eval(graph, row, expr, params).ok()
}

pub(crate) use function::is_row_unstable_node;
use function::{entity_backing_map, function};
pub(crate) use predicates::plain_where_holds;
#[cfg(test)]
pub(crate) use predicates::take_candidate_passes;
pub(crate) use predicates::{
    candidate_passes, filter_rows_where, filter_rows_where_clause, inline_predicates_ok,
    predicate_holds, where_clause_holds,
};
use quantifier::{eval_all_reduce, eval_quantifier, eval_reduce};
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

/// The one retaining value collector (ADR-0463): charge the output
/// container's slot buffer, then land each value with its owned payload.
/// Every charged list-building site — the two comprehension projections
/// and the list literal — routes through it, so the charge protocol has
/// one spelling; the aggregate evaluator reaches it through the rebuilt
/// literal's scalar `eval` arm.
pub(crate) fn retain_values(
    graph: &ExecGraph<'_>,
    bound: usize,
    values: impl Iterator<Item = Result<Value>>,
) -> Result<Value> {
    let mut retained = graph.retain_slots(bound)?;
    let mut out = Vec::with_capacity(bound);
    for value in values {
        let value = value?;
        retained.add_owned(graph, &value)?;
        out.push(value);
    }
    Value::checked_list(out)
}

/// The one retaining map collector — the map twin of [`retain_values`]:
/// charge the map entry-storage figure, then land each entry's key bytes
/// and value payload. The map literal and the map comprehension's
/// projections route through it; the aggregate evaluator reaches it
/// through the rebuilt literal's scalar `eval` arm.
pub(crate) fn retain_map(
    graph: &ExecGraph<'_>,
    bound: usize,
    entries: impl Iterator<Item = Result<(String, Value)>>,
) -> Result<Value> {
    let mut retained = graph.retain_map_entries(bound)?;
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for entry in entries {
        let (key, value) = entry?;
        retained.add_map_entry(graph, &key, &value)?;
        out.insert(key, value);
    }
    Value::checked_map(out)
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
            prop(graph, &v, &(key.clone(), *key_id))
        }
        Expr::DynamicProperty { expr, key } => {
            let v = eval_operand(graph, row, expr, params)?;
            if v.is_null() {
                return Ok(Value::Null);
            }
            let key = dynamic_property_key(eval(graph, row, key, params)?)?;
            prop(
                graph,
                &v,
                &relate_names::unresolved(SmallName::from_name(&key)),
            )
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
            // The map literal is the list literal's twin (REL8-1372): the
            // same query-text-bounded container, collected — charged —
            // through the one map collector its comprehension forms use.
            retain_map(
                graph,
                entries.len(),
                entries
                    .iter()
                    .map(|(k, e)| eval(graph, row, e, params).map(|v| (k.clone(), v))),
            )
        }
        Expr::List(xs) => {
            // REL8-1372: the literal's own materialization is charged —
            // its length is bounded by the query-text cap, so this is a
            // bound check rather than a length guard, but an uncharged
            // build let a query-sized literal allocate hundreds of MB
            // under a small expression cap.
            retain_values(
                graph,
                xs.len(),
                xs.iter().map(|e| eval(graph, row, e, params)),
            )
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
        Expr::PathConstruct { elements } => path_value::construct(
            graph,
            elements
                .iter()
                .map(|e| eval(graph, row, e, params))
                .collect::<Result<Vec<_>>>()?,
        ),
        Expr::DistanceMetric(_) => Err(Error::semantic(
            "distance metrics are only valid as a vector_distance/vector_norm argument",
        )),
        Expr::PatternPred { path } => {
            // LABS-193/LABS-246: existence only — the anchored probe, then
            // a limit-1 walk; never a full match collection.
            Ok(Value::Bool(pattern_exists(graph, row, path, params)?))
        }
        Expr::ShortestPath { all, path } => {
            eval_shortest(graph, row, params, *all, path, ShortestWant::Path)
        }
        Expr::ExistsSubquery { query } => crate::exec::eval_exists_query(graph, row, params, query),
        Expr::CountSubquery { query } => crate::exec::eval_count_query(graph, row, params, query),
        Expr::CollectSubquery { query, path_shadow } => {
            crate::exec::eval_collect_query(graph, row, params, query, path_shadow.as_ref())
        }
    }
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
    // The whole-projection admission: `n{.*}` and a `{k: v}`-only
    // projection read no property off the target, so without this call they
    // would silently project `{}` while `n.p` raises DeletedEntityAccess
    // (one projection, two answers). The `Property` arm's read goes through
    // `prop`, which applies the same policy idempotently.
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
                let value = prop(graph, &target, &relate_names::unresolved(key.clone()))?;
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
