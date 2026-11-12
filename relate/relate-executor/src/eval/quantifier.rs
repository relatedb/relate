//! List quantifiers (`all`/`any`/`none`/`single`) and reduce expressions.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, QuantifierKind};
use relate_names::SmallName;

use crate::ctx::EvalCtx;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::{bool_or_null, eval, fold_quantifier, into_items};

/// The one list-argument triage, shared by every form that iterates a
/// evaluated list argument — the four quantifier spellings, `reduce()`,
/// `allReduce()` and the list comprehension: the evaluated source's
/// elements, `None` for a null source (the whole form is null), or the
/// typed shape error naming `what`. One spelling, so null handling and
/// the shape rejection cannot drift between the forms.
pub(super) fn list_argument(value: Value, what: &str) -> Result<Option<Arc<Vec<Value>>>> {
    match value {
        Value::List(items) => Ok(Some(items)),
        Value::Null => Ok(None),
        _ => Err(Error::ty(what)),
    }
}

/// Whether `expression` is exactly `[<accumulator>]` — the canonical wrap
/// whose container depth grows by exactly one per iteration. The reduce
/// loops track that depth incrementally and build the list through
/// [`Value::checked_list_with_depth`], so they never re-walk the
/// accumulator (ADR-0462). Any other shape uses the general evaluator,
/// whose checked constructors enforce the cap.
fn wraps_accumulator(expression: &Expr, accumulator: &SmallName) -> bool {
    matches!(
        expression,
        Expr::List(elems)
            if matches!(elems.as_slice(), [Expr::Variable(name)] if name == accumulator)
    )
}

/// One iteration of the canonical wrap (ADR-0462): build `[acc]` through the
/// known-child-depth constructor and bump the tracked depth. The caller
/// keeps the loop's row bindings; only the expression evaluation is skipped.
fn wrap_acc_once(acc: Value, acc_depth: &mut u32) -> Result<Value> {
    let child_depth = *acc_depth;
    *acc_depth = child_depth.saturating_add(1);
    Value::checked_list_with_depth(vec![acc], child_depth)
}

/// The accumulator's top-level allocation: the container the reduce loop just
/// built — a list/vector's value-buffer slots, a map's entry storage, a
/// string's header plus bytes, a bytes payload. Deliberately **not** a deep
/// `logical_bytes` walk: shared children in a fan-out DAG (`[acc, acc]`) are
/// not re-allocated, and ADR-0462's memoized depth walk exists so a reduce
/// never expands the DAG. This is the size the per-allocation ADR-0463 cap
/// checks; the children were charged where they were built.
fn accumulator_allocation_bytes(value: &Value) -> u64 {
    use crate::workspace::buffer_bytes;
    match value {
        Value::List(xs) | Value::Vector { values: xs, .. } => {
            buffer_bytes(xs.len(), std::mem::size_of::<Value>() as u64)
        }
        Value::Map(entries) => crate::workspace::map_entries_bytes(entries.len()),
        Value::String(text) => relate_core::str_logical_bytes(text),
        Value::Bytes(bytes) => relate_core::bytes_logical_bytes(bytes),
        _ => 0,
    }
}

/// The reduce loops' one growth check (ADR-0463): when the accumulator's new
/// top-level container is larger than the previous one, the one allocation
/// it made is checked against the per-allocation cap and recorded as
/// construction-work telemetry. Never a running statement total, and never a
/// deep walk (see [`accumulator_allocation_bytes`], which keeps ADR-0462's
/// fan-out DAG linear).
fn charge_growth(graph: &crate::overlay::ExecGraph<'_>, before: u64, next: &Value) -> Result<()> {
    let after = accumulator_allocation_bytes(next);
    if after > before {
        graph.charge_expression(after)?;
    }
    Ok(())
}

pub(crate) fn eval_quantifier(
    ctx: EvalCtx<'_>,
    kind: QuantifierKind,
    ident: &SmallName,
    list: &Expr,
    predicate: &Expr,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    let Some(items) = list_argument(eval(graph, row, list, params)?, "quantifier needs a list")?
    else {
        return Ok(Value::Null);
    };
    fold_quantifier(kind, into_items(items), |item| {
        let mut r = row.clone();
        r.insert(ident.to_string(), item);
        let p = eval(graph, &r, predicate, params)?;
        bool_or_null(&p, "quantifier WHERE")
    })
}

pub(crate) fn eval_reduce(
    ctx: EvalCtx<'_>,
    accumulator: &SmallName,
    initial: &Expr,
    ident: &SmallName,
    list: &Expr,
    expression: &Expr,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    let acc = eval(graph, row, initial, params)?;
    let Some(items) = list_argument(eval(graph, row, list, params)?, "reduce() requires a list")?
    else {
        return Ok(Value::Null);
    };
    // The reduce loop drives the same fold the allReduce step does: the
    // accumulator advance — bindings, update, growth charge — is one
    // implementation ([`AllReduceFold::advance`]).
    let mut fold = AllReduceFold::start(acc, accumulator, ident, expression);
    let mut inner = row.clone();
    for item in into_items(items) {
        fold.advance(graph, &mut inner, &item, params)?;
    }
    Ok(fold.acc)
}

pub(crate) fn eval_all_reduce(
    ctx: EvalCtx<'_>,
    accumulator: &SmallName,
    initial: &Expr,
    ident: &SmallName,
    list: &Expr,
    expression: &Expr,
    predicate: &Expr,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    let acc = eval(graph, row, initial, params)?;
    let Some(items) = list_argument(
        eval(graph, row, list, params)?,
        "allReduce() requires a list",
    )?
    else {
        return Ok(Value::Null);
    };
    let mut fold = AllReduceFold::start(acc, accumulator, ident, expression);
    let mut saw_null = false;
    let mut inner = row.clone();
    for item in into_items(items) {
        match fold.step(graph, &mut inner, &item, predicate, params)? {
            // The deciding element ends the scan: the `1 / 0` element after
            // it is never evaluated (pinned by the quantifier tests below).
            AllReduceStep::Failed => return Ok(Value::Bool(false)),
            AllReduceStep::Null => saw_null = true,
            AllReduceStep::Holds => {}
        }
    }
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(true)
    })
}

/// The running state of one accumulator fold: the accumulator value, the
/// integer-start invariant, ADR-0462's incremental depth tracking, and the
/// per-call invariants every advance re-reads — the update expression and
/// both rendered names (the row schema's names are String, so the two IR
/// names render once, at start) — carried by every driver of the
/// accumulator advance: the reduce loop ([`eval_reduce`]), the list
/// evaluator above ([`eval_all_reduce`]), and the QPP expansion's
/// mid-path inliner (`exec/gpm/qpp.rs`) — so the advance itself exists
/// exactly once and takes only per-element facts.
#[derive(Clone)]
pub(crate) struct AllReduceFold<'e> {
    acc: Value,
    started_as_integer: bool,
    acc_depth: u32,
    wrap_acc: bool,
    acc_name: String,
    ident_name: String,
    expression: &'e Expr,
}

/// One element's verdict, as the fold's driver consumes it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllReduceStep {
    /// The predicate holds on the updated prefix — keep scanning.
    Holds,
    /// The predicate is false on the updated prefix: `allReduce` is false,
    /// and no extension of the list can change that.
    Failed,
    /// The predicate is null on the updated prefix: `allReduce` can no
    /// longer be true (null at best, false at worst).
    Null,
}

impl<'e> AllReduceFold<'e> {
    /// Start from the evaluated `initial`, owning the fold's per-call
    /// invariants: the wrap detection runs once here, not per element,
    /// and the accumulator and loop-variable names render once here —
    /// the per-element row inserts inside the advance are text-keyed (the
    /// row schema's names are String).
    pub(crate) fn start(
        acc: Value,
        accumulator: &SmallName,
        ident: &SmallName,
        expression: &'e Expr,
    ) -> Self {
        Self {
            started_as_integer: matches!(acc, Value::Int(_)),
            acc_depth: acc.depth(),
            wrap_acc: wraps_accumulator(expression, accumulator),
            acc_name: accumulator.to_string(),
            ident_name: ident.to_string(),
            acc,
            expression,
        }
    }

    /// Advance the accumulator one element: bind the pre-update accumulator
    /// and the loop variable over `inner` (the caller's reusable clone of
    /// its base row), evaluate the update, charge the growth, and bind the
    /// updated accumulator — the binding the allReduce predicate reads, and
    /// the same value the reduce loop's next iteration re-binds. The one
    /// implementation of the advance, driven by both [`eval_reduce`] and
    /// [`Self::step`]. Errors (a raising update) propagate to the driver,
    /// which decides how a partial scan's error is surfaced.
    fn advance(
        &mut self,
        graph: &ExecGraph<'_>,
        inner: &mut Row,
        element: &Value,
        params: &BTreeMap<String, Value>,
    ) -> Result<()> {
        // Bind before the expression (the step's predicate reads these
        // bindings): the wrap fast path skips only the expression
        // evaluation. Pre-update accumulator + loop variable are visible
        // to the expression; the updated accumulator is bound before the
        // predicate.
        // ADR-0463: sample cancellation and charge the accumulator's growth;
        // the fold owns the accumulator, so the delta is measured here.
        graph.cancel_tick()?;
        let before = accumulator_allocation_bytes(&self.acc);
        inner.insert(self.acc_name.clone(), self.acc.clone());
        inner.insert(self.ident_name.clone(), element.clone());
        let next = if self.wrap_acc {
            // ADR-0462: the canonical wrap grows one container level per
            // element; the depth is tracked incrementally and the list is
            // built with the known-child-depth constructor.
            let acc = std::mem::replace(&mut self.acc, Value::Null);
            wrap_acc_once(acc, &mut self.acc_depth)?
        } else {
            eval(graph, inner, self.expression, params)?
        };
        charge_growth(graph, before, &next)?;
        self.acc = next;
        inner.insert(self.acc_name.clone(), self.acc.clone());
        Ok(())
    }

    /// Advance one element ([`Self::advance`]), then evaluate the predicate
    /// on the updated prefix. The integer-start invariant is the allReduce
    /// scan's own — the reduce loop widens freely. Errors (a raising update
    /// or predicate) propagate to the driver, which decides how a partial
    /// scan's error is surfaced.
    pub(crate) fn step(
        &mut self,
        graph: &ExecGraph<'_>,
        inner: &mut Row,
        element: &Value,
        predicate: &Expr,
        params: &BTreeMap<String, Value>,
    ) -> Result<AllReduceStep> {
        self.advance(graph, inner, element, params)?;
        if self.started_as_integer && matches!(self.acc, Value::Float(_)) {
            return Err(Error::ty(
                "Invalid input 'FLOAT' for accumulator. Expected to be INTEGER.",
            ));
        }
        // A non-finite accumulator is a legitimate value: float division by
        // zero yields ±Infinity / NaN in Neo4j (`1.0 / 0` never raises; only
        // the all-integer `/` raises DivisionByZero, inside `div` itself).
        match eval(graph, inner, predicate, params)? {
            Value::Bool(false) => Ok(AllReduceStep::Failed),
            Value::Bool(true) => Ok(AllReduceStep::Holds),
            Value::Null => Ok(AllReduceStep::Null),
            _ => Err(Error::ty("allReduce() predicate must be boolean")),
        }
    }
}

#[cfg(test)]
mod tests {
    use relate_core::Value;

    use crate::exec::test_support::exec_cypher;
    use crate::graph::Graph;

    fn single_value(cypher: &str) -> Value {
        let rows = exec_cypher(&Graph::default(), cypher).rows;
        assert_eq!(rows.len(), 1, "{cypher}: {rows:?}");
        rows[0][0].clone()
    }

    /// The deciding element ends the scan: the `1 / 0` element after it is
    /// never evaluated, and the list bound in the row stays readable.
    #[test]
    fn quantifiers_stop_at_the_deciding_element() {
        for (cypher, expected) in [
            (
                "WITH [1, 0] AS xs RETURN any(x IN xs WHERE 1 / x = 1)",
                true,
            ),
            (
                "WITH [1, 0] AS xs RETURN all(x IN xs WHERE 1 / x = 2)",
                false,
            ),
            (
                "WITH [1, 0] AS xs RETURN none(x IN xs WHERE 1 / x = 1)",
                false,
            ),
            (
                "WITH [1, 1, 0] AS xs RETURN single(x IN xs WHERE 1 / x = 1)",
                false,
            ),
            (
                "WITH [1, 0] AS xs \
                 RETURN allReduce(acc = 0, x IN xs | acc + 1 / x, acc > 5)",
                false,
            ),
        ] {
            assert_eq!(single_value(cypher), Value::Bool(expected), "{cypher}");
        }
        let rows = exec_cypher(
            &Graph::default(),
            "WITH [1, 2, 3] AS xs RETURN any(x IN xs WHERE x = 1) AS hit, xs",
        )
        .rows;
        assert_eq!(rows[0][0], Value::Bool(true));
        assert_eq!(
            rows[0][1],
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
        );
    }

    /// A full scan still sees every element in order, including nulls.
    #[test]
    fn quantifiers_scan_every_element_without_a_decision() {
        for (cypher, expected) in [
            (
                "WITH [1, null, 3] AS xs RETURN any(x IN xs WHERE x > 5)",
                Value::Null,
            ),
            (
                "WITH [1, 2, 3] AS xs RETURN all(x IN xs WHERE x > 0)",
                Value::Bool(true),
            ),
            (
                "WITH [1, 2, 3] AS xs RETURN single(x IN xs WHERE x = 2)",
                Value::Bool(true),
            ),
            (
                "WITH [1, 2, 3] AS xs \
                 RETURN allReduce(acc = 0, x IN xs | acc + x, acc < 10)",
                Value::Bool(true),
            ),
        ] {
            assert_eq!(single_value(cypher), expected, "{cypher}");
        }
    }

    /// ADR-0462 (PR #547 round-3): the reduce loop tracks a wrapping
    /// accumulator's depth incrementally and builds `[acc]` through the
    /// known-child-depth constructor, so it never re-walks the accumulated
    /// value. A 100 000-row list-building reduction returns promptly (the
    /// per-iteration full-subtree walk this replaces was O(N²)); the wrap fast
    /// path keeps the exact value and still enforces the cap.
    #[test]
    fn hundred_thousand_row_list_building_reduce_completes() {
        assert_eq!(
            single_value("RETURN reduce(acc = 0, x IN range(1, 100000) | acc + size([x])) AS v"),
            Value::Int(100000)
        );

        // The canonical wrap: one level per iteration, exact value.
        let wrapped = single_value("RETURN reduce(acc = [], x IN range(1, 3) | [acc]) AS v");
        assert_eq!(wrapped.depth(), 4);
    }

    /// Regression (PR #547 round-4, F-4-1/F-4-3): the wrap fast path may skip
    /// only the expression evaluation — `allReduce`'s predicate must still see
    /// the loop variable and the updated accumulator. `[acc]` takes the fast
    /// path; `[acc] + []` is the same value through the general evaluator, so
    /// the pair pins identical answers.
    #[test]
    fn wrap_fast_path_keeps_the_loop_bindings_for_the_predicate() {
        for (cypher, expected) in [
            // The predicate reads the loop variable.
            (
                "RETURN allReduce(acc = [], x IN [1, 2, 3] | [acc], x > 1) AS v",
                Value::Bool(false),
            ),
            (
                "RETURN allReduce(acc = [], x IN [1, 2, 3] | [acc] + [], x > 1) AS v",
                Value::Bool(false),
            ),
            (
                "RETURN allReduce(acc = [], x IN [1, 2, 3] | [acc], x > 0) AS v",
                Value::Bool(true),
            ),
            (
                "RETURN allReduce(acc = [], x IN [1, 2, 3] | [acc] + [], x > 0) AS v",
                Value::Bool(true),
            ),
            // The predicate reads the updated accumulator (`[[]]` after one
            // wrap).
            (
                "RETURN allReduce(acc = [], x IN [1, 2] | [acc], size(acc) = 1) AS v",
                Value::Bool(true),
            ),
            (
                "RETURN allReduce(acc = [], x IN [1, 2] | [acc] + [], size(acc) = 1) AS v",
                Value::Bool(true),
            ),
        ] {
            assert_eq!(single_value(cypher), expected, "{cypher}");
        }
    }

    /// ADR-0462 security regression (PR #547 round-5): a fan-out accumulator
    /// (`[acc, acc]`) shares its payloads, so the unmemoized depth walk would
    /// expand 2^40 nodes and never finish. The memoized walk is linear in
    /// distinct payloads; the query returns promptly with the exact depth.
    #[test]
    fn shared_fan_out_reduce_does_not_expand_exponentially() {
        let value = single_value(
            "RETURN reduce(acc = range(1, 10000), x IN range(1, 40) | [acc, acc]) AS v",
        );
        assert_eq!(value.depth(), 41);
    }
}
