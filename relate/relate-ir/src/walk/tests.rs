//! The walk module's tests (split out of `walk.rs`, LABS-311 size gate).
use super::*;
use crate::{NodePattern, PatternElement, RelPattern};

fn var(name: &str) -> Expr {
    Expr::Variable(name.into())
}

#[test]
fn scope_bearing_variants_match_children_skip_set() {
    let path = crate::PathPattern {
        variable: None,
        subpath_variable: None,
        elements: vec![],
        nodes: vec![],
        rels: vec![],
        where_clause: None,
        mode: None,
        selector: None,
        is_shortest_path_function: false,
    };
    let query = || crate::Query {
        parts: vec![],
        union_all: vec![],
        next: None,
    };
    let bearing: Vec<Expr> = vec![
        Expr::PatternPred { path: path.clone() },
        Expr::PatternComp {
            path: path.clone(),
            predicate: None,
            projection: Box::new(var("n")),
        },
        Expr::ShortestPath { all: false, path },
        Expr::ExistsSubquery { query: query() },
        Expr::CountSubquery { query: query() },
        Expr::CollectSubquery { query: query() },
    ];
    for expr in &bearing {
        assert!(expr.is_scope_bearing(), "{expr:?} must be scope-bearing");
        assert!(expr.children().is_empty(), "{expr:?} has no children");
    }
    for expr in [var("n"), Expr::CountStar, Expr::Int(1)] {
        assert!(!expr.is_scope_bearing(), "{expr:?} is scope-transparent");
    }
}

#[test]
fn map_children_visits_every_traversable_subtree() {
    // A CASE whose arms are lists containing a marker variable; the rewrite
    // must reach every one, not just Unary/Binary/Function.
    let marker = Expr::Variable("marker".into());
    let expr = Expr::Case {
        when_then: vec![(
            Expr::List(vec![marker.clone()]),
            Expr::List(vec![marker.clone()]),
        )],
        else_expr: Some(Box::new(Expr::List(vec![marker.clone()]))),
    };
    fn rewrite(expr: &Expr) -> Result<Expr, ()> {
        if matches!(expr, Expr::Variable(name) if name == "marker") {
            return Ok(Expr::Variable("rewritten".into()));
        }
        expr.map_children(&mut rewrite)
    }
    let rewritten = rewrite(&expr).unwrap();
    let mut seen = HashSet::new();
    rewritten.visit(&mut |e| {
        if let Expr::Variable(name) = e {
            seen.insert(name.to_string());
        }
    });
    assert!(seen.contains("rewritten"), "{rewritten:?}");
    assert!(!seen.contains("marker"), "{rewritten:?}");
}

#[test]
fn children_covers_every_variant_exactly_once() {
    // A list-comprehension under a subscript: the historical
    // `expr_contains_variable` walker missed the ListComp arm entirely.
    let expr = Expr::Subscript {
        expr: Box::new(Expr::ListComp {
            ident: "x".into(),
            list: Box::new(Expr::List(vec![var("n")])),
            predicate: None,
            projection: Box::new(var("x")),
        }),
        index: Box::new(Expr::Int(0)),
    };
    assert!(expr.any(&mut |e| matches!(e, Expr::Variable(v) if v == "n")));
    assert!(expr.any(&mut |e| matches!(e, Expr::Variable(v) if v == "x")));
}

#[test]
fn children_include_dynamic_label_expressions() {
    let expr = Expr::LabelPredicate {
        expr: Box::new(var("n")),
        label_expr: LabelExpr::And(vec![
            LabelExpr::Label("A".into()),
            LabelExpr::Dynamic {
                expr: Box::new(var("l")),
                quantifier: crate::DynamicLabelQuantifier::Default,
            },
        ]),
        strict: false,
    };
    assert_eq!(expr.children().len(), 2);
    assert!(expr.any(&mut |e| matches!(e, Expr::Variable(v) if v == "l")));
}

#[test]
fn label_expr_vars_folds_the_dynamic_child_expressions() {
    // `label_expr_vars` is a fold over `each_label_expr_child`, so
    // every dynamic `$(…)` form contributes through any boolean
    // structure, while static labels and wildcards contribute nothing.
    let expr = LabelExpr::And(vec![
        LabelExpr::Label("A".into()),
        LabelExpr::Not(Box::new(LabelExpr::Dynamic {
            expr: Box::new(var("l")),
            quantifier: crate::DynamicLabelQuantifier::Default,
        })),
        LabelExpr::Or(vec![
            LabelExpr::Wildcard,
            LabelExpr::Dynamic {
                expr: Box::new(var("m")),
                quantifier: crate::DynamicLabelQuantifier::All,
            },
        ]),
    ]);
    let mut vars = HashSet::new();
    label_expr_vars(&expr, &mut vars);
    assert!(vars.contains("l"));
    assert!(vars.contains("m"));
    assert_eq!(vars.len(), 2);
}

#[test]
fn children_do_not_descend_into_patterns_or_subqueries() {
    let node = |variable: &str, properties: Option<Expr>| NodePattern {
        variable: Some(variable.into()),
        labels: Vec::new(),
        label_expr: None,
        properties,
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    };
    let rel = || RelPattern {
        variable: Some("r".into()),
        types: Vec::new(),
        type_expr: Some(LabelExpr::Dynamic {
            expr: Box::new(var("t")),
            quantifier: crate::DynamicLabelQuantifier::Default,
        }),
        direction: crate::Direction::Outgoing,
        properties: Some(var("rp")),
        prop_keys: Vec::new(),
        var_length: false,
        min_hops: 1,
        max_hops: None,
        where_clause: None,
        quantified: false,
    };
    let path = PathPattern {
        variable: None,
        subpath_variable: None,
        elements: vec![
            PatternElement::Node(node("a", Some(var("p")))),
            PatternElement::Relationship(rel()),
            PatternElement::Node(node("b", None)),
        ],
        nodes: vec![node("a", Some(var("p"))), node("b", None)],
        rels: vec![rel()],
        where_clause: None,
        mode: None,
        selector: None,
        is_shortest_path_function: false,
    };
    let expr = Expr::PatternPred { path };
    assert!(expr.children().is_empty());
    // ...but expr_vars still looks inside the pattern: node and rel
    // variables (both via the shared `path_pattern_vars` iterator),
    // the property maps, and the dynamic type expressions.
    let mut vars = HashSet::new();
    expr_vars(&expr, &mut vars);
    for expected in ["a", "b", "r", "p", "t", "rp"] {
        assert!(vars.contains(expected), "missing variable {expected}");
    }
    assert_eq!(vars.len(), 6);
}

#[test]
fn expr_vars_excludes_comprehension_bound_idents() {
    // `[x IN [n] | x + y]` references `n` and `y`, not the bound `x`.
    let expr = Expr::ListComp {
        ident: "x".into(),
        list: Box::new(Expr::List(vec![var("n")])),
        predicate: None,
        projection: Box::new(Expr::Binary {
            op: crate::BinaryOp::Add,
            left: Box::new(var("x")),
            right: Box::new(var("y")),
        }),
    };
    let mut vars = HashSet::new();
    expr_vars(&expr, &mut vars);
    let mut collected: Vec<&str> = vars.iter().map(String::as_str).collect();
    collected.sort_unstable();
    assert_eq!(collected, vec!["n", "y"]);
    // An outer `x` next to the comprehension is still reported: the
    // historical planner walker removed it depending on traversal order.
    let expr = Expr::Binary {
        op: crate::BinaryOp::Add,
        left: Box::new(var("x")),
        right: Box::new(Expr::ListComp {
            ident: "x".into(),
            list: Box::new(Expr::List(vec![])),
            predicate: None,
            projection: Box::new(var("x")),
        }),
    };
    let mut vars = HashSet::new();
    expr_vars(&expr, &mut vars);
    assert!(vars.contains("x"));
}

#[test]
fn expr_vars_collects_map_projection_variable_items() {
    let expr = Expr::MapProjection {
        expr: Box::new(var("n")),
        items: vec![
            MapProjectionItem::Property { key: "name".into() },
            MapProjectionItem::Variable { name: "v".into() },
            MapProjectionItem::KeyValue {
                key: "k".into(),
                value: var("w"),
            },
            MapProjectionItem::AllProperties,
        ],
    };
    let mut vars = HashSet::new();
    expr_vars(&expr, &mut vars);
    assert!(vars.contains("n"));
    assert!(vars.contains("v"));
    assert!(vars.contains("w"));
    assert_eq!(vars.len(), 3);
}

#[test]
fn needs_gpm_is_the_single_shared_predicate() {
    let node = || NodePattern {
        variable: None,
        labels: Vec::new(),
        label_expr: None,
        properties: None,
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    };
    let rel = || RelPattern {
        variable: None,
        types: Vec::new(),
        type_expr: None,
        direction: crate::Direction::Outgoing,
        properties: None,
        prop_keys: Vec::new(),
        var_length: false,
        min_hops: 1,
        max_hops: None,
        where_clause: None,
        quantified: false,
    };
    let compact = PathPattern {
        variable: None,
        subpath_variable: None,
        elements: vec![
            PatternElement::Node(node()),
            PatternElement::Relationship(rel()),
            PatternElement::Node(node()),
        ],
        nodes: vec![node(), node()],
        rels: vec![rel()],
        where_clause: None,
        mode: None,
        selector: None,
        is_shortest_path_function: false,
    };
    assert!(!compact.needs_gpm());
    // Compact representation cleared at lowering => GPM.
    let mut gpm = compact.clone();
    gpm.nodes.clear();
    gpm.rels.clear();
    assert!(gpm.needs_gpm());
    // GPM-only features on an otherwise compact path.
    let mut with_mode = compact.clone();
    with_mode.mode = Some(crate::PathMode::Walk);
    assert!(with_mode.needs_gpm());
    let mut with_element_pred = compact.clone();
    with_element_pred.elements[1] = PatternElement::Relationship(RelPattern {
        where_clause: Some(Expr::Bool(true)),
        ..rel()
    });
    assert!(with_element_pred.needs_gpm());
    let mut with_qpp = compact.clone();
    with_qpp
        .elements
        .push(PatternElement::Quantified(crate::QuantifiedPathPattern {
            subpath_variable: None,
            elements: vec![PatternElement::Node(node())],
            where_clause: None,
            min: 1,
            max: None,
        }));
    assert!(with_qpp.needs_gpm());
}

#[test]
fn references_any_sees_map_projection_shorthand_reads() {
    use std::collections::HashSet;

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // `n {n}` — the shorthand item is a name, not an expression, so the
    // children walk cannot see it; the shared shorthand policy must.
    let shorthand = Expr::MapProjection {
        expr: Box::new(var("n")),
        items: vec![crate::MapProjectionItem::Variable { name: "n".into() }],
    };
    assert!(references_any(&shorthand, &names(&["n"])));
    assert!(!references_any(&shorthand, &names(&["other"])));
    // `n {k: v}` reads only what the value reads.
    let keyed = Expr::MapProjection {
        expr: Box::new(var("n")),
        items: vec![crate::MapProjectionItem::KeyValue {
            key: "k".into(),
            value: var("v"),
        }],
    };
    assert!(references_any(&keyed, &names(&["v"])));
    assert!(!references_any(&keyed, &names(&["k"])));
}

#[test]
fn references_any_drops_comprehension_bound_identifiers() {
    use std::collections::HashSet;

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // `[x IN [1,2] | x]` under an outer `x`: the projection's `x` is the
    // comprehension's own binding, so nothing reads the outer scope.
    let expr = Expr::ListComp {
        ident: "x".into(),
        list: Box::new(Expr::List(vec![Expr::Int(1), Expr::Int(2)])),
        predicate: None,
        projection: Box::new(var("x")),
    };
    assert!(!references_any(&expr, &names(&["x"])));
    // The source expression is evaluated *outside* the binding.
    let source = Expr::ListComp {
        ident: "x".into(),
        list: Box::new(var("x")),
        predicate: None,
        projection: Box::new(var("x")),
    };
    assert!(references_any(&source, &names(&["x"])));
    // Another outer name in the projection still counts.
    let other = Expr::ListComp {
        ident: "x".into(),
        list: Box::new(Expr::List(vec![Expr::Int(1)])),
        predicate: None,
        projection: Box::new(Expr::Binary {
            op: crate::BinaryOp::Add,
            left: Box::new(var("x")),
            right: Box::new(var("y")),
        }),
    };
    assert!(references_any(&other, &names(&["y"])));
    assert!(!references_any(&other, &names(&["x"])));
}

#[test]
fn references_any_treats_a_subquery_body_as_opaque() {
    use std::collections::HashSet;

    let query = crate::Query {
        parts: vec![crate::SingleQuery {
            clauses: vec![Clause::Return {
                distinct: false,
                group_by: None,
                items: vec![crate::Projection {
                    expr: var("outer"),
                    alias: None,
                    source_text: None,
                }],
                order: Vec::new(),
                skip: None,
                limit: None,
            }],
        }],
        union_all: Vec::new(),
        next: None,
    };
    let expr = Expr::ExistsSubquery { query };
    let names: HashSet<String> = ["outer".to_string()].into_iter().collect();
    assert!(!references_any(&expr, &names));
}

/// The two vocabularies cannot drift: every variant
/// [`Expr::binds_identifiers`] names opens at least one element scope in
/// [`walk_scope_children`], and every other variant opens none. A binding
/// variant added to the predicate without a fold arm trips the debug assert
/// in the fold's fallback; this test pins the positive direction.
// The keeping-test linking `binds_identifiers` to `walk_scope_children`'s
// arms: every binding variant opens an Element-scoped body, so a new
// binding variant must grow both the predicate and the walk (the same
// role `scope_bearing_variants_match_children_skip_set` plays for
// `is_scope_bearing`). The exact per-variant child/scope expectations
// live in `walk_scope_children_classifies_every_binding_variant` below.
#[test]
fn every_identifier_binding_variant_opens_an_element_scope() {
    let binding: Vec<Expr> = vec![
        Expr::ListComp {
            ident: "x".into(),
            list: Box::new(Expr::List(vec![])),
            predicate: None,
            projection: Box::new(var("x")),
        },
        Expr::MapComp {
            key_ident: None,
            value_ident: "v".into(),
            source: Box::new(Expr::Map(vec![])),
            predicate: None,
            key_expr: Box::new(var("k")),
            value_expr: Box::new(var("v")),
        },
        Expr::Quantifier {
            kind: crate::QuantifierKind::All,
            ident: "x".into(),
            list: Box::new(Expr::List(vec![])),
            predicate: Box::new(var("x")),
        },
        Expr::Reduce {
            accumulator: "a".into(),
            initial: Box::new(Expr::Int(0)),
            ident: "x".into(),
            list: Box::new(Expr::List(vec![])),
            expression: Box::new(var("a")),
        },
        Expr::AllReduce {
            accumulator: "a".into(),
            initial: Box::new(Expr::Int(0)),
            ident: "x".into(),
            list: Box::new(Expr::List(vec![])),
            expression: Box::new(var("a")),
            predicate: Box::new(var("x")),
        },
    ];
    for expr in &binding {
        assert!(expr.binds_identifiers(), "{expr:?} must bind identifiers");
        let mut opened = Vec::new();
        walk_scope_children(expr, &mut |_, scope| {
            if let ChildScope::Element(names) = scope {
                opened.push(names);
            }
        });
        assert!(
            !opened.is_empty(),
            "{expr:?} needs a walk_scope_children arm with Element scope, not the fallback"
        );
    }
    let mut opened = 0;
    walk_scope_children(&var("n"), &mut |_, scope| {
        if matches!(scope, ChildScope::Element(_)) {
            opened += 1;
        }
    });
    assert_eq!(opened, 0, "a plain variable binds nothing");
}

// The one scope classification the aggregation walks share: every
// identifier-binding variant reports its source in the enclosing scope and
// its per-element bodies with exactly the names that variant binds.
#[test]
fn walk_scope_children_classifies_every_binding_variant() {
    fn seen(expr: &Expr) -> Vec<(Expr, Vec<String>)> {
        let mut out = Vec::new();
        walk_scope_children(expr, &mut |child, scope| {
            let bound = match scope {
                ChildScope::Enclosing => Vec::new(),
                ChildScope::Element(names) => names.iter().map(|n| n.to_string()).collect(),
            };
            out.push((child.clone(), bound));
        });
        out
    }
    let pair = |child: Expr, names: &[&str]| {
        let bound: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        (child, bound)
    };

    let list_comp = Expr::ListComp {
        ident: "x".into(),
        list: Box::new(var("list")),
        predicate: Some(Box::new(var("pred"))),
        projection: Box::new(var("proj")),
    };
    assert_eq!(
        seen(&list_comp),
        vec![
            pair(var("list"), &[]),
            pair(var("pred"), &["x"]),
            pair(var("proj"), &["x"]),
        ]
    );

    let map_comp = Expr::MapComp {
        key_ident: Some("k".into()),
        value_ident: "v".into(),
        source: Box::new(var("source")),
        predicate: None,
        key_expr: Box::new(var("key")),
        value_expr: Box::new(var("value")),
    };
    assert_eq!(
        seen(&map_comp),
        vec![
            pair(var("source"), &[]),
            pair(var("key"), &["k", "v"]),
            pair(var("value"), &["k", "v"]),
        ]
    );

    let quantifier = Expr::Quantifier {
        kind: crate::QuantifierKind::All,
        ident: "x".into(),
        list: Box::new(var("list")),
        predicate: Box::new(var("pred")),
    };
    assert_eq!(
        seen(&quantifier),
        vec![pair(var("list"), &[]), pair(var("pred"), &["x"])]
    );

    let reduce = Expr::Reduce {
        accumulator: "a".into(),
        initial: Box::new(var("initial")),
        ident: "x".into(),
        list: Box::new(var("list")),
        expression: Box::new(var("expr")),
    };
    assert_eq!(
        seen(&reduce),
        vec![
            pair(var("initial"), &[]),
            pair(var("list"), &[]),
            pair(var("expr"), &["a", "x"]),
        ]
    );

    let all_reduce = Expr::AllReduce {
        accumulator: "a".into(),
        initial: Box::new(var("initial")),
        ident: "x".into(),
        list: Box::new(var("list")),
        expression: Box::new(var("expr")),
        predicate: Box::new(var("pred")),
    };
    assert_eq!(
        seen(&all_reduce),
        vec![
            pair(var("initial"), &[]),
            pair(var("list"), &[]),
            pair(var("expr"), &["a", "x"]),
            pair(var("pred"), &["a", "x"]),
        ]
    );

    // Everything outside the binding class reports its children (from
    // `Expr::children`) in the enclosing scope — a subquery call and a
    // binary over the same call shape.
    let call = Expr::Function {
        name: "collect".into(),
        distinct: false,
        args: vec![Expr::Property {
            key_id: Default::default(),
            expr: Box::new(var("n")),
            key: "x".into(),
        }],
    };
    let sum = Expr::Binary {
        op: crate::BinaryOp::Add,
        left: Box::new(call.clone()),
        right: Box::new(var("y")),
    };
    assert_eq!(seen(&sum), vec![pair(call, &[]), pair(var("y"), &[])]);
}
