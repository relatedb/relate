//! The agg module's tests (split out of `agg.rs`, LABS-311 size gate).
use super::subclause::{check_post_projection_leaves, is_projected_item, SubclauseScope};
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

fn dyn_prop(base: Expr, key: Expr) -> Expr {
    Expr::DynamicProperty {
        expr: Box::new(base),
        key: Box::new(key),
    }
}

/// One constructor for the comprehension literals these tests build —
/// the ident is the `x` element and the projection returns it, so a
/// case differs only in its list and predicate.
fn list_comp(list: Expr, predicate: Option<Expr>) -> Expr {
    Expr::ListComp {
        ident: "x".into(),
        list: Box::new(list),
        predicate: predicate.map(Box::new),
        projection: Box::new(var("x")),
    }
}

/// One constructor for a single-argument aggregate call — the
/// `Expr::Function` literal exists once; every aggregate-named helper
/// delegates.
fn agg_fn(name: &str, arg: Expr) -> Expr {
    Expr::Function {
        name: name.into(),
        distinct: false,
        args: vec![arg],
    }
}

fn collect(arg: Expr) -> Expr {
    agg_fn("collect", arg)
}

fn max(arg: Expr) -> Expr {
    agg_fn("max", arg)
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
        Some(std::slice::from_ref(&key)),
        &Imports::new(),
        &Default::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("`b`"), "got: {err}");
    // Verbatim the key is fine.
    check_agg_projections(
        &[item(key.clone())],
        Some(std::slice::from_ref(&key)),
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
    let outer: crate::binding::Env = ["a", "c"].into_iter().map(scope_binding).collect();
    let check = |items: &[Projection], expr: &Expr, keys: Option<&[Expr]>| {
        let imports = Imports::new();
        let scope = SubclauseScope::new(items, keys, false, &imports, &outer);
        check_post_projection_leaves(&scope, expr, "ORDER BY")
    };
    // Exact key and alias-free accepted; another property rejected.
    check(&items, &prop("a", "p"), Some(std::slice::from_ref(&key))).unwrap();
    let err = check(&items, &prop("a", "q"), Some(std::slice::from_ref(&key))).unwrap_err();
    assert!(err.to_string().contains("ORDER BY"), "got: {err}");
    // A comprehension over the key is still the key's own value ([86]).
    let comp = list_comp(key.clone(), None);
    check(&items, &comp, Some(std::slice::from_ref(&key))).unwrap();
    // Under implicit grouping the complex key is recognized as a
    // sub-expression ([67]/[69]); with an explicit key list it is not.
    let complex = plus(prop("a", "p"), Expr::Int(0));
    let order = plus(complex.clone(), Expr::Int(1));
    let implicit_items = vec![item(complex.clone()), item(collect(var("c")))];
    check(&implicit_items, &order, None).unwrap();
    let err = check(&items, &order, Some(std::slice::from_ref(&complex))).unwrap_err();
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

/// A property whose target carries an aggregation is an aggregating
/// expression, not a row leaf: `max(a.p).q` and `collect(n)[0].q` carry
/// no variable reference outside their aggregate calls, so the grouping
/// discipline admits them (Neo4j plans both). A mixed target surfaces
/// its own non-aggregated leaf, and a chained property of a row
/// expression stays one atomic leaf the key match compares whole.
#[test]
fn property_over_an_aggregate_is_aggregating() {
    // `max(a.p).q` under implicit grouping: fully aggregated, admitted.
    check_agg_projections(
        &[item(Expr::property(max(prop("a", "p")), "q".into()))],
        None,
        &Imports::new(),
        &Default::default(),
    )
    .expect("a property over an aggregate carries no row leaf");
    // A mixed target surfaces its non-aggregated leaf: `b.q` is read
    // outside the aggregate.
    let err = check_agg_projections(
        &[item(Expr::property(
            plus(prop("b", "q"), collect(var("x"))),
            "p".into(),
        ))],
        None,
        &Imports::new(),
        &Default::default(),
    )
    .expect_err("the target's non-aggregated leaf must surface");
    assert!(err.to_string().contains("`b`"), "got: {err}");
    // A chained property of a row expression stays atomic: `a.p.q` under
    // `GROUP BY a.p` is not the key (the leaf names no variable — a
    // chained property's best-effort name is empty by design).
    let chained = Expr::property(prop("a", "p"), "q".into());
    let err = check_agg_projections(
        &[item(chained.clone()), item(collect(var("c")))],
        Some(&[prop("a", "p")]),
        &Imports::new(),
        &Default::default(),
    )
    .expect_err("a chained property of a row expression stays one leaf");
    assert!(
        err.to_string().contains("must be a grouping key"),
        "got: {err}"
    );
    // ...and under `GROUP BY a.p.q` the verbatim key is admitted.
    check_agg_projections(
        &[item(chained), item(collect(var("c")))],
        Some(&[Expr::property(prop("a", "p"), "q".into())]),
        &Imports::new(),
        &Default::default(),
    )
    .expect("the verbatim chained key is its own grouping key");
    // The dynamic property spelling rides the same arms: over an
    // aggregate it is aggregating (`max(a.p)[$k]` — a parameter key, a
    // per-row constant that is no grouping candidate), and of a row
    // variable it is the atomic leaf — verbatim as its own key (`a[k]`
    // under `GROUP BY a[k]`) or, with a constant key, through the
    // base-variable allowance (`a[$k]` under `GROUP BY a`).
    check_agg_projections(
        &[item(dyn_prop(
            max(prop("a", "p")),
            Expr::Parameter("k".into()),
        ))],
        None,
        &Imports::new(),
        &Default::default(),
    )
    .expect("a dynamic property over an aggregate carries no row leaf");
    let dynamic_key = dyn_prop(var("a"), var("k"));
    check_agg_projections(
        &[item(dynamic_key.clone()), item(collect(var("c")))],
        Some(&[dynamic_key]),
        &Imports::new(),
        &Default::default(),
    )
    .expect("the verbatim dynamic key is its own grouping key");
    // A constant key rides the base-variable allowance: `a[$k]` under
    // `GROUP BY a` resolves like the key. A row-expression key does not
    // — it surfaces as its own non-grouping leaf, so a group-varying key
    // is rejected exactly as an ungrouped variable would be.
    check_agg_projections(
        &[
            item(dyn_prop(var("a"), Expr::Parameter("k".into()))),
            item(collect(var("c"))),
        ],
        Some(&[var("a")]),
        &Imports::new(),
        &Default::default(),
    )
    .expect("a constant-keyed dynamic property of a bare-variable key resolves like the key");
    let err = check_agg_projections(
        &[item(dyn_prop(var("a"), var("k"))), item(collect(var("c")))],
        Some(&[var("a")]),
        &Imports::new(),
        &Default::default(),
    )
    .expect_err("a row-varying dynamic key is a non-grouping leaf");
    assert!(
        err.to_string().contains("`k`"),
        "the varying key is the named leaf: {err}"
    );
}

#[test]
fn a_chained_dynamic_target_descends_to_its_resolving_base_leaf() {
    // A dynamic property of a non-variable target is not an atomic leaf
    // (its base name is unresolvable whole): it descends, and the base
    // leaf resolves through the base-variable allowance — under
    // `GROUP BY a` via the allowance, under `GROUP BY a.meta` as the
    // exact key. Both were accepted before the dynamic leaf shape
    // landed and stay accepted after the variable-target gate.
    for keys in [var("a"), prop("a", "meta")] {
        check_agg_projections(
            &[
                item(dyn_prop(prop("a", "meta"), Expr::Parameter("k".into()))),
                item(collect(var("c"))),
            ],
            Some(std::slice::from_ref(&keys)),
            &Imports::new(),
            &Default::default(),
        )
        .unwrap_or_else(|e| panic!("GROUP BY {keys:?}: the base leaf resolves: {e}"));
    }
}

#[test]
fn a_dynamic_grouping_key_does_not_exempt_its_base_variable() {
    // `GROUP BY a[$k]` groups the keyed property's value, not the base:
    // the bare `a` beside it is its own grouping decision. Under implicit
    // grouping `a` is itself a key (every non-aggregating item is), so
    // the shape that would ask the question — an explicit dynamic key —
    // leaves `a` to the walk, which rejects it as a non-grouping read
    // naming `a`.
    let err = check_agg_projections(
        &[item(var("a")), item(collect(var("c")))],
        Some(&[dyn_prop(var("a"), Expr::Parameter("k".into()))]),
        &Imports::new(),
        &Default::default(),
    )
    .expect_err("the bare base is not exempted by the dynamic key");
    assert!(
        err.to_string().contains("`a`") || err.to_string().contains("grouping"),
        "the base read is named: {err}"
    );
}

#[test]
fn a_row_varying_dynamic_key_in_a_nested_scope_is_its_own_read() {
    // The nested-scope walker mirrors `is_row_leaf`: a dynamic property
    // whose key is a row read is not an atomic leaf, so the key surfaces
    // as its own read — `a[k]` inside a comprehension's WHERE under
    // `GROUP BY a` (a per-row `k`) is rejected naming `k`, instead of
    // being swallowed by the base-variable allowance on `a`.
    let items = vec![item(var("a")), item(collect(var("c")))];
    let outer: crate::binding::Env = ["a", "c", "k"].into_iter().map(scope_binding).collect();
    let check = |expr: &Expr| {
        let imports = Imports::new();
        let keys = [var("a")];
        let scope = SubclauseScope::new(&items, Some(&keys), false, &imports, &outer);
        check_post_projection_leaves(&scope, expr, "ORDER BY")
    };
    let varying = list_comp(
        collect(var("c")),
        Some(plus(dyn_prop(var("a"), var("k")), Expr::Int(1))),
    );
    let err = check(&varying)
        .expect_err("a row-varying dynamic key in a nested scope is a non-grouping read");
    assert!(
        err.to_string()
            .contains("ORDER BY refers to a non-projecting variable"),
        "the varying key is the rejected read: {err}"
    );
    // A constant key keeps the base variable's leaf shape: admitted
    // through the grouped `a`.
    let constant = list_comp(
        collect(var("c")),
        Some(plus(
            dyn_prop(var("a"), Expr::Parameter("k".into())),
            Expr::Int(1),
        )),
    );
    check(&constant)
        .expect("a constant-keyed dynamic property in a nested scope resolves through its base");
}
