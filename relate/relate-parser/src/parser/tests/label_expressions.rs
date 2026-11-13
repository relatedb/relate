use super::*;

// ---- CIP-14 / CIP-126 / CIP-249 label expression tests ----

#[test]
fn test_label_expr_precedence_and_binds_tighter_than_or() {
    // A|B&C == A|(B&C)
    let stmt = parse("MATCH (n) WHERE n:A|B&C RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::HasLabel(hl) => match &hl.label_expr {
                LabelExpr::Or(parts) => {
                    assert_eq!(parts[0], LabelExpr::Label("A".into()));
                    assert_eq!(
                        parts[1],
                        LabelExpr::And(vec![
                            LabelExpr::Label("B".into()),
                            LabelExpr::Label("C".into())
                        ])
                    );
                }
                other => panic!("expected Or, got {other:?}"),
            },
            other => panic!("expected HasLabel, got {other:?}"),
        },
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn test_label_expr_negation_binds_only_following_primary() {
    // !A&B == (!A)&B
    let stmt = parse("MATCH (n) WHERE n:!A&B RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::HasLabel(hl) => match &hl.label_expr {
                LabelExpr::And(parts) => {
                    assert_eq!(
                        parts[0],
                        LabelExpr::Not(Box::new(LabelExpr::Label("A".into())))
                    );
                    assert_eq!(parts[1], LabelExpr::Label("B".into()));
                }
                other => panic!("expected And, got {other:?}"),
            },
            other => panic!("expected HasLabel, got {other:?}"),
        },
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn test_label_expr_wildcard_and_grouping() {
    let stmt = parse("MATCH (n:(A|B)&!%) RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[0] {
            PatternElement::Node(n) => match n.label_expr.as_ref().unwrap() {
                LabelExpr::And(parts) => {
                    assert_eq!(
                        parts[0],
                        LabelExpr::Or(vec![
                            LabelExpr::Label("A".into()),
                            LabelExpr::Label("B".into())
                        ])
                    );
                    assert_eq!(parts[1], LabelExpr::Not(Box::new(LabelExpr::Wildcard)));
                }
                other => panic!("expected And, got {other:?}"),
            },
            _ => panic!("expected node"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_label_expr_colon_conjunction_mixed_with_gql_symbol_same_expr_rejected() {
    // A colon-chain segment that is neither a label name nor a dynamic
    // atom (`!B`, `%`, a plain `$p`) is rejected like a trailing symbol.
    for q in [
        "MATCH (n:A:B|C) RETURN n",
        "MATCH (n:A:!B) RETURN n",
        "MATCH (n:A:$p) RETURN n",
        "MATCH (n:$(a):%) RETURN n",
    ] {
        let err = parse(q).unwrap_err();
        assert!(
            err.message
                .starts_with("InvalidLabelExpression: cannot mix colon-conjunction"),
            "{q}: {}",
            err.message
        );
    }
}

#[test]
fn test_label_expr_paren_colon_chain_segment_with_gql_symbol_rejected() {
    // A parenthesized colon-chain segment whose body uses a GQL
    // label-expression symbol (`|`, `!`, `%`) is rejected with the
    // colon-conjunction mixing error, never silently dropped -- Neo4j
    // (2026.09-community probe: `EXPLAIN MATCH (n:(A|B):C) RETURN n`)
    // rejects the form with 42I10 "Mixing label expression symbols
    // (`|`, `&`, `!`, and `%`) with colon (`:`) between labels is not
    // allowed".
    for q in [
        "MATCH (n:(A|B):C) RETURN n",
        "MATCH (n:(!A):C) RETURN n",
        "MATCH (n:(%):C) RETURN n",
        "MATCH (n:((A|B)):C) RETURN n",
        "MATCH (n:((A|B):C):D) RETURN n",
        "CREATE (n:(A|B):C)",
    ] {
        let err = parse(q).unwrap_err();
        assert!(
            err.message
                .starts_with("InvalidLabelExpression: cannot mix colon-conjunction"),
            "{q}: {}",
            err.message
        );
    }
    // The same parenthesized expressions are fine outside a colon-chain
    // (no `:` follows the group).
    assert!(parse("MATCH (n:(A|B)) RETURN n").is_ok());
    assert!(parse("MATCH (n:(!A)) RETURN n").is_ok());
    // And a pure legacy colon-chain inside the segment still flattens.
    assert!(parse("MATCH (n:(A:B):C) RETURN n").is_ok());
}

#[test]
fn test_label_expr_colon_conjunction_mixed_with_gql_symbol_same_clause_rejected() {
    assert!(parse("MATCH (n:A:B)-[r:!T&!U]->(m:B) RETURN n").is_err());
    // But colon-conjunction beside plain single labels is fine.
    assert!(parse("MATCH (n:A:B)-[r:S]->(m:B) RETURN n").is_ok());
    // And mixing across separate clauses of the same statement is fine.
    assert!(parse("MATCH (n:A:B) MATCH (m:!A&!B) RETURN n, m").is_ok());
}

#[test]
fn test_label_expr_square_bracket_grouping() {
    let stmt = parse("MATCH (n:[A&B]) RETURN n").unwrap();
    let Clause::Match(clause) = &stmt.clauses[0] else {
        panic!("expected MATCH");
    };
    let PatternElement::Node(node) = &clause.pattern[0].elements[0] else {
        panic!("expected node pattern");
    };
    assert!(matches!(node.label_expr, Some(LabelExpr::And(_))));
}

#[test]
fn test_label_expr_dynamic_label_predicate() {
    let stmt = parse("MATCH (n) RETURN n IS LABELED $('A')&$('B') AS r").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::HasLabel(hl) => {
                assert!(hl.strict);
                match &hl.label_expr {
                    LabelExpr::And(parts) => {
                        assert!(matches!(parts[0], LabelExpr::Dynamic(_)));
                        assert!(matches!(parts[1], LabelExpr::Dynamic(_)));
                    }
                    other => panic!("expected And, got {other:?}"),
                }
            }
            other => panic!("expected HasLabel, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_label_expr_dynamic_label_quantifiers_parse() {
    for (q, want) in [
        // $(...) -- Default quantifier.
        (
            "MATCH (n) WHERE n:$('A') RETURN n",
            DynamicLabelQuantifier::Default,
        ),
        // $all(...) -- All quantifier, case-insensitive.
        (
            "MATCH (n) WHERE n:$ALL(['A']) RETURN n",
            DynamicLabelQuantifier::All,
        ),
        // $any(...) -- Any quantifier.
        (
            "MATCH (n) WHERE n:$any(['A']) RETURN n",
            DynamicLabelQuantifier::Any,
        ),
    ] {
        assert_eq!(dynamic_quantifier_of(q), want);
    }
}

/// Parses a single-`MATCH` statement and returns the quantifier of the
/// dynamic label expression in its `WHERE`, so the three quantifier cases
/// share one clause/`HasLabel`/`Dynamic` walk instead of repeating it
/// verbatim.
fn dynamic_quantifier_of(q: &str) -> DynamicLabelQuantifier {
    let stmt = parse(q).unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::HasLabel(hl) => match &hl.label_expr {
                LabelExpr::Dynamic(d) => d.quantifier,
                other => panic!("expected Dynamic, got {other:?}"),
            },
            other => panic!("expected HasLabel, got {other:?}"),
        },
        other => panic!("expected match, got {other:?}"),
    }
}

/// Parses a single-`CREATE` statement and returns the parts of the created
/// node's label expression, so the two CIP-156 colon-chain checks share one
/// clause/element/`And` walk instead of repeating it verbatim.
fn create_node_and_parts(q: &str) -> Vec<LabelExpr> {
    let stmt = parse(q).unwrap();
    match &stmt.clauses[0] {
        Clause::Create(cc) => match &cc.pattern[0].elements[0] {
            PatternElement::Node(np) => match np.label_expr.as_ref().unwrap() {
                LabelExpr::And(parts) => parts.clone(),
                other => panic!("expected And, got {other:?}"),
            },
            other => panic!("expected node pattern, got {other:?}"),
        },
        other => panic!("expected create, got {other:?}"),
    }
}

#[test]
fn test_label_expr_dynamic_colon_chain_in_node_pattern() {
    // CIP-156: `:$(a):$(b)` is a colon-chain of two dynamic label
    // atoms, lowering to `And([Dynamic, Dynamic])` -- same shape as the
    // legacy `:A:B` colon-conjunction of plain labels.
    let parts = create_node_and_parts("CREATE (n:$(a):$(b))");
    assert_eq!(parts.len(), 2);
    assert!(matches!(parts[0], LabelExpr::Dynamic(_)));
    assert!(matches!(parts[1], LabelExpr::Dynamic(_)));

    // Mixed with a plain label in the chain: `:A:$(b)`.
    let parts = create_node_and_parts("CREATE (n:A:$(b))");
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], LabelExpr::Label(l) if l == "A"));
    assert!(matches!(parts[1], LabelExpr::Dynamic(_)));
}

#[test]
fn test_label_expr_dynamic_label_in_case_when_colon_form() {
    // Generalized-CASE `WHEN :$(...)`  alongside `WHEN IS $(...)`.
    let stmt =
        parse("MATCH (n) RETURN CASE n WHEN IS $('A') THEN 1 WHEN :$('B') THEN 2 ELSE 3 END AS r")
            .unwrap();
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::Case(case) => {
                assert_eq!(case.whens.len(), 2);
                assert!(matches!(&case.whens[0].condition, Expr::HasLabel(_)));
                assert!(matches!(&case.whens[1].condition, Expr::HasLabel(_)));
                match &case.whens[1].condition {
                    Expr::HasLabel(hl) => {
                        assert!(matches!(hl.label_expr, LabelExpr::Dynamic(_)))
                    }
                    other => panic!("expected HasLabel, got {other:?}"),
                }
            }
            other => panic!("expected Case, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_list_comprehension_dynamic_label_where_then_map_pipe_not_swallowed() {
    // The `|` after `x:$('A')` must be the comprehension's map
    // separator, not a label-disjunction continuation that would eat
    // `x` from `x.name`.
    let stmt = parse("MATCH (n) RETURN [x IN [n] WHERE x:$('A') | x.name] AS filtered").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::ListComprehension(lc) => {
                match lc.where_expr.as_ref().unwrap() {
                    Expr::HasLabel(hl) => {
                        assert!(matches!(hl.label_expr, LabelExpr::Dynamic(_)))
                    }
                    other => panic!("expected HasLabel, got {other:?}"),
                }
                assert!(matches!(lc.map_expr.as_ref().unwrap(), Expr::Property(_)));
            }
            other => panic!("expected ListComprehension, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_list_comprehension_real_label_disjunction_still_parses_with_no_map() {
    // Without a following map-expr, a genuine `|`-disjunction inside a
    // comprehension WHERE must still parse as a label expression (the
    // ambiguity-avoidance must not over-fire when it's unambiguous).
    let stmt = parse("MATCH (n) RETURN [x IN [n] WHERE x:A|B] AS filtered").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::ListComprehension(lc) => match lc.where_expr.as_ref().unwrap() {
                Expr::HasLabel(hl) => match &hl.label_expr {
                    LabelExpr::Or(parts) => assert_eq!(parts.len(), 2),
                    other => panic!("expected Or, got {other:?}"),
                },
                other => panic!("expected HasLabel, got {other:?}"),
            },
            other => panic!("expected ListComprehension, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_is_labeled_not_allowed_in_pattern_or_set() {
    assert!(parse("MATCH (n IS LABELED Person) RETURN n").is_err());
    assert!(parse("CREATE (n IS LABELED Person) RETURN n").is_err());
    assert!(parse("MATCH (n) SET n IS LABELED Person RETURN n").is_err());
    assert!(parse("MATCH (n) SET n IS NOT LABELED Person RETURN n").is_err());
}

#[test]
fn test_is_not_labeled_negates_whole_expression() {
    // IS NOT LABELED negates the whole label expression, unlike inline `!`.
    let stmt = parse("MATCH (n) RETURN n IS NOT LABELED A&B AS r").unwrap();
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::HasLabel(hl) => {
                assert!(hl.negated);
                assert!(hl.strict);
                assert_eq!(
                    hl.label_expr,
                    LabelExpr::And(vec![
                        LabelExpr::Label("A".into()),
                        LabelExpr::Label("B".into())
                    ])
                );
            }
            other => panic!("expected HasLabel, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn test_set_label_set_colon_and_is_forms() {
    // Single-label colon and single-label IS items may coexist on the
    // same statement.
    let stmt = parse("MATCH (n) SET n:A, n IS B RETURN n").unwrap();
    match &stmt.clauses[1] {
        Clause::Set(sc) => {
            assert_eq!(sc.items[0].labels, vec![LabelExpr::Label("A".into())]);
            assert!(!sc.items[0].is_keyword);
            assert_eq!(sc.items[1].labels, vec![LabelExpr::Label("B".into())]);
            assert!(sc.items[1].is_keyword);
        }
        other => panic!("expected set clause, got {other:?}"),
    }
}

#[test]
fn test_set_label_set_rejects_gql_symbols() {
    assert!(parse("MATCH (n) SET n:A|B RETURN n").is_err());
    assert!(parse("MATCH (n) SET n:A&B RETURN n").is_err());
    assert!(parse("MATCH (n) SET n:!A RETURN n").is_err());
    assert!(parse("MATCH (n) SET n:% RETURN n").is_err());
    assert!(parse("MATCH (n) SET n IS A&B RETURN n").is_err());
    assert!(parse("MATCH (n) REMOVE n:A|B").is_err());
}

#[test]
fn test_set_label_set_rejects_is_and_colon_conjunction_same_item() {
    assert!(parse("MATCH (n) SET n IS A:B RETURN n").is_err());
    assert!(parse("MATCH (n) REMOVE n IS A:B").is_err());
}

#[test]
fn test_set_label_set_rejects_is_and_colon_conjunction_same_statement() {
    assert!(parse("MATCH (n), (m) SET n IS A, m:A:B RETURN n").is_err());
    // Single-label items of either syntax may coexist.
    assert!(parse("MATCH (n), (m) SET n IS A, m:B RETURN n").is_ok());
}

#[test]
fn test_case_simple_operand_is_labeled_predicate() {
    let stmt = parse(
        "MATCH (n) RETURN CASE n WHEN IS LABELED A THEN 1 WHEN IS NOT LABELED B THEN 2 ELSE 0 END AS r",
    )
    .unwrap();
    match &stmt.clauses[1] {
        Clause::Return(rc) => match &rc.items[0].expr {
            Expr::Case(case) => {
                assert!(matches!(&case.whens[0].condition, Expr::HasLabel(_)));
                assert!(matches!(&case.whens[1].condition, Expr::HasLabel(_)));
                match &case.whens[1].condition {
                    Expr::HasLabel(hl) => assert!(hl.negated),
                    other => panic!("expected HasLabel, got {other:?}"),
                }
            }
            other => panic!("expected case, got {other:?}"),
        },
        other => panic!("expected return, got {other:?}"),
    }
}

// ---- the `mixed-label-syntax` preview opt-in (ADR-0371 gate threading,
// Neo4j's mixed-label-syntax allowance) ----

/// Each form below is rejected on the dialect floor (the CIP-40/CIP-14
/// mixing rejections); the ones that keep their forms in *separate* label
/// expressions of one clause are accepted under the
/// `mixed-label-syntax` preview opt-in, with `:` parsed as an `&`-level
/// conjunction operator. Neo4j keeps mixing *within* one label expression
/// illegal even under the switch
/// (GpmSyntaxMixingAllowedAcceptance "Mixing : conjunction with
/// GPM-only label/type expression operators"), so those stay rejected.
#[test]
fn test_mixed_label_syntax_preview_gating() {
    let on = crate::lexer::ParseOptions {
        mixed_label_syntax: true,
        ..Default::default()
    };
    for q in [
        "MATCH (m)-->+(n IS S:R) RETURN *",
        "MATCH (m)-[IS Q]->+(n:S:R) RETURN *",
        "MATCH (n:A:B)-[]-(m) WHERE m:(A&B)|C RETURN *",
        "CREATE (IS A:B:C)",
        "CREATE (:A:B)-[IS R]->(:T:S)",
        "RETURN n:A&B, n:A:B",
        "RETURN COUNT { MATCH (n:A:B) MATCH (m:A|B) RETURN m } AS c",
    ] {
        assert!(parse(q).is_err(), "floor must reject {q}");
        assert!(
            parse_with_options(q, on).is_ok(),
            "mixed-label-syntax must accept {q}"
        );
    }
    // Within ONE label expression the mixing stays illegal, exactly as
    // Neo4j's allowed-suite pins it (`(n:A\|B:C)`, `CREATE (n:A:B&C)`).
    for q in [
        "MATCH (n:A|B:C) RETURN *",
        "MATCH (n:A:B&C) RETURN n",
        "MERGE (n:A:B&C)",
    ] {
        assert!(parse(q).is_err(), "floor must reject {q}");
        assert!(
            parse_with_options(q, on).is_err(),
            "mixed-label-syntax must still reject within-expression mixing: {q}"
        );
    }
    // A CALL subquery body is one label-mixing scope
    // (`test_call_subquery_body_is_one_label_scope`), and a body mixing
    // across its own statements is exactly what the opt-in admits (Neo4j
    // GpmSyntaxMixingAllowedAcceptance "Allowed to mix label expression
    // syntax in CALL" under its mixed-label allowance).
    let q = "CALL () { MATCH (e:A|B) RETURN e UNION MATCH (e:A:C) RETURN e } RETURN *";
    assert!(parse(q).is_err(), "floor must reject {q}");
    assert!(parse_with_options(q, on).is_ok(), "{q}");
}

/// Under the opt-in, `:` binds at `&` level. The observable consequence is
/// that a pure colon chain still flattens to one conjunction (`:A:B:C`),
/// while a colon *conjunction* inside one expression together with a GQL
/// symbol is rejected before the AST is built.
#[test]
fn test_mixed_label_syntax_colon_binds_at_and_level() {
    let on = crate::lexer::ParseOptions {
        mixed_label_syntax: true,
        ..Default::default()
    };
    let stmt = parse_with_options("MATCH (n:A:B:C) RETURN n", on).unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[0] {
            PatternElement::Node(n) => match n.label_expr.as_ref().unwrap() {
                LabelExpr::And(parts) => assert_eq!(parts.len(), 3, "flat colon chain: {parts:?}"),
                other => panic!("expected And, got {other:?}"),
            },
            other => panic!("expected node, got {other:?}"),
        },
        other => panic!("expected match, got {other:?}"),
    }
    assert!(parse_with_options("MATCH (n:A|B:C) RETURN n", on).is_err());
}

/// The floor's pure-form behavior is unchanged by the opt-in's presence
/// in the codebase: plain colon chains, plain GQL expressions and plain
/// IS introducers all parse both with and without the opt-in.
#[test]
fn test_mixed_label_syntax_pure_forms_unaffected() {
    let on = crate::lexer::ParseOptions {
        mixed_label_syntax: true,
        ..Default::default()
    };
    for q in [
        "MATCH (n:A:B:C) RETURN n",
        "MATCH (n:(A&B)|C) RETURN n",
        "MATCH (n IS (A&B)|C)-[IS R]->+() RETURN n",
        "MATCH (n:A:B)-[r:S|T|U]-() RETURN n",
    ] {
        assert!(parse(q).is_ok(), "{q}");
        assert!(parse_with_options(q, on).is_ok(), "{q}");
    }
}
