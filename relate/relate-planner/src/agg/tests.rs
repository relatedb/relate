//! The agg module's tests (split out of `agg.rs`, LABS-311 size gate).
use super::*;
/// Every scope-name test entry: the aggregation checks read only the
/// environment's keys, so any binding kind serves.
fn scope_binding(name: &str) -> (String, crate::binding::Binding) {
    (name.to_string(), crate::binding::Binding::Value)
}

fn var(name: &str) -> Expr {
    Expr::Variable(name.into())
}

fn prop(name: &str, key: &str) -> Expr {
    Expr::Property {
        key_id: Default::default(),
        expr: Box::new(var(name)),
        key: key.into(),
    }
}

fn collect(arg: Expr) -> Expr {
    Expr::Function {
        name: "collect".into(),
        distinct: false,
        args: vec![arg],
    }
}

fn plus(left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        op: relate_ir::BinaryOp::Add,
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn item(expr: Expr) -> Projection {
    Projection {
        expr,
        alias: Some("xs".into()),
        source_text: None,
    }
}

// GroupByAcceptance [21]/[33]/[35]/[36]/[41]/[42]: a non-aggregating
// item of an explicit-key projection follows the same grouping-key
// discipline as an aggregating one.
#[test]
fn non_aggregating_items_must_be_grouping_keys() {
    let items = || vec![item(var("a")), item(var("b")), item(collect(var("c")))];
    // `GROUP BY a` leaves `b` uncovered.
    let err = check_agg_projections(
        &items(),
        Some(&[var("a")]),
        &Imports::new(),
        &Default::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("`b`"), "got: {err}");
    // `GROUP BY a, b` covers every non-aggregating item.
    check_agg_projections(
        &items(),
        Some(&[var("a"), var("b")]),
        &Imports::new(),
        &Default::default(),
    )
    .unwrap();
    // `GROUP BY ()` has no keys, so every non-constant item is rejected.
    let err = check_agg_projections(&items(), Some(&[]), &Imports::new(), &Default::default())
        .unwrap_err();
    assert!(err.to_string().contains("`a`"), "got: {err}");
}

/// A deeply nested subquery body is validated in one pass: the earlier
/// recursive containment re-expanded each body once per ancestor node, so
/// nesting of this depth (the security review's DoS shape) could not
/// finish. Only completion is asserted — an exponential walk would hang.
#[test]
fn deeply_nested_subquery_bodies_contain_in_one_pass() {
    // The containment walk re-expands each subquery body once; the
    // recursive form re-expanded a nested body once per ancestor node —
    // exponential in nesting depth, reachable during validation where no
    // cancel token is polled. 40 levels deep, driven through
    // `expr_contains_deep` itself: the recursive form would still be
    // expanding long after these asserts.
    let count_subquery = |body: Expr| Expr::CountSubquery {
        query: relate_ir::Query {
            parts: vec![relate_ir::SingleQuery {
                clauses: vec![relate_ir::Clause::Return {
                    distinct: false,
                    group_by: None,
                    items: vec![item(body)],
                    order: vec![],
                    skip: None,
                    limit: None,
                }],
            }],
            union_all: vec![],
            next: None,
            cypher_version: None,
        },
    };
    let mut body = var("x");
    for _ in 0..40 {
        body = count_subquery(body);
    }
    assert!(expr_contains_deep(&body, &var("x")));
    assert!(!expr_contains_deep(&body, &var("y")));
}

/// A rejected leaf whose shape names no variable must still raise: the
/// rejection is decided by the leaf, the *name* in the message is
/// best-effort. Chained properties (`a.p.q`), parameter properties
/// (`$p.x`) and map-projection shorthand on a non-variable target have no
/// bare-variable root, and gating the error on the extracted name let
/// them pass silently.
#[test]
fn a_rejected_leaf_without_a_nameable_variable_still_raises() {
    let chained = Expr::Property {
        key_id: Default::default(),
        expr: Box::new(prop("a", "p")),
        key: "q".into(),
    };
    let parameter_prop = Expr::Property {
        key_id: Default::default(),
        expr: Box::new(Expr::Parameter("p".into())),
        key: "x".into(),
    };
    for leaf in [chained, parameter_prop] {
        let err = check_item_grouping(&leaf, &[], &Imports::new(), &Default::default())
            .expect_err("a non-grouping leaf must be rejected");
        // The rejection fires; with no nameable variable the message
        // carries no `…sub-expression `x`` clause.
        assert!(
            err.to_string().contains("must be a grouping key"),
            "got: {err}"
        );
    }
    // A nameable leaf still names the variable.
    let err = check_item_grouping(&var("a"), &[], &Imports::new(), &Default::default())
        .expect_err("a bare non-grouping variable is rejected");
    assert!(err.to_string().contains("`a`"), "got: {err}");
}

// [33]: a complex key is recognized verbatim, never decomposed further;
// a property of a bare-variable key may be composed ([22]/[23]).
#[test]
fn keys_are_recognized_verbatim_or_through_a_bare_variable() {
    let complex = || vec![item(plus(plus(var("b"), Expr::Int(1)), Expr::Int(1)))];
    let key = plus(var("b"), Expr::Int(1));
    let err = check_agg_projections(
        &complex(),
        Some(&[key.clone()]),
        &Imports::new(),
        &Default::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("`b`"), "got: {err}");
    // Verbatim the key is fine.
    check_agg_projections(
        &[item(key.clone())],
        Some(&[key.clone()]),
        &Imports::new(),
        &Default::default(),
    )
    .unwrap();
    // A property of a bare key is fine; a property of a complex key's
    // variable is not ([68]/[70]-style).
    check_agg_projections(
        &[item(plus(prop("a", "p"), Expr::Int(1)))],
        Some(&[var("a")]),
        &Imports::new(),
        &Default::default(),
    )
    .unwrap();
    let err = check_agg_projections(
        &[item(prop("a", "q"))],
        Some(&[plus(prop("a", "p"), Expr::Int(0))]),
        &Imports::new(),
        &Default::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("`a`"), "got: {err}");
}

// GroupByAcceptance [20]/[68]/[70]/[91]/[92]: an aggregating projection's
// non-aggregating ORDER BY / WHERE leaves must be keys or projected
// names; exactly a key is fine, a property of a bare key is fine, and an
// implicit-grouping complex key stays referable as a sub-expression.
#[test]
fn post_projection_leaves_follow_the_grouping_keys() {
    let items = vec![item(prop("a", "p")), item(collect(var("c")))];
    let key = prop("a", "p");
    let scope: crate::binding::Env = ["a", "c"].into_iter().map(scope_binding).collect();
    let check = |expr: &Expr, keys: Option<&[Expr]>| {
        check_post_projection_leaves(&items, keys, &Imports::new(), &scope, expr, "ORDER BY")
    };
    // Exact key and alias-free accepted; another property rejected.
    check(&prop("a", "p"), Some(std::slice::from_ref(&key))).unwrap();
    let err = check(&prop("a", "q"), Some(std::slice::from_ref(&key))).unwrap_err();
    assert!(err.to_string().contains("ORDER BY"), "got: {err}");
    // A comprehension over the key is still the key's own value ([86]).
    let comp = Expr::ListComp {
        ident: "x".into(),
        list: Box::new(key.clone()),
        predicate: None,
        projection: Box::new(var("x")),
    };
    check(&comp, Some(std::slice::from_ref(&key))).unwrap();
    // Under implicit grouping the complex key is recognized as a
    // sub-expression ([67]/[69]); with an explicit key list it is not.
    let complex = plus(prop("a", "p"), Expr::Int(0));
    let order = plus(complex.clone(), Expr::Int(1));
    let implicit_items = vec![item(complex.clone()), item(collect(var("c")))];
    check_post_projection_leaves(
        &implicit_items,
        None,
        &Imports::new(),
        &scope,
        &order,
        "ORDER BY",
    )
    .unwrap();
    let err = check(&order, Some(std::slice::from_ref(&complex))).unwrap_err();
    assert!(err.to_string().contains("ORDER BY"), "got: {err}");
}

// ADR-0198 ruling 1: a `*` item carries every in-scope variable as a
// grouping key, so the carried `z` in `collect(n.x) + [z]` resolves.
#[test]
fn star_covers_variable_leaves() {
    let items = vec![
        item(Expr::Star),
        item(plus(collect(prop("n", "x")), Expr::List(vec![var("z")]))),
    ];
    check_agg_projections(&items, None, &Imports::new(), &Default::default()).unwrap();
}

// `*` projects the variables, not their property expressions: the
// unprojected property leaf `n.y` stays rejected (battery i03).
#[test]
fn star_does_not_cover_property_leaves() {
    let items = vec![
        item(Expr::Star),
        item(plus(
            collect(prop("n", "x")),
            Expr::List(vec![prop("n", "y")]),
        )),
    ];
    let err =
        check_agg_projections(&items, None, &Imports::new(), &Default::default()).unwrap_err();
    assert!(
        err.to_string().contains("AmbiguousAggregationExpression"),
        "got: {err}"
    );
}

// The projected-item predicate is exact item-expression equality: a
// restated projection matches, any other expression does not, and the
// `*` marker item never stands in for one.
#[test]
fn projected_item_matches_only_the_exact_item_expression() {
    let items = vec![item(prop("n", "x")), item(Expr::Star)];
    assert!(is_projected_item(&items, &prop("n", "x")));
    assert!(!is_projected_item(&items, &prop("n", "y")));
    assert!(!is_projected_item(&[], &prop("n", "x")));
}

// Without `*`, a carried variable that is not projected stays rejected
// (battery i01).
#[test]
fn unprojected_variable_leaf_stays_rejected() {
    let items = vec![item(plus(
        collect(prop("n", "x")),
        Expr::List(vec![var("y")]),
    ))];
    let err =
        check_agg_projections(&items, None, &Imports::new(), &Default::default()).unwrap_err();
    assert!(
        err.to_string().contains("AmbiguousAggregationExpression"),
        "got: {err}"
    );
}

// A `*` projection under an explicit GROUP BY key list would silently
// project carried non-key variables with first-row-of-group values
// (wrong-answer audit finding; ADR-0198 Later references): reject it.
#[test]
fn star_with_explicit_group_by_keys_rejected() {
    let items = vec![item(Expr::Star), item(Expr::CountStar)];
    let keys = vec![prop("n", "x")];
    let err = check_agg_projections(&items, Some(&keys), &Imports::new(), &Default::default())
        .unwrap_err();
    assert!(
        err.to_string().contains("AmbiguousAggregationExpression"),
        "got: {err}"
    );
}

// An explicit key list naming exactly the variables `*` expands to is the
// same grouping, so the star stays legal there (GroupByAcceptance [52]
// `RETURN *, count(*) AS cnt GROUP BY a, b`); one variable short leaves a
// carried non-key variable projected with first-row-of-group values.
#[test]
fn star_with_explicit_keys_covering_scope_allowed() {
    let items = vec![item(Expr::Star), item(Expr::CountStar)];
    let scope: crate::binding::Env = ["a", "b"].into_iter().map(scope_binding).collect();
    let keys = vec![var("a"), var("b")];
    check_agg_projections(&items, Some(&keys), &Imports::new(), &scope).unwrap();
    let keys = vec![var("a")];
    let err = check_agg_projections(&items, Some(&keys), &Imports::new(), &scope).unwrap_err();
    assert!(
        err.to_string().contains("AmbiguousAggregationExpression"),
        "got: {err}"
    );
}

// `GROUP BY ()` (the empty grouping set) rejects `*` for the same
// reason: the star is not among the (zero) keys.
#[test]
fn star_with_empty_grouping_set_rejected() {
    let items = vec![item(Expr::Star), item(Expr::CountStar)];
    let err =
        check_agg_projections(&items, Some(&[]), &Imports::new(), &Default::default()).unwrap_err();
    assert!(
        err.to_string().contains("AmbiguousAggregationExpression"),
        "got: {err}"
    );
}

// The zero-aggregate form takes the same executor path (an explicit
// GROUP BY with no aggregate items acts as DISTINCT), so the rejection
// must fire before the no-aggregates early return.
#[test]
fn star_with_explicit_group_by_no_aggregate_rejected() {
    let items = vec![item(Expr::Star), item(var("n"))];
    let keys = vec![var("n")];
    let err = check_agg_projections(&items, Some(&keys), &Imports::new(), &Default::default())
        .unwrap_err();
    assert!(
        err.to_string().contains("AmbiguousAggregationExpression"),
        "got: {err}"
    );
}

// `GROUP BY ALL` lowers the `*` item into the key list, where the
// executor expands it to the carried variables: the combination stays
// valid.
#[test]
fn group_by_all_star_allowed() {
    let items = vec![item(Expr::Star), item(Expr::CountStar)];
    let keys = vec![Expr::Star];
    check_agg_projections(&items, Some(&keys), &Imports::new(), &Default::default()).unwrap();
}

// Implicit grouping (no GROUP BY clause) keeps the ADR-0198 ruling-1
// behavior: `*` carries every in-scope variable as a grouping key.
#[test]
fn implicit_grouping_star_allowed() {
    let items = vec![item(Expr::Star), item(Expr::CountStar)];
    check_agg_projections(&items, None, &Imports::new(), &Default::default()).unwrap();
}

// The zero-aggregate form takes the DISTINCT path and stays rejected even
// when the key list covers the scope (CollectListConcat s23
// `MATCH (n:N) WITH *, n GROUP BY n`).
#[test]
fn star_with_covering_keys_and_no_aggregate_rejected() {
    let items = vec![item(Expr::Star), item(var("n"))];
    let scope: crate::binding::Env = ["n"].into_iter().map(scope_binding).collect();
    let keys = vec![var("n")];
    let err = check_agg_projections(&items, Some(&keys), &Imports::new(), &scope).unwrap_err();
    assert!(
        err.to_string().contains("AmbiguousAggregationExpression"),
        "got: {err}"
    );
}
