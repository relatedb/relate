//! Element-by-element consumption of an evaluated list (ADR-0170), the
//! lazy element sources a generator clause pulls (REL8-1372), and the
//! comprehension protocol's shared per-element steps (bind, gate,
//! project): the element-walk kit, the pull form's types, and the steps
//! both comprehension forms route through live together here — the eval
//! layer owns them all, so no clause executor re-spells comprehension
//! semantics. The comprehension value forms live in `comprehension` and
//! import this module; the edge is one way.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Result, Value};
use relate_ir::Expr;
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::eval;
use super::function::{lazy_range, RangeValues};
use super::plain_where_holds;

/// The comprehension protocol's bind step: one source element bound to
/// `ident` in a clone of the input row — shared by the value form's row
/// loop (`eval_list_comp`) and the `UNWIND` pull form
/// (`CompElements::next`), so the element protocol covers
/// bind → gate → project, not gate → project alone.
pub(in crate::eval) fn bind_element(row: &Row, ident: &SmallName, item: Value) -> Row {
    let mut bound = row.clone();
    bound.insert(ident.to_string(), item);
    bound
}

/// The comprehension's one gate-and-project step over a bound row: sample
/// `cancel_tick` (the skipped rows included, so a predicate that rejects
/// everything stays cancellable), apply the optional WHERE (the
/// comprehension WHERE gate, [`plain_where_holds`]) and project —
/// `Ok(None)` when the row is filtered out. Both comprehension forms route
/// through it: the value form's collector loop (`retain_projected`) and
/// the `UNWIND` pull form (`CompElements::next`), so the two cannot drift.
pub(in crate::eval) fn comprehension_element(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    row: &Row,
    predicate: Option<&Expr>,
    projection: &Expr,
) -> Result<Option<Value>> {
    graph.cancel_tick()?;
    if !plain_where_holds(graph, row, predicate, params)? {
        return Ok(None);
    }
    eval(graph, row, projection, params).map(Some)
}

/// The one list-argument triage, shared by every form that iterates an
/// evaluated list argument — the four quantifier spellings, `reduce()`,
/// `allReduce()` and the list comprehension: the evaluated source's
/// elements, `None` for a null source (the whole form is null), or the
/// typed shape error naming `what`. One spelling, so null handling and
/// the shape rejection cannot drift between the forms. Lives here, beside
/// the element walk every consumer pairs it with.
pub(in crate::eval) fn list_argument(value: Value, what: &str) -> Result<Option<Arc<Vec<Value>>>> {
    match value {
        Value::List(items) => Ok(Some(items)),
        Value::Null => Ok(None),
        _ => Err(relate_core::Error::ty(what)),
    }
}

/// The comprehension source requirement — the `what` text every
/// comprehension form (the value loop and the `UNWIND` pull form) names
/// the shape error with, so the authored message has one home.
pub(in crate::eval) const COMPREHENSION_SOURCE: &str = "list comprehension needs a list";

/// Consume an evaluated list element by element. A uniquely owned list
/// (freshly built, e.g. `nodes(p)` or a literal) moves its elements out; a
/// shared one (still bound in a row, a parameter, …) is left intact and each
/// element is cloned only when `next()` reaches it, so a consumer that stops
/// early clones just the elements it visited.
pub(crate) fn into_items(xs: Arc<Vec<Value>>) -> Items {
    match Arc::try_unwrap(xs) {
        Ok(owned) => Items::Owned(owned.into_iter()),
        Err(shared) => Items::Shared { xs: shared, at: 0 },
    }
}

/// The elements a `FOREACH`-style iteration visits for one evaluated
/// source: a list's elements, no element for `null`, and any other scalar as
/// its own single element.
pub(crate) fn iterated(value: Value) -> Items {
    match value {
        Value::List(xs) => into_items(xs),
        Value::Null => Items::Owned(Vec::new().into_iter()),
        single => Items::Owned(vec![single].into_iter()),
    }
}

/// The element walk [`into_items`] returns, named so a consumer can hold it
/// across calls (the `UNWIND` row stream).
pub(crate) enum Items {
    Owned(std::vec::IntoIter<Value>),
    Shared { xs: Arc<Vec<Value>>, at: usize },
}

impl Iterator for Items {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self {
            Items::Owned(items) => items.next(),
            Items::Shared { xs, at } => {
                let item = xs.get(*at)?.clone();
                *at += 1;
                Some(item)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = match self {
            Items::Owned(items) => items.len(),
            Items::Shared { xs, at } => xs.len() - *at,
        };
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Items {}

/// The elements one input row's generator source yields, walked one at a
/// time (REL8-1372): a `range(...)` call is a counter (no list is ever
/// built), an evaluated list moves or clones its elements as the walk
/// reaches them, a list comprehension binds, filters and projects per
/// element (the projected list is never built), and a non-list scalar is
/// its own single element.
pub(crate) enum Elements<'a> {
    Range(RangeValues),
    List(Items),
    /// A null source: no elements, ever — the state the comprehension's
    /// null verdict and `UNWIND`'s no-rows answer read, named apart from
    /// the scalar element so the two cannot be confused.
    Empty,
    /// A non-list scalar as its own single element. The `Option` is the
    /// once-taken marker, never a null source — that is [`Elements::Empty`].
    One(Option<Value>),
    Comp(Box<CompElements<'a>>),
}

/// One list-comprehension source `[x IN source WHERE … | projection]`
/// pulled element by element: the source is itself an [`Elements`] (a
/// `range()` counter, or an evaluated list walked without a second
/// copy), and each pulled element is bound to `ident` in a clone of the
/// input row, gated by the comprehension `WHERE`
/// ([`plain_where_holds`]) and projected on demand. The
/// projected output list the comprehension's value form builds
/// (`eval_list_comp` → `retain_projected`) never exists here, so an
/// aggregating consumer's memory follows its groups, not the list length.
pub(crate) struct CompElements<'a> {
    source: Elements<'a>,
    ident: &'a SmallName,
    predicate: Option<&'a Expr>,
    projection: &'a Expr,
}

impl Elements<'_> {
    /// The charge bound — elements not yet yielded. `Range`/`List`/`One`
    /// answer the exact count; `Comp` answers its source's remaining
    /// count, the unfiltered upper bound (its `WHERE` may drop elements),
    /// the same figure the value form's `retain_slots(bound)` charges
    /// before its own `WHERE` runs. Read by the feed spine's materializing
    /// form alone; a streamed consumer never counts what it will not bill.
    pub(crate) fn pending_bound(&self) -> Result<usize> {
        match self {
            Elements::Range(range) => range.remaining(),
            Elements::List(items) => Ok(items.len()),
            Elements::Empty => Ok(0),
            Elements::One(value) => Ok(usize::from(value.is_some())),
            Elements::Comp(comp) => comp.source.pending_bound(),
        }
    }

    /// The next element, `Ok(None)` once spent. `ctx` is the per-pull
    /// context and `row` the input row the comprehension arm binds
    /// against; the three stored forms read neither.
    pub(crate) fn next(&mut self, ctx: ExecCtx<'_>, row: &Row) -> Result<Option<Value>> {
        match self {
            Elements::Range(range) => Ok(range.next().transpose()?),
            Elements::List(items) => Ok(items.next()),
            Elements::Empty => Ok(None),
            Elements::One(value) => Ok(value.take()),
            Elements::Comp(comp) => comp.next(ctx, row),
        }
    }

    /// The comprehension-source verdict — the pull form's one triage of an
    /// opened source, the twin of [`list_argument`]'s value triage: a
    /// list-shaped source (counter, list, nested comprehension) streams as
    /// itself, a null source answers no elements, and any other scalar is
    /// the typed [`COMPREHENSION_SOURCE`] error (a `One` is only ever a
    /// non-list, non-null scalar — the fallback arm of [`open_source`]
    /// builds it).
    fn comprehension_source(self) -> Result<Option<Self>> {
        match self {
            Elements::Empty => Ok(None),
            Elements::One(_) => Err(relate_core::Error::ty(COMPREHENSION_SOURCE)),
            source => Ok(Some(source)),
        }
    }
}

impl CompElements<'_> {
    /// Pull source elements until one passes the comprehension's `WHERE`
    /// and project it — every iteration samples `cancel_tick`, the
    /// skipped iterations included, so a predicate that rejects everything
    /// stays cancellable. The row is the caller's (the feed spine holds
    /// the input row); each bound element works on a fresh clone, the
    /// same per-element cost the value form pays.
    fn next(&mut self, ctx: ExecCtx<'_>, row: &Row) -> Result<Option<Value>> {
        let ExecCtx { graph, params, .. } = ctx;
        while let Some(item) = self.source.next(ctx, row)? {
            let bound = bind_element(row, self.ident, item);
            if let Some(value) =
                comprehension_element(graph, params, &bound, self.predicate, self.projection)?
            {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }
}

/// Open `expression` as a lazily-walked element source for one input row
/// (REL8-1372): `range(...)` becomes a counter, a list comprehension
/// becomes a per-element pull whose source opens recursively (a nested
/// comprehension stays lazy both levels), everything else is evaluated
/// once — a list is then walked, never copied wholesale, a null yields
/// nothing, and any other scalar is its own single element. A
/// pattern-comprehension source still evaluates to its whole list: the
/// eval walk is not resumable, so no pull form exists for it yet.
pub(crate) fn open_source<'a>(
    graph: &ExecGraph<'_>,
    row: &Row,
    expression: &'a Expr,
    params: &BTreeMap<String, Value>,
) -> Result<Elements<'a>> {
    if let Expr::Function { name, args, .. } = expression {
        let plan = name.with_str(|text| lazy_range(graph, row, params, text, args))?;
        if let Some(plan) = plan {
            return Ok(Elements::Range(plan.values()));
        }
    }
    if let Expr::ListComp {
        ident,
        list,
        predicate,
        projection,
    } = expression
    {
        let source = open_source(graph, row, list, params)?;
        // The one source verdict: null → no elements, scalar → the typed
        // error, list-shaped → the per-element pull.
        return Ok(match source.comprehension_source()? {
            Some(source) => Elements::Comp(Box::new(CompElements {
                source,
                ident,
                predicate: predicate.as_deref(),
                projection,
            })),
            None => Elements::Empty,
        });
    }
    Ok(match eval(graph, row, expression, params)? {
        Value::List(xs) => Elements::List(into_items(xs)),
        // A null source yields no rows; any other scalar is its own
        // single element.
        Value::Null => Elements::Empty,
        other => Elements::One(Some(other)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Elements whose payload is itself an `Arc`, so a move and a clone are
    /// told apart by the payload's strong count.
    fn nested(n: i64) -> Vec<Value> {
        (0..n)
            .map(|i| Value::List(vec![Value::Int(i)].into()))
            .collect()
    }

    fn payload(v: &Value) -> &Arc<Vec<Value>> {
        let Value::List(inner) = v else {
            panic!("expected a list element, got {v:?}");
        };
        inner
    }

    #[test]
    fn unique_list_moves_its_elements_out() {
        let elements = nested(4);
        let originals: Vec<Arc<Vec<Value>>> =
            elements.iter().map(|v| Arc::clone(payload(v))).collect();
        let mut items = into_items(Arc::new(elements));
        assert_eq!(items.len(), 4);
        for (i, original) in originals.iter().enumerate() {
            let yielded = items.next().expect("element");
            let yielded = payload(&yielded);
            assert!(Arc::ptr_eq(yielded, original));
            // Only `originals` and the yielded value hold the payload: the
            // element left the list instead of being copied out of it.
            assert_eq!(Arc::strong_count(yielded), 2);
            assert_eq!(items.len(), 3 - i);
        }
        assert!(items.next().is_none());
    }

    #[test]
    fn shared_list_clones_only_the_visited_elements() {
        let xs = Arc::new(nested(5));
        let held = Arc::clone(&xs);
        let mut items = into_items(xs);
        assert_eq!(items.len(), 5);
        let taken: Vec<Value> = items.by_ref().take(2).collect();
        assert_eq!(taken, held[..2]);
        assert_eq!(items.len(), 3);
        for (i, element) in held.iter().enumerate() {
            let expected = if i < 2 { 2 } else { 1 };
            assert_eq!(Arc::strong_count(payload(element)), expected, "element {i}");
        }
        drop(items);
        drop(taken);
        assert_eq!(Arc::strong_count(&held), 1);
        assert_eq!(*held, nested(5));
    }

    #[test]
    fn shared_list_yields_every_element_in_order() {
        let xs: Arc<Vec<Value>> = Arc::new((0..3).map(Value::Int).collect());
        let held = Arc::clone(&xs);
        let all: Vec<Value> = into_items(xs).collect();
        assert_eq!(all, *held);
    }
}
