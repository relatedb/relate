//! Collection function helpers (`coll.*`).

use std::sync::Arc;

use relate_core::{next_depth, value_depth_exceeded, Error, Result, Value};
use relate_ir::QuantifierKind;

use crate::overlay::ExecGraph;

use super::{fold_extreme, fold_quantifier, membership, Extreme};

pub(crate) fn collection_flatten(graph: &ExecGraph<'_>, vals: &[Value]) -> Result<Value> {
    let Some(value) = vals.first() else {
        return Err(Error::semantic("coll.flatten() requires an argument"));
    };
    if value.is_null() {
        return Ok(Value::Null);
    }
    let Value::List(items) = value else {
        return Err(Error::ty("coll.flatten() requires a list"));
    };
    let depth = match vals.get(1) {
        None => 1,
        Some(Value::Null) => return Ok(Value::Null),
        Some(Value::Int(i)) if (0..=i32::MAX as i64).contains(i) => *i,
        Some(Value::Int(_)) => return Err(Error::ty("InvalidArgumentValue: flatten depth")),
        Some(_) => return Err(Error::invalid_argument_type("flatten depth")),
    };
    // ADR-0462 + ADR-0463: one descent over the nesting rule, two visitors —
    // counting and flattening cannot drift because they are the same walk.
    // The ADR-0462 depth guard rides the walk: the checked constructors cap
    // input depth at construction, so it is a safety valve, not a reachable
    // rejection. `nesting` is the ancestor depth; `next_depth` is the one
    // guard spelling.
    fn walk(
        items: &[Value],
        depth: i64,
        nesting: u32,
        visit: &mut impl FnMut(&Value),
    ) -> Result<()> {
        for item in items {
            if depth > 0 {
                if let Value::List(nested) = item {
                    let Some(next) = next_depth(nesting) else {
                        return Err(Error::execution(value_depth_exceeded(
                            "coll.flatten() input",
                        )));
                    };
                    walk(nested, depth - 1, next, visit)?;
                    continue;
                }
            }
            visit(item);
        }
        Ok(())
    }
    let depth = depth.max(0);
    let mut count = 0usize;
    walk(items, depth, 0, &mut |_| count += 1)?;
    graph.charge_value_buffer(count)?;
    let mut out = Vec::with_capacity(count);
    walk(items, depth, 0, &mut |item| out.push(item.clone()))?;
    Value::checked_list(out)
}

pub(crate) fn collection_sort(value: Value) -> Result<Value> {
    let Some(items) = list_arg(value, "coll.sort()")? else {
        return Ok(Value::Null);
    };
    let mut out = Arc::unwrap_or_clone(items);
    out.sort_by(Value::order_by_cmp);
    Value::checked_list(out)
}

pub(crate) fn collection_index_of(vals: &[Value]) -> Result<Value> {
    match vals.first() {
        Some(Value::List(_)) if vals.get(1).is_none_or(Value::is_null) => Ok(Value::Null),
        Some(Value::List(items)) => {
            let search = &vals[1];
            Ok(Value::Int(
                items
                    .iter()
                    .position(|v| v.eq_non_null(search))
                    .map(|i| i as i64)
                    .unwrap_or(-1),
            ))
        }
        Some(Value::Null) => Ok(Value::Null),
        _ => Err(Error::ty("coll.indexOf() requires a list")),
    }
}

/// The shared (list, index) argument policy of `coll.insert` and
/// `coll.remove`: null propagates from either position, the index must be
/// an integer within bounds (`end_inclusive` also admits `len`, the
/// append position, for insertion), and `op` names the function in the
/// pinned denial texts. The index returns as `usize` so the bounds
/// invariant the checks establish survives in the type and callers never
/// re-cast. `None` is the null-propagation exit; the caller keeps only its
/// one-line operation.
fn list_index_args(
    args: &mut std::vec::IntoIter<Value>,
    op: &str,
    end_inclusive: bool,
) -> Result<Option<(Arc<Vec<Value>>, usize)>> {
    let items = match args.next() {
        Some(Value::List(items)) => items,
        Some(Value::Null) => return Ok(None),
        _ => return Err(Error::ty(format!("{op} requires a list"))),
    };
    let i = match args.next() {
        Some(Value::Null) | None => return Ok(None),
        Some(Value::Int(i)) => i,
        _ => return Err(Error::invalid_argument_type(format!("{op} index"))),
    };
    if i < 0 {
        return Err(Error::ty(format!("InvalidArgumentValue: {op} index")));
    }
    let i =
        usize::try_from(i).map_err(|_| Error::ty(format!("InvalidArgumentValue: {op} index")))?;
    let past_end = if end_inclusive {
        i > items.len()
    } else {
        i >= items.len()
    };
    if past_end {
        return Err(Error::ty(format!("InvalidArgumentValue: {op} index")));
    }
    Ok(Some((items, i)))
}

pub(crate) fn collection_insert(vals: Vec<Value>) -> Result<Value> {
    let mut args = vals.into_iter();
    let Some((items, i)) = list_index_args(&mut args, "coll.insert()", true)? else {
        return Ok(Value::Null);
    };
    let mut out = Arc::unwrap_or_clone(items);
    out.insert(i, args.next().unwrap_or(Value::Null));
    Value::checked_list(out)
}

pub(crate) fn collection_remove(vals: Vec<Value>) -> Result<Value> {
    let mut args = vals.into_iter();
    let Some((items, i)) = list_index_args(&mut args, "coll.remove()", false)? else {
        return Ok(Value::Null);
    };
    let mut out = Arc::unwrap_or_clone(items);
    out.remove(i);
    Value::checked_list(out)
}

/// `coll.min()`/`coll.max()`: the shared extreme fold over one list, using
/// the one cross-type order policy — [`Value::order_by_cmp`], the openCypher
/// ORDER BY rank (MAP < NODE < RELATIONSHIP < LIST < … < STRING < BOOLEAN <
/// NUMBER). A mixed list therefore keeps Neo4j's answer: the minimum of
/// `[7, 2, [2], 2, 5, 'a']` is the list `[2]`, not the number `2`.
///
/// The two spellings differ on null elements — [`Extreme::coll_null_poison`]
/// owns that per-variant fact — so the coll surface checks before the
/// shared fold, which stays null-skipping because the `max()`/`min()`
/// aggregates keep Cypher's null-ignoring semantics.
pub(crate) fn collection_extreme(value: Value, extreme: Extreme) -> Result<Value> {
    let Some(items) = list_arg(value, extreme.op())? else {
        return Ok(Value::Null);
    };
    if extreme.coll_null_poison() && items.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    Ok(fold_extreme(
        items.iter().cloned(),
        extreme,
        Value::order_by_cmp,
    ))
}

/// The `coll.zip()` name as its type and arity errors spell it — one owner,
/// consulted by [`collection_zip`]'s checks and [`list_arg`]'s type error.
const ZIP_OP: &str = "coll.zip()";

/// `coll.zip(list1, list2, ...)` (CIP-274): zips two or more lists into a
/// list of per-index lists, to the length of the longest input, padding
/// missing positions with null. A null argument propagates null; a non-list
/// argument is a type error.
pub(crate) fn collection_zip(graph: &ExecGraph<'_>, vals: &[Value]) -> Result<Value> {
    if vals.len() < 2 {
        return Err(Error::syntax(format!(
            "InvalidNumberOfArguments: {} requires at least two arguments, got {}",
            ZIP_OP,
            vals.len()
        )));
    }
    let mut lists: Vec<Arc<Vec<Value>>> = Vec::with_capacity(vals.len());
    for v in vals {
        let Some(items) = list_arg(v.clone(), ZIP_OP)? else {
            return Ok(Value::Null);
        };
        lists.push(items);
    }
    // The arity check above guarantees at least two list arguments, so the
    // max is invariant-bound — expect, never a fabricated 0 default.
    let len = lists
        .iter()
        .map(|l| l.len())
        .max()
        .expect("coll.zip() arity check guarantees at least two lists");
    // ADR-0463: the output is `len` inner lists of `arity` values; charge
    // the outer buffer plus every inner buffer before allocating either.
    let outer = crate::workspace::value_buffer_bytes(len);
    let inner = (len as u64).saturating_mul(crate::workspace::value_buffer_bytes(vals.len()));
    graph.charge_expression(outer.saturating_add(inner))?;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(Value::checked_list(
            lists
                .iter()
                .map(|l| l.get(i).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>(),
        )?);
    }
    Value::checked_list(out)
}

/// The one mapping of the existing quantifier vocabulary
/// (`relate_ir::QuantifierKind`) onto the `coll.has*Of` function names
/// (CIP-286) — the names travel with the variants, never as a second
/// stringly argument. No `coll.has*Of` adapter constructs the `Single`
/// quantifier, so that arm is unreachable.
fn coll_op(kind: QuantifierKind) -> &'static str {
    match kind {
        QuantifierKind::All => "coll.hasAllOf()",
        QuantifierKind::Any => "coll.hasAnyOf()",
        QuantifierKind::None => "coll.hasNoneOf()",
        QuantifierKind::Single => {
            unreachable!("no coll.has*Of function constructs the Single quantifier")
        }
    }
}

/// The one list-argument policy of the `coll.*` functions — `coll.zip`, the
/// `coll.has*Of` family, and the one-argument list builtins: a null argument
/// propagates null and anything but a list is the typed `{op} requires a
/// list` error, so every rejection names its function. `None` is the
/// null-propagation exit.
fn list_arg(value: Value, op: &str) -> Result<Option<Arc<Vec<Value>>>> {
    match value {
        Value::List(items) => Ok(Some(items)),
        Value::Null => Ok(None),
        _ => Err(Error::ty(format!("{op} requires a list"))),
    }
}

/// `coll.hasAnyOf` / `coll.hasAllOf` / `coll.hasNoneOf` (CIP-286): the
/// [`QuantifierKind`] reduction over the canonical membership predicate
/// [`membership`], via the one shared three-valued quantifier fold
/// (`eval::fold_quantifier`). Each quantifier exits early on
/// its decided value — `Any` on the first member, `All` on the first
/// non-member, `None` on the first member — and otherwise returns null
/// when any membership was unknown, never a fabricated boolean.
pub(crate) fn collection_has(kind: QuantifierKind, vals: &[Value]) -> Result<Value> {
    if vals.len() != 2 {
        return Err(Error::syntax(format!(
            "InvalidNumberOfArguments: {} requires exactly two arguments, got {}",
            coll_op(kind),
            vals.len()
        )));
    }
    // Left-to-right, like collection_zip: a null argument propagates null
    // before the other argument's type is checked.
    let Some(list) = list_arg(vals[0].clone(), coll_op(kind))? else {
        return Ok(Value::Null);
    };
    let Some(values) = list_arg(vals[1].clone(), coll_op(kind))? else {
        return Ok(Value::Null);
    };
    fold_quantifier(kind, values.iter(), |x| {
        // A null element of `list` could equal `x`: the membership is
        // unknown, never decided for this quantifier's exit.
        Ok(membership(x, list.as_slice()).truthy())
    })
}

pub(crate) fn collection_distinct(value: Value) -> Result<Value> {
    let Some(items) = list_arg(value, "coll.distinct()")? else {
        return Ok(Value::Null);
    };
    // First-seen emission under the one `dedup_first_seen` policy (the same
    // the batch aggregate's DISTINCT fold uses); this caller returns the
    // deduped list instead of retaining it.
    let items = Arc::unwrap_or_clone(items);
    Value::checked_list(crate::distinct_set::dedup_first_seen(items)?)
}
