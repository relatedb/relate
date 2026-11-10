//! Scalar-function arity: a call outside its builtin row's argument window
//! is the typed arity rejection (Neo4j 42I13), never a null or a downstream
//! type error. The three sides here are zero-argument calls of a builtin
//! that needs arguments (LABS-667), calls above an exact or capped contract,
//! and named per-function windows (range's two-to-three, LABS-1307) — while
//! the nullary builtins keep returning values. The exhaustive guards are the
//! executor's own tests (the nullary rows locked to the verified set, every
//! row's contract behaviorally checked); this file is the engine-level smoke
//! of representative names.

mod common;

#[test]
fn nullary_calls_keep_returning_values() {
    let mut conn = common::connection();
    // Representatives of the three nullary families — the clock/entropy
    // builtins, the constants, and the temporal constructors — asserting
    // the VALUE class, not just a row (a Null regression would also yield
    // one row).
    for name in ["timestamp", "rand", "e", "pi"] {
        let value = common::scalar_one(&mut conn, &format!("RETURN {name}()"));
        assert!(
            matches!(value, relate::Value::Int(_) | relate::Value::Float(_)),
            "{name}(): {value:?}"
        );
    }
    for name in ["date", "datetime"] {
        let value = common::scalar_one(&mut conn, &format!("RETURN {name}()"));
        assert!(
            matches!(value, relate::Value::Temporal(_)),
            "{name}(): {value:?}"
        );
    }
}

#[test]
fn every_other_zero_argument_call_is_an_arity_error() {
    let mut conn = common::connection();
    // The Jepsen-shrunk one-liners (LABS-657 Class C) plus the rest of the
    // scalar set the shrinker's generator draws from — including the trim
    // pair the dispatcher owns (`trim` itself is refused earlier, by the
    // grammar).
    for name in [
        "toString",
        "toInteger",
        "toBoolean",
        "toBooleanOrNull",
        "toFloat",
        "coalesce",
        "abs",
        "ceil",
        "floor",
        "sign",
        "length",
        "size",
        "left",
        "right",
        "substring",
        "replace",
        "split",
        "range",
        "reverse",
        "lTrim",
        "rTrim",
    ] {
        let text = common::err_text(conn.execute(&format!("RETURN {name}()")));
        assert!(
            text.contains("InvalidNumberOfArguments"),
            "{name}(): {text}"
        );
    }
    // `trim()` is refused before the dispatcher: the CIP-149 grammar makes
    // its argument non-optional, so the refusal is a parse error.
    let text = common::err_text(conn.execute("RETURN trim()"));
    assert!(text.contains("parse error"), "{text}");
}

/// `range()` takes two or three arguments (verified live Neo4j 2026.10.1:
/// one is "Insufficient parameters for function 'range'", four is "Too many
/// parameters", both `Neo.ClientError.Statement.SyntaxError`), and the valid
/// shapes keep their values. LABS-1307: the one-argument call used to reach
/// the implementation and report a type error.
#[test]
fn range_takes_two_or_three_arguments() {
    let mut conn = common::connection();
    for query in ["RETURN range(1)", "RETURN range(1, 2, 3, 4)"] {
        let text = common::err_text(conn.execute(query));
        assert!(
            text.contains("InvalidNumberOfArguments")
                && text.contains("requires two to three arguments"),
            "{query}: {text}"
        );
    }
    assert_eq!(
        common::scalar_one(&mut conn, "RETURN range(1, 3)"),
        common::list(vec![
            relate::Value::Int(1),
            relate::Value::Int(2),
            relate::Value::Int(3)
        ])
    );
    assert_eq!(
        common::scalar_one(&mut conn, "RETURN range(1, 5, 2)"),
        common::list(vec![
            relate::Value::Int(1),
            relate::Value::Int(3),
            relate::Value::Int(5)
        ])
    );
}

/// The high side of the same contracts (verified against Neo4j 2026.09):
/// an exact-zero builtin refuses any argument, the temporal constructors
/// accept the (value, pattern) two-argument form but refuse a third, and
/// uuid's CIP-246 construction stops at two.
#[test]
fn calls_above_their_rows_contract_are_arity_errors() {
    let mut conn = common::connection();
    for (query, requirement) in [
        ("RETURN e(1)", "requires no arguments"),
        ("RETURN rand(1)", "requires no arguments"),
        ("RETURN date(1, 2, 3)", "requires at most two arguments"),
        ("RETURN isnan(1, 2)", "requires exactly one argument"),
        // CIP-246's two-integer uuid construction stays valid; three is
        // refused (both verified live). (The temporal 2-arg literal form is
        // accepted by the front's temporal-literal lowering before the
        // dispatcher sees it — a separate lower-layer divergence, LABS-758.)
        ("RETURN uuid(1, 2, 3)", "requires at most two arguments"),
    ] {
        let text = common::err_text(conn.execute(query));
        assert!(
            text.contains("InvalidNumberOfArguments") && text.contains(requirement),
            "{query}: {text}"
        );
    }
}
