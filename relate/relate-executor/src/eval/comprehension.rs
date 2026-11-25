//! The comprehension family, value and pull forms together (REL8-1372):
//! the list, pattern and map comprehensions' value forms and the one
//! bind / gate / project steps both forms route through. One concept, one
//! file: the value forms live here, the pull form's types and the shared
//! per-element steps (bind, gate, project) in `items` — the element-walk
//! kit this module consumes beside the source-shape triage and the
//! `COMPREHENSION_SOURCE` requirement. The edge is one way:
//! `comprehension` imports `items`, never the reverse, so each file can
//! be read alone.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::Expr;
use relate_names::SmallName;

use crate::ctx::EvalCtx;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::eval;
use super::items::{
    bind_element, comprehension_element, into_items, list_argument, COMPREHENSION_SOURCE,
};
use super::plain_where_holds;
use super::{retain_map, retain_values};

/// The one retaining-projection loop: for each bound row apply the
/// optional WHERE (the comprehension WHERE gate,
/// [`plain_where_holds`]) and project, collecting through
/// [`retain_values`]. The rows are exact-size, so the container charge
/// derives from the source count itself — the one spelling its map twin
/// uses — and cannot be decoupled from it. Shared by the list and
/// pattern comprehensions.
fn retain_projected(
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
    rows: impl ExactSizeIterator<Item = Row>,
    predicate: Option<&Expr>,
    projection: &Expr,
) -> Result<Value> {
    let bound = rows.len();
    let values = rows
        .map(|r| comprehension_element(graph, params, &r, predicate, projection))
        .filter_map(Result::transpose);
    retain_values(graph, bound, values)
}

/// CIP list comprehension: bind one item per source element, filter with the
/// optional WHERE, project, and build one retaining output container.
pub(in crate::eval) fn eval_list_comp(
    ctx: EvalCtx<'_>,
    ident: &SmallName,
    list: &Expr,
    predicate: Option<&Expr>,
    projection: &Expr,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    let Some(items) = list_argument(eval(graph, row, list, params)?, COMPREHENSION_SOURCE)? else {
        return Ok(Value::Null);
    };
    let rows = into_items(items).map(move |item| bind_element(row, ident, item));
    retain_projected(graph, params, rows, predicate, projection)
}

/// CIP-272 map comprehension: the source-shape triage, owned beside the
/// loop it feeds. The evaluated source and the comprehension's binding
/// shape must agree — a MAP source with (key, value) bindings and a LIST
/// source with the item binding alone are the two productive pairings; a
/// null source is null; every other pairing is the typed shape error.
pub(in crate::eval) fn eval_map_comp(
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
                .map(|(key, value)| MapBinding::Entry {
                    key: Value::String(key.into()),
                    value,
                }),
        ),
        (Value::List(xs), None) => retain_projected_map(
            ctx,
            None,
            value_ident,
            predicate,
            key_expr,
            value_expr,
            into_items(xs).map(MapBinding::Item),
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

/// One map-comprehension element binding: the (key, value) pair of a MAP
/// source with the ident its key binds to, or the bare item of a LIST
/// source — one typed value instead of a pair of Options, so the
/// malformed combinations (a key under item bindings, an item under
/// key/value bindings) cannot be built, let alone silently dropped.
enum MapBinding {
    Entry { key: Value, value: Value },
    Item(Value),
}

/// The map comprehension's retaining-projection loop — the map twin of
/// [`retain_projected`]: bind each element's [`MapBinding`] — an entry's
/// key and value, or a list source's item — filter with the optional
/// `WHERE` (the comprehension WHERE gate, `plain_where_holds`),
/// and project `keyExpr : valueExpr` into a map collected through
/// [`retain_map`]. A non-string key is a type error; duplicate keys take
/// the last write (BTreeMap insert). Generic over the bindings iterator
/// so the map-source and list-source arms each keep their concrete
/// iterator type.
fn retain_projected_map(
    ctx: EvalCtx<'_>,
    key_ident: Option<&SmallName>,
    value_ident: &SmallName,
    predicate: Option<&Expr>,
    key_expr: &Expr,
    value_expr: &Expr,
    bindings: impl ExactSizeIterator<Item = MapBinding>,
) -> Result<Value> {
    let EvalCtx { graph, row, params } = ctx;
    // The bindings iterator is exact-size, so its len is the source bound —
    // read before the map below consumes it.
    let bound = bindings.len();
    let entries = bindings
        .map(|binding| {
            graph.cancel_tick()?;
            let r = match binding {
                // An `Entry` only arrives from the map-source arm, which
                // always passes its key ident — the invariant `expect`
                // states, never a silent drop.
                MapBinding::Entry { key, value } => {
                    let ident = key_ident.expect("an Entry binding carries the key's ident");
                    let mut bound = bind_element(row, ident, key);
                    bound.insert(value_ident.to_string(), value);
                    bound
                }
                MapBinding::Item(item) => bind_element(row, value_ident, item),
            };
            if !plain_where_holds(graph, &r, predicate, params)? {
                return Ok(None);
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
            Ok(Some((key, value_v)))
        })
        .filter_map(Result::transpose);
    retain_map(graph, bound, entries)
}
