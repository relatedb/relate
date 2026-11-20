//! The collect module's tests, offloaded by the file-size gate: the
//! scope pairs and the walker's reach pins.

use super::super::tree::tests::return_expr_of;
use super::*;
use crate::agg::contains_aggregation;

#[test]
fn collect_expr_variables_matches_historical_reach() {
    // The walker-based collector must visit exactly the node set the
    // hand-rolled recursion did.
    let collected = |query: &str| {
        let e = return_expr_of(query);
        let mut vars = IdentHashSet::default();
        collect_expr_variables(&e, &mut vars);
        vars
    };
    // Plain combinators are descended into.
    let vars = collected("RETURN a.prop + (b IN [c]) AND CASE WHEN d THEN e ELSE f END");
    for v in ["a", "b", "c", "d", "e", "f"] {
        assert!(vars.contains(&SmallName::from(v)), "missing `{v}`");
    }
    // Scope-introducing constructs are NOT descended into at all — not
    // even their list expressions (historical semantics).
    let vars = collected("RETURN [x IN n.list | x + q]");
    assert!(vars.is_empty(), "comprehension leaked: {vars:?}");
    let vars = collected("RETURN reduce(s = i, y IN m.list | s + y)");
    assert!(vars.is_empty(), "reduce leaked: {vars:?}");
    // CIP-214: the bare distance-metric keyword is not a variable.
    let vars = collected("RETURN vector_distance(v1, v2, EUCLIDEAN)");
    assert!(vars.contains(&SmallName::from("v1")) && vars.contains(&SmallName::from("v2")));
    assert!(
        !vars.contains(&SmallName::from("EUCLIDEAN")),
        "metric keyword collected"
    );
    // A dynamic property KEY (`n[<expr>]`) is not descended into; the
    // value side is. (Only SET/REMOVE targets parse key_expr, so build
    // the node directly.)
    let e = Expr::Property(Box::new(relate_parser::PropertyAccess {
        expr: Expr::Variable(SmallName::from_name("n")),
        property: SmallName::from_name(""),
        key_expr: Some(Expr::Variable(SmallName::from_name("k"))),
    }));
    let mut vars = IdentHashSet::default();
    collect_expr_variables(&e, &mut vars);
    assert!(vars.contains(&SmallName::from("n")));
    assert!(
        !vars.contains(&SmallName::from("k")),
        "dynamic property key collected"
    );
}

#[test]
fn contains_aggregation_matches_historical_reach() {
    // The walker-based check must look through exactly the combinators
    // the hand-rolled recursion did: function args, binary/unary/
    // comparison operands, parens, property values — and nothing else.
    assert!(contains_aggregation(&return_expr_of("RETURN count(*)")));
    assert!(contains_aggregation(&return_expr_of("RETURN 1 + count(*)")));
    assert!(contains_aggregation(&return_expr_of(
        "RETURN abs((sum(x)))"
    )));
    assert!(contains_aggregation(&return_expr_of(
        "RETURN n.p < max(m.q)"
    )));
    // Aggregates inside a list literal, CASE arm, IN operand or
    // comprehension are NOT "contained" (historical verdict); a
    // subscript/slice *index or bound* is not walked either, but the
    // postfix *base* is.
    assert!(!contains_aggregation(&return_expr_of("RETURN [count(*)]")));
    assert!(!contains_aggregation(&return_expr_of(
        "RETURN CASE WHEN true THEN count(*) ELSE 0 END"
    )));
    assert!(!contains_aggregation(&return_expr_of("RETURN n[count(*)]")));
    assert!(!contains_aggregation(&return_expr_of(
        "RETURN 1 IN [count(*)]"
    )));
    assert!(!contains_aggregation(&return_expr_of(
        "RETURN [x IN [1] | count(*)]"
    )));
    // Aggregates in the postfix base of a subscript or slice ARE
    // "contained" (the walker descends into the base, not the
    // index/bounds).
    assert!(contains_aggregation(&return_expr_of(
        "RETURN collect(x)[..1]"
    )));
    assert!(contains_aggregation(&return_expr_of(
        "RETURN collect(x)[0]"
    )));
    // Nor one hiding in a dynamic property key.
    let e = Expr::Property(Box::new(relate_parser::PropertyAccess {
        expr: Expr::Variable(SmallName::from_name("n")),
        property: SmallName::from_name(""),
        key_expr: Some(Expr::CountStar),
    }));
    assert!(!contains_aggregation(&e));
}
