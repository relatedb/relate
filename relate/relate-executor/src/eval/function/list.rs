//! List and collection builtins: `head`/`last`/`tail`/`reverse`, `range`,
//! `size`/`cardinality`/`length`/`isEmpty`, and the `coll.*` wrappers over
//! the `eval::collection` helpers.

use std::sync::Arc;

use relate_core::{
    expected_type_detail, integer_args, value_type_name, Error, ErrorKind, Result, Value,
    FUNCTION_REQUIRES_ONE_ARGUMENT, LENGTH_REQUIRES_A_PATH,
};
use relate_ir::QuantifierKind;

use crate::overlay::ExecGraph;

use super::super::collection::{
    collection_distinct, collection_extreme, collection_flatten, collection_has,
    collection_index_of, collection_insert, collection_remove, collection_sort, collection_zip,
};
use super::super::{into_items, Extreme};

pub(super) fn bi_length(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // `length()` accepts only a path (or null): live Neo4j 2026.10.1
    // resolves the signature at compile time, so a string or list is
    // rejected like any other non-path value. Same message as the planner
    // rule (`function_meta::rule_length_non_path_literal`).
    unary_arg(vals, |value| match value {
        Value::Path(p) => Ok(Value::Int(p.rels.len() as i64)),
        _ => Err(Error::invalid_argument_type(LENGTH_REQUIRES_A_PATH)),
    })
}

/// The wrong-argument-type error of the list builtins (`22N01`): the one
/// shared sentence (see `relate_core::expected_type_detail`) in the
/// typed argument-error channel.
fn expected_type_error(expected: &str, value: &Value) -> Error {
    Error::invalid_argument_type_expected(expected, &value_type_name(value))
}

/// The shared unary-argument preamble of every list builtin that takes
/// exactly one argument — `size`, `isEmpty`, `head`, `last`, `tail`,
/// `reverse`, and the one-argument `coll.*` functions (`coll.sort`,
/// `coll.min`, `coll.max`, `coll.distinct`). Anything but exactly one
/// argument is the shared core `FUNCTION_REQUIRES_ONE_ARGUMENT` arity
/// error (the planner's `FIXED_ARITY_FUNCTIONS` row owns the user-facing
/// rejection; this is the backstop for callers that reach the executor
/// without a plan-time check) — extra
/// arguments are rejected, never truncated to the first; a null argument
/// propagates null; and every other value is `f`'s to judge — each builtin
/// matches only the present, non-null value and owns its own wrong-type
/// rejection. (`length` and `cardinality` keep their own first-argument
/// reading and are deliberately not routed here.)
fn unary_arg(vals: Vec<Value>, f: impl FnOnce(Value) -> Result<Value>) -> Result<Value> {
    let mut vals = vals.into_iter();
    match (vals.next(), vals.next()) {
        (Some(Value::Null), None) => Ok(Value::Null),
        (Some(value), None) => f(value),
        _ => Err(Error::invalid_number_of_arguments(
            FUNCTION_REQUIRES_ONE_ARGUMENT,
        )),
    }
}

/// `size()`: STRING, LIST or VECTOR only. A MAP is not a sized collection
/// in Neo4j (use `cardinality()`), so it raises the `22N27` invalid-type
/// error instead of counting keys; a PATH points at `length()`.
pub(super) fn bi_size(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| match value {
        Value::List(xs) => Ok(Value::Int(xs.len() as i64)),
        Value::String(s) => Ok(Value::Int(s.chars().count() as i64)),
        Value::Vector { values, .. } => Ok(Value::Int(values.len() as i64)),
        Value::Path(_) => Err(Error::invalid_argument_value("size() on paths")),
        other => {
            // Same sentence as the 22N01 family, in size()'s legacy
            // `InvalidArgumentValue` class — the class is what the corpus
            // pins, the sentence is shared.
            Err(Error::invalid_argument_value(expected_type_detail(
                "LIST<ANY>, STRING or VECTOR",
                &value_type_name(&other),
            )))
        }
    })
}

/// `isEmpty()`: true for an empty LIST, MAP or STRING, false otherwise,
/// null-in-null-out. Graph values are rejected like every other type —
/// Neo4j reports them as an invalid argument rather than as false.
pub(super) fn bi_isempty(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| match value {
        Value::List(xs) => Ok(Value::Bool(xs.is_empty())),
        Value::Map(m) => Ok(Value::Bool(m.is_empty())),
        Value::String(s) => Ok(Value::Bool(s.is_empty())),
        other => Err(expected_type_error("STRING, LIST or MAP", &other)),
    })
}

pub(super) fn bi_head(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| match value {
        Value::List(xs) => Ok(xs.first().cloned().unwrap_or(Value::Null)),
        other => Err(expected_type_error("LIST<ANY>", &other)),
    })
}

pub(super) fn bi_last(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| match value {
        Value::List(xs) => Ok(xs.last().cloned().unwrap_or(Value::Null)),
        other => Err(expected_type_error("LIST<ANY>", &other)),
    })
}

pub(super) fn bi_tail(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| match value {
        // `into_items` owns the owned-vs-shared split: a unique list moves
        // its tail elements out, a shared one clones only the tail, and an
        // empty list falls out of `skip(1)`.
        Value::List(xs) => Ok(Value::List(
            into_items(xs).skip(1).collect::<Vec<_>>().into(),
        )),
        other => Err(expected_type_error("LIST<ANY>", &other)),
    })
}

pub(super) fn bi_reverse(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| match value {
        Value::List(xs) => {
            let mut o = Arc::unwrap_or_clone(xs);
            o.reverse();
            Ok(Value::List(o.into()))
        }
        Value::String(s) => Ok(Value::String(s.chars().rev().collect::<String>().into())),
        other => Err(expected_type_error("STRING or LIST<ANY>", &other)),
    })
}

pub(super) fn bi_cardinality(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| match value {
        Value::List(xs) => Ok(Value::Int(xs.len() as i64)),
        Value::Map(m) => Ok(Value::Int(m.len() as i64)),
        Value::Path(p) => Ok(Value::Int((p.nodes.len() + p.rels.len()) as i64)),
        _ => Err(Error::ty("CARDINALITY() requires a list, path, or map")),
    })
}

/// A resolved `range(start, end[, step])` call: the exact element count and
/// the walk that yields the elements, with no list built. `range()` builds
/// its list from this plan and `UNWIND range(...)` walks it row by row, so
/// the two read one argument contract and one size computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RangePlan {
    start: i64,
    step: i64,
    size: i64,
}

/// The elements of a [`RangePlan`], one at a time. In range by construction
/// (the size is exact); the advance is checked anyway so an extreme input
/// errors honestly instead of wrapping or looping forever.
#[derive(Clone, Debug)]
pub(crate) struct RangeValues {
    next: i64,
    step: i64,
    remaining: i64,
}

impl RangePlan {
    /// The element count as a `usize`, or the arithmetic-overflow error when
    /// it does not fit this platform's address space.
    pub(crate) fn len(&self) -> Result<usize> {
        usize::try_from(self.size).map_err(|_| Error::ty("integer out of range"))
    }

    /// The walk over the plan's elements.
    pub(crate) fn values(self) -> RangeValues {
        RangeValues {
            next: self.start,
            step: self.step,
            remaining: self.size,
        }
    }
}

impl RangeValues {
    /// Elements not yet yielded.
    pub(crate) fn remaining(&self) -> i64 {
        self.remaining
    }
}

impl Iterator for RangeValues {
    type Item = Result<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining <= 0 {
            return None;
        }
        let value = Value::Int(self.next);
        self.remaining -= 1;
        if self.remaining > 0 {
            match self.next.checked_add(self.step) {
                Some(next) => self.next = next,
                None => {
                    self.remaining = 0;
                    return Some(Err(Error::ty("integer out of range")));
                }
            }
        }
        Some(Ok(value))
    }
}

/// Resolve `range()`'s arguments: the ADR-0175 conversions and the size
/// computation, with no allocation.
pub(crate) fn range_plan(vals: &[Value]) -> Result<RangePlan> {
    // ADR-0175: a float argument converts with Java `(long)`-cast
    // semantics (truncate toward zero, NaN → 0, ±Infinity and
    // out-of-range saturate at the i64 limits); every non-numeric
    // argument stays a type error.
    let int_arg_or_type_error = |v: Option<&Value>, detail: &str| {
        v.and_then(super::super::numeric::int_arg)
            .ok_or_else(|| Error::invalid_argument_type(detail))
    };
    let start = int_arg_or_type_error(vals.first(), integer_args::RANGE_START.type_error)?;
    let end = int_arg_or_type_error(vals.get(1), integer_args::RANGE_END.type_error)?;
    let step = if vals.len() >= 3 {
        int_arg_or_type_error(vals.get(2), integer_args::RANGE_STEP.type_error)?
    } else {
        1
    };
    if step == 0 {
        // ADR-0175's 22N03 mapping: NumberOutOfRange is the ArgumentError
        // class, so the kind travels as data (live 2026.10.1 reports
        // Neo.ClientError.Statement.ArgumentError).
        return Err(Error::argument_value_kind(
            ErrorKind::NumberOutOfRange,
            "range() step cannot be 0",
        ));
    }
    // ADR-0175: a size computation overflowing i64 is the
    // arithmetic-overflow error (Neo4j 22N28, IntegralRangeListValue's
    // subtractExact/divideExact/addExact) — `range(1, -Infinity)`
    // errors because `end - start` overflows once -Infinity has
    // saturated to i64::MIN, while `range(5, 1, -Infinity)` is `[5]`.
    // A direction-inconsistent range (the end unreachable from the
    // start at this step sign) is empty (TCK List11 [2]/[3]).
    let diff = end
        .checked_sub(start)
        .ok_or_else(|| Error::ty("integer out of range"))?;
    let size = if (diff > 0 && step < 0) || (diff < 0 && step > 0) {
        0
    } else {
        diff.checked_div(step)
            .and_then(|q| q.checked_add(1))
            .ok_or_else(|| Error::ty("integer out of range"))?
    };
    Ok(RangePlan { start, step, size })
}

pub(super) fn bi_range(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    let plan = range_plan(&vals)?;
    // ADR-0463: the exact element count is known here, so one guarded
    // block charges the whole result buffer and allocates exactly it —
    // the charged model and the allocation cannot disagree. A
    // billion-element `range` is refused before a slot is reserved.
    let xs = if plan.size > 0 {
        let len = plan.len()?;
        graph.charge_value_buffer(len)?;
        let mut xs = Vec::with_capacity(len);
        for value in plan.values() {
            graph.cancel_tick()?;
            xs.push(value?);
        }
        xs
    } else {
        Vec::new()
    };
    Ok(Value::List(xs.into()))
}

pub(super) fn bi_coll_flatten(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    collection_flatten(graph, &vals)
}

pub(super) fn bi_coll_sort(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, collection_sort)
}

pub(super) fn bi_coll_indexof(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    collection_index_of(&vals)
}

pub(super) fn bi_coll_insert(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    collection_insert(vals)
}

pub(super) fn bi_coll_remove(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    collection_remove(vals)
}

pub(super) fn bi_coll_max(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| collection_extreme(value, Extreme::Max))
}

pub(super) fn bi_coll_min(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    unary_arg(vals, |value| collection_extreme(value, Extreme::Min))
}

pub(super) fn bi_coll_distinct(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    unary_arg(vals, collection_distinct)
}

pub(super) fn bi_coll_zip(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    collection_zip(graph, &vals)
}

pub(super) fn bi_coll_has_any_of(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    collection_has(QuantifierKind::Any, &vals)
}

pub(super) fn bi_coll_has_all_of(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    collection_has(QuantifierKind::All, &vals)
}

pub(super) fn bi_coll_has_none_of(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    collection_has(QuantifierKind::None, &vals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::test_support::ints;

    #[test]
    fn tail_drops_the_head_of_a_shared_or_unique_list() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let tail = |v: Value| bi_tail(&graph, "tail", vec![v]).unwrap();

        // Unique payload: moved out and shifted in place.
        assert_eq!(tail(ints(&[1, 2, 3])), ints(&[2, 3]));
        // Shared payload: only the tail is copied; the input is untouched.
        let input = ints(&[1, 2, 3]);
        assert_eq!(tail(input.clone()), ints(&[2, 3]));
        assert_eq!(input, ints(&[1, 2, 3]));
        // An empty list stays empty, shared or not.
        let empty = ints(&[]);
        assert_eq!(tail(empty.clone()), ints(&[]));
        assert_eq!(tail(empty), ints(&[]));
        assert_eq!(tail(Value::Null), Value::Null);
    }

    fn assert_invalid_argument(error: Error, expected: &str) {
        match error {
            Error::InvalidArgumentType(detail) => {
                assert!(
                    detail.contains(expected),
                    "detail should name the accepted type: {detail}"
                )
            }
            other => panic!("expected InvalidArgumentType, got {other:?}"),
        }
    }

    /// The legacy `InvalidArgumentValue` channel (`22N27`, [`Error::Type`]):
    /// the class token is the variant's, the detail carries the shared
    /// acceptance sentence.
    fn assert_invalid_argument_value(error: Error, expected: &str) {
        match error {
            Error::Type(detail) => assert!(
                detail.contains(expected),
                "detail should name the accepted type: {detail}"
            ),
            other => panic!("expected the InvalidArgumentValue type error, got {other:?}"),
        }
    }

    /// The zero-argument backstop rejection: the one core spelling the
    /// planner's `FIXED_ARITY_FUNCTIONS` row and the executor backstop
    /// share.
    fn assert_one_argument_error(error: Error) {
        assert_eq!(
            error,
            Error::invalid_number_of_arguments(FUNCTION_REQUIRES_ONE_ARGUMENT),
            "zero or extra arguments is the one-argument arity rejection"
        );
    }

    /// LABS-709: `head`/`last`/`tail`/`reverse` raise Neo4j's typed
    /// wrong-argument error instead of silently returning null. Null input
    /// still propagates null (documented for each function), and the
    /// positive cases keep their documented results.
    #[test]
    fn head_last_tail_and_reverse_reject_wrong_argument_types() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);

        for (name, builtin, nonempty, empty) in [
            (
                "head",
                bi_head as super::super::registry::Builtin,
                Value::Int(1),
                Value::Null,
            ),
            (
                "last",
                bi_last as super::super::registry::Builtin,
                Value::Int(3),
                Value::Null,
            ),
            (
                "tail",
                bi_tail as super::super::registry::Builtin,
                ints(&[2, 3]),
                ints(&[]),
            ),
        ] {
            // The positive cases ride the same table as the negative ones,
            // so a builtin added here cannot lack either.
            assert_eq!(
                builtin(&graph, name, vec![ints(&[1, 2, 3])]).unwrap(),
                nonempty
            );
            assert_eq!(builtin(&graph, name, vec![ints(&[])]).unwrap(), empty);
            // Null-in stays null-out; zero arguments is the typed arity
            // error (the planner's FIXED_ARITY_FUNCTIONS row owns the
            // user-facing rejection, this is the executor backstop).
            assert_eq!(
                builtin(&graph, name, vec![Value::Null]).unwrap(),
                Value::Null
            );
            assert_one_argument_error(builtin(&graph, name, vec![]).unwrap_err());
            // Extra arguments are rejected, not silently truncated to the
            // first: `head(1, 2)` is the same arity error, not `head(1)`.
            assert_one_argument_error(
                builtin(&graph, name, vec![ints(&[1, 2]), Value::Null]).unwrap_err(),
            );
            // Every other type is the 22N01 wrong-argument error.
            for bad in [
                Value::Int(1),
                Value::Float(1.5),
                Value::Bool(true),
                Value::String("abc".into()),
                Value::Map(std::collections::BTreeMap::new().into()),
                Value::Vector {
                    coord: relate_core::VectorCoordKind::Float64,
                    values: vec![Value::Float(1.0)].into(),
                },
            ] {
                let error = builtin(&graph, name, vec![bad]).unwrap_err();
                assert_invalid_argument(error, "LIST<ANY>");
            }
        }

        assert_eq!(
            bi_reverse(&graph, "reverse", vec![Value::Null]).unwrap(),
            Value::Null
        );
        assert_one_argument_error(
            bi_reverse(&graph, "reverse", vec![ints(&[1]), ints(&[2])]).unwrap_err(),
        );
        assert_eq!(
            bi_reverse(&graph, "reverse", vec![ints(&[1, 2, 3])]).unwrap(),
            ints(&[3, 2, 1])
        );
        assert_eq!(
            bi_reverse(&graph, "reverse", vec![Value::String("abc".into())]).unwrap(),
            Value::String("cba".into())
        );
        for bad in [
            Value::Int(1),
            Value::Bool(true),
            Value::Map(std::collections::BTreeMap::new().into()),
            Value::Vector {
                coord: relate_core::VectorCoordKind::Float64,
                values: vec![Value::Float(1.0)].into(),
            },
        ] {
            let error = bi_reverse(&graph, "reverse", vec![bad]).unwrap_err();
            assert_invalid_argument(error, "STRING or LIST<ANY>");
        }
    }

    /// LABS-709: `size()` is STRING/LIST/VECTOR only. A MAP used to count
    /// its keys; Neo4j rejects it as an invalid entity type (`22N27`).
    #[test]
    fn size_rejects_maps_and_other_unsized_types() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);

        assert_eq!(
            bi_size(&graph, "size", vec![ints(&[1, 2, 3])]).unwrap(),
            Value::Int(3)
        );
        assert_one_argument_error(bi_size(&graph, "size", vec![]).unwrap_err());
        assert_one_argument_error(
            bi_size(&graph, "size", vec![ints(&[1]), ints(&[2])]).unwrap_err(),
        );
        assert_eq!(
            bi_size(&graph, "size", vec![Value::String("héllo".into())]).unwrap(),
            Value::Int(5)
        );
        assert_eq!(
            bi_size(
                &graph,
                "size",
                vec![Value::Vector {
                    coord: relate_core::VectorCoordKind::Float64,
                    values: vec![Value::Float(1.0), Value::Float(2.0)].into(),
                }]
            )
            .unwrap(),
            Value::Int(2)
        );
        assert_eq!(
            bi_size(&graph, "size", vec![Value::Null]).unwrap(),
            Value::Null
        );

        for bad in [
            Value::Map(std::collections::BTreeMap::new().into()),
            Value::Int(1),
            Value::Bool(true),
        ] {
            let error = bi_size(&graph, "size", vec![bad]).unwrap_err();
            assert_invalid_argument_value(error, "LIST<ANY>, STRING or VECTOR");
        }
    }

    /// LABS-709: `coll.zip` type-checks every argument as a LIST — a
    /// non-list raises, and only a null argument propagates null.
    #[test]
    fn coll_zip_rejects_non_list_arguments() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);

        for args in [
            vec![Value::Int(1), ints(&[1])],
            vec![ints(&[1]), Value::Int(1)],
            vec![ints(&[1]), Value::String("x".into())],
        ] {
            let error = bi_coll_zip(&graph, "coll.zip", args).unwrap_err();
            assert_invalid_argument_value(error, "coll.zip() requires a list");
        }
        // A null argument is the documented null-propagation exit, whichever
        // position it occupies.
        assert_eq!(
            bi_coll_zip(&graph, "coll.zip", vec![Value::Null, ints(&[1])]).unwrap(),
            Value::Null
        );
        assert_eq!(
            bi_coll_zip(&graph, "coll.zip", vec![ints(&[1]), Value::Null]).unwrap(),
            Value::Null
        );
        assert_eq!(
            bi_coll_zip(&graph, "coll.zip", vec![ints(&[1, 2]), ints(&[3, 4])]).unwrap(),
            Value::List(vec![ints(&[1, 3]), ints(&[2, 4])].into())
        );
    }

    /// LABS-709: the one-argument `coll.*` builtins reject extra arguments
    /// through the same `unary_arg` contract as the six list builtins — the
    /// planner's `FIXED_ARITY_FUNCTIONS` row owns the plan-time rejection,
    /// this is the executor backstop — and reject a non-list with the
    /// list-argument error naming the function (never a generic message).
    #[test]
    fn one_argument_coll_builtins_reject_extra_arguments_and_non_lists() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        for (name, builtin) in [
            ("coll.sort", bi_coll_sort as super::super::registry::Builtin),
            ("coll.min", bi_coll_min as super::super::registry::Builtin),
            ("coll.max", bi_coll_max as super::super::registry::Builtin),
            (
                "coll.distinct",
                bi_coll_distinct as super::super::registry::Builtin,
            ),
        ] {
            assert_one_argument_error(
                builtin(&graph, name, vec![ints(&[1, 2]), ints(&[3])]).unwrap_err(),
            );
            assert_eq!(
                builtin(&graph, name, vec![Value::Null]).unwrap(),
                Value::Null
            );
            let error = builtin(&graph, name, vec![Value::Int(1)]).unwrap_err();
            assert_invalid_argument_value(error, &format!("{name}() requires a list"));
        }
    }

    /// `length()`/`cardinality()` share the one unary-arity contract of the
    /// other list builtins: extra and zero arguments are the typed arity
    /// error, null stays null.
    #[test]
    fn length_and_cardinality_use_the_unary_arity_contract() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        for (name, builtin) in [
            ("length", bi_length as super::super::registry::Builtin),
            (
                "cardinality",
                bi_cardinality as super::super::registry::Builtin,
            ),
        ] {
            assert_one_argument_error(builtin(&graph, name, vec![]).unwrap_err());
            assert_one_argument_error(
                builtin(&graph, name, vec![ints(&[1]), ints(&[2])]).unwrap_err(),
            );
            assert_eq!(
                builtin(&graph, name, vec![Value::Null]).unwrap(),
                Value::Null
            );
        }
        // `cardinality` counts a list; `length` accepts a path only (live
        // Neo4j 2026.10.1 rejects a list at the signature check).
        assert_eq!(
            bi_cardinality(&graph, "cardinality", vec![ints(&[1, 2])]).unwrap(),
            Value::Int(2)
        );
        assert!(bi_length(&graph, "length", vec![ints(&[1, 2])]).is_err());
    }

    /// `coll.min`/`coll.max` use the one openCypher ORDER BY rank
    /// (MAP < … < LIST < … < STRING < BOOLEAN < NUMBER), not an ad-hoc
    /// type grouping: the harness pins
    /// `coll.min([7, 2, [2], 2, 5, 'a'])[0]` = 2, which only holds when the
    /// list `[2]` is the minimum.
    #[test]
    fn coll_min_and_max_rank_across_types_like_order_by() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let mixed = Value::List(
            vec![
                Value::Int(7),
                Value::Int(2),
                ints(&[2]),
                Value::Int(2),
                Value::Int(5),
                Value::String("a".into()),
            ]
            .into(),
        );
        assert_eq!(
            bi_coll_min(&graph, "coll.min", vec![mixed.clone()]).unwrap(),
            ints(&[2])
        );
        assert_eq!(
            bi_coll_max(&graph, "coll.max", vec![mixed]).unwrap(),
            Value::Int(7)
        );
        // A string outranks a list, and every number outranks both.
        let ranked = Value::List(
            vec![
                Value::String("a".into()),
                ints(&[1]),
                Value::Int(3),
                Value::Bool(true),
            ]
            .into(),
        );
        assert_eq!(
            bi_coll_min(&graph, "coll.min", vec![ranked.clone()]).unwrap(),
            ints(&[1])
        );
        assert_eq!(
            bi_coll_max(&graph, "coll.max", vec![ranked]).unwrap(),
            Value::Int(3)
        );
    }

    /// `coll.max` poisons on any null element while `coll.min` skips
    /// nulls — live Neo4j 2026.10.1, probed 2026-11-07; the plain
    /// `max()`/`min()` aggregates keep the null-skipping fold.
    #[test]
    fn coll_max_poisons_on_null_while_coll_min_skips() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let with_null = Value::List(
            vec![
                Value::String("hello".into()),
                Value::Int(1),
                Value::Bool(true),
                Value::Null,
            ]
            .into(),
        );
        assert_eq!(
            bi_coll_max(&graph, "coll.max", vec![with_null.clone()]).unwrap(),
            Value::Null
        );
        assert_eq!(
            bi_coll_min(&graph, "coll.min", vec![with_null]).unwrap(),
            Value::String("hello".into())
        );
        // A null among numbers poisons max the same way, and a null input
        // list is null for both.
        let nums = Value::List(vec![Value::Int(1), Value::Null, Value::Int(2)].into());
        assert_eq!(
            bi_coll_max(&graph, "coll.max", vec![nums.clone()]).unwrap(),
            Value::Null
        );
        assert_eq!(
            bi_coll_min(&graph, "coll.min", vec![nums]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            bi_coll_max(&graph, "coll.max", vec![Value::Null]).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn isempty_reads_list_map_and_string_emptiness() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let isempty = |v: Value| bi_isempty(&graph, "isEmpty", vec![v]).unwrap();
        // Zero arguments is the same typed arity backstop as the other
        // builtins in the planner's one-argument row.
        assert_one_argument_error(bi_isempty(&graph, "isEmpty", vec![]).unwrap_err());
        assert_one_argument_error(
            bi_isempty(&graph, "isEmpty", vec![ints(&[]), ints(&[])]).unwrap_err(),
        );
        let map = |entries: &[(&str, Value)]| {
            Value::Map(
                entries
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into(),
            )
        };

        assert_eq!(isempty(Value::Null), Value::Null);
        assert_eq!(isempty(ints(&[])), Value::Bool(true));
        assert_eq!(isempty(ints(&[1])), Value::Bool(false));
        assert_eq!(isempty(map(&[])), Value::Bool(true));
        assert_eq!(isempty(map(&[("a", Value::Int(1))])), Value::Bool(false));
        assert_eq!(isempty(Value::String("".into())), Value::Bool(true));
        assert_eq!(isempty(Value::String("hello".into())), Value::Bool(false));
        // Only the collection and string types are admissible; every other
        // type is the shared 22N01 sentence naming the actual type.
        let error = bi_isempty(&graph, "isEmpty", vec![Value::Int(1)]).unwrap_err();
        assert_invalid_argument(error, "STRING, LIST or MAP");
    }

    /// ADR-0463: `coll.flatten` and `coll.zip` charge their output buffers
    /// before building them. With the statement budget already spent, a
    /// tiny input is refused with the typed capacity error.
    #[test]
    fn expression_budget_refuses_flatten_and_zip_allocations() {
        let mut g = Graph::default();
        g.configure_expression_memory_limit(Some(1));
        let graph = ExecGraph::read(&g);
        let list = |xs: Vec<Value>| Value::List(xs.into());
        let nested = list(vec![Value::Int(1), list(vec![Value::Int(2)])]);
        let error = bi_coll_flatten(&graph, "coll.flatten", vec![nested]).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Capacity {
                    limit: 1,
                    kind: relate_core::CapacityKind::Expression,
                    ..
                }
            ),
            "{error}"
        );
        let a = list(vec![Value::Int(1), Value::Int(2)]);
        let b = list(vec![Value::Int(3)]);
        let error = bi_coll_zip(&graph, "coll.zip", vec![a, b]).unwrap_err();
        assert!(
            matches!(
                error,
                Error::Capacity {
                    limit: 1,
                    kind: relate_core::CapacityKind::Expression,
                    ..
                }
            ),
            "{error}"
        );
    }
}
