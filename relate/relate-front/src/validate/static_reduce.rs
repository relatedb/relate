//! The `reduce()` static type fault: a numerically-initialised accumulator
//! whose body applies an arithmetic operator to an element of a list *literal*
//! of temporal constructor calls. Live Neo4j's front-end type checker rejects
//! every such body during semantic analysis (probed 2026.10.1, `CYPHER 25`:
//! `reduce(a = 1, b in [date()] | a + b)` -> 42001 `SyntaxError`), while the
//! executor would otherwise raise the TypeError one phase too late. No
//! arithmetic on a temporal yields a number, so the body is either an operator
//! type error or a temporal result the numeric accumulator cannot absorb.
//! Only the statically decidable shape qualifies; every other operand type
//! stays runtime-decided.

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Expr, ReduceExpr};

use super::static_types::{
    applies_arithmetic_to_var, is_arithmetic_op, static_literal_type, unparen, StaticLitType,
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
    if !static_literal_type(&r.init_expr).is_some_and(StaticLitType::is_numeric) {
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
        for query in [
            // A numeric element is valid arithmetic.
            "RETURN reduce(a = 1, b in [1, 2] | a + b) AS r",
            // `+` on a string element is concatenation.
            "RETURN reduce(a = 1, b in ['s'] | a + b) AS r",
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
}
