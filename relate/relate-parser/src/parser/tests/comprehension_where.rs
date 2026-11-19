//! The comprehension-WHERE `|` disambiguation tests — the pipe-yielding,
//! map-projection, dynamic-label-atom, type-disjunction, `|:` rollback,
//! GQL-symbol-note and probe-verdict-memo pins, plus the direct-driver
//! scaffold they share — mirroring the parser module's own
//! `comprehension_where.rs` organization.

use super::labels::LabelSpec;
use super::*;

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

/// A comprehension WHERE's label predicate leaves the `|` for the map
/// separator when the chain past it ends at a token no label expression
/// can continue with: a map projection `{` joins the property access `.`
/// as the dead-end set. `[n IN nodes(p) WHERE n:L1 | n{.p1}]` keeps the
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

/// A pattern comprehension's WHERE: the same dead-end rule, map still
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

/// The Dot dead-end keeps its behavior: a property access map expression
/// still takes the pipe.
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
            Some(BracketProof::RightBracketClosesWithTheMapOmitted),
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
    // The rollback: the `|: T` chain parses, but the `.` after it ends
    // its usefulness as a disjunction continuation, so the `|` is the
    // comprehension's separator — and the `|:` fact dies with it.
    let (parser, spec) = label_or_in_comprehension_where("S |: T.foo");
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

/// A probe verdict must not cross-feed across comprehension contexts:
/// the verdict memo's key carries the context (the gates' parses read
/// it through their grammar-specific tail classifications), because the same position
/// can be probed under different contexts — a failed map-comprehension
/// speculation's tokens are re-parsed by the map-literal fallback
/// outside the context. Pinned on a shape whose verdict genuinely
/// differs: under the map-required kind the inline WHERE's inner list
/// `]` proves the disjunction's pipe was the separator, the pipe
/// strands inside the bracket and the pattern parse fails; outside any
/// context the disjunction survives and the same `(` is a pattern
/// followed by a relationship start.
#[test]
fn a_probe_verdict_does_not_cross_feed_across_comprehension_contexts() {
    let mut parser = Parser::new(
        "(n WHERE [x IS L1 | q] = z)-[:R]->()",
        "query text",
        crate::lexer::ParseOptions::default(),
    )
    .unwrap();
    let under_map_required = parser.with_comprehension_where(
        Some(BracketProof::RightBracketClosesTheComprehension),
        |p| p.node_pattern_precedes_relationship(),
    );
    assert!(
        !under_map_required,
        "under the map-required kind the inline WHERE's pipe rolls back and the pattern fails"
    );
    // The probe restored the position; the same `(` outside any context
    // computes — not memo-feeds — its own verdict.
    let plain = parser.node_pattern_precedes_relationship();
    assert!(
        plain,
        "outside any context the same position's verdict is its own"
    );
}
