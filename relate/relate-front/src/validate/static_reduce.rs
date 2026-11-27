//! The `reduce()` static type rules — two front-end faults live Neo4j's type
//! checker raises during semantic analysis (both probed 2026.10.1), one home
//! per policy, both reading the shared literal-type vocabulary in
//! `static_literal` and dispatched from `static_types`:
//!
//! * the temporal-arithmetic fault — a numerically-initialised accumulator
//!   whose body applies an arithmetic operator to an element of a list
//!   *literal* of temporal constructor calls (`reduce(a = 1, b in [date()] |
//!   a + b)` -> 42001 `SyntaxError`): no arithmetic on a temporal yields a
//!   number, so the body is either an operator type error or a temporal
//!   result the numeric accumulator cannot absorb;
//! * the accumulator/body unification — when both sides have decided static
//!   types, the body's type must be one the accumulator can absorb
//!   (`reduce(a = 1, b in ['s'] | 's')` -> 22G03 `Type mismatch:
//!   accumulator is Integer but expression has type String`).
//!
//! Only the statically decidable shapes qualify; every other operand type
//! stays runtime-decided.

use relate_core::LiteralValueKind;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Expr, ReduceExpr};

use crate::ast::tree::unparen;

use super::static_arithmetic::applies_arithmetic_to_var;
use super::static_literal::{
    is_arithmetic_op, static_list_element_kinds, static_literal_type, static_type_in,
};

/// Whether `list` is a non-empty list *literal* whose every element is a call
/// to a temporal constructor (`date`, `datetime`, `localtime`, `localdatetime`,
/// `time`, `duration` — the name table is `relate_core::temporal`'s). Such a
/// list binds its `reduce` variable to a temporal value on every iteration.
fn list_literal_all_temporal(list: &Expr) -> bool {
    let Expr::List(items) = unparen(list) else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| {
            matches!(unparen(item),
                Expr::FunctionCall(fc) if fc.name.with_str(relate_core::temporal::is_constructor))
        })
}

/// `reduce(acc = <numeric>, v IN [<temporal>…] | <body>)`: the front-end type
/// fault live Neo4j raises when the body does arithmetic on the statically
/// temporal element. No arithmetic on a temporal yields a number, so the pair
/// is either an operator type error or a temporal result the numeric
/// accumulator cannot absorb — live rejects both. The find is the shared
/// [`applies_arithmetic_to_var`] over the arithmetic vocabulary (the binder-
/// shadowing walk defers a rebinding nested scope to the runtime backstop).
pub(super) fn check_reduce_temporal_arithmetic(r: &ReduceExpr) -> Result<(), EngineError> {
    if !static_literal_type(&r.init_expr).is_some_and(LiteralValueKind::is_numeric) {
        return Ok(());
    }
    if !list_literal_all_temporal(&r.list_expr) {
        return Ok(());
    }
    let faulty = r
        .variable
        .with_str(|element| applies_arithmetic_to_var(&r.expression, element, is_arithmetic_op));
    if !faulty {
        return Ok(());
    }
    Err(EngineError::semantic(
        ErrorKind::InvalidArgumentType,
        "Type mismatch: expected Float, Integer, String or List<T> but was a temporal value",
    ))
}

/// `reduce(acc = <literal>, v IN <list> | <body>)`: the accumulator/body
/// type unification — when both sides have decided static types, the
/// body's type must be one the accumulator can absorb. Live 2026.10.1
/// rejects `reduce(a = 1, b in ['s'] | 's')` at compile time (SyntaxError
/// 22G03, `Type mismatch: accumulator is Integer but expression has type
/// String`), and the same for a body whose arithmetic resolves to String
/// — the `+` is not the fault (`1 + 's'` is `'1s'`), the mismatch is what
/// the accumulator cannot absorb. Numeric kinds absorb each other and a
/// null body absorbs into anything; only statically decidable shapes
/// qualify, and the temporal pair keeps
/// [`check_reduce_temporal_arithmetic`]'s own text.
pub(super) fn check_reduce_accumulator_type(r: &ReduceExpr) -> Result<(), EngineError> {
    let Some(acc_ty) = static_literal_type(&r.init_expr) else {
        return Ok(());
    };
    let elem_ty = uniform_list_element_type(&r.list_expr);
    // The one resolver, with the accumulator and element variables bound
    // to their decided types — the body's type through the same operator
    // table every static check reads.
    let Some(body_ty) = static_type_in(&r.expression, &|name| {
        if name == &r.accumulator {
            Some(acc_ty)
        } else if name == &r.variable {
            elem_ty
        } else {
            None
        }
    }) else {
        return Ok(());
    };
    if absorbs(acc_ty, body_ty) {
        return Ok(());
    }
    Err(EngineError::semantic(
        ErrorKind::InvalidArgumentType,
        format!(
            "Type mismatch: accumulator is {} but expression has type {}",
            acc_ty.camel_name(),
            body_ty.camel_name()
        ),
    ))
}

/// Whether an accumulator of kind `acc` can absorb a `reduce` body of kind
/// `body`: the same kind, the one numeric family, or a null on either side.
fn absorbs(acc: LiteralValueKind, body: LiteralValueKind) -> bool {
    acc == body
        || acc == LiteralValueKind::Null
        || body == LiteralValueKind::Null
        || (acc.is_numeric() && body.is_numeric())
}

/// The uniform element kind of a list *literal* (or its plan-cache
/// placeholder): every element statically typed and all the same kind.
/// A mixed or untyped list binds its element variably, so the body's type
/// stays runtime-decided.
fn uniform_list_element_type(list: &Expr) -> Option<LiteralValueKind> {
    let kinds = static_list_element_kinds(list)?;
    let first = *kinds.first()?;
    if kinds.iter().all(|kind| *kind == first) {
        first
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::tree::tests::return_expr_of;
    use crate::validate::static_types::parse_and_check_static_ops;

    fn assert_temporal_fault(query: &str) {
        let err = parse_and_check_static_ops(query)
            .expect_err(&format!("expected a compile-time type fault for `{query}`"));
        assert!(
            matches!(err, EngineError::Semantic { .. }),
            "`{query}` must fail at compile time (Semantic), got {err:?}"
        );
        assert_eq!(
            err.kind(),
            ErrorKind::InvalidArgumentType,
            "`{query}` must carry the InvalidArgumentType detail"
        );
    }

    fn assert_untouched(query: &str) {
        assert!(
            parse_and_check_static_ops(query).is_ok(),
            "`{query}` must not be rejected by the static pass"
        );
    }

    #[test]
    fn numeric_accumulator_with_temporal_element_is_a_compile_time_fault() {
        // Every operator in both orders, over every temporal constructor: live
        // rejects each one at compile time (a type error, or for duration
        // scaling a body type that is not the accumulator's).
        for op in ["+", "-", "*", "/", "%", "^"] {
            for ctor in [
                "date()",
                "datetime()",
                "time()",
                "localtime()",
                "localdatetime()",
                "duration('P1D')",
            ] {
                for body in [format!("a {op} b"), format!("b {op} a")] {
                    assert_temporal_fault(&format!(
                        "RETURN reduce(a = 1, b in [{ctor}] | {body}) AS r"
                    ));
                }
            }
        }
        // A float accumulator and a multi-element list are the same fault, and
        // a nested operator node is reached too.
        for query in [
            "RETURN reduce(a = 1.0, b in [date()] | a + b) AS r",
            "RETURN reduce(a = 1, b in [date(), date()] | a + b) AS r",
            "RETURN reduce(a = 1, b in [date()] | (a + b) * 2) AS r",
        ] {
            assert_temporal_fault(query);
        }
        // Any other operand of the temporal element is the same fault: no
        // arithmetic on a temporal yields the numeric accumulator's type.
        for body in ["b + b", "b + null", "b + 'x'", "b + [1]", "b - b"] {
            assert_temporal_fault(&format!(
                "RETURN reduce(a = 1, b in [date()] | {body}) AS r"
            ));
        }
        assert_temporal_fault("RETURN reduce(a = 1, b in [duration('P1D')] | b - b) AS r");
    }

    #[test]
    fn non_temporal_or_non_numeric_accumulator_is_untouched() {
        // (`reduce(a = 1, b in ['s'] | a + b)` — the `+` resolves to String —
        // used to sit here as concatenation-allowed; live 2026.10.1 rejects
        // it as the accumulator/body mismatch, pinned by
        // `an_integer_accumulator_rejects_a_string_body`.)
        for query in [
            // A numeric element is valid arithmetic.
            "RETURN reduce(a = 1, b in [1, 2] | a + b) AS r",
            // A string accumulator concatenates the temporal element.
            "RETURN reduce(a = 'x', b in [date('2020-01-01')] | a + b) AS r",
            // A list accumulator appends the temporal element.
            "RETURN reduce(a = [1], b in [date()] | a + b) AS r",
            // A runtime-typed list: the element type is not statically known.
            "RETURN reduce(a = 1, b in $xs | a + b) AS r",
            // The body never combines the element with the accumulator.
            "RETURN reduce(a = 1, b in [date()] | a + 1) AS r",
            // A nested binder rebinding the element name is not decidable here.
            "RETURN reduce(a = 1, b in [date()] | a + reduce(b = 1, c IN [1] | b + 1)) AS r",
        ] {
            assert_untouched(query);
        }
    }

    #[test]
    fn helper_reads_only_a_non_empty_temporal_list_literal() {
        let all_temporal =
            |q: &str| list_literal_all_temporal(&return_expr_of(&format!("RETURN {q}")));
        assert!(all_temporal("[date()]"));
        assert!(all_temporal("[duration('P1D')]"));
        assert!(all_temporal("[date(), duration('P1D')]"));
        assert!(!all_temporal("[]"));
        assert!(!all_temporal("[1]"));
        assert!(!all_temporal("[date('2020-01-01'), 1]"));
        assert!(!all_temporal("$xs"));
    }

    /// REL8-2289: the accumulator/body unification — live's 22G03 text for
    /// the string-body-over-integer-accumulator pair, in both the literal
    /// body and the arithmetic-body spellings, and the absorbed shapes that
    /// stay green.
    #[test]
    fn an_integer_accumulator_rejects_a_string_body() {
        for query in [
            "RETURN reduce(a = 1, b in ['s'] | 's') AS r",
            "RETURN reduce(a = 0, b in ['a', 'b'] | a + b) AS r",
        ] {
            let err = parse_and_check_static_ops(query)
                .expect_err(&format!("expected a compile-time fault for `{query}`"));
            assert!(
                err.to_string().contains(
                    "Type mismatch: accumulator is Integer but expression has type String"
                ),
                "`{query}` must carry live's 22G03 text, got {err}"
            );
        }
    }

    #[test]
    fn absorbable_reducer_bodies_stay_green() {
        for query in [
            "RETURN reduce(a = 1, b in [1, 2] | a + b) AS r",
            "RETURN reduce(a = 1.0, b in [1] | a + b) AS r",
            "RETURN reduce(a = 1, b in [1] | a * b) AS r",
            "RETURN reduce(a = 'x', b in ['s'] | b) AS r",
            "RETURN reduce(a = null, b in ['s'] | b) AS r",
            "RETURN reduce(a = 1, b in ['s'] | null) AS r",
            "RETURN reduce(a = 1, b in [] | a) AS r",
            // The canonical string-reduce idiom: the list accumulator
            // appends each element (`add`'s container arm), so the body's
            // type over containers is runtime-decided — never a String.
            "RETURN reduce(s = [], x IN ['a', 'b'] | s + x) AS r",
            // `+` with a null element propagates null at runtime — the
            // pair is runtime-decided, never a typed mismatch.
            "RETURN reduce(a = 1, b in [null] | a + b) AS r",
            "RETURN reduce(a = 1, b in ['s'] | a + null) AS r",
        ] {
            assert_untouched(query);
        }
    }
}
