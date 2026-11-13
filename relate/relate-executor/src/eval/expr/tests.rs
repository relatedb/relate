//! The scalar operators' unit tests, split from `expr.rs` (the same
//! `tests.rs`-beside-the-module precedent as `eval`'s and `workspace`'s
//! own split): the checked-overflow and ADR-0175 slice-bound pins, the
//! regex whole-string-match anchors, the `+` rendering and charging arms,
//! and the equality-tolerant ordering operators.

use super::*;

fn s(x: &str) -> Value {
    Value::String(x.into())
}

fn int(x: i64) -> Value {
    Value::Int(x)
}

fn ty_err(msg: &str) -> String {
    Error::ty(msg).to_string()
}

/// Live Neo4j 2026.10.1 raises 22003 for `i64::MIN / -1`; modulo keeps
/// answering 0 (the exact result, ADR-0569).
#[test]
fn integer_division_overflow_is_rejected_and_modulo_wraps() {
    assert_eq!(
        div(int(i64::MIN), int(-1)).unwrap_err().to_string(),
        ty_err("integer out of range")
    );
    assert_eq!(mod_op(int(i64::MIN), int(-1)).unwrap(), int(0));
}

/// Five characters of 1, 2, 4, 1 and 2 bytes: slice bounds count
/// characters, so a byte-indexed slice would split a code point.
const MB: &str = "añ😀bç";

fn mb_slice(start: Option<Value>, end: Option<Value>) -> Value {
    slice(&s(MB), start, end).unwrap()
}

#[test]
fn string_slice_indexes_by_character() {
    assert_eq!(mb_slice(Some(int(1)), Some(int(3))), s("ñ😀"));
    assert_eq!(mb_slice(Some(int(2)), None), s("😀bç"));
    assert_eq!(mb_slice(None, Some(int(2))), s("añ"));
    assert_eq!(mb_slice(None, None), s(MB));
    assert_eq!(mb_slice(Some(int(4)), Some(int(5))), s("ç"));
    // Integral floats are integer bounds.
    assert_eq!(
        mb_slice(Some(Value::Float(1.0)), Some(Value::Float(2.0))),
        s("ñ")
    );
}

#[test]
fn string_slice_negative_bounds_count_from_the_end() {
    assert_eq!(mb_slice(Some(int(-2)), None), s("bç"));
    assert_eq!(mb_slice(Some(int(-4)), Some(int(-1))), s("ñ😀b"));
    assert_eq!(mb_slice(None, Some(int(-3))), s("añ"));
}

#[test]
fn string_slice_out_of_range_bounds_clamp() {
    assert_eq!(mb_slice(Some(int(-10)), Some(int(10))), s(MB));
    assert_eq!(mb_slice(Some(int(3)), Some(int(99))), s("bç"));
    assert_eq!(mb_slice(Some(int(-99)), Some(int(2))), s("añ"));
}

#[test]
fn string_slice_empty_range_is_the_empty_string() {
    assert_eq!(mb_slice(Some(int(2)), Some(int(2))), s(""));
    assert_eq!(mb_slice(Some(int(4)), Some(int(1))), s(""));
    assert_eq!(mb_slice(Some(int(7)), None), s(""));
    assert_eq!(mb_slice(None, Some(int(-9))), s(""));
    assert_eq!(slice(&s(""), Some(int(0)), Some(int(1))).unwrap(), s(""));
}

#[test]
fn string_slice_null_and_non_integer_bounds() {
    assert_eq!(mb_slice(Some(Value::Null), Some(int(2))), Value::Null);
    assert_eq!(mb_slice(Some(int(1)), Some(Value::Null)), Value::Null);
    assert_eq!(mb_slice(Some(Value::Null), None), Value::Null);
    // A null bound wins over a non-integer one.
    assert_eq!(mb_slice(Some(s("x")), Some(Value::Null)), Value::Null);
    let err = |start, end| slice(&s(MB), start, end).unwrap_err().to_string();
    assert_eq!(err(Some(s("x")), None), ty_err("slice start"));
    // ADR-0175: a float bound truncates instead of erroring; a
    // non-numeric bound keeps the bare type error.
    assert_eq!(err(None, Some(Value::Bool(true))), ty_err("slice end"));
    // The start bound is checked first.
    assert_eq!(err(Some(s("x")), Some(s("y"))), ty_err("slice start"));
}

#[test]
fn list_slice_follows_the_same_bounds_rules() {
    let list = |xs: &[i64]| Value::List(xs.iter().map(|&x| int(x)).collect::<Vec<_>>().into());
    let xs = list(&[0, 1, 2, 3]);
    let sl = |start, end| slice(&xs, start, end);
    assert_eq!(sl(Some(int(1)), Some(int(3))).unwrap(), list(&[1, 2]));
    assert_eq!(sl(Some(int(-3)), Some(int(-1))).unwrap(), list(&[1, 2]));
    assert_eq!(
        sl(Some(int(-9)), Some(int(9))).unwrap(),
        list(&[0, 1, 2, 3])
    );
    assert_eq!(sl(Some(int(3)), Some(int(1))).unwrap(), list(&[]));
    assert_eq!(sl(None, Some(Value::Null)).unwrap(), Value::Null);
    assert_eq!(
        sl(Some(Value::Bool(true)), None).unwrap_err().to_string(),
        ty_err("slice start")
    );
    assert_eq!(
        sl(None, Some(s("x"))).unwrap_err().to_string(),
        ty_err("slice end")
    );
}

/// The harness pins slice bounds across the whole i64 range: a bound
/// beyond Integer.MAX_VALUE clamps instead of raising NumberOutOfRange
/// (only a literal that does not fit an i64 is a parse-time error).
#[test]
fn list_slice_bounds_clamp_at_and_beyond_the_int32_range() {
    let list = |xs: &[i64]| Value::List(xs.iter().map(|&x| int(x)).collect::<Vec<_>>().into());
    let xs = list(&[1, 2, 3, 4, 5]);
    let sl = |start, end| slice(&xs, start, end).unwrap();
    let i32_max_plus_1 = i64::from(i32::MAX) + 1;
    assert_eq!(sl(None, Some(int(i32_max_plus_1))), xs);
    assert_eq!(
        sl(Some(int(0)), Some(int(i32_max_plus_1))),
        list(&[1, 2, 3, 4, 5])
    );
    assert_eq!(
        sl(Some(int(i32_max_plus_1)), Some(int(i32_max_plus_1 + 1))),
        list(&[])
    );
    assert_eq!(sl(None, Some(int(i64::from(i32::MIN) - 1))), list(&[]));
    assert_eq!(sl(Some(int(i64::from(i32::MIN) - 1)), None), xs);
    assert_eq!(sl(None, Some(int(i64::MAX))), xs);
    assert_eq!(sl(Some(int(i64::MAX)), None), list(&[]));
    assert_eq!(sl(None, Some(int(i64::MIN))), list(&[]));
    assert_eq!(sl(Some(int(i64::MIN)), None), xs);
}

#[test]
fn add_concatenates_a_string_with_temporal_and_point_scalars() {
    let g = crate::graph::Graph::default();
    let graph = ExecGraph::read(&g);
    let point = Value::Point(relate_core::PointValue::cartesian_2d(1.0, 0.0));
    let point_text = "point({x: 1.0, y: 0.0, crs: 'cartesian'})";
    let cases: Vec<(Value, Value, String)> = vec![
        (s("hi"), duration(0, 1), "hiP1D".into()),
        (duration(0, 1), s("hi"), "P1Dhi".into()),
        (s("hi"), point.clone(), format!("hi{point_text}")),
        (point, s("hi"), format!("{point_text}hi")),
        // The scalar arms keep their old rendering.
        (s("hi"), int(2), "hi2".to_string()),
        (int(2), s("hi"), "2hi".to_string()),
    ];
    for (l, r, expected) in cases {
        assert_eq!(add(&graph, l, r).unwrap(), s(&expected), "{expected}");
    }
}

#[test]
fn slice_of_null_or_a_non_sequence() {
    assert_eq!(
        slice(&Value::Null, Some(s("x")), None).unwrap(),
        Value::Null
    );
    assert_eq!(
        slice(&int(5), Some(s("x")), None).unwrap_err().to_string(),
        ty_err("slice needs a list")
    );
}

/// A non-null non-string regex operand is a type error (22NB1), and an
/// invalid pattern is an argument error (22N11) — neither is a null
/// result.
#[test]
fn regex_rejections_carry_the_neo4j_classes() {
    let g = crate::graph::Graph::default();
    let graph = ExecGraph::read(&g);
    let run = |l: Value, r: Value| binary(&graph, BinaryOp::Regex, l, r);
    // 22NB1: a non-string operand is a type mismatch, not null.
    assert!(run(s("a"), int(42)).is_err());
    // 22N11: an invalid pattern is an argument error, not a syntax error.
    assert_eq!(
        run(s("a"), s("[")).unwrap_err(),
        Error::argument_error("invalid regular expression")
    );
    // Null still propagates.
    assert_eq!(run(Value::Null, int(42)).unwrap(), Value::Null);
}

// `=~` is a whole-string match, not a substring search.
#[test]
fn regex_anchors_at_both_ends() {
    assert!(!regex_is_match("21_val", "1_.*").unwrap());
    assert!(regex_is_match("1_val", "1_.*").unwrap());
    // An alternation stays inside the anchors.
    assert!(!regex_is_match("ab", "a|b").unwrap());
    assert!(regex_is_match("a", "a|b").unwrap());
    assert!(regex_is_match("", "x*").unwrap());
    assert!(regex_is_match("x1_val", ".*1_.*").unwrap());
    assert!(regex_is_match("abc", "abc").unwrap());
    assert!(regex_is_match("abc", "b").is_ok_and(|m| !m));
    assert!(regex_is_match("abc", "[").is_err());
}

fn duration(months: i64, days: i64) -> Value {
    Value::Temporal(relate_core::Temporal::Duration {
        months,
        days,
        nanos: 0,
    })
}

/// A CASE WHEN arm's condition is a predicate: a runtime non-boolean
/// (a property read the plan cannot type) raises the typed
/// `requires a boolean` rejection — the same one every other predicate
/// position raises (Neo4j 22G03) — never a silent fall-through to the
/// next arm or ELSE.
#[test]
fn case_when_rejects_a_runtime_non_boolean_condition() {
    use crate::exec::test_support::{exec_autocommit, try_exec_cypher};
    use crate::graph::Graph;
    let graph = Graph::default();
    exec_autocommit(&graph, "CREATE (:Person {age: 38, name: 'Alice'})").unwrap();
    for predicate in ["n.age", "n.name"] {
        let query =
            format!("MATCH (n:Person) RETURN CASE WHEN {predicate} THEN 'yes' ELSE 'no' END AS a");
        let error = try_exec_cypher(&graph, &query)
            .err()
            .unwrap_or_else(|| panic!("{predicate}: a non-boolean WHEN must be rejected"));
        assert!(
            error.to_string().contains("requires a boolean"),
            "{predicate}: {error}"
        );
    }
}

/// A CASE without an aggregate keeps its lazy branches: only the taken
/// arm evaluates, so the plain form's unselected `1/0` never runs — the
/// behavior the aggregating projection's CASE arm must match (the shared
/// [`eval_case_with`] policy is what holds the two together).
#[test]
fn plain_case_evaluates_only_the_taken_branch() {
    use crate::exec::test_support::try_exec_cypher;
    use crate::graph::Graph;
    let result = try_exec_cypher(
        &Graph::default(),
        "RETURN CASE 1 WHEN 1 THEN 1 ELSE 1/0 END AS x",
    )
    .expect("the unselected ELSE arm must not evaluate");
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

/// ADR-0463 regression (rust review round 1): a mixed list+string
/// operand pair builds a **list** (`other + 1` slots), not a string, so
/// its charge must be the list buffer. The former estimator's wildcard
/// string arms matched first and charged ~len+32 bytes, letting
/// `bigList + 'x'` bypass the budget.
#[test]
fn mixed_list_and_string_charges_the_list_buffer() {
    let g = crate::graph::Graph::default();
    let graph = ExecGraph::read(&g);
    let list = Value::List(vec![int(1), int(2), int(3)].into());
    let out = add(&graph, list, s("x")).unwrap();
    assert!(matches!(out, Value::List(_)), "{out:?}");
    assert_eq!(
        graph.expression_built_bytes(),
        crate::workspace::buffer_bytes(4, std::mem::size_of::<Value>() as u64,)
    );
}

/// The same pair under a one-byte cap: the list buffer allocation (one
/// slot per element plus the header) is refused, where the old
/// string-shaped estimate would have slipped through.
#[test]
fn mixed_list_and_string_is_refused_when_the_buffer_exceeds_the_cap() {
    let mut g = crate::graph::Graph::default();
    g.configure_expression_memory_limit(Some(1));
    let graph = ExecGraph::read(&g);
    let list = Value::List(vec![int(1), int(2), int(3)].into());
    let error = add(&graph, list, s("x")).unwrap_err();
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

fn cmp(op: BinaryOp, l: Value, r: Value) -> Value {
    let g = crate::graph::Graph::default();
    let graph = ExecGraph::read(&g);
    binary(&graph, op, l, r).unwrap()
}

// Durations have `=` but no ordering: `<`/`>` are null, `<=`/`>=` still
// hold for equal values (TCK TernaryComparisonAcceptance).
#[test]
fn equal_durations_satisfy_the_equality_tolerant_operators() {
    let d = duration(0, 1);
    assert_eq!(cmp(BinaryOp::Lt, d.clone(), d.clone()), Value::Null);
    assert_eq!(cmp(BinaryOp::Gt, d.clone(), d.clone()), Value::Null);
    assert_eq!(cmp(BinaryOp::Lte, d.clone(), d.clone()), Value::Bool(true));
    assert_eq!(cmp(BinaryOp::Gte, d.clone(), d.clone()), Value::Bool(true));
    // Different durations are incomparable in every direction.
    let other = duration(0, 2);
    for op in [BinaryOp::Lt, BinaryOp::Gt, BinaryOp::Lte, BinaryOp::Gte] {
        assert_eq!(cmp(op, d.clone(), other.clone()), Value::Null);
    }
}

/// Neo4j's `-` takes a duration right operand only: subtracting one
/// temporal from another is the 22N01 type mismatch, not a duration
/// between (`duration.between` answers that).
#[test]
fn subtracting_two_temporals_is_a_type_mismatch() {
    // One 2020-01-01 fixture serves both assertions.
    let date = Value::Temporal(temporal::parse_iso("date", "2020-01-01").unwrap());
    let error = sub(date.clone(), date.clone()).unwrap_err();
    assert_eq!(error.to_string(), ty_err("- type mismatch"));

    // A temporal minus a duration is still arithmetic.
    assert_eq!(
        sub(date, duration(0, 1)).unwrap(),
        Value::Temporal(temporal::parse_iso("date", "2019-12-31").unwrap())
    );
}

// UUIDs have `=` but no ordering either.
#[test]
fn equal_uuids_satisfy_the_equality_tolerant_operators() {
    let a = Value::Uuid(1);
    let b = Value::Uuid(2);
    assert_eq!(cmp(BinaryOp::Gt, a.clone(), b.clone()), Value::Null);
    assert_eq!(cmp(BinaryOp::Lt, b.clone(), a.clone()), Value::Null);
    assert_eq!(cmp(BinaryOp::Gte, a.clone(), a.clone()), Value::Bool(true));
    assert_eq!(cmp(BinaryOp::Lte, a.clone(), a.clone()), Value::Bool(true));
}
