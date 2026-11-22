//! The comprehension-WHERE `|` disambiguation tests — the pipe-yielding,
//! map-projection, dynamic-label-atom, type-disjunction, `|:` rollback,
//! GQL-symbol-note and probe-verdict-memo pins, plus the direct-driver
//! scaffold they share — mirroring the parser module's own
//! `comprehension_where.rs` organization.

use super::labels::LabelSpec;
use super::*;
use crate::parser::comprehension_where::ComprehensionWhereContext;

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

/// Live Neo4j resolves the comprehension-WHERE pipe by preferring the
/// map separator whenever the rest parses as the map expression, so a
/// single `|` with no later pipe in the comprehension is the separator:
/// `x:A|B]` reads `WHERE x:A` with the map expression `B` (which then
/// fails on the undefined name) — the disjunction reading survives only
/// when a later pipe in the same comprehension makes the separator
/// unparseable.
#[test]
fn test_list_comprehension_single_pipe_yields_to_the_map_separator() {
    for (q, want_or) in [
        // No second pipe: the separator reading — the WHERE keeps the
        // first label and the map expression is the bare `B`.
        ("MATCH (n) RETURN [x IN [n] WHERE x:A|B] AS filtered", false),
        // No second pipe but a boolean operator after the would-be
        // continuation: still the separator — the map expression is the
        // whole `B AND ...` chain.
        (
            "MATCH (n) RETURN [x IN [n] WHERE x:A|B AND x.p1 IS NOT NULL] AS filtered",
            false,
        ),
        // A later pipe makes the separator unparseable, so the
        // disjunction commits and the second pipe is the separator.
        (
            "MATCH (n) RETURN [x IN [n] WHERE x:A|B | x.p1] AS filtered",
            true,
        ),
    ] {
        let stmt = parse_ok(q);
        let Expr::ListComprehension(lc) = return_expr(&stmt) else {
            panic!("{q}: expected a list comprehension");
        };
        let Some(where_expr) = lc.where_expr.as_ref() else {
            panic!("{q}: expected a WHERE");
        };
        let Expr::HasLabel(hl) = where_expr else {
            panic!("{q}: expected a HasLabel WHERE");
        };
        let is_or = matches!(&hl.label_expr, LabelExpr::Or(parts) if parts.len() == 2);
        assert_eq!(is_or, want_or, "{q}: WHERE is {:#?}", hl.label_expr);
        assert!(lc.map_expr.is_some(), "{q}: the map expression exists");
    }
}

/// A comprehension WHERE's label predicate leaves the `|` for the map
/// separator when no later pipe in the comprehension keeps the
/// disjunction alive — the closer-scan rule, whatever token follows the
/// would-be disjunct. `[n IN nodes(p) WHERE n:L1 | n{.p1}]` keeps the
/// WHERE `n:L1` and takes `n{.p1}` as its map expression.
#[test]
fn comprehension_where_label_predicate_yields_the_pipe_to_a_map_projection() {
    for query in [
        "MATCH p=(a:L1)-[:R]->(b:L2) RETURN [n IN nodes(p) WHERE n:L1 | n{.p1}] AS x",
        // The empty map projection spelling.
        "MATCH (n) RETURN [m IN [n] WHERE m:L1 | m {}] AS x",
    ] {
        let stmt = parse_ok(query);
        match return_expr(&stmt) {
            Expr::ListComprehension(lc) => {
                assert!(
                    is_plain_label(&lc.where_expr),
                    "{query}: the WHERE stayed the label predicate"
                );
                assert!(
                    lc.map_expr.as_ref().is_some_and(is_map_projection),
                    "{query}: the map expression is the projection"
                );
            }
            other => panic!("{query}: expected a list comprehension, got {other:?}"),
        }
    }
}

/// A pattern comprehension's WHERE: the same closer-scan rule, map
/// required — `[(a)-[:R]->(b) WHERE a:L1 | b{.p1}]` keeps its WHERE
/// predicate and projects through `b{.p1}`.
#[test]
fn pattern_comprehension_where_yields_the_pipe_to_a_map_projection() {
    let stmt = parse_ok("MATCH (a) RETURN [(a)-[:R]->(b) WHERE a:L1 | b{.p1}] AS x");
    match return_expr(&stmt) {
        Expr::PatternComprehension(pc) => {
            assert!(
                is_plain_label(&pc.where_expr),
                "the WHERE stayed the label predicate"
            );
            assert!(
                is_map_projection(&pc.map_expr),
                "the map expression is the projection"
            );
        }
        other => panic!("expected PatternComprehension, got {other:?}"),
    }
}

/// A property access map expression still takes the pipe: with no later
/// pipe in the comprehension the closer-scan rule yields the `|` to the
/// map separator, whatever token follows the would-be disjunct.
#[test]
fn a_property_access_map_expression_still_takes_the_pipe() {
    let stmt = parse_ok("MATCH (n) RETURN [m IN [n] WHERE m:L1 | m.p1] AS x");
    match return_expr(&stmt) {
        Expr::ListComprehension(lc) => {
            assert!(matches!(lc.map_expr.as_ref(), Some(Expr::Property(_))));
            assert!(is_plain_label(&lc.where_expr));
        }
        other => panic!("expected ListComprehension, got {other:?}"),
    }
}

/// A map expression beginning with a function name
/// (`substring(m.p1, 0, 1)`) takes the pipe after a label-filter WHERE
/// exactly as a property access does — no later pipe, so the
/// closer-scan rule yields the `|` to the separator. The REL8-1871 gap
/// this family once covered: the would-be continuation committed the
/// function's name as a label atom and the `(` then failed the
/// comprehension (`expected RightBracket, got "("`).
#[test]
fn a_function_call_map_expression_takes_the_pipe_after_a_label_filter() {
    for query in [
        "MATCH (n) RETURN [m IN [n] WHERE m:L1 | substring(m.p1, 0, 1)] AS x",
        // The label-filter WHERE's `IS` introducer spelling, same reader.
        "MATCH (n) RETURN [m IN [n] WHERE m IS L1 | substring(m.p1, 0, 1)] AS x",
        // A zero-argument call, and a nested call as the argument.
        "MATCH (n) RETURN [m IN [n] WHERE m:L1 | toString(m)] AS x",
        "MATCH (n) RETURN [m IN [n] WHERE m:L1 | substring(toString(m.p1), 0, 1)] AS x",
    ] {
        let stmt = parse_ok(query);
        match return_expr(&stmt) {
            Expr::ListComprehension(lc) => {
                assert!(
                    is_plain_label(&lc.where_expr),
                    "{query}: the WHERE stayed the label predicate"
                );
                assert!(
                    matches!(lc.map_expr.as_ref(), Some(Expr::FunctionCall(_))),
                    "{query}: the map expression is the function call"
                );
            }
            other => panic!("{query}: expected ListComprehension, got {other:?}"),
        }
    }
}

/// A pattern comprehension's map expression gets the same treatment: the
/// required map after a label-filter WHERE may be a function call.
#[test]
fn a_pattern_comprehension_function_call_map_expression_takes_the_pipe() {
    let stmt = parse_ok("MATCH (a) RETURN [(a)-[:R]->(b) WHERE a:L1 | toString(b.p1)] AS x");
    match return_expr(&stmt) {
        Expr::PatternComprehension(pc) => {
            assert!(is_plain_label(&pc.where_expr));
            assert!(matches!(pc.map_expr, Expr::FunctionCall(_)));
        }
        other => panic!("expected PatternComprehension, got {other:?}"),
    }
}

/// The scan does not over-fire: a real label disjunction whose
/// comprehension holds a later pipe still commits (`m:A|B AND … | …`
/// keeps the disjunction as the WHERE), so only the no-later-pipe tails
/// yield the pipe.
#[test]
fn a_label_disjunction_still_commits_before_a_map_expression_tail() {
    let stmt = parse_ok("MATCH (n) RETURN [m IN [n] WHERE m:A|B AND m.p1 IS NOT NULL | m.p1] AS x");
    match return_expr(&stmt) {
        Expr::ListComprehension(lc) => {
            // The WHERE is the conjunction over the committed disjunction,
            // and the map expression is the property access.
            assert!(!is_plain_label(&lc.where_expr));
            assert!(matches!(lc.map_expr.as_ref(), Some(Expr::Property(_))));
        }
        other => panic!("expected ListComprehension, got {other:?}"),
    }
}

/// A pattern element's inline property map is not a map projection: the
/// `{` dead end is suppressed inside an element's own label/type chain
/// (`pattern_element_depth`), so a type/label disjunction followed by a
/// property map keeps its reading inside a comprehension WHERE — the
/// list-comprehension WHERE's pattern predicate
/// `[x IN [a] WHERE (x)-[:S|T {w: 1}]->() | x]` and the pattern
/// comprehension's own pattern alike.
#[test]
fn a_list_comprehension_property_map_after_a_disjunction_keeps_the_disjunction() {
    let stmt = parse_ok("MATCH (a) RETURN [x IN [a] WHERE (x)-[:S|T {w: 1}]->() | x] AS out");
    let Expr::ListComprehension(lc) = return_expr(&stmt) else {
        panic!("expected a list comprehension");
    };
    assert!(
        matches!(lc.map_expr.as_ref(), Some(Expr::Variable(_))),
        "the map expression survived: {:?}",
        lc.map_expr
    );
}

#[test]
fn a_pattern_comprehension_property_map_after_a_disjunction_keeps_the_disjunction() {
    let stmt = parse_ok("MATCH (a) RETURN [(x)-[:S|T {w: 1}]->() | x] AS out");
    let Expr::PatternComprehension(pc) = return_expr(&stmt) else {
        panic!("expected a pattern comprehension");
    };
    assert!(
        matches!(&pc.map_expr, Expr::Variable(_)),
        "the map expression survived: {:?}",
        pc.map_expr
    );
}

/// A dynamic label atom's inner expression is a full expression
/// context: a comprehension nested there resolves its `|` ambiguity as
/// an expression would — the element-chain scope (which suppresses the
/// `{` dead-end for the chain's own inline property map) must not leak
/// into the atom.
#[test]
fn a_comprehension_inside_a_dynamic_label_atom_takes_its_own_pipe() {
    let stmt = parse_ok("MATCH (n:$([x IN [n] WHERE x:L1 | x{.p1}])) RETURN n");
    let Clause::Match(m) = &stmt.clauses[0] else {
        panic!("expected a match");
    };
    let Some(PatternElement::Node(node)) = m.pattern[0].elements.first() else {
        panic!("expected a leading node");
    };
    let Some(LabelExpr::Dynamic(DynamicLabelExpr { expr, .. })) = &node.label_expr else {
        panic!("expected a dynamic label atom, got {:?}", node.label_expr);
    };
    let Expr::ListComprehension(lc) = expr.as_ref() else {
        panic!("expected the atom to hold a list comprehension");
    };
    assert!(is_plain_label(&lc.where_expr));
    assert!(lc.map_expr.as_ref().is_some_and(is_map_projection));
}

/// Inside a pattern element's own type chain, a `|` is a type
/// disjunction even in a comprehension's WHERE (whose map-required `]`
/// dead-end would otherwise eat it): the `]` that ends the type
/// brackets is never the comprehension's closer.
#[test]
fn a_type_disjunction_in_a_comprehension_where_keeps_the_pipe() {
    let stmt = parse_ok("MATCH (a) RETURN [x IN [a] WHERE (x)-[:S|T]->(y) | x] AS out");
    let Expr::ListComprehension(lc) = return_expr(&stmt) else {
        panic!("expected a list comprehension");
    };
    assert!(is_plain_label(&lc.where_expr) || lc.where_expr.is_some());
    let pattern_stmt = parse_ok("MATCH (a) RETURN [(x)-[:S|T]->(y) | y] AS out");
    let Expr::PatternComprehension(pc) = return_expr(&pattern_stmt) else {
        panic!("expected a pattern comprehension");
    };
    // The disjunction survived as the relationship's type expression.
    let Some(PatternElement::Relationship(rel)) = pc.pattern.elements.get(1) else {
        panic!("expected the relationship element");
    };
    assert!(
        matches!(&rel.type_expr, Some(LabelExpr::Or(parts)) if parts.len() == 2),
        "the type disjunction survived: {:?}",
        rel.type_expr
    );
}

/// A continuation that cannot start a map expression keeps the chain
/// reading: the yield is confirmed by a parse, not assumed — each of
/// these shapes parsed before the closer-scan branch and regressed on
/// its first head (the rust review's round-3 parse regressions), because
/// the rolled-back separator reading is itself unparseable.
#[test]
fn an_unparseable_separator_reading_keeps_the_chain_reading() {
    for (query, disjunct_shape) in [
        // `&` continues the label chain: `B&C` is a label conjunction,
        // never a map expression.
        (
            "RETURN [x IN [1] WHERE x:A|B&C] AS r",
            "Or([A, And([B, C])])",
        ),
        // A negation atom cannot start a map expression.
        ("RETURN [x IN [1] WHERE x:A|!B] AS r", "Or([A, Not(B)])"),
        // A dynamic label atom cannot either.
        (
            "RETURN [x IN [1] WHERE x:A|$(x.p)] AS r",
            "Or([A, Dynamic])",
        ),
    ] {
        let stmt = parse_ok(query);
        let Expr::ListComprehension(lc) = return_expr(&stmt) else {
            panic!("{query}: expected a list comprehension");
        };
        let Some(where_expr) = lc.where_expr.as_ref() else {
            panic!("{query}: expected a WHERE");
        };
        let Expr::HasLabel(hl) = where_expr else {
            panic!("{query}: expected a HasLabel WHERE, got {where_expr:?}");
        };
        assert!(
            matches!(&hl.label_expr, LabelExpr::Or(parts) if parts.len() == 2),
            "{query}: the disjunction survived ({disjunct_shape}), got {:#?}",
            hl.label_expr
        );
        assert!(
            lc.map_expr.is_none(),
            "{query}: no map expression — the pipe stayed in the chain"
        );
    }
}

/// Drives `parse_label_or(true)` on `source` inside a list-comprehension
/// WHERE context — the direct-driver scaffold the pipe-mechanism rollback
/// test's two cases share: a parser over `source`, the disjunction parsed
/// through the context's one lifecycle owner, and both handed back so the
/// caller can assert on the spec and the cursor the parse stopped at.
fn label_or_in_comprehension_where(source: &str) -> (Parser<'_>, LabelSpec) {
    let mut parser = Parser::new(source, "query text", crate::lexer::ParseOptions::default())
        .expect("the scaffold's source always tokenizes");
    let spec = parser
        .with_comprehension_where(
            Some(ComprehensionWhereContext {
                closer: TokenType::RightBracket,
                // The fragment's first token: the direct driver owns no
                // opener, so the scan's span starts at the fragment's
                // beginning.
                opener: 0,
            }),
            |p| p.parse_label_or(true),
        )
        .expect("the scaffold's sources always parse");
    (parser, spec)
}

/// The revertible region of the shared pipe mechanism owns nothing
/// outside its snapshot: a would-be label continuation whose legacy
/// `|:` tail is rolled back (the chain it feeds proves not a
/// continuation) must not leave the legacy-`|:` fact behind — the fact
/// folds into `saw_legacy_pipe_colon` only after the mechanism commits.
/// Driven directly on `parse_label_or` through the shared scaffold above
/// (`label_or_in_comprehension_where`): the rollback shape has no
/// successfully-parsing statement spelling, because the restored `:`
/// would then sit at the map expression's start, so the whole statement
/// errors before the spec is observable.
#[test]
fn a_rolled_back_legacy_pipe_colon_continuation_leaves_no_fact_behind() {
    // The rollback: the `|: T` chain parses, but the comprehension's
    // `]` closer arrives before any later pipe, so the mechanism yields
    // the `|` to the map separator — and the `|:` fact dies with the
    // rollback (drop the `]` and the scan hits EOF, the continuation
    // commits, and the fact survives — the committed case below).
    let (parser, spec) = label_or_in_comprehension_where("S |: T.foo]");
    assert!(
        !spec.saw_legacy_pipe_colon,
        "the rolled-back `|:` stays unseen"
    );
    assert!(matches!(spec.expr, LabelExpr::Label(_)));
    assert_eq!(
        parser.peek_type(),
        TokenType::Pipe,
        "the `|` was left for the comprehension's separator"
    );

    // The committed case keeps the fact: a `|:` continuation that proves
    // real (nothing after it ends the chain) still reports the legacy
    // form.
    let (_, spec) = label_or_in_comprehension_where("S |: T");
    assert!(spec.saw_legacy_pipe_colon);
    assert!(matches!(spec.expr, LabelExpr::Or(_)));
}

/// A comprehension WHERE's rolled-back pipe keeps its GQL label-symbol
/// note: `note_gql_label_symbol` fires at the delimiter's own
/// consumption site, before the shared mechanism's snapshot, so it
/// survives the rollback with the post-note state the snapshot was
/// taken over — the pre-existing (origin/main) semantics this branch
/// preserves. The observable, pinned as a decision: a label disjunction
/// whose pipe yields to the comprehension's map separator beside a
/// legal legacy colon-chain in the same clause 42I10-rejects, even
/// though the committed parse carries no GQL symbol at all — a deferred
/// behavioral edge (noting only committed pipes would fix it).
#[test]
fn a_rolled_back_pipe_keeps_its_gql_symbol_note() {
    // The rolled-back pipe alone parses: the WHERE predicate is
    // `x IS L1`, the map expression `a.name`, no GQL symbol in the
    // committed parse.
    let stmt = parse_ok("MATCH (a) WHERE [x IN [a] WHERE x IS L1 | a.name] IS NOT NULL RETURN a");
    assert!(matches!(&stmt.clauses[0], Clause::Match(_)));

    // The same rolled-back pipe beside a legal legacy colon-chain in the
    // same clause rejects: the surviving note's gql_symbol flag meets
    // the chain's legacy_conjunction flag and the clause-level
    // label-mixing check 42I10s.
    let err = parse("MATCH (a:A:B) WHERE [x IN [a] WHERE x IS L1 | a.name] IS NOT NULL RETURN a")
        .unwrap_err();
    assert!(
        err.to_string().contains("InvalidLabelExpression"),
        "the 42I10 mixing rejection fired, got {err}"
    );

    // The control: a pipe that commits as a real label disjunction in
    // the comprehension WHERE carries a genuine GQL symbol, so the same
    // colon-chain rejects it for the right reason.
    let err =
        parse("MATCH (a:A:B) WHERE [x IN [a] WHERE x IS L1 | L2 | a.name] IS NOT NULL RETURN a")
            .unwrap_err();
    assert!(
        err.to_string().contains("InvalidLabelExpression"),
        "the committed disjunction's note rejected it, got {err}"
    );
}

/// An inline node-pattern WHERE's pipe is nesting-owned: the node
/// pattern's own parentheses enclose it, so no comprehension context —
/// not even the map-required kind whose closer would otherwise yield a
/// top-level pipe — can read it as a comprehension's map separator. The
/// disjunction survives and the pattern parses identically under the
/// context and outside it. (This resolves the hazard this test once
/// pinned: the pre-nesting classifier let the map-required proof yield
/// the pipe inside the brackets, stranding it — a verdict that changed
/// with the context and could cross-feed the speculation memo. The
/// verdict key still carries the context; no gate's verdict depends on
/// it through a nested pipe anymore.)
#[test]
fn an_inline_node_pattern_where_pipe_is_nesting_owned_in_every_context() {
    let mut parser = Parser::new(
        "(n WHERE [x IS L1 | q] = z)-[:R]->()",
        "query text",
        crate::lexer::ParseOptions::default(),
    )
    .unwrap();
    let under_map_required = parser.with_comprehension_where(
        Some(ComprehensionWhereContext {
            closer: TokenType::RightBracket,
            // The fragment's first token: the direct driver owns no
            // opener, so the nesting scan's span starts at the
            // fragment's beginning.
            opener: 0,
        }),
        |p| p.node_pattern_precedes_relationship(),
    );
    assert!(
        under_map_required,
        "under the map-required kind the inline WHERE's pipe is still the inner grammar's"
    );
    // The probe restored the position; the same `(` outside any context
    // computes the same verdict.
    let plain = parser.node_pattern_precedes_relationship();
    assert!(
        plain,
        "outside any context the same position's verdict is its own — and equal"
    );
}
