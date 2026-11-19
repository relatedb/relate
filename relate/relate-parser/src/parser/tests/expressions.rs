use super::*;
use relate_test_support::run_with_big_stack;

// CIP-272 round-3 review: a `{` literal with no top-level `|` is a map
// literal, and the comprehension speculation must not full-parse it first.
// Before the lookahead in front of `finish_map_comprehension`, nesting like
// this doubled the parse work per level (measured on the pre-fix parser in
// a debug build: depth 12 took 6.5 ms, depth 16 took 105 ms, depth 20 took
// 1.7 s -- four times the work per two levels, so depth 50 never finishes).
// Depth 50 pins the shape as a bounded-time parse, and the counter staying
// at zero pins it as a single non-speculative pass. Parsing recurses once per
// nesting level, which overflows a default-sized (2 MiB) test thread before
// depth 50, so the parse runs on an explicitly sized stack -- for the same
// reason the retired in-repo CIP-eval gate set RUST_MIN_STACK for its
// binary (ADR-0520).
#[test]
fn test_pipe_less_nesting_is_not_speculatively_parsed() {
    run_with_big_stack(|| {
        let depth = 50;
        let mut source = String::from("m");
        for _ in 0..depth {
            source = format!("{{a: b IN {source}}}");
        }
        let count = || super::SPECULATIVE_COMPREHENSION_PARSES.with(std::cell::Cell::get);
        let before = count();
        let expr =
            parse_expression_str(&source).expect("pipe-less nesting has to parse as a map literal");
        let speculative = count() - before;
        assert_eq!(
            speculative, 0,
            "no speculative comprehension parse may start for a pipe-less brace"
        );
        assert!(
            matches!(expr, Expr::Map(_)),
            "expected a map literal, got {expr:?}"
        );
    });
}

// A speculative map comprehension that fails mid-tail rewinds to the
// map-literal reading of the same tokens: `{k : v IN m, y: x:A|B}` has
// the `{ident : ident IN` head and a brace-top-level `|` (in the second
// pair's label-predicate value), so the speculation runs (the counter
// ticks once), but its source parse stops at the `,` — no token run
// inside a comprehension may contain one — leaving no `|` where the
// projection needs it, so the tail reports the structural mismatch and
// the literal reading takes over: two pairs, the first value the `IN`
// expression, the second the label predicate. Pins
// `map_comprehension_tail`'s single rewind at its caller: the position
// returns to the start and the literal re-parses the same tokens.
#[test]
fn test_a_failed_map_comprehension_speculation_falls_back_to_the_literal() {
    let count = || super::SPECULATIVE_COMPREHENSION_PARSES.with(std::cell::Cell::get);
    let before = count();
    let expr = parse_expression_str("{k : v IN m, y: x:A|B}").unwrap();
    assert_eq!(
        count() - before,
        1,
        "the speculative comprehension parse ran exactly once"
    );
    let Expr::Map(pairs) = &expr else {
        panic!("expected the map-literal fallback, got {expr:?}")
    };
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].key, SmallName::from_name("k"));
    // The first pair's value is the `IN` expression the comprehension's
    // source parse would have read — the literal reading of the same
    // tokens the speculation rewound from.
    assert!(matches!(pairs[0].value, Expr::In(_)));
    // The second pair carries the pipe that pulled the shape into the
    // speculative path, as its label-predicate value.
    assert_eq!(pairs[1].key, SmallName::from_name("y"));
    assert!(matches!(pairs[1].value, Expr::HasLabel(_)));
}

// The map form's inner-bracket premise: its closer is `}`, so every `]`
// inside it belongs to an inner bracket (a list literal in the source or
// in the WHERE) and must never prove a `|` was the comprehension's
// separator — the form passes the map-optional kind for exactly that
// reason, not because its `| key : value` projection is optional. Both
// shapes regressed when the form passed the map-required kind: the
// inner list's `]` rolled the disjunction's pipe back and stranded it
// inside the bracket.
#[test]
fn test_a_map_comprehension_inner_bracket_pipe_is_not_the_separator() {
    // Source side: the source's inner list ends in `]`, and the `|` it
    // contains is the label disjunction's, not the separator.
    let expr = parse_expression_str("{k: v IN [n:A|B] | k : v}").unwrap();
    let Expr::MapComprehension(mc) = &expr else {
        panic!("expected a map comprehension, got {expr:?}")
    };
    let Expr::List(items) = &mc.source_expr else {
        panic!("expected the list-literal source, got {:?}", mc.source_expr)
    };
    assert_eq!(items.len(), 1);
    assert!(
        matches!(&items[0], Expr::HasLabel(h)
            if matches!(&h.label_expr, LabelExpr::Or(parts) if parts.len() == 2)),
        "the label disjunction survived the inner `]`: {:?}",
        items[0]
    );

    // WHERE side: the same premise inside the filtered form's WHERE.
    let expr = parse_expression_str("{k: v IN [{a:1}] WHERE [n:A|B] = [] | k : v}").unwrap();
    let Expr::MapComprehension(mc) = &expr else {
        panic!("expected a map comprehension, got {expr:?}")
    };
    let Some(where_expr) = &mc.where_expr else {
        panic!("expected the WHERE predicate")
    };
    let Expr::Comparison(cmp) = where_expr else {
        panic!("expected the comparison predicate, got {where_expr:?}")
    };
    assert!(
        matches!(&cmp.left, Expr::List(items)
            if matches!(&items[0], Expr::HasLabel(h)
                if matches!(&h.label_expr, LabelExpr::Or(parts) if parts.len() == 2))),
        "the label disjunction survived the inner `]`: {:?}",
        cmp.left
    );
}

// `[true IN [true], false]` is a two-element list literal whose first
// element is a boolean `IN` expression, not a malformed comprehension:
// a comma can never appear inside a comprehension, so the parser restores
// the literal reading when it meets one (Neo4j's preference).
#[test]
fn test_comma_after_in_source_parses_as_a_list_literal() {
    let stmt = parse("RETURN [true IN [true], false]").unwrap();
    let Clause::Return(ret) = &stmt.clauses[0] else {
        panic!("expected RETURN");
    };
    let Expr::List(items) = &ret.items[0].expr else {
        panic!("expected a list literal, got {:?}", ret.items[0].expr);
    };
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0], Expr::In(_)), "got {:?}", items[0]);

    // The nested shape from the source: the outer `[v IN ...]` is still a
    // comprehension, while the inner list is a literal.
    let stmt = parse("RETURN null IN [var0 IN [true IN [true], false]]").unwrap();
    let Clause::Return(ret) = &stmt.clauses[0] else {
        panic!("expected RETURN");
    };
    let Expr::In(outer) = &ret.items[0].expr else {
        panic!("expected an IN expression, got {:?}", ret.items[0].expr);
    };
    let Expr::ListComprehension(inner) = &outer.list else {
        panic!("expected a list comprehension, got {:?}", outer.list);
    };
    assert!(matches!(inner.list_expr, Expr::List(_)));
}

// The legacy repeated-colon relationship type separator is legal for a
// bare type list and rejected beside a variable, inline properties or a
// variable length (Neo4j HelpfulErrorMessages).
#[test]
fn test_legacy_pipe_colon_relationship_types() {
    parse("MATCH ()-[:A|:B]->() RETURN 1").expect("a bare legacy type list parses");
    for query in [
        "MATCH ()-[x:A|:B]->() RETURN 1",
        "MATCH ()-[:A|:B {p: 1}]->() RETURN 1",
        "MATCH ()-[:A|:B*1..2]->() RETURN 1",
        "MATCH ()-[:A|:B*..2]->() RETURN 1",
        // Regression (fail-open): the legacy-pipe-colon fact is returned
        // from the type-spec parse, not carried in a clause-scoped flag.
        // The earlier `a:X|:Y` predicate used to leave that flag set (its
        // `parse_label_spec(true)` caller never restored it), which made
        // the relationship rejection below silently not fire.
        "MATCH (a WHERE a:X|:Y)-[r:A|:B]->(b) RETURN 1",
    ] {
        assert!(parse(query).is_err(), "{query} must be rejected");
    }
}

// SET/REMOVE property targets accept the same general expression bases
// Neo4j does: a parameter, a null/literal, and a CASE expression.
#[test]
fn test_set_and_remove_accept_expression_targets() {
    parse("MATCH (n) SET $node['k'] = 1").expect("a parameter base parses");
    parse("REMOVE null.p").expect("a null base parses");
    parse("REMOVE CASE WHEN true THEN 1 ELSE 2 END.p").expect("a CASE base parses");
    // SET reads the same CIP-131 production as REMOVE, so the literal bases
    // parse there too (the two used to disagree: `SET null.p = 1` fell
    // through to a variable read).
    parse("SET null.p = 1").expect("SET accepts a null base");
    parse("SET true.p = 1").expect("SET accepts a boolean-literal base");
    parse("SET CASE WHEN true THEN 1 ELSE 2 END.p = 1").expect("SET accepts a CASE base");
}

// A map projection with no selectors (`x {}`) is the empty map projection,
// not an error: the brace lookahead must recognize `{` + `}`.
#[test]
fn test_empty_map_projection_parses() {
    let stmt = parse("RETURN x {}").unwrap();
    let Clause::Return(ret) = &stmt.clauses[0] else {
        panic!("expected RETURN");
    };
    let Expr::MapProjection(projection) = &ret.items[0].expr else {
        panic!("expected a map projection, got {:?}", ret.items[0].expr);
    };
    assert!(projection.items.is_empty());
}

#[test]
fn test_parse_expressions() {
    // Arithmetic
    let stmt = parse("RETURN 1 + 2 * 3").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Binary(b) => {
                assert_eq!(b.op, "+");
                match &b.right {
                    Expr::Binary(b2) => assert_eq!(b2.op, "*"),
                    _ => panic!("expected binary"),
                }
            }
            _ => panic!("expected binary"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_comparison() {
    let stmt = parse("RETURN 1 = 1").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Comparison(c) => assert_eq!(c.op, "="),
            _ => panic!("expected comparison"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_boolean_ops() {
    let stmt = parse("RETURN true AND false OR NOT true").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Binary(b) => assert_eq!(b.op, "OR"),
            _ => panic!("expected binary"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_is_null() {
    let stmt = parse("MATCH (n) WHERE n.name IS NULL RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::IsNull(e) => assert!(!e.negated),
            _ => panic!("expected is null"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_is_not_null() {
    let stmt = parse("MATCH (n) WHERE n.name IS NOT NULL RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::IsNull(e) => assert!(e.negated),
            _ => panic!("expected is not null"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_in_expr() {
    let stmt = parse("RETURN 1 IN [1, 2, 3]").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::In(_) => {}
            _ => panic!("expected in expr"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_function_call() {
    let stmt = parse("RETURN toUpper('hello')").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::FunctionCall(f) => {
                assert_eq!(f.name, "toUpper");
                assert_eq!(f.args.len(), 1);
            }
            _ => panic!("expected function call"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_count_star() {
    let stmt = parse("RETURN count(*)").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::CountStar => {}
            _ => panic!("expected count star"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_case_expr() {
    let stmt = parse("RETURN CASE WHEN true THEN 1 ELSE 2 END").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Case(c) => {
                assert!(matches!(
                    &c.whens[0].condition,
                    Expr::Literal(LiteralValue::Bool(true))
                ));
                assert_eq!(c.whens.len(), 1);
                assert!(c.else_expr.is_some());
            }
            _ => panic!("expected case"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_simple_case() {
    let stmt = parse("RETURN CASE n.type WHEN 'A' THEN 1 WHEN 'B' THEN 2 END").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Case(c) => {
                // The simple form's test operand is lowered into each WHEN
                // as the equality comparison's left side.
                assert_eq!(c.whens.len(), 2);
                for (when, value) in c.whens.iter().zip(["A", "B"]) {
                    let Expr::Comparison(cmp) = &when.condition else {
                        panic!("expected equality comparison, got {:?}", when.condition);
                    };
                    assert_eq!(cmp.op, "=");
                    assert!(matches!(cmp.left, Expr::Property(_)));
                    assert!(matches!(
                        &cmp.right,
                        Expr::Literal(LiteralValue::String(s)) if s == value
                    ));
                }
            }
            _ => panic!("expected case"),
        },
        _ => panic!("expected return"),
    }
}

/// The RETURN item's expression as a simple-CASE `Expr::Case`.
fn case_return(cypher: &str) -> crate::ast::CaseExpr {
    let stmt = parse(cypher).unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Case(c) => (**c).clone(),
            other => panic!("expected case, got {other:?}"),
        },
        _ => panic!("expected return"),
    }
}

/// Generalized simple-CASE `WHEN <comparison operator> <expr>`
/// (CIP-143/CIP-190): the CASE operand is the implicit left-hand side.
#[test]
fn test_parse_simple_case_comparison_when_operand() {
    let c = case_return("RETURN CASE x WHEN < 1 THEN 1 ELSE 2 END");
    assert_eq!(c.whens.len(), 1);
    let Expr::Comparison(cmp) = &c.whens[0].condition else {
        panic!("expected comparison predicate");
    };
    assert_eq!(cmp.op, "<");
    assert!(matches!(&cmp.left, Expr::Variable(v) if v == "x"));
}

/// Generalized simple-CASE `WHEN =~ <regex>` operand.
#[test]
fn test_parse_simple_case_regex_when_operand() {
    let c = case_return("RETURN CASE n.name WHEN =~ 'C.*t' THEN 10 ELSE 15 END");
    let Expr::Comparison(cmp) = &c.whens[0].condition else {
        panic!("expected comparison predicate");
    };
    assert_eq!(cmp.op, "=~");
}

/// Comma-separated `<when operand list>` (CIP-143/CIP-190): the elements
/// fold into one OR-ed predicate over the CASE operand.
#[test]
fn test_parse_simple_case_when_operand_list() {
    let c = case_return("RETURN CASE n.salary WHEN 1000, 1500 THEN 'low' END");
    assert_eq!(c.whens.len(), 1);
    let Expr::Binary(or) = &c.whens[0].condition else {
        panic!("expected OR of equality predicates");
    };
    assert_eq!(or.op, "OR");
    assert!(matches!(&or.left, Expr::Comparison(cmp) if cmp.op == "="));
    assert!(matches!(&or.right, Expr::Comparison(cmp) if cmp.op == "="));
}

/// A list mixing a comparison predicate and an `IS NULL` predicate.
#[test]
fn test_parse_simple_case_mixed_when_operand_list() {
    let c = case_return("RETURN CASE x WHEN < 1500, IS NULL THEN 'low' END");
    let Expr::Binary(or) = &c.whens[0].condition else {
        panic!("expected OR of predicates");
    };
    assert_eq!(or.op, "OR");
    assert!(matches!(&or.left, Expr::Comparison(cmp) if cmp.op == "<"));
    assert!(matches!(&or.right, Expr::IsNull(_)));
}

/// Simple-CASE `WHEN IS [NOT] [<normal form>] NORMALIZED` (CIP-133).
#[test]
fn test_parse_simple_case_normalized_when_operand() {
    let c = case_return("RETURN CASE n.name WHEN IS NOT NFKD NORMALIZED THEN 3 ELSE 15 END");
    let Expr::IsNormalized(norm) = &c.whens[0].condition else {
        panic!("expected normalized predicate");
    };
    assert_eq!(norm.form.as_deref(), Some("NFKD"));
    assert!(norm.negated);
}

/// Simple-CASE bare `WHEN :: <value type>` (CIP-87).
#[test]
fn test_parse_simple_case_bare_type_predicate_when_operand() {
    let c = case_return("RETURN CASE n WHEN :: POINT THEN 6 ELSE 15 END");
    assert!(matches!(&c.whens[0].condition, Expr::IsType(_)));
}

#[test]
fn test_parse_parameter() {
    let stmt = parse("MATCH (n) WHERE n.name = $name RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::Comparison(c) => match &c.right {
                Expr::Parameter(name) => assert_eq!(name, "name"),
                _ => panic!("expected parameter"),
            },
            _ => panic!("expected comparison"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_parameter_name_spellings() {
    // LABS-1602/1612: the alternate parameter-name spellings lower to the
    // same `Expr::Parameter` — a backtick-quoted name (spaces and doubled
    // backticks included) and a whitespace-separated `$ p2`.
    for (query, name) in [
        ("RETURN $`p1` AS x", "p1"),
        ("RETURN $`p 1` AS x", "p 1"),
        ("RETURN $`a``b` AS x", "a`b"),
        ("RETURN $ p2 AS x", "p2"),
        ("RETURN $\np2 AS x", "p2"),
    ] {
        let stmt = parse_ok(query);
        match return_expr(&stmt) {
            Expr::Parameter(n) => assert_eq!(n, name, "{query:?}"),
            other => panic!("{query:?}: expected a parameter, got {other:?}"),
        }
    }
}

#[test]
fn test_parse_property_access() {
    let stmt = parse("RETURN n.name").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Property(p) => {
                assert_eq!(p.property, "name");
            }
            _ => panic!("expected property access"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_list_literal() {
    let stmt = parse("RETURN [1, 2, 3]").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::List(l) => assert_eq!(l.len(), 3),
            _ => panic!("expected list"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_map_literal() {
    let stmt = parse("RETURN {a: 1, b: 2}").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].key, "a");
                assert_eq!(pairs[1].key, "b");
            }
            _ => panic!("expected map"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_subscript() {
    let stmt = parse("RETURN list[0]").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Subscript(s) => {
                assert!(!s.slice);
                assert!(s.index.is_some());
            }
            _ => panic!("expected subscript"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_slice() {
    let stmt = parse("RETURN list[1..3]").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Subscript(s) => {
                assert!(s.slice);
                assert!(s.from.is_some());
                assert!(s.to.is_some());
            }
            _ => panic!("expected subscript"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_list_comprehension() {
    let stmt = parse("RETURN [x IN [1,2,3] WHERE x > 1 | x * 2]").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::ListComprehension(lc) => {
                assert_eq!(lc.variable, "x");
                assert!(lc.where_expr.is_some());
                assert!(lc.map_expr.is_some());
            }
            _ => panic!("expected list comprehension"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_reduce() {
    let stmt = parse("RETURN reduce(acc = 0, x IN [1,2,3] | acc + x)").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Reduce(re) => {
                assert_eq!(re.accumulator, "acc");
                assert_eq!(re.variable, "x");
            }
            _ => panic!("expected reduce"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_allreduce() {
    let stmt = parse("RETURN allReduce(acc = 0, x IN [1,2,3] | acc + x, acc < 10)").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::AllReduce(ar) => {
                assert_eq!(ar.accumulator, "acc");
                assert_eq!(ar.variable, "x");
            }
            _ => panic!("expected allReduce"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_allreduce_requires_the_third_predicate_argument() {
    // allReduce is the three-argument Cypher 25 form (accumulator, update,
    // keep-going predicate); the two-argument `reduce()` spelling inside
    // an allReduce call is a parse error, not a silent fallback to
    // `reduce` semantics.
    assert!(parse("RETURN allReduce(acc = 0, x IN [1,2,3] | acc + x)").is_err());
}

#[test]
fn test_parse_concat_operator_is_left_associative_like_plus() {
    // `||` shares precedence with `+`/`-` and is parsed left-associatively,
    // producing the same nested-Binary shape `+` would.
    let stmt = parse("RETURN [1] || [2] || [3]").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Binary(outer) => {
                assert_eq!(outer.op, "||");
                match &outer.left {
                    Expr::Binary(inner) => assert_eq!(inner.op, "||"),
                    _ => panic!("expected nested binary || on the left"),
                }
            }
            _ => panic!("expected binary ||"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_quantifier() {
    let stmt = parse("RETURN all(x IN [1,2,3] WHERE x > 0)").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Quantifier(q) => {
                assert_eq!(q.quantifier, "all");
                assert_eq!(q.variable, "x");
                assert!(q.predicate.is_some());
            }
            _ => panic!("expected quantifier"),
        },
        _ => panic!("expected return"),
    }
}

// A list predicate used as a *function argument* (`toBoolean(all(...))`)
// must keep its quantifier reading: the enclosing call's explicit
// `f(ALL x)` set-quantifier spelling had been swallowing the leading ALL,
// so the argument parsed as a plain `all(...)` call and died on WHERE.
#[test]
fn test_quantifier_as_function_argument_is_not_a_set_quantifier() {
    let stmt = parse("RETURN toBoolean(all(n0 IN [1, 2] WHERE n0 > 0)) AS r").unwrap();
    let Clause::Return(r) = &stmt.clauses[0] else {
        panic!("expected return");
    };
    let Expr::FunctionCall(call) = &r.items[0].expr else {
        panic!("expected function call, got {:?}", r.items[0].expr);
    };
    assert_eq!(call.name, "toBoolean");
    assert!(
        matches!(call.args[0], Expr::Quantifier(_)),
        "the argument must stay a list predicate, got {:?}",
        call.args[0]
    );
    // The nested reading composes with the aggregate quantifier spelling:
    // `count(all(x IN … WHERE …))` aggregates the predicate's value.
    assert!(parse("RETURN count(all(x IN [1, 2] WHERE x > 1)) AS c").is_ok());
    // The plain set-quantifier spelling still reads as one.
    assert!(parse("RETURN count(ALL 1) AS c").is_ok());
}

// LABS-201: agents repeatedly use patterns inside list predicates without
// the required EXISTS {}, and spell the predicate with the GQL pipe instead
// of WHERE. Both used to fail as a bare `expected RightParen, got |`; the
// parser now names the fix in each case.
#[test]
fn test_quantifier_pipe_instead_of_where_names_the_fix() {
    for q in ["any", "all", "none", "single"] {
        let err = parse(&format!("MATCH (n) WHERE {q}(x IN [1,2] | x > 1) RETURN n")).unwrap_err();
        assert!(
            err.message.contains("WHERE"),
            "{q}: expected the error to point at WHERE, got {err:?}"
        );
    }
}

#[test]
fn test_pattern_in_list_predicate_points_at_exists() {
    for q in ["any", "all", "none", "single"] {
        let err = parse(&format!(
            "MATCH (a) WHERE {q}((a)-[:R]->(b) | b.name = 'x') RETURN a"
        ))
        .unwrap_err();
        assert!(
            err.message.contains("EXISTS"),
            "{q}: expected the error to point at EXISTS, got {err:?}"
        );
    }
}

// The pipe-less pattern form keeps its existing parse: the EXISTS-targeted
// lookahead must not hijack it (the validator reports it as a pattern
// predicate outside WHERE).
#[test]
fn test_pattern_in_quantifier_without_pipe_still_parses() {
    assert!(parse("RETURN any((a)-[:R]->(b))").is_ok());
    // An arithmetic argument is not a pattern and keeps the generic
    // right-paren error path.
    assert!(parse("RETURN any((1 + 2) > 3)").is_ok());
}

#[test]
fn test_parse_has_label() {
    let stmt = parse("MATCH (n) WHERE n:Person RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::HasLabel(h) => {
                assert_eq!(h.label_expr.as_label_list(), Some(vec!["Person".into()]));
            }
            _ => panic!("expected has label"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_shortest_path_expr() {
    // shortestPath is parsed as an expression, not in MATCH pattern context
    let stmt = parse("RETURN shortestPath((a)-[*]->(b))").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::ShortestPath(sp) => {
                assert!(!sp.all);
                assert_eq!(sp.pattern.elements.len(), 3);
            }
            _ => panic!("expected shortest path expr"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_unary_minus() {
    let stmt = parse("RETURN -42").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Unary(u) => {
                assert_eq!(u.op, "-");
                match &u.expr {
                    Expr::Literal(LiteralValue::Integer(v)) => assert_eq!(*v, 42),
                    _ => panic!("expected integer"),
                }
            }
            _ => panic!("expected unary"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_namespaced_function() {
    let stmt = parse("RETURN point.distance(p1, p2)").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::FunctionCall(f) => {
                assert_eq!(f.name, "point.distance");
                assert_eq!(f.args.len(), 2);
            }
            _ => panic!("expected function call"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_exponentiation() {
    let stmt = parse("RETURN 2 ^ 3").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Binary(b) => assert_eq!(b.op, "^"),
            _ => panic!("expected binary"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_xor() {
    let stmt = parse("RETURN true XOR false").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Binary(b) => assert_eq!(b.op, "XOR"),
            _ => panic!("expected binary"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_count_distinct() {
    let stmt = parse("RETURN count(DISTINCT n)").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::FunctionCall(f) => {
                assert_eq!(f.name, "count");
                assert!(f.distinct);
            }
            _ => panic!("expected function call"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_modulo() {
    let stmt = parse("RETURN 10 % 3").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Binary(b) => assert_eq!(b.op, "%"),
            _ => panic!("expected binary"),
        },
        _ => panic!("expected return"),
    }
}

#[test]
fn test_parse_chained_comparison() {
    let stmt = parse("RETURN 1 < 2 < 3").unwrap();
    match &stmt.clauses[0] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::Binary(b) => {
                assert_eq!(b.op, "AND");
            }
            _ => panic!("expected binary AND for chained comparison"),
        },
        _ => panic!("expected return"),
    }
}

// -- PROPERTY_EXISTS (CIP-247) --

#[test]
fn test_parse_property_exists_basic() {
    let stmt = parse("MATCH (p) RETURN PROPERTY_EXISTS(p, email) AS e").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(r) => match &r.items[0].expr {
            Expr::FunctionCall(fc) => {
                assert_eq!(fc.name, "property_exists");
                assert_eq!(fc.args.len(), 2);
                assert_eq!(fc.args[0], Expr::Variable("p".into()));
                assert_eq!(
                    fc.args[1],
                    Expr::Literal(LiteralValue::String("email".into()))
                );
            }
            other => panic!("expected function call, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_parse_property_exists_literal_property_name_is_error() {
    // A string literal in property-name position is a compile-time error.
    assert!(parse("RETURN PROPERTY_EXISTS(p, \"email\")").is_err());
}

#[test]
fn test_parse_property_exists_non_variable_first_arg_is_error() {
    assert!(parse("RETURN PROPERTY_EXISTS(1, name)").is_err());
}

#[test]
fn test_parse_property_exists_expression_property_name_is_error() {
    assert!(parse("RETURN PROPERTY_EXISTS(p, a + b)").is_err());
    assert!(parse("RETURN PROPERTY_EXISTS(p, toString(x))").is_err());
}
