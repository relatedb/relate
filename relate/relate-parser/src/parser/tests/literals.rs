use super::*;
use relate_names::SmallName;

#[test]
fn test_integer_overflow_literals_tag_detail() {
    // A too-large / too-small integer literal is a compile-time
    // SyntaxError whose ParseError carries the "IntegerOverflow" detail tag
    // (consumed by EngineError::kind()); the rendered message is unchanged.
    for q in [
        "RETURN 9223372036854775808 AS l",
        "RETURN -9223372036854775809 AS l",
        "RETURN 0x8000000000000000 AS l",
        "RETURN -0x8000000000000001 AS l",
    ] {
        let err = parse(q).unwrap_err();
        assert_eq!(
            err.detail,
            Some("IntegerOverflow"),
            "expected IntegerOverflow detail for {q:?}"
        );
        assert!(
            err.message.starts_with("integer overflow"),
            "message drifted for {q:?}: {}",
            err.message
        );
    }
}

#[test]
fn test_float_overflow_literal_tags_detail() {
    let err = parse("RETURN 1.34E999").unwrap_err();
    assert_eq!(err.detail, Some("FloatingPointOverflow"));
    assert_eq!(
        err.message,
        "FloatingPointOverflow: float literal is too large"
    );
}

#[test]
fn test_map_key_starting_with_digit_is_unexpected_syntax() {
    // Literals8 [19]: a digit-leading map key (`{1B2c3e67: 1}`)
    // lexes as an InvalidNumberLiteral error token, but in map-key position
    // the openCypher TCK expects the generic `UnexpectedSyntax` detail
    // (SyntaxError class), not `InvalidNumberLiteral`.
    for q in [
        "RETURN {1B2c3e67:1} AS literal",
        "RETURN {1B2c3e67: 'x'}",
        "WITH {2ab: 1} AS m RETURN m",
    ] {
        let err = parse(q).unwrap_err();
        assert_eq!(
            err.detail,
            Some("UnexpectedSyntax"),
            "expected UnexpectedSyntax detail for {q:?}, got {:?}",
            err.detail
        );
        assert!(
            err.message
                .starts_with("expected map key (identifier or string)"),
            "message drifted for {q:?}: {}",
            err.message
        );
    }
}

#[test]
fn test_map_key_pure_number_is_unexpected_syntax() {
    // A pure-digit key (`{123: 1}`) lexes as a plain Integer token (no
    // trailing ident), which is also rejected in key position with the
    // generic `UnexpectedSyntax` detail — matching the digit-leading case.
    let err = parse("RETURN {123: 1} AS literal").unwrap_err();
    assert_eq!(err.detail, Some("UnexpectedSyntax"));
    assert!(err
        .message
        .starts_with("expected map key (identifier or string)"));
}

#[test]
fn test_valid_map_keys_still_parse() {
    // Identifier, backtick-quoted, and single-quoted keys are all valid map
    // keys and must keep parsing after the digit-leading-key fix.
    for q in [
        "RETURN {valid_key: 1}",
        "RETURN {`123`: 1}",
        "RETURN {'123': 1}",
        "RETURN {`1B2c3e67`: 1}",
    ] {
        let stmt = parse(q).unwrap_or_else(|e| panic!("{q:?} should parse: {e}"));
        match &stmt.clauses[0] {
            Clause::Return(r) => match &r.items[0].expr {
                Expr::Map(pairs) => assert_eq!(pairs.len(), 1, "{q:?}"),
                other => panic!("expected map for {q:?}, got {other:?}"),
            },
            _ => panic!("expected return for {q:?}"),
        }
    }
}

#[test]
fn test_digit_leading_token_in_expression_position_stays_invalid_number_literal() {
    // The same malformed token OUTSIDE map-key position (as a bare
    // expression) must keep its `InvalidNumberLiteral` detail — the fix is
    // strictly local to the map-key parse path.
    for q in ["RETURN 1B2c3e67", "RETURN 1B2c3e67 AS l"] {
        let err = parse(q).unwrap_err();
        assert_eq!(
            err.detail,
            Some("InvalidNumberLiteral"),
            "expected InvalidNumberLiteral detail for {q:?}, got {:?}",
            err.detail
        );
    }
}

#[test]
fn test_parse_hex_integer() {
    let stmt = parse("RETURN 0xFF").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Literal(LiteralValue::Integer(v)) => assert_eq!(*v, 255),
            _ => panic!("expected integer literal"),
        },
        _ => panic!("expected return"),
    }
}

/// Cypher 25 keeps the lowercase `0x`/`0o` prefixes but rejects the
/// uppercase spellings and the legacy leading-zero octal decimal
/// (live-verified against neo4j:2026.09-enterprise).
#[test]
fn test_uppercase_radix_and_leading_zero_integers_are_rejected() {
    for q in [
        "RETURN 0X123",
        "RETURN 0O17",
        "RETURN 01277",
        "WITH 00 AS octal RETURN octal",
        "RETURN abs(-0X1fff)",
        "RETURN -0X1",
    ] {
        let err = parse(q).unwrap_err();
        assert_eq!(
            err.detail,
            Some("InvalidNumberLiteral"),
            "expected InvalidNumberLiteral for {q:?}, got {err:?}"
        );
    }
    // The Cypher 25 forms stay valid.
    for q in ["RETURN 0x1F", "RETURN 0o17", "RETURN 0", "RETURN -0x1"] {
        assert!(parse(q).is_ok(), "expected {q:?} to parse");
    }
}

#[test]
fn test_parse_octal_integer() {
    let stmt = parse("RETURN 0o77").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Literal(LiteralValue::Integer(v)) => assert_eq!(*v, 63),
            _ => panic!("expected integer literal"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
#[allow(clippy::approx_constant)]
fn test_parse_float_literal() {
    let stmt = parse("RETURN 3.14").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Literal(LiteralValue::Float(v)) => assert!((*v - 3.14).abs() < 1e-10),
            _ => panic!("expected float literal"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_int_literal_parsing() {
    assert_eq!(parse_int_literal("42").unwrap(), 42);
    assert_eq!(parse_int_literal("0xFF").unwrap(), 255);
    assert_eq!(parse_int_literal("0o77").unwrap(), 63);
    assert_eq!(parse_int_literal("-10").unwrap(), -10);
    assert_eq!(parse_int_literal("-0x8000000000000000").unwrap(), i64::MIN);
}

// -- CIP-55: Infinity/Inf/NaN literals --

#[test]
fn test_parse_infinity_and_nan_literals() {
    let stmt = parse("RETURN Infinity AS a, Inf AS b, NaN AS c").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => {
            assert_eq!(
                r.items[0].expr,
                Expr::Literal(LiteralValue::Float(f64::INFINITY))
            );
            assert_eq!(
                r.items[1].expr,
                Expr::Literal(LiteralValue::Float(f64::INFINITY))
            );
            match &r.items[2].expr {
                Expr::Literal(LiteralValue::Float(f)) => assert!(f.is_nan()),
                other => panic!("expected NaN float literal, got {other:?}"),
            }
        }
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_parse_infinity_is_case_insensitive() {
    for src in ["infinity", "INFINITY", "InFiNiTy", "inf", "INF"] {
        let stmt = parse(&format!("RETURN {src} AS x")).unwrap();
        match &stmt.clauses[0] {
            Clause::Return(r) => assert_eq!(
                r.items[0].expr,
                Expr::Literal(LiteralValue::Float(f64::INFINITY)),
                "failed for input {src:?}"
            ),
            other => panic!("expected return clause, got {other:?}"),
        }
    }
}

#[test]
fn test_parse_negative_infinity_is_unary_minus_over_literal() {
    let stmt = parse("RETURN -Infinity AS x").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Unary(u) => {
                assert_eq!(u.op, "-");
                assert_eq!(u.expr, Expr::Literal(LiteralValue::Float(f64::INFINITY)));
            }
            other => panic!("expected unary minus, got {other:?}"),
        },
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_parse_double_unary_minus_on_infinity_is_syntax_error() {
    assert!(parse("RETURN --Infinity AS x").is_err());
    assert!(parse("RETURN --NaN AS x").is_err());
    // Parenthesized double negation is fine (not a direct `--` run).
    assert!(parse("RETURN -(-Infinity) AS x").is_ok());
}

#[test]
fn test_parse_double_unary_minus_on_plain_number_still_parses() {
    // Only the Infinity/NaN keyword literals are restricted; ordinary
    // numeric double negation is unaffected.
    assert!(parse("RETURN --5 AS x").is_ok());
}

#[test]
fn test_parse_infinity_usable_as_label_and_property_key() {
    assert!(parse("CREATE (n:Infinity) RETURN labels(n)").is_ok());
    assert!(parse("CREATE (n {NaN: 1, Infinity: 2}) RETURN n.NaN, n.Infinity").is_ok());
}

#[test]
fn test_parse_infinity_wins_over_variable_of_same_name() {
    // The literal takes precedence over an in-scope variable named
    // `infinity` in expression position.
    let stmt = parse("WITH 0 AS infinity RETURN infinity AS x").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(r) => assert_eq!(
            r.items[0].expr,
            Expr::Literal(LiteralValue::Float(f64::INFINITY))
        ),
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_parse_nan_and_inf_win_over_variables_of_same_name() {
    // ADR-0280 pin: the literal wins over an in-scope variable for all three
    // keyword spellings, silently (Neo4j 2026.09 probe 2).
    let stmt = parse("WITH 0 AS nan, 1 AS inf RETURN nan AS a, inf AS b").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(r) => {
            match &r.items[0].expr {
                Expr::Literal(LiteralValue::Float(f)) => assert!(f.is_nan()),
                other => panic!("expected NaN float literal, got {other:?}"),
            }
            assert_eq!(
                r.items[1].expr,
                Expr::Literal(LiteralValue::Float(f64::INFINITY))
            );
        }
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_parse_keyword_names_bind_as_pattern_variable_and_alias() {
    // ADR-0280 pin: binding `inf`/`nan`/`infinity` is legal and silent, in
    // both pattern-variable and alias position (Neo4j binds them too).
    let stmt = parse("CREATE (inf:N)-[:R]->(nan:N)").unwrap();
    match &stmt.clauses[0] {
        Clause::Create(c) => {
            let vars: Vec<SmallName> = c.pattern[0]
                .elements
                .iter()
                .filter_map(|e| match e {
                    PatternElement::Node(n) => n.variable.clone(),
                    _ => None,
                })
                .collect();
            assert_eq!(vars, vec!["inf", "nan"]);
        }
        other => panic!("expected create clause, got {other:?}"),
    }
    let stmt = parse("WITH 0 AS nan, 1 AS inf, 2 AS infinity RETURN 1").unwrap();
    match &stmt.clauses[0] {
        Clause::With(w) => {
            let aliases: Vec<Option<SmallName>> = w.items.iter().map(|i| i.alias.clone()).collect();
            assert_eq!(
                aliases,
                vec![
                    Some(SmallName::from_name("nan")),
                    Some(SmallName::from_name("inf")),
                    Some(SmallName::from_name("infinity"))
                ]
            );
        }
        other => panic!("expected with clause, got {other:?}"),
    }
}

#[test]
fn test_parse_backticked_keyword_is_a_variable() {
    // ADR-0280 pin: backtick-escaping forces variable resolution in every
    // position — the one workaround when a variable of that name is bound.
    let stmt = parse("WITH 0 AS `nan` RETURN `nan` AS x, `inf`.v AS y").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(r) => {
            assert_eq!(r.items[0].expr, Expr::Variable("nan".into()));
            match &r.items[1].expr {
                Expr::Property(p) => {
                    assert_eq!(p.expr, Expr::Variable("inf".into()));
                    assert_eq!(p.property, "v");
                }
                other => panic!("expected property access on variable, got {other:?}"),
            }
        }
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_parse_property_access_base_is_the_literal() {
    // ADR-0280 pin: `inf.v` takes the literal as the access base (it fails
    // later, at runtime, with InvalidArgumentType — Neo4j probe 3).
    let stmt = parse("RETURN inf.v AS x").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Property(p) => {
                assert_eq!(p.expr, Expr::Literal(LiteralValue::Float(f64::INFINITY)));
                assert_eq!(p.property, "v");
            }
            other => panic!("expected property access, got {other:?}"),
        },
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_parse_delete_keyword_target_is_the_literal() {
    // ADR-0280 pin: `DELETE inf` parses the target as the CIP-55 float
    // literal (the static rejection lives in relate-front's
    // check_delete_expr, not the parser).
    let stmt = parse("MATCH (n) DELETE inf, nan").unwrap();
    match &stmt.clauses[1] {
        Clause::Delete(d) => {
            assert_eq!(
                d.expressions[0],
                Expr::Literal(LiteralValue::Float(f64::INFINITY))
            );
            match &d.expressions[1] {
                Expr::Literal(LiteralValue::Float(f)) => assert!(f.is_nan()),
                other => panic!("expected NaN float literal, got {other:?}"),
            }
        }
        other => panic!("expected delete clause, got {other:?}"),
    }
}

#[test]
fn test_parse_keyword_names_unaffected_as_property_and_map_keys() {
    // ADR-0280 pin: property keys after a dot and map keys are unaffected
    // (Neo4j probe 6) — `SET n.inf = 1` and `{nan: 1}.nan` work.
    let stmt = parse("MATCH (n) SET n.inf = 1, n.nan = 2").unwrap();
    match stmt.clauses.last().unwrap() {
        Clause::Set(sc) => {
            assert_eq!(sc.items[0].property.as_ref().unwrap().property, "inf");
            assert_eq!(sc.items[1].property.as_ref().unwrap().property, "nan");
        }
        other => panic!("expected set clause, got {other:?}"),
    }
    let stmt = parse("RETURN {nan: 1}.nan AS x").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Property(p) => {
                assert!(matches!(p.expr, Expr::Map(_)));
                assert_eq!(p.property, "nan");
            }
            other => panic!("expected property access on map literal, got {other:?}"),
        },
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_parse_overflowing_float_literal_is_rejected() {
    assert!(parse("RETURN 1.34E999").is_err());
    // But a legitimate Infinity keyword literal is fine.
    assert!(parse("RETURN Infinity").is_ok());
}

#[test]
fn test_parse_set_property_named_after_keyword() {
    // `limit` collides with the LIMIT keyword but must still be usable
    // as a property name in a SET target.
    let stmt = parse("MATCH (n) SET n.limit = Infinity").unwrap();
    match stmt.clauses.last().unwrap() {
        Clause::Set(sc) => {
            assert_eq!(sc.items[0].property.as_ref().unwrap().property, "limit");
        }
        other => panic!("expected set clause, got {other:?}"),
    }
}

/// The RETURN projection's first item's expression, for literal tests.
fn first_return_expr(stmt: &Statement) -> &Expr {
    match &stmt.clauses[0] {
        Clause::Return(r) => &r.items[0].expr,
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_underscore_digit_separators_parse_to_plain_values() {
    // Cypher 25 digit separators (LiteralAcceptance): a single `_` between
    // digits, and the one `_` directly after the 0x/0o prefix, carry no
    // value -- the token's literal is the separator-free text.
    for (q, want) in [
        ("RETURN 4_2 AS l", 42i64),
        ("RETURN -4_2 AS l", -42),
        ("RETURN 1_0_0_0 AS l", 1000),
        ("RETURN 0x_2A_40_7F AS l", 2769023),
        ("RETURN -0x_2A_40_7F AS l", -2769023),
        ("RETURN 0o_66_44_22 AS l", 223506),
        ("RETURN -0o_66_44_22 AS l", -223506),
    ] {
        let stmt = parse(q).unwrap();
        match first_return_expr(&stmt) {
            // `-4_2` parses as unary minus over the positive literal.
            Expr::Literal(LiteralValue::Integer(v)) => assert_eq!(*v, want, "{q}"),
            Expr::Unary(u) => match &u.expr {
                Expr::Literal(LiteralValue::Integer(v)) => assert_eq!(-v, want, "{q}"),
                other => panic!("expected literal under unary, got {other:?} in {q}"),
            },
            other => panic!("expected numeric literal, got {other:?} in {q}"),
        }
    }
}

#[test]
fn test_underscore_separator_floats() {
    // The separator carries no value in the fraction or the exponent
    // (LiteralAcceptance [8]-[11]).
    for (q, want) in [
        ("RETURN 1_000.000_1 AS l", 1000.0001),
        ("RETURN -1_0.0_1 AS l", -10.01),
        ("RETURN 1_0_0_0.0_0_0_1 AS l", 1000.0001),
        ("RETURN 1.0E1_0 AS l", 1e10),
    ] {
        let stmt = parse(q).unwrap();
        let got = match first_return_expr(&stmt) {
            Expr::Literal(LiteralValue::Float(v)) => *v,
            Expr::Unary(u) => match &u.expr {
                Expr::Literal(LiteralValue::Float(v)) => -v,
                other => panic!("expected float under unary, got {other:?} in {q}"),
            },
            other => panic!("expected float literal, got {other:?} in {q}"),
        };
        assert!((got - want).abs() < 1e-9, "{q} parsed as {got}");
    }
}

#[test]
fn test_misplaced_underscore_separators_stay_invalid() {
    // The separator is only valid between digits (and after the 0x/0o
    // prefix): doubled, leading, trailing, in the prefix, after a leading
    // zero, or before the exponent's first digit, the whole run is one
    // InvalidNumberLiteral (LiteralAcceptance's fail rows).
    for q in [
        "RETURN 1__000 AS l",
        "RETURN 1000_ AS l",
        "RETURN 0_xFF AS l",
        "RETURN 0_66_44_22 AS l",
        "RETURN 0_o77 AS l",
        "RETURN 0x2A_ AS l",
        "RETURN 0x2__A AS l",
        "RETURN 0o77_ AS l",
        "RETURN 0o7__7 AS l",
        "RETURN 1E_1 AS l",
        "RETURN 1.00__01 AS l",
    ] {
        let err = parse(q).unwrap_err();
        assert_eq!(
            err.detail,
            Some("InvalidNumberLiteral"),
            "expected InvalidNumberLiteral detail for {q:?}, got {:?} ({})",
            err.detail,
            err.message
        );
    }
}

#[test]
fn test_obfuscated_literal_is_preview_gated_at_the_lexer() {
    // `******` (six stars) parses to a dummy `false` only with the
    // `obfuscated-literals` gate open; closed, the token stream is the
    // ordinary stars and the parser fails with its own syntax error --
    // Neo4j's disabled-form behavior, not the authored preview error.
    let open = crate::lexer::ParseOptions {
        obfuscated_literals: true,
        ..Default::default()
    };
    let stmt = parse_with_options("RETURN ****** AS l", open).unwrap();
    match first_return_expr(&stmt) {
        Expr::Literal(LiteralValue::Bool(false)) => {}
        other => panic!("expected dummy false literal, got {other:?}"),
    }
    // Any other star count never lexes as the obfuscated literal.
    assert!(parse_with_options("RETURN ******* AS l", open).is_err());
    assert!(parse_with_options("RETURN ***** AS l", open).is_err());
    let err = parse("RETURN ****** AS l").unwrap_err();
    assert_eq!(
        err.detail, None,
        "closed gate is a plain syntax error: {err}"
    );
}

#[test]
fn test_chronological_comparison_parses_with_not_forms() {
    // `a IS [NOT] BEFORE | AFTER | SAME INSTANT | SAME REPRESENTATION b`
    // (Cypher 25), including inside a simple-CASE WHEN.
    for (q, want_op, want_negated) in [
        ("RETURN a IS BEFORE b AS r", ChronologicalOp::Before, false),
        (
            "RETURN a IS NOT BEFORE b AS r",
            ChronologicalOp::Before,
            true,
        ),
        ("RETURN a IS AFTER b AS r", ChronologicalOp::After, false),
        ("RETURN a IS NOT AFTER b AS r", ChronologicalOp::After, true),
        (
            "RETURN a IS SAME INSTANT b AS r",
            ChronologicalOp::SameInstant,
            false,
        ),
        (
            "RETURN a IS NOT SAME INSTANT b AS r",
            ChronologicalOp::SameInstant,
            true,
        ),
        (
            "RETURN a IS SAME REPRESENTATION b AS r",
            ChronologicalOp::SameRepresentation,
            false,
        ),
        (
            "RETURN a IS NOT SAME REPRESENTATION b AS r",
            ChronologicalOp::SameRepresentation,
            true,
        ),
    ] {
        let stmt = parse(q).unwrap();
        match first_return_expr(&stmt) {
            Expr::ChronologicalCompare(cc) => {
                assert_eq!(cc.op, want_op, "{q}");
                assert_eq!(cc.negated, want_negated, "{q}");
                assert!(matches!(&cc.left, Expr::Variable(v) if v == "a"), "{q}");
                assert!(matches!(&cc.right, Expr::Variable(v) if v == "b"), "{q}");
            }
            other => panic!("expected ChronologicalCompare for {q}, got {other:?}"),
        }
    }
}

#[test]
fn test_chronological_words_stay_non_reserved_identifiers() {
    // The four operator words are contextual only: as variables they still
    // parse (TemporalChronologicalCompare's non-reserved-identifier
    // scenario).
    let stmt = parse(
        "WITH 1 AS before, 2 AS after, 3 AS same, 4 AS instant, 5 AS representation \
         RETURN before + after + same + instant + representation AS total",
    )
    .unwrap();
    match stmt.clauses.last().unwrap() {
        Clause::Return(r) => {
            assert_eq!(
                r.items[0].alias.as_ref(),
                Some(&SmallName::from_name("total"))
            );
        }
        other => panic!("expected return clause, got {other:?}"),
    }
}

#[test]
fn test_case_when_chronographical_prefix_form() {
    // `CASE x WHEN IS BEFORE y THEN ...` applies the predicate to the CASE
    // operand (TemporalChronologicalCompare's CASE prefix form).
    let stmt = parse(
        "RETURN CASE datetime('2020-01-01T00:00Z') \
                 WHEN IS BEFORE datetime('2020-01-02T00:00Z') THEN 'before' \
                 ELSE 'same' END AS result",
    )
    .unwrap();
    match first_return_expr(&stmt) {
        Expr::Case(case) => match case.whens.first().map(|w| &w.condition) {
            Some(Expr::ChronologicalCompare(cc)) => {
                assert_eq!(cc.op, ChronologicalOp::Before);
                assert!(!cc.negated);
                // The left operand is the CASE operand, not a variable.
                assert!(matches!(&cc.left, Expr::FunctionCall(_)));
            }
            other => panic!("expected ChronologicalCompare WHEN, got {other:?}"),
        },
        other => panic!("expected case expression, got {other:?}"),
    }
}

#[test]
fn test_float32_suffix_lexes_as_a_float_literal() {
    // `1.0f` is the FLOAT32 literal spelling (VectorValueAcceptance /
    // NaNAcceptance); the engine's only scalar floating value is FLOAT, so
    // the suffix lexes the same FLOAT literal.
    match first_return_expr(&parse("RETURN 1.0f AS x").unwrap()) {
        Expr::Literal(LiteralValue::Float(f)) => assert_eq!(*f, 1.0),
        other => panic!("expected float literal, got {other:?}"),
    }
    // The suffix belongs to the number: an identifier glued to it still
    // makes the whole run one malformed literal.
    let err = parse("RETURN 1.0fx AS x").unwrap_err();
    assert_eq!(err.detail, Some("InvalidNumberLiteral"), "{}", err.message);
}

#[test]
fn test_separated_extreme_integer_literals_parse() {
    // SquareRoot's corner cases spell i64::MAX/-i64::MIN with Cypher 25
    // digit separators.
    for q in [
        "RETURN 9_223_372_036_854_775_807 AS x",
        "RETURN -9_223_372_036_854_775_808 AS x",
    ] {
        parse(q).unwrap_or_else(|e| panic!("{q:?} must parse: {e}"));
    }
}
