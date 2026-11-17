//! The collect module's tests, offloaded by the file-size gate: the
//! naming rule, the scope pairs and the walker's reach pins.

use super::super::tree::tests::return_expr_of;
use super::*;
use crate::agg::contains_aggregation;

/// The paren-family test fixtures — one spelling of the
/// item/var/paren/prop constructor vocabulary (pub(crate): the
/// aggregation battery's projected-item test reads the same shapes
/// instead of re-spelling them, the ast/tree.rs tests-module precedent).
pub(crate) fn paren_item(expr: Expr, alias: Option<SmallName>, source_text: &str) -> ReturnItem {
    ReturnItem {
        expr,
        alias,
        all: false,
        source_text: Some(source_text.into()),
    }
}

/// A bare variable item shape's variable, named.
pub(crate) fn paren_var(name: &str) -> Expr {
    Expr::Variable(SmallName::from_name(name))
}

/// A `v.key` static property access on the variable `name`.
pub(crate) fn paren_prop(name: &str, key: &str) -> Expr {
    Expr::Property(Box::new(relate_parser::PropertyAccess {
        expr: paren_var(name),
        property: SmallName::from_name(key),
        key_expr: None,
    }))
}

/// The redundant-paren wrapper, one level.
pub(crate) fn paren_wrap(e: Expr) -> Expr {
    Expr::Paren(Box::new(e))
}

/// A dynamic-key property access `name[key]` in expression position —
/// the parser's `Expr::Subscript` shape (the `key_expr` Property form is
/// the SET/REMOVE *target* spelling, a different grammar position).
pub(crate) fn paren_subscript(name: &str, key: Expr) -> Expr {
    Expr::Subscript(Box::new(relate_parser::SubscriptExpr {
        expr: paren_var(name),
        index: Some(key),
        from: None,
        to: None,
        slice: false,
    }))
}

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
// LABS-1615/1616's paren family: redundant parens around a projected
// variable are transparent to the name and node-safety rules — the
// lowered item is the bare variable, so `WITH (n)` projects the column
// `n` (the same name the IR's `Projection::output_name` gives it) and
// carries node-ness exactly like `WITH n`. A parenthesized
// non-variable keeps the source-text name and no node-ness.

#[test]
fn parenthesized_variables_project_their_name() {
    let item = |expr, alias| paren_item(expr, alias, "(n.p1)");
    // The variable's own name, through any paren depth; an alias wins.
    assert_eq!(
        effective_name(&item(paren_wrap(paren_var("n")), None)),
        Some(SmallName::from_name("n"))
    );
    assert_eq!(
        effective_name(&item(paren_wrap(paren_wrap(paren_var("n"))), None)),
        Some(SmallName::from_name("n"))
    );
    assert_eq!(
        effective_name(&item(
            paren_wrap(paren_var("n")),
            Some(SmallName::from_name("m"))
        )),
        Some(SmallName::from_name("m"))
    );
    // A property access is NOT a canonical shape: Neo4j (probed live
    // 2026.10) names `n.p1`, `n . p1`, `n.` + backtick + `p1` + backtick
    // and `(n.p1)` each by its verbatim source text — so every spelling
    // beyond the plain one is its own column name, and only an
    // exact-text duplicate conflicts (the drift test pins the same
    // agreement with the IR, whose canonical_name drops the property arm
    // with this change).
    assert_eq!(
        effective_name(&item(paren_wrap(paren_prop("n", "p1")), None)),
        Some(SmallName::from_name("(n.p1)"))
    );
    let spaced = paren_item(paren_prop("n", "p1"), None, "n . p1");
    assert_eq!(
        effective_name(&spaced),
        Some(SmallName::from_name("n . p1"))
    );
    // A dynamic key likewise keeps the source text.
    let dynamic = paren_item(paren_subscript("n", paren_var("k")), None, "n[k]");
    assert_eq!(effective_name(&dynamic), Some(SmallName::from_name("n[k]")));
}
