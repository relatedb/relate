//! Direct-call unit tests for the conversion builtins. The two test
//! homes keep separate expectation vocabularies: this module's tables
//! state one rendered string per value; the probe corpus in [`probes`]
//! keeps its own `Want` (a whole-expression outcome). The exact-decimal
//! parse-grammar test lives in `string.rs`'s own test module, beside the
//! grammar it pins.

use super::super::function;
use super::*;
use crate::graph::Graph;
use crate::row::Row;
use std::collections::BTreeMap;

use relate_ir::Expr;
use relate_names::SmallName;

mod probes;

#[test]
fn to_boolean_or_null_and_to_string_or_null_match_neo4j() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let row = Row::new();
    let params = BTreeMap::new();
    let call =
        |name: &str, arg: Expr| function(&graph, &row, &params, name, false, &[arg]).unwrap();

    // success passthrough
    assert_eq!(
        call("toBooleanOrNull", Expr::String("true".into())),
        Value::Bool(true)
    );
    assert_eq!(
        call("toBooleanOrNull", Expr::Bool(false)),
        Value::Bool(false)
    );
    // INTEGER: zero is false, every other integer true (`toBoolean(0)`
    // never raises).
    assert_eq!(call("toBoolean", Expr::Int(0)), Value::Bool(false));
    assert_eq!(call("toBoolean", Expr::Int(1)), Value::Bool(true));
    assert_eq!(call("toBoolean", Expr::Int(-7)), Value::Bool(true));
    // parse failure and null stay null
    assert_eq!(
        call("toBooleanOrNull", Expr::String("nope".into())),
        Value::Null
    );
    assert_eq!(call("toBooleanOrNull", Expr::Null), Value::Null);
    assert_eq!(
        call("toStringOrNull", Expr::Int(7)),
        Value::String("7".into())
    );
    assert_eq!(call("toStringOrNull", Expr::Null), Value::Null);
}

/// The value-level rendering contract, one table: every list value renders
/// — whatever its element classes, and through a literal or a variable
/// alike — identically for `toString` and `toStringOrNull` (ADR-0688; the
/// literal rows pin the synced corpus's expectations
/// exactly). The one kind with no string form — a byte array — is the
/// explicit refusal below the table.
#[test]
fn tostring_renders_every_list_value() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let params = BTreeMap::new();
    let mut row = Row::new();
    row.insert(
        "xs".into(),
        Value::List(vec![Value::Int(1), Value::Int(2)].into()),
    );
    row.insert("bytes".into(), Value::Bytes(vec![1, 2].into()));
    let call = |name: &str, arg: &Expr| {
        function(
            &graph,
            &row,
            &params,
            name,
            false,
            std::slice::from_ref(arg),
        )
    };

    for (what, arg, expected) in [
        (
            "mixed",
            Expr::List(vec![Expr::Int(1), Expr::String("a".into())]),
            "[1, a]",
        ),
        ("empty", Expr::List(Vec::new()), "[]"),
        (
            "with null",
            Expr::List(vec![Expr::Int(1), Expr::Null, Expr::Int(2)]),
            "[1, null, 2]",
        ),
        // INTEGER and FLOAT are one numeric class, and it renders here too.
        (
            "numeric classes",
            Expr::List(vec![Expr::Int(1), Expr::Float(2.0)]),
            "[1, 2.0]",
        ),
        // The homogeneous literal shapes the corpus pins (ADR-0688): the
        // seven that the retired static rejection used to refuse.
        (
            "integer list",
            Expr::List(vec![Expr::Int(1), Expr::Int(2)]),
            "[1, 2]",
        ),
        (
            "string list",
            Expr::List(vec![Expr::String("a".into()), Expr::String("b".into())]),
            "[a, b]",
        ),
        (
            "boolean list",
            Expr::List(vec![Expr::Bool(true), Expr::Bool(false)]),
            "[true, false]",
        ),
        (
            "float list",
            Expr::List(vec![Expr::Float(1.0), Expr::Float(2.0)]),
            "[1.0, 2.0]",
        ),
        (
            "nested list",
            Expr::List(vec![
                Expr::List(vec![Expr::Int(1), Expr::Int(2)]),
                Expr::List(vec![Expr::Int(3)]),
            ]),
            "[[1, 2], [3]]",
        ),
        (
            "list of maps",
            Expr::List(vec![
                Expr::Map(vec![("a".into(), Expr::Int(1))]),
                Expr::Map(vec![("b".into(), Expr::Int(2))]),
            ]),
            "[{a: 1}, {b: 2}]",
        ),
        ("variable", Expr::Variable("xs".into()), "[1, 2]"),
    ] {
        for name in ["toString", "toStringOrNull"] {
            assert_eq!(
                call(name, &arg).unwrap(),
                Value::String(expected.into()),
                "{name} of {what}"
            );
        }
    }

    // The one kind with no string form: `toString` raises the value-level
    // type error with its authored class; `toStringOrNull` is null.
    let bytes = Expr::Variable("bytes".into());
    let err = call("toString", &bytes).expect_err("toString of a byte array");
    assert!(
        err.to_string()
            .starts_with("type error: InvalidArgumentValue: toString()"),
        "toString of a byte array: wrong error class: {err}"
    );
    assert_eq!(
        call("toStringOrNull", &bytes).unwrap(),
        Value::Null,
        "toStringOrNull of a byte array"
    );
}

/// The non-list container and scalar rendering, through the one display
/// policy: a map renders, and a scalar renders through the same
/// policy. Lists are the table test above.
#[test]
fn tostring_map_and_scalar_render_in_display_form() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let row = Row::new();
    let params = BTreeMap::new();
    let call = |name: &str, arg: Expr| function(&graph, &row, &params, name, false, &[arg]);
    let map = Expr::Map(vec![("a".into(), Expr::Int(1))]);

    assert_eq!(
        call("toStringOrNull", map).unwrap(),
        Value::String("{a: 1}".into())
    );

    // Scalars render through the same one display policy.
    assert_eq!(
        call("toString", Expr::Int(7)).unwrap(),
        Value::String("7".into())
    );
}

#[test]
fn to_integer_or_null_and_to_float_or_null_match_neo4j() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let row = Row::new();
    let params = BTreeMap::new();
    let call =
        |name: &str, arg: Expr| function(&graph, &row, &params, name, false, &[arg]).unwrap();

    // Success cases behave exactly like toInteger()/toFloat().
    assert_eq!(
        call("toIntegerOrNull", Expr::String("42".into())),
        Value::Int(42)
    );
    assert_eq!(
        call("toIntegerOrNull", Expr::String("3.5".into())),
        Value::Int(3)
    );
    assert_eq!(call("toIntegerOrNull", Expr::Int(7)), Value::Int(7));
    assert_eq!(call("toIntegerOrNull", Expr::Float(2.9)), Value::Int(2));
    assert_eq!(call("toIntegerOrNull", Expr::Null), Value::Null);

    assert_eq!(
        call("toFloatOrNull", Expr::String("3.5".into())),
        Value::Float(3.5)
    );
    assert_eq!(
        call("toFloatOrNull", Expr::String("42".into())),
        Value::Float(42.0)
    );
    assert_eq!(call("toFloatOrNull", Expr::Int(3)), Value::Float(3.0));
    assert_eq!(call("toFloatOrNull", Expr::Float(1.25)), Value::Float(1.25));
    assert_eq!(call("toFloatOrNull", Expr::Null), Value::Null);

    // Parse failures and non-convertible inputs yield null, never an error.
    assert_eq!(
        call("toIntegerOrNull", Expr::String("abc".into())),
        Value::Null
    );
    assert_eq!(
        call("toFloatOrNull", Expr::String("nope".into())),
        Value::Null
    );
    // Neo4j converts booleans to 1/0 for toInteger (live 2026.10.1);
    // toFloat has no boolean arm.
    assert_eq!(call("toIntegerOrNull", Expr::Bool(true)), Value::Int(1));
    assert_eq!(call("toFloatOrNull", Expr::Bool(true)), Value::Null);
    assert_eq!(
        call("toIntegerOrNull", Expr::List(vec![Expr::Int(1)])),
        Value::Null
    );
    assert_eq!(
        call("toFloatOrNull", Expr::List(vec![Expr::Int(1)])),
        Value::Null
    );

    // The base functions keep the same arms: toInteger converts a boolean
    // (live 2026.10.1: toInteger(true) is 1), toFloat has no boolean arm.
    assert_eq!(
        function(
            &graph,
            &row,
            &params,
            "toInteger",
            false,
            &[Expr::Bool(true)]
        )
        .unwrap(),
        Value::Int(1)
    );
    assert!(function(&graph, &row, &params, "toFloat", false, &[Expr::Bool(true)]).is_err());
}

// ADR-0448: the rendering policy is a property of the value — one
// full-precision rendering, through the value layer's display policy,
// whether the temporal is top-level, nested in a map, or built by any
// spelling. Top-level and nested are both pinned so the two paths
// cannot drift apart again.
#[test]
fn to_string_renders_temporals_at_full_precision() {
    let t = relate_core::temporal::datetime_from_epoch(1_709_164_800, 0).unwrap();
    let full = t.full_seconds_string();
    assert_eq!(
        to_string_value(Some(&Value::Temporal(t.clone()))),
        Ok(Value::String(full.clone().into()))
    );
    let nested = Value::Map(BTreeMap::from([("at".to_string(), Value::Temporal(t))]).into());
    assert_eq!(
        to_string_value(Some(&nested)),
        Ok(Value::String(format!("{{at: {full}}}").into()))
    );
}

// `toStringList()`: non-scalar elements and null
// itself render in display form, the one behavior.
#[test]
fn to_string_list_renders_non_scalar_elements_in_display_form() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let list = |xs: Vec<Value>| Value::List(xs.into());
    let mixed = || {
        list(vec![
            Value::Int(1),
            list(vec![Value::Int(2), Value::Int(3)]),
            Value::Null,
        ])
    };

    assert_eq!(
        bi_tostringlist(&graph, "toStringList", vec![mixed()]),
        Ok(list(vec![s("1"), s("[2, 3]"), s("null")]))
    );
    // Null-in-null-out, and only a list is admissible.
    assert_eq!(
        bi_tostringlist(&graph, "toStringList", vec![Value::Null]),
        Ok(Value::Null)
    );
    assert!(bi_tostringlist(&graph, "toStringList", vec![Value::Int(1)]).is_err());
}

/// LABS-785: `toString`/`toStringOrNull` take exactly one argument. A second
/// argument is Neo4j's 42I13 arity error, never silently ignored.
#[test]
fn tostring_rejects_a_second_argument() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    for name in ["toString", "toStringOrNull"] {
        let err = function(
            &graph,
            &Row::new(),
            &BTreeMap::new(),
            name,
            false,
            &[Expr::Int(7), Expr::Int(8)],
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("InvalidNumberOfArguments"),
            "{name}: {err}"
        );
    }
}

/// WB5: the arity contract is decided from the row *before* any argument is
/// evaluated, so `toString(1, boom())` reports the arity, never the
/// argument's own error. The `split` pair covers the exactly-two contract.
#[test]
fn arity_is_reported_before_any_argument_is_evaluated() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let boom = Expr::Function {
        name: SmallName::from_name("no_such_function"),
        distinct: false,
        args: Vec::new(),
    };
    let string = |value: &str| Expr::String(value.to_string());
    for (name, args) in [
        ("toString", vec![Expr::Int(1), boom.clone()]),
        ("toStringOrNull", vec![Expr::Int(1), boom.clone()]),
        ("split", vec![string("a,b"), string(","), boom.clone()]),
        ("split", vec![string("a,b")]),
        ("split", vec![string("a"), string(","), string("b")]),
    ] {
        let err = function(&graph, &Row::new(), &BTreeMap::new(), name, false, &args).unwrap_err();
        assert!(
            err.to_string().contains("InvalidNumberOfArguments"),
            "{name}: {err}"
        );
    }
}
