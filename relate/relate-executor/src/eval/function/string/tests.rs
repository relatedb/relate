use super::super::render_scalar;
use super::*;
use crate::graph::Graph;

#[test]
fn string_interp_concatenates_every_coerced_part() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let interp = |vals: Vec<Value>| bi_string_interp(&graph, "string.interp", vals);
    let s = |v: &str| Value::String(v.into());

    assert_eq!(
        interp(vec![
            s("n="),
            Value::Int(1),
            s(", s="),
            s("añ😀"),
            s(", f="),
            Value::Float(2.0),
            s(", "),
            Value::Bool(true),
        ])
        .unwrap(),
        s("n=1, s=añ😀, f=2.0, true")
    );
    assert_eq!(interp(vec![]).unwrap(), s(""));
    assert_eq!(interp(vec![s("a"), Value::Null]).unwrap(), Value::Null);
    assert!(interp(vec![s("a"), Value::List(vec![Value::Int(1)].into())]).is_err());
}

#[test]
fn string_interp_renders_every_scalar_kind_in_order() {
    use relate_core::{PointValue, Temporal, VectorCoordKind};
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let interp = |vals: Vec<Value>| bi_string_interp(&graph, "string.interp", vals);
    let s = |v: &str| Value::String(v.into());
    let uuid = Value::Uuid(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
    let date = Value::Temporal(Temporal::Date(
        chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap(),
    ));
    let point = Value::Point(PointValue::cartesian_2d(1.5, -2.0));
    let vector = Value::Vector {
        coord: VectorCoordKind::Int64,
        values: vec![Value::Int(1), Value::Int(2)].into(),
    };

    // Each part renders as its toString() form, except a Vector, which
    // uses its display form.
    let parts = vec![
        Value::Int(-42),
        s("|"),
        Value::Float(0.5),
        s("|"),
        Value::Bool(false),
        s("|ñ|"),
        uuid.clone(),
        s("|"),
        date.clone(),
        s("|"),
        point.clone(),
        s("|"),
        vector.clone(),
    ];
    let expected = format!(
        "-42|0.5|false|ñ|{}|{}|{}|{}",
        render_scalar(&uuid).unwrap(),
        render_scalar(&date).unwrap(),
        render_scalar(&point).unwrap(),
        vector.to_display_string(),
    );
    assert_eq!(interp(parts).unwrap(), s(&expected));
    assert_eq!(
        interp(vec![uuid.clone()]).unwrap(),
        s("01234567-89ab-cdef-0123-456789abcdef")
    );
    assert_eq!(interp(vec![date]).unwrap(), s("2024-02-29"));

    // A lone non-STRING part renders on its own.
    assert_eq!(interp(vec![Value::Int(7)]).unwrap(), s("7"));
    assert_eq!(interp(vec![Value::Null]).unwrap(), Value::Null);
    // The first null or uncoercible part, in order, decides the result.
    let list = Value::List(vec![Value::Int(1)].into());
    assert_eq!(
        interp(vec![s("a"), Value::Null, list.clone()]).unwrap(),
        Value::Null
    );
    let err = interp(vec![s("a"), list.clone(), Value::Null]).unwrap_err();
    assert!(
        err.to_string().contains("cannot coerce value to STRING"),
        "{err}"
    );
    assert!(interp(vec![Value::Map(Default::default())]).is_err());
}

/// Five characters of 1, 2, 4, 1 and 2 bytes: positions count
/// characters, so a byte-indexed cut would split a code point.
const MB: &str = "añ😀bç";

#[test]
fn substring_counts_characters_and_clamps_at_the_boundaries() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let int = Value::Int;
    let sub = |vals: Vec<Value>| bi_substring(&graph, "substring", vals).unwrap();

    assert_eq!(sub(vec![s(MB), int(0)]), s(MB));
    assert_eq!(sub(vec![s(MB), int(0), int(2)]), s("añ"));
    assert_eq!(sub(vec![s(MB), int(1), int(2)]), s("ñ😀"));
    assert_eq!(sub(vec![s(MB), int(2)]), s("😀bç"));
    assert_eq!(sub(vec![s(MB), int(4), int(1)]), s("ç"));
    // A length beyond the end stops at the end.
    assert_eq!(sub(vec![s(MB), int(0), int(99)]), s(MB));
    assert_eq!(sub(vec![s(MB), int(3), int(i64::from(i32::MAX))]), s("bç"));
    assert_eq!(
        sub(vec![
            s(MB),
            int(i64::from(i32::MAX)),
            int(i64::from(i32::MAX))
        ]),
        s("")
    );
    // A zero length, or a start at or past the end, is empty.
    assert_eq!(sub(vec![s(MB), int(0), int(0)]), s(""));
    assert_eq!(sub(vec![s(MB), int(2), int(0)]), s(""));
    assert_eq!(sub(vec![s(MB), int(5)]), s(""));
    assert_eq!(sub(vec![s(MB), int(9), int(2)]), s(""));
    assert_eq!(sub(vec![s(""), int(0), int(1)]), s(""));
    // ADR-0175: a negative or beyond-int32 position is NumberOutOfRange;
    // a float truncates; a missing or non-numeric position is ignored.
    let oor = |vals: Vec<Value>| {
        bi_substring(&graph, "substring", vals)
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        oor(vec![s(MB), int(-3), int(2)]),
        Error::ty("NumberOutOfRange: substring() start").to_string()
    );
    assert_eq!(
        oor(vec![s(MB), int(1), int(-1)]),
        Error::ty("NumberOutOfRange: substring() length").to_string()
    );
    assert_eq!(
        oor(vec![s(MB), int(i64::MAX)]),
        Error::ty("NumberOutOfRange: substring() start").to_string()
    );
    assert_eq!(
        sub(vec![s(MB), Value::Float(1.0), Value::Float(1.0)]),
        s("ñ")
    );
    // A null start or length nulls the call, before the sibling's range
    // check (live Neo4j 2026.10.1: both `substring('HELLO', null, 2^40)` and
    // `substring('HELLO', -1, null)` are null, never a range error).
    assert_eq!(sub(vec![s(MB), Value::Null, Value::Null]), Value::Null);
    assert_eq!(sub(vec![s(MB), Value::Null]), Value::Null);
    assert_eq!(sub(vec![s(MB), Value::Null, int(1)]), Value::Null);
    assert_eq!(sub(vec![s(MB), int(0), Value::Null]), Value::Null);
    assert_eq!(
        sub(vec![s(MB), Value::Null, int(3708444074355087135)]),
        Value::Null
    );
    assert_eq!(sub(vec![s(MB), int(-1), Value::Null]), Value::Null);
    // The string argument's type check still runs before the null guard.
    assert_eq!(
        bi_substring(&graph, "substring", vec![int(1), Value::Null])
            .unwrap_err()
            .to_string(),
        Error::ty("substring() needs a string").to_string()
    );
    // Null input is null; any other non-string input is an error.
    assert_eq!(sub(vec![Value::Null, int(1)]), Value::Null);
    assert_eq!(sub(vec![]), Value::Null);
    assert_eq!(
        bi_substring(&graph, "substring", vec![int(1), int(0)])
            .unwrap_err()
            .to_string(),
        Error::ty("substring() needs a string").to_string()
    );
}

#[test]
fn left_and_right_count_characters_and_clamp_at_the_boundaries() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let int = Value::Int;
    let left = |vals: Vec<Value>| bi_left(&graph, "left", vals);
    let right = |vals: Vec<Value>| bi_right(&graph, "right", vals);

    assert_eq!(left(vec![s(MB), int(3)]).unwrap(), s("añ😀"));
    assert_eq!(right(vec![s(MB), int(3)]).unwrap(), s("😀bç"));
    assert_eq!(left(vec![s(MB), int(1)]).unwrap(), s("a"));
    assert_eq!(right(vec![s(MB), int(1)]).unwrap(), s("ç"));
    // A length beyond the end is the whole string.
    assert_eq!(left(vec![s(MB), int(5)]).unwrap(), s(MB));
    assert_eq!(right(vec![s(MB), int(99)]).unwrap(), s(MB));
    assert_eq!(left(vec![s(MB), int(i64::from(i32::MAX))]).unwrap(), s(MB));
    assert_eq!(right(vec![s(MB), int(i64::from(i32::MAX))]).unwrap(), s(MB));
    // A zero or missing length is empty; a null one nulls the call.
    for n in [vec![int(0)], vec![]] {
        let mut vals = vec![s(MB)];
        vals.extend(n);
        assert_eq!(left(vals.clone()).unwrap(), s(""));
        assert_eq!(right(vals).unwrap(), s(""));
    }
    // Live Neo4j 2026.10.1: left('HELLO', null) is null, and the null wins
    // over the sibling argument's range error (`left(null, -1)` is null).
    assert_eq!(left(vec![s(MB), Value::Null]).unwrap(), Value::Null);
    assert_eq!(right(vec![s(MB), Value::Null]).unwrap(), Value::Null);
    assert_eq!(left(vec![Value::Null, int(-2)]).unwrap(), Value::Null);
    assert_eq!(
        right(vec![Value::Null, int(i64::MAX)]).unwrap(),
        Value::Null
    );
    // ADR-0175: a negative or beyond-int32 length is NumberOutOfRange.
    for n in [int(-2), int(i64::MAX)] {
        assert_eq!(
            left(vec![s(MB), n.clone()]).unwrap_err().to_string(),
            Error::ty("NumberOutOfRange: left() length").to_string()
        );
        assert_eq!(
            right(vec![s(MB), n]).unwrap_err().to_string(),
            Error::ty("NumberOutOfRange: right() length").to_string()
        );
    }
    assert_eq!(left(vec![s(""), int(2)]).unwrap(), s(""));
    assert_eq!(right(vec![s(""), int(2)]).unwrap(), s(""));
    // Null input is null; any other non-string input is an error.
    assert_eq!(left(vec![Value::Null, int(2)]).unwrap(), Value::Null);
    assert_eq!(right(vec![Value::Null, int(2)]).unwrap(), Value::Null);
    let ty = Error::ty("expected string argument").to_string();
    assert_eq!(left(vec![int(1), int(2)]).unwrap_err().to_string(), ty);
    assert_eq!(right(vec![int(1), int(2)]).unwrap_err().to_string(), ty);
}

/// Neo4j counts code points, not bytes: `string.indexOf('a😀b','b')` is 2
/// even though `b` starts at byte 5. The byte-offset result was the
/// supplementary-character family's remaining split/indexOf asymmetry.
#[test]
fn string_indexof_counts_characters_not_bytes() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let index_of = |input: &str, needle: &str| {
        bi_string_indexof(&graph, "string.indexOf", vec![s(input), s(needle)]).unwrap()
    };
    assert_eq!(index_of("a😀b", "b"), Value::Int(2));
    assert_eq!(index_of("𐀀x", "x"), Value::Int(1));
    assert_eq!(index_of("Hello world", " "), Value::Int(5));
    assert_eq!(index_of("hello", "lo"), Value::Int(3));
    // No match keeps the -1 sentinel; an empty needle sits at character 0.
    assert_eq!(index_of("hello", "a"), Value::Int(-1));
    assert_eq!(index_of("", ""), Value::Int(0));
}

/// Neo4j evaluates `string.regexReplace` with Java `Matcher.replaceAll`, so
/// the replacement's escapes are Java's: `\$` is a literal dollar while
/// `$n`/`${name}` stay group references. The regex crate's own replacement
/// syntax treats `\$` as a backslash plus an empty group reference and
/// produced `\$b` for the `'a$b'` scenario.
#[test]
fn regex_replace_expands_java_style_replacements() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let replace = |input: &str, pattern: &str, replacement: &str| {
        bi_string_regexreplace(
            &graph,
            "string.regexReplace",
            vec![s(input), s(pattern), s(replacement)],
        )
    };
    assert_eq!(replace("a$b", "a", "\\$X").unwrap(), s("$X$b"));
    assert_eq!(
        replace("hello world", "(\\w+) (\\w+)", "$2 $1").unwrap(),
        s("world hello")
    );
    assert_eq!(
        replace("hello", "(?<word>l+)", "${word}${word}").unwrap(),
        s("hellllo")
    );
    // An escaped backslash is one literal backslash (`\` has no other
    // meaning outside a group reference).
    assert_eq!(replace("a", "a", "\\\\").unwrap(), s("\\"));
    // Java rejects a `$` that starts no group reference, and a group number
    // past the pattern's group count.
    assert!(replace("abc", "b", "$").is_err());
    assert!(replace("abc", "b", "$x").is_err());
    assert!(replace("abc", "b", "$9").is_err());
    // A group that exists but did not participate in this match is Java's
    // literal text `null` — not an error.
    assert_eq!(replace("abc", "(a)|(x)", "$2").unwrap(), s("nullbc"));
}

/// Neo4j's list-of-delimiters `split` (`StringValue.split(List<String>)`):
/// a delimiter equal to the whole input yields two empty parts, an empty
/// delimiter splits per character after the other delimiters are removed,
/// and otherwise the input is cut at the earliest delimiter occurrence.
/// The old regex-alternation split produced `['','a','𐀀','']` for
/// `split('a𐀀b', ['b',''])`.
#[test]
fn split_by_a_delimiter_list_matches_neo4j() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let list = |parts: Vec<&str>| Value::List(parts.into_iter().map(s).collect::<Vec<_>>().into());
    let split = |source: &str, delimiters: &[&str]| {
        bi_split(&graph, "split", vec![s(source), list(delimiters.to_vec())]).unwrap()
    };
    // An empty delimiter wins: the others are removed first, then the
    // remainder is split per character.
    assert_eq!(split("a𐀀b", &["b", ""]), list(vec!["a", "𐀀"]));
    // A delimiter equal to the whole input yields two empty parts.
    assert_eq!(split("b", &["b", ""]), list(vec!["", ""]));
    assert_eq!(split("ab", &["ab"]), list(vec!["", ""]));
    // Otherwise the earliest delimiter occurrence cuts, keeping empties.
    assert_eq!(split("a,b;c", &[",", ";"]), list(vec!["a", "b", "c"]));
    assert_eq!(split(",a", &[","]), list(vec!["", "a"]));
    // An empty delimiter list leaves the input whole.
    assert_eq!(split("ab", &[]), list(vec!["ab"]));
}

/// The list-of-delimiters split scans the input once: every position a part
/// consumes is never examined again. Re-running `find` on the remaining tail
/// per delimiter re-scanned the whole tail for every emitted part, making
/// `split(repeat('a', n), ['a', 'b'])` quadratic — this 100 000 character
/// input (one empty part per character) stays a single pass.
#[test]
fn split_by_a_delimiter_list_scans_a_long_input_once() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let source = "a".repeat(100_000);
    let delimiters = Value::List(vec![Value::String("a".into()), Value::String("b".into())].into());
    let result = bi_split(
        &graph,
        "split",
        vec![Value::String(source.into()), delimiters],
    )
    .expect("the split must complete");
    let Value::List(parts) = result else {
        panic!("split returns a list");
    };
    assert_eq!(parts.len(), 100_001);
    assert!(parts
        .iter()
        .all(|part| matches!(part, Value::String(text) if text.is_empty())));
}

#[test]
fn split_on_the_empty_delimiter_yields_each_character() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    assert_eq!(
        bi_split(&graph, "split", vec![s("añ😀b"), s("")]).unwrap(),
        Value::List(vec![s("a"), s("ñ"), s("😀"), s("b")].into())
    );
    assert_eq!(
        bi_split(&graph, "split", vec![s(""), s("")]).unwrap(),
        Value::List(Vec::new().into())
    );
    // The list shape must agree with the scalar shape for empty input: the
    // whole-input rule does not apply when the input is empty, so an empty
    // delimiter still yields no parts rather than two empty ones.
    assert_eq!(
        bi_split(
            &graph,
            "split",
            vec![s(""), Value::List(vec![s("")].into())],
        )
        .unwrap(),
        Value::List(Vec::new().into())
    );
}

/// Any argument count other than two is the canonical 42I13, decided by the
/// row's `ARITY_EXACTLY_TWO` contract before the implementation runs — the
/// implementation indexes `vals[0]`/`vals[1]` directly, so the dispatch
/// layer is the sole owner of the count.
#[test]
fn split_with_the_wrong_argument_count_is_an_arity_error() {
    use std::collections::BTreeMap;

    use relate_ir::Expr;

    use crate::eval::{function, Row};

    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let string = |value: &str| Expr::String(value.to_string());
    let cases: Vec<(&str, Vec<Expr>)> = vec![
        ("no arguments", vec![]),
        ("missing delimiter", vec![string("a,b")]),
        (
            "extra argument",
            vec![string("a,b"), string(","), string("x")],
        ),
    ];
    for (label, args) in cases {
        let err = function(
            &graph,
            &Row::new(),
            &BTreeMap::new(),
            "split",
            false,
            &args,
            None,
        )
        .expect_err(label);
        assert!(
            err.to_string().contains("InvalidNumberOfArguments"),
            "{label}: expected the canonical arity error, got {err}"
        );
    }
}

/// ADR-0463 round 2: the `regexReplace` pre-bound covers empty-matching
/// patterns and empty inputs. A tiny configured cap (10 bytes) makes
/// both incidences refuse; the old `input.len() * (replacement.len()+1)`
/// bound charged 0 for an empty pattern or empty input and would have
/// let the replacement allocation through.
#[test]
fn regex_replace_bound_covers_empty_pattern_and_empty_input() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(10));
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let replacement = "01234567890123456789"; // 20 bytes
                                              // Empty pattern on a one-byte input: (1+1) * (20+1) = 42 > 10.
    let error = bi_string_regexreplace(
        &graph,
        "string.regexReplace",
        vec![s("x"), s(""), s(replacement)],
    )
    .expect_err("the empty-pattern bound must refuse");
    assert!(
        matches!(
            error,
            Error::Capacity {
                limit: 10,
                kind: relate_core::CapacityKind::Expression,
                ..
            }
        ),
        "{error}"
    );
    // Empty input: (0+1) * (20+1) = 21 > 10 (the old bound charged 0).
    let error = bi_string_regexreplace(
        &graph,
        "string.regexReplace",
        vec![s(""), s("x"), s(replacement)],
    )
    .expect_err("the empty-input bound must refuse");
    assert!(
        matches!(
            error,
            Error::Capacity {
                limit: 10,
                kind: relate_core::CapacityKind::Expression,
                ..
            }
        ),
        "{error}"
    );
}

/// ADR-0463: every string builtin charges its projected allocation
/// before producing it. With the statement budget already spent, even a
/// tiny input is refused with the typed capacity error — the pin that
/// each site charges at all (and before, not after, its allocation).
/// `split`'s bound charges every part's own `Arc<str>` header: with a cap
/// sized to the payload alone, a wide split refuses — the bound the
/// reviewer's finding asked for (parts × header + payload + buffer).
#[test]
fn split_bound_charges_every_parts_string_header() {
    let mut g = Graph::default();
    // 100 bytes: enough for the payload+buffer figure of a 60-byte source
    // (~60 + 61 slots x 24 = ~1.5 KB would refuse at any small cap, so use a
    // cap just above the payload-free bound but below the per-part-header
    // total). The empty delimiter yields one part per character.
    g.configure_expression_memory_limit(Some(1_500));
    let graph = ExecGraph::read(&g);
    let source = "x".repeat(60);
    let err = super::bi_split(
        &graph,
        "split",
        vec![Value::String(source.into()), Value::String("".into())],
    )
    .expect_err("the per-part headers must push the split over the cap");
    assert!(
        matches!(err, Error::Capacity { .. }),
        "expected a typed capacity refusal, got {err}"
    );
}

/// S1/X1 (availability): the delimiter scan tests every delimiter byte at
/// every source position — O(source × Σ delimiter lengths) — so the bound
/// charges that. With a cap that comfortably fits the payload and a short
/// delimiter, a list of long delimiters is refused instead of pinning a
/// core in an uncharged per-position scan.
#[test]
fn split_bound_charges_the_delimiter_scan() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(12_000));
    let graph = ExecGraph::read(&g);
    let source = "x".repeat(100);
    let s = |v: &str| Value::String(v.into());
    // Control: the payload plus a 2-byte delimiter fits.
    bi_split(&graph, "split", vec![s(&source), s("d0")])
        .expect("the payload with one short delimiter must fit");
    // A 100 × 8-byte delimiter list adds 100 × 800 scan bytes over the same
    // payload.
    let delimiters: Vec<Value> = (0..100).map(|i| s(&format!("d{i:07}"))).collect();
    let err = bi_split(
        &graph,
        "split",
        vec![s(&source), Value::List(delimiters.into())],
    )
    .expect_err("the delimiter byte scan must be charged");
    assert!(
        matches!(err, Error::Capacity { .. }),
        "expected a typed capacity refusal, got {err}"
    );
}

/// S1 (availability): the delimiter scan samples the statement's
/// cancellation token per examined position, so a cancelled statement stops
/// inside the split instead of finishing the scan.
#[test]
fn a_cancelled_split_stops_inside_the_scan() {
    let g = Graph::default();
    let token = relate_core::CancelToken::new();
    token.cancel();
    let graph = ExecGraph::read(&g).with_cancel(Some(&token));
    // More positions than the cancellation sampling window (1024).
    let source = "a".repeat(2_048);
    let err = bi_split(
        &graph,
        "split",
        vec![Value::String(source.into()), Value::String(",".into())],
    )
    .expect_err("the split must observe the cancelled statement");
    assert!(
        err.to_string().contains("query cancelled"),
        "expected the cancellation error, got {err}"
    );
}

#[test]
fn expression_budget_refuses_string_builtin_allocations() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(1));
    let graph = ExecGraph::read(&g);
    let s = |v: &str| Value::String(v.into());
    let cases: Vec<(&str, Result<Value>)> = vec![
        (
            "string.join",
            bi_string_join(
                &graph,
                "string.join",
                vec![Value::List(vec![s("ab"), s("cd")].into()), s(",")],
            ),
        ),
        (
            "replace",
            bi_replace(&graph, "replace", vec![s("aaaa"), s("a"), s("bb")]),
        ),
        ("split", bi_split(&graph, "split", vec![s("a,b,c"), s(",")])),
        (
            "string.regexReplace",
            bi_string_regexreplace(
                &graph,
                "string.regexReplace",
                vec![s("abc"), s("b"), s("xx")],
            ),
        ),
        (
            "string.interp",
            bi_string_interp(&graph, "string.interp", vec![s("a"), Value::Int(1)]),
        ),
    ];
    for (name, result) in cases {
        let error = result.expect_err(name);
        assert!(
            matches!(
                error,
                Error::Capacity {
                    limit: 1,
                    kind: relate_core::CapacityKind::Expression,
                    ..
                }
            ),
            "{name}: {error}"
        );
    }
}

/// ADR-0463 round 6 (security): Unicode normalization and case mapping can
/// expand their input several-fold, so they charge a conservative
/// `len × 12` bound through the seam before allocating. A 23 MB parameter —
/// well under the 256 MiB cap itself — crosses the bound (276 MB) and must
/// be refused typed, not handed to the allocator.
#[test]
fn unicode_expansion_charges_a_sound_bound_before_allocating() {
    const LEN: usize = 23_000_000;
    let g = Graph::default();
    let params = std::collections::BTreeMap::from([(
        "s".to_string(),
        Value::String("a".repeat(LEN).into()),
    )]);
    for query in [
        "RETURN normalize($s, NFKC) AS v",
        "RETURN toUpper($s) AS v",
        "RETURN toLower($s) AS v",
    ] {
        let error = crate::exec::test_support::try_exec_cypher_params(&g, query, &params)
            .err()
            .unwrap_or_else(|| panic!("{query} must be refused before allocating"));
        assert!(
            matches!(
                error,
                Error::Capacity {
                    limit,
                    kind: relate_core::CapacityKind::Expression,
                    ..
                } if limit == crate::memory::EXPRESSION_BUDGET_BYTES
            ),
            "{query}: {error}"
        );
    }
}
