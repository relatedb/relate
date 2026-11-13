use super::*;
use relate_names::SmallName;

#[test]
fn test_parse_simple_match_return() {
    let stmt = parse("MATCH (n) RETURN n").unwrap();
    assert_eq!(stmt.clauses.len(), 2);
    match &stmt.clauses[0] {
        Clause::Match(m) => {
            assert!(!m.optional);
            assert_eq!(m.pattern.len(), 1);
            assert_eq!(m.pattern[0].elements.len(), 1);
            match &m.pattern[0].elements[0] {
                PatternElement::Node(n) => {
                    assert_eq!(n.variable, Some("n".into()));
                }
                _ => panic!("expected node pattern"),
            }
        }
        _ => panic!("expected match clause"),
    }
    match &stmt.clauses[1] {
        Clause::Return(r) => {
            assert_eq!(r.items.len(), 1);
            match &r.items[0].expr {
                Expr::Variable(name) => assert_eq!(name, "n"),
                _ => panic!("expected variable"),
            }
        }
        _ => panic!("expected return clause"),
    }
}

#[test]
fn test_parse_match_with_labels() {
    let stmt = parse("MATCH (n:Person) RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[0] {
            PatternElement::Node(n) => {
                assert_eq!(
                    n.label_expr.as_ref().and_then(|e| e.as_label_list()),
                    Some(vec!["Person".into()])
                );
            }
            _ => panic!("expected node"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_match_with_properties() {
    let stmt = parse("MATCH (n:Person {name: 'Alice'}) RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[0] {
            PatternElement::Node(n) => {
                assert!(n.has_properties);
                assert_eq!(n.properties.len(), 1);
                assert_eq!(n.properties[0].0, "name");
                assert_eq!(
                    n.properties[0].1,
                    Expr::Literal(LiteralValue::String("Alice".into()))
                );
            }
            _ => panic!("expected node"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_relationship() {
    let stmt = parse("MATCH (a)-[r:KNOWS]->(b) RETURN a, b").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => {
            assert_eq!(m.pattern[0].elements.len(), 3);
            match &m.pattern[0].elements[1] {
                PatternElement::Relationship(r) => {
                    assert_eq!(r.variable, Some("r".into()));
                    assert_eq!(
                        r.type_expr.as_ref().and_then(|e| e.as_label_list()),
                        Some(vec!["KNOWS".into()])
                    );
                    assert_eq!(r.direction, Direction::Right);
                }
                _ => panic!("expected relationship"),
            }
        }
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_left_arrow() {
    let stmt = parse("MATCH (a)<-[r]-(b) RETURN a").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[1] {
            PatternElement::Relationship(r) => {
                assert_eq!(r.direction, Direction::Left);
            }
            _ => panic!("expected relationship"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_undirected() {
    let stmt = parse("MATCH (a)-[r]-(b) RETURN a").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[1] {
            PatternElement::Relationship(r) => {
                assert_eq!(r.direction, Direction::Both);
            }
            _ => panic!("expected relationship"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_where() {
    let stmt = parse("MATCH (n) WHERE n.age > 30 RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => {
            assert!(m.where_expr.is_some());
        }
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_named_path() {
    let stmt = parse("MATCH p = (a)-[r]->(b) RETURN p").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => {
            assert_eq!(m.pattern[0].variable, Some("p".into()));
        }
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_variable_length_rel() {
    let stmt = parse("MATCH (a)-[*1..3]->(b) RETURN a").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[1] {
            PatternElement::Relationship(r) => {
                assert_eq!(r.min_hops, Some(1));
                assert_eq!(r.max_hops, Some(3));
            }
            _ => panic!("expected relationship"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_pattern_predicate() {
    let stmt = parse("MATCH (n) WHERE (n)-[:KNOWS]->() RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match m.where_expr.as_ref().unwrap() {
            Expr::PatternPredicate(_) => {}
            _ => panic!("expected pattern predicate"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_multiple_rel_types() {
    let stmt = parse("MATCH (a)-[:KNOWS|LIKES]->(b) RETURN a").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[1] {
            PatternElement::Relationship(r) => match r.type_expr.as_ref().unwrap() {
                LabelExpr::Or(parts) => {
                    let names: Vec<SmallName> = parts
                        .iter()
                        .map(|p| match p {
                            LabelExpr::Label(n) => n.clone(),
                            _ => panic!("expected label"),
                        })
                        .collect();
                    assert_eq!(names, vec!["KNOWS", "LIKES"]);
                }
                _ => panic!("expected disjunction"),
            },
            _ => panic!("expected relationship"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_multiple_patterns() {
    let stmt = parse("MATCH (a), (b) RETURN a, b").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => {
            assert_eq!(m.pattern.len(), 2);
        }
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_empty_node() {
    let stmt = parse("MATCH () RETURN 1").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[0] {
            PatternElement::Node(n) => {
                assert!(n.variable.is_none());
                assert!(n.label_expr.is_none());
            }
            _ => panic!("expected node"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_bare_star_rel() {
    let stmt = parse("MATCH (a)-[*]->(b) RETURN a").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[1] {
            PatternElement::Relationship(r) => {
                assert_eq!(r.min_hops, Some(1));
                assert!(r.max_hops.is_none());
            }
            _ => panic!("expected relationship"),
        },
        _ => panic!("expected match"),
    }
}

#[test]
fn test_parse_legacy_hop_bounds_all_spellings() {
    // All six legacy bracket-syntax hop-bound spellings (`*`, `*n`, `*n..`,
    // `*n..m`, `*..`, `*..m`) and their (min, max) lowering.
    for (q, want_min, want_max) in [
        ("MATCH (a)-[*]->(b) RETURN a", 1, None),
        ("MATCH (a)-[*3]->(b) RETURN a", 3, Some(3)),
        ("MATCH (a)-[*2..]->(b) RETURN a", 2, None),
        ("MATCH (a)-[*1..3]->(b) RETURN a", 1, Some(3)),
        ("MATCH (a)-[*..]->(b) RETURN a", 1, None),
        ("MATCH (a)-[*..4]->(b) RETURN a", 1, Some(4)),
    ] {
        let stmt = parse(q).unwrap();
        match &stmt.clauses[0] {
            Clause::Match(m) => match &m.pattern[0].elements[1] {
                PatternElement::Relationship(r) => {
                    assert_eq!(r.min_hops, Some(want_min), "{q}: min_hops");
                    assert_eq!(r.max_hops, want_max, "{q}: max_hops");
                }
                _ => panic!("{q}: expected relationship"),
            },
            _ => panic!("{q}: expected match"),
        }
    }
}

#[test]
fn test_relationship_type_negation_or_wildcard_with_quantifier_accepted() {
    // The legacy bracket-syntax variable-length form accepts a type
    // expression containing negation or the wildcard: it is the same AST
    // the GPM postfix form normalizes to (CIP-28 Form 5), so the old
    // bracket-only rejection was a syntax-gap (Neo4j
    // GpmSyntaxMixingAcceptance "Var-length relationship with GPM
    // relationship type expression - OK"; the CIP-40 quantifier-mixing
    // [12] DEVIATION row is retired with it).
    assert!(parse("MATCH ()-[:!S*]->() RETURN 1").is_ok());
    assert!(parse("MATCH ()-[:%*]->() RETURN 1").is_ok());
    assert!(parse("MATCH ()-[r:!A&!B*1..5]->() RETURN r").is_ok());
    // Single-type and legacy pipe-disjunction quantified patterns remain valid.
    assert!(parse("MATCH ()-[:S*]->() RETURN 1").is_ok());
    assert!(parse("MATCH ()-[:S|T*1..2]->() RETURN 1").is_ok());
    assert!(parse("MATCH ()-[:(S)*]->() RETURN 1").is_ok());
}

#[test]
fn test_node_and_relationship_element_pattern_where() {
    assert!(parse("MATCH (n WHERE n:A&B)-[r]->(m) RETURN n").is_ok());
    assert!(parse("MATCH (n)-[r WHERE r:!S|!T]->(m) RETURN n").is_ok());
}

#[test]
fn test_parenthesized_path_pattern_where() {
    let stmt = parse("MATCH ((n)-[r]->(m) WHERE n.p > m.q AND r:S) RETURN n").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(mc) => {
            assert!(mc.pattern[0].where_expr.is_some());
            assert_eq!(mc.pattern[0].elements.len(), 3);
        }
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn test_path_variable_on_top_level_parenthesized_pattern() {
    // CIP-19 basic-structure #15: `p = ((a)-[r]->(b))`.
    let stmt = parse("MATCH p = ((a)-[r:T]->(b)) RETURN p").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(mc) => {
            assert_eq!(
                mc.pattern[0].variable.as_ref(),
                Some(&SmallName::from_name("p"))
            );
            assert_eq!(mc.pattern[0].elements.len(), 3);
        }
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn test_path_variable_on_top_level_parenthesized_pattern_with_where() {
    let stmt = parse("MATCH p = ((a)-[r:T]->(b) WHERE a.p > b.q) RETURN p").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(mc) => {
            assert_eq!(
                mc.pattern[0].variable.as_ref(),
                Some(&SmallName::from_name("p"))
            );
            assert!(mc.pattern[0].where_expr.is_some());
        }
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn test_inline_where_on_legacy_var_length_accepted() {
    // `-[r*1..5 WHERE ...]->` is the same AST the GPM postfix form
    // `-[r WHERE ...]->{1,5}` normalizes to (CIP-28 Form 5), so the old
    // bracket-only rejection was a syntax-gap (Neo4j
    // GpmSyntaxMixingAcceptance "Var-length relationship with inline
    // predicate - OK"; the quantifier-mixing DEVIATION row is
    // retired with it).
    let stmt = parse("MATCH ()-[r*1..5 WHERE r.p > 30]->() RETURN r").unwrap();
    match &stmt.clauses[0] {
        Clause::Match(m) => match &m.pattern[0].elements[1] {
            PatternElement::Relationship(r) => {
                assert_eq!(r.min_hops, Some(1));
                assert_eq!(r.max_hops, Some(5));
                assert!(r.where_expr.is_some());
            }
            other => panic!("expected relationship, got {other:?}"),
        },
        other => panic!("expected match, got {other:?}"),
    }
    assert!(parse("MATCH ()-[r WHERE r.p > 30]->{1,5}() RETURN r").is_ok());
}

#[test]
fn test_pattern_comprehension_quantifiers_isolated_from_clause_forms() {
    // A pattern comprehension's inner pattern is its own pattern context
    // for the quantifier-mixing check: a legacy bracket quantifier inside
    // a comprehension nested in a quantified path pattern's WHERE is
    // legal (Neo4j QuantifiedPathPatternAcceptance "Pattern comprehension
    // containing a variable-length relationship inside a QPP is
    // allowed")...
    assert!(parse(
        "MATCH ( (a)-[]-(b)-[]-(c) WHERE size([(a)-[:Z*1..2]->(:X) | a]) > 0 )+ RETURN *"
    )
    .is_ok());
    // ...while two comprehensions' quantifier forms still combine with
    // each other in the same clause (CIP-40 quantifier-mixing scenario
    // 10), and the clause's own forms still combine with each other
    // (scenario 3).
    assert!(parse("RETURN [(n)--+(m) | m], [(n)-[*3]-(m) | m]").is_err());
    assert!(parse("MATCH ()-[r:A*1..2]->+() RETURN r").is_err());
}

#[test]
fn test_call_subquery_body_is_one_label_scope() {
    // A CALL subquery's body is one label-mixing scope *per side*: mixing
    // colon-conjunction in one read clause with the IS introducer or a GQL
    // symbol in another read clause is rejected (Neo4j
    // GpmSyntaxMixingAcceptance "Conflicting syntax in separate statements
    // within a CALL subquery").
    assert!(parse("CALL () { MATCH (n:A:B) MATCH (n)--(m:A&!B) RETURN * } RETURN n").is_err());
    assert!(parse("CALL () { MATCH (n:A:B) MATCH (n)--(IS A) RETURN * } RETURN n").is_err());
    // Two write clauses combine too, and MERGE counts as both sides.
    assert!(parse("CALL () { CREATE (n:A:B) CREATE (m:C&D) RETURN * } RETURN *").is_err());
    assert!(parse("CALL () { MERGE (n:A:B) CREATE (m IS C) RETURN * } RETURN *").is_err());
    // A read clause and a write clause do NOT combine: the pinned
    // acceptance row pairs `MATCH (n:A:B)` with a CREATE using `IS`, which
    // live Neo4j 2026.09 accepts.
    assert!(
        parse("CALL () { MATCH (n:A:B) CREATE (n)-[IS R]->(m IS B) RETURN * } RETURN n").is_ok()
    );
    // ...while a body whose statements each stay on one side of the
    // mixing line still parses (Neo4j GpmSyntaxMixingAllowedAcceptance
    // "Mixing GPM and non-GPM of unrelated features within a subquery").
    assert!(parse("CALL () { MATCH (n:A:B) MATCH (n)--+(:A) RETURN * } RETURN n").is_ok());
    assert!(
        parse("CALL () { MATCH (:A)-[*1..5]->(m:B) WITH m:!A AS n RETURN n } RETURN n").is_ok()
    );
    // The same mixing inside one clause still errors...
    assert!(parse("MATCH (n:A:B)-[IS R]->(m) RETURN n").is_err());
    // ...and an EXISTS/COUNT subquery *expression* still bubbles its
    // label usage into the enclosing clause (CIP-40
    // label-expression-mixing scenarios 11/12).
    assert!(parse("MATCH (n:A:B) WHERE EXISTS { MATCH (m:C|D) } RETURN n").is_err());
    assert!(parse("RETURN COUNT { MATCH (n:A:B) MATCH (m:C|D) RETURN m } AS c").is_err());
}

#[test]
fn test_pattern_comprehension_with_inline_node_predicate() {
    // CIP-17/CIP-18: the node pattern's own `WHERE` runs to the node's
    // closing paren, so the comprehension (not a list literal) wins.
    let stmt = parse("RETURN [(a:A WHERE a.prop > 1)-[r]-(b:B) | a.prop] AS result").unwrap();
    let Clause::Return(ret) = &stmt.clauses[0] else {
        panic!("expected return");
    };
    let Expr::PatternComprehension(pc) = &ret.items[0].expr else {
        panic!(
            "expected pattern comprehension, got {:?}",
            ret.items[0].expr
        );
    };
    let PatternElement::Node(node) = &pc.pattern.elements[0] else {
        panic!("expected node pattern");
    };
    assert!(node.where_expr.is_some());
}

#[test]
fn test_pattern_comprehension_predicate_references_later_element() {
    // A predicate on one element may reference another element declared
    // later in the same pattern (CIP-17/CIP-18 5.2.1).
    let stmt =
        parse("RETURN [(a:A WHERE b.prop > 1)-[r]-(b:B) | [a.prop, b.prop]] AS result").unwrap();
    let Clause::Return(ret) = &stmt.clauses[0] else {
        panic!("expected return");
    };
    assert!(matches!(&ret.items[0].expr, Expr::PatternComprehension(_)));
}

#[test]
fn test_pattern_comprehension_where_label_expression() {
    // The comprehension's own WHERE ends in a label predicate; the `|` must
    // be read as the map separator, not a label disjunction.
    let stmt =
        parse("MATCH (n) RETURN [p = (n)-->() WHERE last(nodes(p)):End | p] AS list").unwrap();
    let Clause::Return(ret) = &stmt.clauses[1] else {
        panic!("expected return");
    };
    let Expr::PatternComprehension(pc) = &ret.items[0].expr else {
        panic!("expected pattern comprehension");
    };
    assert!(matches!(pc.where_expr, Some(Expr::HasLabel(_))));
}

#[test]
fn test_path_and_subpath_variables_bind_same_pattern() {
    let stmt = parse("MATCH p = ANY SHORTEST (q = (:A)-->+(:B)) RETURN p = q AS result").unwrap();
    let Clause::Match(mc) = &stmt.clauses[0] else {
        panic!("expected match");
    };
    assert_eq!(
        mc.pattern[0].variable.as_ref(),
        Some(&SmallName::from_name("p"))
    );
    assert_eq!(
        mc.pattern[0].subpath_variable.as_ref(),
        Some(&SmallName::from_name("q"))
    );
    assert_eq!(mc.pattern[0].path_names().collect::<Vec<_>>(), ["p", "q"]);
}

#[test]
fn test_path_names_of_unnamed_path_is_empty() {
    let stmt = parse("MATCH (n) RETURN n").unwrap();
    let Clause::Match(mc) = &stmt.clauses[0] else {
        panic!("expected match");
    };
    assert!(mc.pattern[0].path_names().next().is_none());
}

#[test]
fn test_different_relationship_singular_match_mode() {
    let stmt = parse("MATCH DIFFERENT RELATIONSHIP (a) RETURN a").unwrap();
    let Clause::Match(mc) = &stmt.clauses[0] else {
        panic!("expected match");
    };
    assert_eq!(mc.match_mode, MatchMode::DifferentRelationships);
}

#[test]
fn test_unbounded_qpp_allowed_inside_exists_but_not_at_top_level() {
    // REPEATABLE ELEMENTS rejects an unbounded quantified path pattern in a
    // top-level match, but an EXISTS body is an existence probe and accepts
    // it (PathModeHourglassGraph).
    parse(
        "MATCH REPEATABLE ELEMENTS q = (x)-->{3,6}(x) \
           WHERE EXISTS { MATCH REPEATABLE ELEMENTS p = (x)-->+({i:2})-->+({i:1}) } \
           RETURN q",
    )
    .expect("unbounded QPP inside EXISTS must parse");
    assert!(parse("MATCH REPEATABLE ELEMENTS ()--+() RETURN 1").is_err());
}

// ---- CIP-51/CIP-60 match modes + path modes (Cypher 25 regression pins) ----

#[test]
fn test_match_mode_spellings_resolve_to_their_mode() {
    // Every position a match mode can be written: directly after `MATCH`
    // (the order the CIP-51 corpus's Neo4j-verified scenarios use), after
    // a path variable declaration in that same clause-initial position
    // (`MATCH REPEATABLE ELEMENTS p = ...`), and the path-variable-first
    // spelling (`MATCH p = REPEATABLE ELEMENTS (...)`). A bare pattern
    // keeps the implicit DIFFERENT RELATIONSHIPS default.
    for (q, want_mode) in [
        (
            "MATCH REPEATABLE ELEMENTS (a)-[r]->(b) RETURN a",
            MatchMode::RepeatableElements,
        ),
        (
            "MATCH REPEATABLE ELEMENTS p = (a)-[r]->(b) RETURN p",
            MatchMode::RepeatableElements,
        ),
        (
            "MATCH p = REPEATABLE ELEMENTS (a)-[r]->(b) RETURN p",
            MatchMode::RepeatableElements,
        ),
        (
            "MATCH DIFFERENT RELATIONSHIPS (a)-[r]->(b) RETURN a",
            MatchMode::DifferentRelationships,
        ),
        (
            "MATCH DIFFERENT RELATIONSHIPS p = (a)-[r]->(b) RETURN p",
            MatchMode::DifferentRelationships,
        ),
        (
            "MATCH p = DIFFERENT RELATIONSHIPS (a)-[r]->(b) RETURN p",
            MatchMode::DifferentRelationships,
        ),
        (
            "MATCH (a)-[r]->(b) RETURN a",
            MatchMode::DifferentRelationships,
        ),
    ] {
        let stmt = parse_ok(q);
        assert_eq!(match_clause(&stmt).match_mode, want_mode, "{q}");
    }
}

#[test]
fn test_match_mode_path_variable_first_binds_the_path_variable() {
    // `MATCH p = REPEATABLE ELEMENTS (pattern)`: the declared variable
    // lands on the first parsed path pattern, exactly as it does for
    // `p = TRAIL (...)`.
    for q in [
        "MATCH p = REPEATABLE ELEMENTS (a)-[r]->(b) RETURN p",
        "MATCH p = DIFFERENT RELATIONSHIPS (a)-[r]->(b) RETURN p",
        "MATCH REPEATABLE ELEMENTS p = (a)-[r]->(b) RETURN p",
        "MATCH DIFFERENT RELATIONSHIPS p = (a)-[r]->(b) RETURN p",
    ] {
        let stmt = parse_ok(q);
        assert_eq!(
            match_clause(&stmt).pattern[0].variable.as_ref(),
            Some(&SmallName::from_name("p")),
            "{q}: path variable"
        );
    }
}

#[test]
fn test_explicit_match_mode_still_rejects_a_shortest_path_function() {
    // The explicitness of a written match-mode keyword is observable
    // through CIP-40's mixing check -- in both the clause-initial and the
    // path-variable-first spelling.
    for q in [
        "MATCH REPEATABLE ELEMENTS shortestPath((a)-[r]->(b)) RETURN p",
        "MATCH p = REPEATABLE ELEMENTS shortestPath((a)-[r]->(b)) RETURN p",
    ] {
        let err = parse(q).expect_err("expected a mixing error");
        assert!(
            err.message.starts_with("MixingShortestPathAndMatchMode"),
            "{q}: wrong error: {}",
            err.message
        );
    }
    // The implicit default never trips the check.
    assert!(parse("MATCH p = shortestPath((a)-[r]->(b)) RETURN p").is_ok());
}

#[test]
fn test_match_mode_phrase_before_path_variable_is_not_a_path_mode() {
    // A match-mode phrase never satisfies the *path*-mode prefix, so the
    // The path-variable-order check keeps firing for real path modes:
    // `MATCH TRAIL p = ...` stays an authored InvalidPathModePosition
    // error (the spec harness's `cip-eval` suite,
    // Neo4j-verified).
    let err = parse("MATCH TRAIL p = (x)--{2}(x) RETURN count(*) AS count")
        .expect_err("expected InvalidPathModePosition");
    assert!(
        err.message.starts_with("InvalidPathModePosition"),
        "wrong error: {}",
        err.message
    );
}

#[test]
fn test_match_mode_path_variable_first_honors_the_combination_rules() {
    // The var-first spelling runs through the same combination checks as
    // the mode-after-MATCH spelling: REPEATABLE ELEMENTS only combines
    // with WALK and rejects unbounded quantified path
    // patterns.
    let err = parse("MATCH p = REPEATABLE ELEMENTS TRAIL (a)-[r]->(b) RETURN p")
        .expect_err("expected UnsupportedCombinationOfMatchModeAndPathMode");
    assert!(
        err.message
            .starts_with("UnsupportedCombinationOfMatchModeAndPathMode"),
        "wrong error: {}",
        err.message
    );
    let err = parse("MATCH p = REPEATABLE ELEMENTS ((a)-[r]->(b))+ RETURN p")
        .expect_err("expected UnsafeRepeatableElements");
    assert!(
        err.message.starts_with("UnsafeRepeatableElements"),
        "wrong error: {}",
        err.message
    );
}

#[test]
fn test_path_mode_keywords_set_the_path_mode() {
    // CIP-51: the four path modes, in the variable-first order the CIP's
    // own grammar specifies (`<path variable declaration>` before the
    // `<path pattern prefix>`).
    for (q, want) in [
        ("MATCH p = WALK (a)-[r]->(b) RETURN p", PathMode::Walk),
        ("MATCH p = TRAIL (a)-[r]->(b) RETURN p", PathMode::Trail),
        ("MATCH p = ACYCLIC (a)-[r]->(b) RETURN p", PathMode::Acyclic),
        ("MATCH p = SIMPLE (a)-[r]->(b) RETURN p", PathMode::Simple),
        // And without a path variable, mode-only prefix.
        ("MATCH TRAIL (a)-[r]->(b) RETURN a", PathMode::Trail),
    ] {
        let stmt = parse_ok(q);
        assert_eq!(match_clause(&stmt).pattern[0].mode, Some(want), "{q}");
    }
    // No keyword: the implicit WALK default is left unset here and
    // resolved at validation.
    let stmt = parse_ok("MATCH (a)-[r]->(b) RETURN a");
    assert_eq!(match_clause(&stmt).pattern[0].mode, None);
}

/// A doubly-parenthesized named group files its name on the inner path's
/// `subpath_variable` (the inner group is itself a `q = …` group), so the
/// outer propagation must read both fields — taking only `inner.variable`
/// dropped the name for `p = ((q = (:A)-->(:B)))`.
#[test]
fn nested_named_groups_keep_their_sub_path_variable() {
    let stmt = parse_ok("MATCH p = ((q = (:A)-->(:B))) RETURN p");
    assert_eq!(
        match_clause(&stmt).pattern[0].subpath_variable.as_ref(),
        Some(&SmallName::from_name("q"))
    );
}
