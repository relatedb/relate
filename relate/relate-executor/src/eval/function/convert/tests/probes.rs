//! The ADR-0168/0175 probe corpus, in its own submodule: the
//! `check_probes` harness that parses, lowers and evaluates whole Cypher
//! expressions against an empty graph and row, the probe-table tests that
//! pin Neo4j's conversion behavior row by row, and the one-row probes
//! that ride the same harness. Split from the direct-call unit tests so
//! neither home silently absorbs the other's additions (the 1k-creep
//! split rule, stellar round 1): everything here rides the harness.

use super::*;

/// The expected outcome of one conversion probe.
#[derive(Clone)]
enum Want {
    Is(Value),
    OutOfRange,
}

const NULL: Want = Want::Is(Value::Null);
const OUT: Want = Want::OutOfRange;
const OUT_OF_RANGE: &str = "type error: InvalidArgumentValue: toInteger()";

fn int(i: i64) -> Want {
    Want::Is(Value::Int(i))
}

fn float(f: f64) -> Want {
    Want::Is(Value::Float(f))
}

fn ints(xs: &[i64]) -> Want {
    Want::Is(Value::List(
        xs.iter()
            .copied()
            .map(Value::Int)
            .collect::<Vec<_>>()
            .into(),
    ))
}

fn list(xs: Vec<Value>) -> Want {
    Want::Is(Value::List(xs.into()))
}

/// Exact equality, except that NaN matches NaN at the top level and
/// inside lists — the only containers the corpus's NaN rows reach.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::List(xs), Value::List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys.iter()).all(|(x, y)| same(x, y))
        }
        _ => a == b,
    }
}

/// Check every `(cypher, want)` row: `cypher` is a whole expression, as
/// the ADR's tables write it, parsed by the product parser, lowered by
/// the product front-end and evaluated against an empty graph and row.
/// The key is generic over owned and borrowed strings: the table tests
/// spell literal keys, [`scalar_forms`] builds owned ones.
fn check_probes<S: AsRef<str>>(rows: Vec<(S, Want)>) {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    for (cypher, want) in rows {
        let cypher = cypher.as_ref();
        let parsed = relate_parser::parser::parse_expression_str(cypher).expect(cypher);
        let expr = relate_front::lower_expr(&parsed).expect(cypher);
        let got = crate::eval::eval(&graph, &Row::new(), &expr, &BTreeMap::new());
        match (want, got) {
            (Want::Is(want), Ok(got)) => {
                assert!(same(&got, &want), "{cypher}: got {got:?}, want {want:?}")
            }
            (Want::OutOfRange, Err(e)) => assert!(
                e.to_string().starts_with(OUT_OF_RANGE),
                "{cypher}: got error {e}, want {OUT_OF_RANGE}"
            ),
            (Want::Is(want), Err(e)) => panic!("{cypher}: got error {e}, want {want:?}"),
            (Want::OutOfRange, Ok(got)) => {
                panic!("{cypher}: got {got:?}, want error {OUT_OF_RANGE}")
            }
        }
    }
}

/// The scalar string-form quartet ADR-0175 pins per input:
/// `toInteger`/`toFloat` and their `OrNull` spellings parse the same
/// string, and every pinned row's `OrNull` outcome equals its plain one
/// (a parse failure is NULL both ways, a value the value both ways).
fn scalar_forms(input: &str, int_want: Want, float_want: Want) -> Vec<(String, Want)> {
    [
        (format!("toInteger('{input}')"), int_want.clone()),
        (format!("toFloat('{input}')"), float_want.clone()),
        (format!("toIntegerOrNull('{input}')"), int_want),
        (format!("toFloatOrNull('{input}')"), float_want),
    ]
    .into()
}

// ADR-0168 "Probe results", verified against neo4j:2026.09-community:
// every row's "Relate after" cell. Floats keep Java's `(long)` cast
// (NaN → 0, saturate at the INTEGER limits, truncate toward zero),
// out-of-range numeric strings raise NumberOutOfRange, toIntegerList()
// keeps INTEGER elements exact, and toFloat() accepts only Java's
// non-finite spellings.
#[test]
fn conversions_match_adr_0168_probe_table() {
    let (max, min) = (i64::MAX, i64::MIN);
    let (nan, inf, ninf) = (f64::NAN, f64::INFINITY, f64::NEG_INFINITY);
    check_probes(vec![
        ("toInteger(0.0/0.0)", int(0)),
        ("toInteger(1.0/0.0)", int(max)),
        ("toInteger(-1.0/0.0)", int(min)),
        ("toInteger(1e300)", int(max)),
        ("toInteger(-1e300)", int(min)),
        ("toInteger(9.3e18)", int(max)),
        ("toInteger(-9.3e18)", int(min)),
        ("toInteger(9.2e18)", int(9_200_000_000_000_000_000)),
        ("toInteger('NaN')", NULL),
        ("toInteger('nan')", NULL),
        ("toInteger('Infinity')", NULL),
        ("toInteger('-Infinity')", NULL),
        ("toInteger('inf')", NULL),
        ("toInteger('1e300')", OUT),
        ("toInteger('9.3e18')", OUT),
        ("toIntegerOrNull(0.0/0.0)", int(0)),
        ("toIntegerOrNull(1.0/0.0)", int(max)),
        ("toIntegerOrNull(-1.0/0.0)", int(min)),
        ("toIntegerOrNull(1e300)", int(max)),
        ("toIntegerOrNull('NaN')", NULL),
        ("toIntegerOrNull('1e300')", NULL),
        ("toIntegerList([0.0/0.0])", ints(&[0])),
        ("toIntegerList([1.0/0.0])", ints(&[max])),
        ("toIntegerList([-1.0/0.0])", ints(&[min])),
        ("toIntegerList([1e300])", ints(&[max])),
        (
            "toIntegerList([2.5, 9007199254740993])",
            ints(&[2, 9_007_199_254_740_993]),
        ),
        ("toFloat('NaN')", float(nan)),
        ("toFloat('-NaN')", float(nan)),
        ("toFloat('nan')", NULL),
        ("toFloat('Infinity')", float(inf)),
        ("toFloat('-Infinity')", float(ninf)),
        ("toFloat('+Infinity')", float(inf)),
        ("toFloat('infinity')", NULL),
        ("toFloat('inf')", NULL),
        ("toFloat('Inf')", NULL),
        ("toFloat('-inf')", NULL),
        ("toFloat('1e400')", float(inf)),
        ("toFloat(0.0/0.0)", float(nan)),
        ("toFloat(1.0/0.0)", float(inf)),
        ("toFloatOrNull('NaN')", float(nan)),
        ("toFloatOrNull('nan')", NULL),
        ("toFloatOrNull('Infinity')", float(inf)),
        ("toFloatOrNull('inf')", NULL),
        (
            "toFloatList([0.0/0.0, 1.0/0.0])",
            list(vec![Value::Float(nan), Value::Float(inf)]),
        ),
    ]);
}

// ADR-0168 "Supplementary probes", verified against
// neo4j:2026.09-community: the boundaries of the exact-decimal string
// rule, INTEGER list elements versus VECTOR coordinates, and the
// toFloat() spellings. `toInteger('1e3000000000')` was ADR-0168's
// deliberate deviation (Neo4j saturates, Relate errored); ADR-0175
// supersedes it — the giant exponent diverts to the Cypher25-literal
// fallback, so the value saturates like Neo4j.
#[test]
fn conversions_match_adr_0168_supplementary_probes() {
    let (max, min) = (i64::MAX, i64::MIN);
    check_probes(vec![
        ("toInteger('-1e300')", OUT),
        ("toInteger('-9.3e18')", OUT),
        ("toInteger('1e400')", OUT),
        ("toInteger('-1e400')", OUT),
        ("toInteger(' 1e300 ')", OUT),
        ("toIntegerOrNull('1e400')", NULL),
        ("toIntegerOrNull('9.3e18')", NULL),
        ("toInteger('9223372036854775807')", int(max)),
        ("toInteger('9223372036854775808')", OUT),
        ("toIntegerOrNull('9223372036854775808')", NULL),
        ("toInteger('-9223372036854775808')", int(min)),
        ("toInteger('-9223372036854775809')", OUT),
        ("toInteger('9223372036854775807.0')", int(max)),
        ("toInteger('9223372036854775807.5')", OUT),
        (
            "toInteger('9223372036854775806.5')",
            int(9_223_372_036_854_775_806),
        ),
        ("toInteger('-9223372036854775808.0')", int(min)),
        ("toInteger('-9223372036854775808.5')", OUT),
        (
            "toInteger('9007199254740993.5')",
            int(9_007_199_254_740_993),
        ),
        ("toInteger('9.2e18')", int(9_200_000_000_000_000_000)),
        ("toInteger('-0.5')", int(0)),
        ("toInteger('1e-400')", int(0)),
        // ADR-0175 supersedes ADR-0168's deliberate deviation here: the
        // giant exponent overflows the BigDecimal path's int32 gate and
        // the literal fallback's float parse saturates, as in Neo4j.
        ("toInteger('1e3000000000')", int(max)),
        ("toInteger('+Infinity')", NULL),
        ("toInteger('-NaN')", NULL),
        (
            "toIntegerList([9223372036854775806])",
            ints(&[9_223_372_036_854_775_806]),
        ),
        (
            "toIntegerList([-9223372036854775807])",
            ints(&[-9_223_372_036_854_775_807]),
        ),
        ("toIntegerList([9.3e18, -1e300])", ints(&[max, min])),
        (
            "toIntegerList([9223372036854775807, -9223372036854775808])",
            ints(&[max, min]),
        ),
        ("toFloat('+NaN')", float(f64::NAN)),
        ("toFloat('NAN')", NULL),
        ("toFloat('INFINITY')", NULL),
        ("toFloat('+inf')", NULL),
        ("toFloat('-1e400')", float(f64::NEG_INFINITY)),
        ("toFloatOrNull('-Infinity')", float(f64::NEG_INFINITY)),
        ("toFloatOrNull('Inf')", NULL),
        // VECTOR coordinates convert through FLOAT, so an INTEGER64
        // coordinate above 2^53 rounds, as in Neo4j.
        (
            "toIntegerList(vector([9007199254740993, 2], 2, INTEGER64))",
            ints(&[9_007_199_254_740_992, 2]),
        ),
        (
            "toIntegerList(vector([2.5, 1e300], 2, FLOAT64))",
            ints(&[2, max]),
        ),
    ]);
}

// ADR-0175 "Probe results", the list-elements group, verified against
// neo4j:2026.09-community: every row's "Relate after" cell. String
// elements of the list conversions parse with the scalar OrNull string
// rules and never raise (an out-of-range numeric string becomes a NULL
// element); boolean elements are 1/0 in toIntegerList() and null in
// toFloatList().
#[test]
fn conversions_match_adr_0175_list_elements() {
    let max = i64::MAX;
    let (nan, inf, ninf) = (f64::NAN, f64::INFINITY, f64::NEG_INFINITY);
    check_probes(vec![
        (
            "toIntegerList(['1', 'NaN'])",
            list(vec![Value::Int(1), Value::Null]),
        ),
        (
            "toFloatList(['1.5', 'NaN'])",
            list(vec![Value::Float(1.5), Value::Float(nan)]),
        ),
        ("toIntegerList(['1e300'])", list(vec![Value::Null])),
        (
            "toIntegerList(['9223372036854775808', 'x', 1.5])",
            list(vec![Value::Null, Value::Null, Value::Int(1)]),
        ),
        (
            "toIntegerList([' 7 ', '2.9', '-0.5'])",
            list(vec![Value::Int(7), Value::Int(2), Value::Int(0)]),
        ),
        (
            "toFloatList(['Infinity', 'inf', ' 1.5 ', '1e400'])",
            list(vec![
                Value::Float(inf),
                Value::Null,
                Value::Float(1.5),
                Value::Float(inf),
            ]),
        ),
        (
            "toIntegerList(['0x1F', '1_000', '1.5f'])",
            list(vec![Value::Int(31), Value::Int(1000), Value::Int(1)]),
        ),
        (
            "toFloatList(['0x1p3', '1.5d'])",
            list(vec![Value::Float(8.0), Value::Float(1.5)]),
        ),
        (
            "toIntegerList([true, 'true'])",
            list(vec![Value::Int(1), Value::Null]),
        ),
        (
            "toFloatList([true, 'true'])",
            list(vec![Value::Null, Value::Null]),
        ),
        (
            "toIntegerList(['1e3000000000'])",
            list(vec![Value::Int(max)]),
        ),
        ("toIntegerList(['1e400'])", list(vec![Value::Null])),
        (
            "toFloatList(['1e3000000000'])",
            list(vec![Value::Float(inf)]),
        ),
        (
            "toIntegerList(['9223372036854775807.5'])",
            list(vec![Value::Null]),
        ),
        (
            "toIntegerList(['9223372036854775807.0'])",
            list(vec![Value::Int(max)]),
        ),
        (
            "toFloatList(['0x1F', '1_000', '0o17'])",
            list(vec![
                Value::Float(31.0),
                Value::Float(1000.0),
                Value::Float(15.0),
            ]),
        ),
        (
            "toIntegerList(['nan', 'inf', '-Infinity'])",
            list(vec![Value::Null, Value::Null, Value::Null]),
        ),
        (
            "toFloatList(['nan', 'inf', '-Infinity'])",
            list(vec![Value::Null, Value::Null, Value::Float(ninf)]),
        ),
        (
            "toIntegerList([null, '42'])",
            list(vec![Value::Null, Value::Int(42)]),
        ),
        (
            "toFloatList([null, '42'])",
            list(vec![Value::Null, Value::Float(42.0)]),
        ),
        (
            "toIntegerList(['+1', '-0', '0001'])",
            list(vec![Value::Int(1), Value::Int(0), Value::Int(1)]),
        ),
        (
            "toFloatList(['.5', '5.', '+.5', '1e3'])",
            list(vec![
                Value::Float(0.5),
                Value::Float(5.0),
                Value::Float(0.5),
                Value::Float(1000.0),
            ]),
        ),
        (
            "toIntegerList(['.5', '5.', '1e3'])",
            list(vec![Value::Int(0), Value::Int(5), Value::Int(1000)]),
        ),
        (
            "toIntegerList([true, false])",
            list(vec![Value::Int(1), Value::Int(0)]),
        ),
        (
            "toFloatList([true, false])",
            list(vec![Value::Null, Value::Null]),
        ),
        ("toIntegerList(['\\t7\\n'])", list(vec![Value::Int(7)])),
        ("toIntegerList([' 0x1F '])", list(vec![Value::Int(31)])),
        ("toFloatList([' 0x1p3 '])", list(vec![Value::Float(8.0)])),
        (
            "toIntegerList(['2L', '2f'])",
            list(vec![Value::Null, Value::Null]),
        ),
        (
            "toFloatList(['2L', '2f'])",
            list(vec![Value::Null, Value::Float(2.0)]),
        ),
    ]);
}

// ADR-0175 "Probe results", the string-forms group, verified against
// neo4j:2026.09-community: the string forms follow Neo4j's pipeline:
// <= U+0020 trim, parseLong / parseDouble (hex floats, f/F/d/D suffixes,
// exact-case NaN/Infinity), the exact-decimal parse with the
// int32-exponent gate, then the Cypher25-literal fallback (Unicode trim,
// lowercase 0x/0o, underscores, suffixed decimal doubles). The full
// quartets come from [`scalar_forms`], one row per input; the rows
// ADR-0175 pins without the full quartet stay explicit below them.
#[test]
fn conversions_match_adr_0175_string_forms() {
    let (max, min) = (i64::MAX, i64::MIN);
    let (nan, inf, ninf) = (f64::NAN, f64::INFINITY, f64::NEG_INFINITY);
    let mut rows = Vec::new();
    rows.extend(scalar_forms(" 1.5 ", int(1), float(1.5)));
    rows.extend(scalar_forms("1.5f", int(1), float(1.5)));
    rows.extend(scalar_forms("1.5F", int(1), float(1.5)));
    rows.extend(scalar_forms("1.5d", int(1), float(1.5)));
    rows.extend(scalar_forms("1.5D", int(1), float(1.5)));
    rows.extend(scalar_forms("2f", NULL, float(2.0)));
    rows.extend(scalar_forms("2L", NULL, NULL));
    rows.extend(scalar_forms("0x1F", int(31), float(31.0)));
    rows.extend(scalar_forms("-0x1F", int(-31), float(-31.0)));
    rows.extend(scalar_forms("0o17", int(15), float(15.0)));
    rows.extend(scalar_forms("017", int(17), float(17.0)));
    rows.extend(scalar_forms("1_000", int(1000), float(1000.0)));
    rows.extend(scalar_forms("1_000.5", int(1000), float(1000.5)));
    rows.extend(scalar_forms("0x1p3", NULL, float(8.0)));
    rows.extend(scalar_forms("0x1.8p1", NULL, float(3.0)));
    rows.extend(scalar_forms("1e3000000000", int(max), float(inf)));
    rows.extend(scalar_forms("-1e3000000000", int(min), float(ninf)));
    rows.extend(scalar_forms("1e-3000000000", int(0), float(0.0)));
    rows.extend(scalar_forms("\\t1.5\\t", int(1), float(1.5)));
    rows.extend(scalar_forms("\\n1.5\\n", int(1), float(1.5)));
    rows.extend(scalar_forms("1e-400", int(0), float(0.0)));
    rows.extend(scalar_forms(".5f", int(0), float(0.5)));
    rows.extend(scalar_forms("-1.5f", int(-1), float(-1.5)));
    rows.extend(scalar_forms("1.5e2f", int(150), float(150.0)));
    // The rows ADR-0175 pins without the full quartet:
    rows.extend([
        ("toInteger('0x')".to_string(), NULL),
        ("toFloat('0x')".to_string(), NULL),
        ("toInteger('0X1F')".to_string(), NULL),
        ("toFloat('0X1F')".to_string(), NULL),
        ("toInteger('+0x1F')".to_string(), NULL),
        ("toFloat('+0x1F')".to_string(), NULL),
        ("toInteger('0X1P3')".to_string(), NULL),
        ("toFloat('0X1P3')".to_string(), float(8.0)),
        ("toFloat(' Infinity ')".to_string(), float(inf)),
        ("toFloat('NaN ')".to_string(), float(nan)),
        ("toFloat('\\u00A01.5\\u00A0')".to_string(), float(1.5)),
        ("toInteger('\\u00A07\\u00A0')".to_string(), int(7)),
        ("toInteger('')".to_string(), NULL),
        ("toFloat('')".to_string(), NULL),
    ]);
    // --- Cypher comments in the literal fallback ---
    // Neo4j parses the string with the Cypher parser, whose lexer skips
    // `//` line and `/* */` block comments, so these are number
    // literals. Whitespace and comments may sit around the sign.
    rows.extend([
        ("toInteger('1 // :(')".to_string(), int(1)),
        ("toInteger('-1 // :(')".to_string(), int(-1)),
        ("toInteger('/* :( */ 1 /* :( */')".to_string(), int(1)),
        ("toInteger('-/* :( */ 1 /* :( */')".to_string(), int(-1)),
        ("toFloat('1 // 2')".to_string(), float(1.0)),
        // Two tokens either side of a comment stay two tokens.
        ("toInteger('1 1')".to_string(), NULL),
        ("toInteger('1/*c*/2')".to_string(), NULL),
        // An unterminated block comment is a lexer error, not a number.
        ("toInteger('1 /* unterminated')".to_string(), NULL),
    ]);
    check_probes(rows);
}

// `toStringList()` routes through the shared list-conversion scaffold like
// `toIntegerList()`/`toFloatList()`, so a VECTOR argument is accepted; the
// scaffold normalises vector coordinates to FLOAT (ADR-0168), so an
// INTEGER64 vector renders as float strings.
#[test]
fn to_string_list_accepts_a_vector_argument() {
    check_probes(vec![(
        "toStringList(vector([1, 2], 2, INTEGER64))",
        Want::Is(Value::List(
            vec![Value::String("1.0".into()), Value::String("2.0".into())].into(),
        )),
    )]);
}
