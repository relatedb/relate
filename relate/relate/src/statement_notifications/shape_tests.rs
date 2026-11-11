//! Unit tests for the clause-shape detectors in [`super`].

use super::*;
use relate_names::SmallName;

fn label(name: &str) -> LabelExpr {
    LabelExpr::Label(name.into())
}

/// ADR-0363: a relationship has exactly one type, so a conjunction of
/// two distinct type names — or a conjunction of a type with a
/// disjunction of other types — admits no type. A disjunction and a
/// negation stay satisfiable; a dynamic part declines the check.
#[test]
fn relationship_type_expression_satisfiability_follows_the_one_type_rule() {
    assert!(type_expression_unsatisfiable(&LabelExpr::And(vec![
        label("A"),
        label("B"),
    ])));
    assert!(type_expression_unsatisfiable(&LabelExpr::And(vec![
        LabelExpr::Not(Box::new(label("A"))),
        label("B"),
        LabelExpr::Or(vec![label("C"), label("D")]),
    ])));
    assert!(!type_expression_unsatisfiable(&LabelExpr::Or(vec![
        label("A"),
        label("B"),
    ])));
    assert!(!type_expression_unsatisfiable(&label("A")));
    assert!(!type_expression_unsatisfiable(&LabelExpr::Not(Box::new(
        label("A")
    ))));
    assert!(!type_expression_unsatisfiable(&LabelExpr::And(vec![
        label("A"),
        LabelExpr::Dynamic {
            expr: Box::new(Expr::Parameter("p".into())),
            quantifier: relate_ir::DynamicLabelQuantifier::Default,
        },
    ])));
}

/// A dynamic member anywhere is the callee's typed `None`, wherever it
/// sits relative to a member that already decides the result.
#[test]
fn dynamic_label_members_are_undecidable_not_false() {
    let dynamic = || LabelExpr::Dynamic {
        expr: Box::new(Expr::Parameter("p".into())),
        quantifier: relate_ir::DynamicLabelQuantifier::Default,
    };
    assert_eq!(label_expression_satisfied(&dynamic(), None), None);
    let decided_first = LabelExpr::And(vec![LabelExpr::Not(Box::new(label("A"))), dynamic()]);
    assert_eq!(label_expression_satisfied(&decided_first, Some("A")), None);
    assert!(!type_expression_unsatisfiable(&decided_first));
    assert_eq!(
        label_expression_satisfied(&label("A"), Some("A")),
        Some(true)
    );
}

/// A crafted wide conjunction must not pay names × expression nodes:
/// every candidate fails (which is the input's point), so nothing
/// short-circuits. Past the shared budget the check declines and returns
/// `false`; a small conjunction is still checked normally.
#[test]
fn wide_type_expression_declines_past_the_work_budget() {
    // (WIDTH + 1) candidate types × (WIDTH + 1) expression nodes is well
    // past SHAPE_CHECK_WORK_BUDGET.
    const WIDTH: usize = 5000;
    let wide = LabelExpr::And(
        (0..WIDTH)
            .map(|index| label(&format!("T{index}")))
            .collect(),
    );
    assert!(
        !type_expression_unsatisfiable(&wide),
        "a wide conjunction declines instead of scanning every candidate"
    );
    assert!(
        type_expression_unsatisfiable(&LabelExpr::And(vec![label("A"), label("B")])),
        "a small conjunction is still checked"
    );
}

/// The sibling scans carry the same budget: a `CREATE` whose property map
/// and created-entity count multiply past `SHAPE_CHECK_WORK_BUDGET`
/// declines (no notification) instead of testing every pair.
#[test]
fn wide_create_self_reference_scan_declines_past_the_work_budget() {
    // WIDTH property values × WIDTH created entities is well past the
    // budget; the values reference the created names, so a checked scan
    // would emit WIDTH notifications.
    const WIDTH: usize = 5000;
    let mut body = String::from("(n0 {p: 1})");
    for index in 1..WIDTH {
        body.push_str(&format!(", (n{index} {{q: n0.p}})"));
    }
    // The self-reference notification is Cypher 5's 01N00; Cypher 25
    // rejects the same shape (42I58) at validation, so the notification
    // scan is exercised under an explicit `CYPHER 5` preamble.
    let query = relate_front::parse_and_lower(&format!("CYPHER 5 CREATE {body}")).unwrap();
    let Clause::Create { patterns } = &query.parts[0].clauses[0] else {
        panic!("expected a CREATE clause");
    };
    let mut notifications = Vec::new();
    observe_create(&mut notifications, patterns);
    assert!(
        notifications.is_empty(),
        "a wide CREATE declines instead of testing every (value, name) pair"
    );

    let small = relate_front::parse_and_lower("CYPHER 5 CREATE (a {p: 1}), (b {q: a.p})").unwrap();
    let Clause::Create { patterns } = &small.parts[0].clauses[0] else {
        panic!("expected a CREATE clause");
    };
    let mut notifications = Vec::new();
    observe_create(&mut notifications, patterns);
    assert_eq!(notifications.len(), 1, "a small CREATE is still checked");
}

/// A complex projection item is anything but a variable, literal, or
/// parameter; aggregates are recognized sort keys and stay out.
#[test]
fn complex_projection_items_exclude_variables_literals_and_aggregates() {
    assert!(!is_complex_projection_item(&Expr::Variable("a".into())));
    assert!(!is_complex_projection_item(&Expr::Int(1)));
    assert!(!is_complex_projection_item(&Expr::Parameter("p".into())));
    assert!(!is_complex_projection_item(&Expr::Function {
        name: "sum".into(),
        distinct: false,
        args: vec![Expr::Variable("a".into())],
    }));
    assert!(is_complex_projection_item(&Expr::Function {
        name: "toInteger".into(),
        distinct: false,
        args: vec![Expr::Variable("a".into())],
    }));
    assert!(is_complex_projection_item(&Expr::Property {
        key_id: Default::default(),
        expr: Box::new(Expr::Variable("a".into())),
        key: "p".into(),
    }));
}

/// A `USE`-headed importing WITH still imports: the leading clause is
/// the first non-`USE` one (`SingleQuery::leading_importing_with`), so
/// the body rebinding `x` through it is an import, not a shadow (the
/// previous first-clause-only derivation reported 03N60 here).
/// The one observe-and-count scaffold the subquery-shadowing tests
/// share: walk the statement, run the detector, count its 03N60 rows.
fn subquery_shadowing_count(query: &relate_ir::Query) -> usize {
    let mut parts = Vec::new();
    relate_planner::walk_query_parts(query, &mut |part| parts.push(part));
    let mut notifications = Vec::new();
    observe_subquery_scopes(&mut notifications, &parts);
    notifications
        .iter()
        .filter(|notification| notification.kind() == NotificationKind::SubqueryVariableShadowing)
        .count()
}

#[test]
fn subquery_shadowing_sees_a_use_headed_importing_with() {
    let query = relate_front::parse_and_lower(
        "MATCH (x:Person) CALL { USE neo4j WITH x RETURN x AS y } RETURN x, y",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&query),
        0,
        "a USE-headed importing WITH imports `x`"
    );
}

/// WB2: imports are per UNION arm. Arm 2's importing WITH must not
/// suppress arm 1's shadow of the same name.
#[test]
fn subquery_shadowing_is_per_union_arm() {
    // `x` is in scope outside; arm 1 rebinds it, arm 2 imports it. Arm
    // 1 still shadows (the flat statement-wide import set hid this).
    let per_arm = relate_front::parse_and_lower(
        "MATCH (x:Old) CALL { MATCH (x:Inner) RETURN x AS y \
         UNION WITH x RETURN x AS y } RETURN x, y",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&per_arm),
        1,
        "arm 1's shadow is not suppressed by arm 2's import"
    );
}

/// A name bound inside one `CALL { … }` body is local to it: a sibling
/// body later in the statement walk does not see it, so rebinding the
/// same spelling there shadows nothing. The flat pre-order occurrence
/// test reported this as a shadow because the first body's `x` preceded
/// the second body in the walk.
#[test]
fn subquery_shadowing_does_not_leak_across_sibling_bodies() {
    let siblings = relate_front::parse_and_lower(
        "CALL () { MATCH (x:First) RETURN x AS y } \
         CALL () { MATCH (x:Second) RETURN x AS z } RETURN y, z",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&siblings),
        0,
        "a sibling body's local name is not in the second body's outer scope"
    );
}

/// The complex-reference check sees every nested occurrence of a
/// projection item's expression — matched structurally, because lowering
/// builds the subclause expression independently of the projection list —
/// but not the root: an expression equal to the whole subclause
/// expression is the projection item itself, a recognized sort key.
#[test]
fn subquery_shadowing_ignores_names_introduced_after_the_body() {
    // `n` is bound inside the body and rebound by a *later* clause: it is
    // not in the subquery's outer scope, so nothing shadows. The
    // statement-wide occurrence test used to report it.
    let after = relate_front::parse_and_lower(
        "CALL () { MATCH (n:Old) RETURN count(n) AS c } MATCH (n:New) RETURN n.title, c",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&after),
        0,
        "a name introduced after the subquery shadows nothing"
    );

    // The control: a name bound *before* the body is in scope, so the
    // body rebinding it still shadows.
    let before = relate_front::parse_and_lower(
        "MATCH (n:Old) CALL () { MATCH (n:New) RETURN n AS x } RETURN n",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&before),
        1,
        "a name bound before the body is in its outer scope"
    );
}

#[test]
fn complex_references_find_nested_occurrences_but_not_the_root() {
    let inner = Expr::Binary {
        op: relate_ir::BinaryOp::Add,
        left: Box::new(Expr::Variable("a".into())),
        right: Box::new(Expr::Int(0)),
    };
    let outer = Expr::Binary {
        op: relate_ir::BinaryOp::Add,
        left: Box::new(inner.clone()),
        right: Box::new(Expr::Int(1)),
    };
    let projection = |expr: Expr| Projection {
        expr,
        alias: None,
        source_text: None,
    };
    let nested = projection(inner.clone());
    let exact = projection(outer.clone());
    let elsewhere = projection(Expr::Variable("b".into()));
    let candidates = [(0, &nested), (1, &exact), (2, &elsewhere)];
    let found = complex_references(&outer, &candidates);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, 0, "the nested occurrence is a reference");
    assert!(
        complex_references(&inner, &candidates).is_empty(),
        "an equal root is the projection item itself, not a reference"
    );
}

/// The complex-reference detector walks the subclause expression once,
/// not once per (expression, item) pair: a projection and an `ORDER BY`
/// this large would be quadratic in the pairing and is not in the walk.
#[test]
fn complex_reference_detection_is_one_walk_per_subclause_expression() {
    const WIDTH: usize = 2000;
    let items: Vec<Projection> = (0..WIDTH)
        .map(|index| Projection {
            expr: Expr::Property {
                key_id: Default::default(),
                expr: Box::new(Expr::Variable("n".into())),
                key: SmallName::from_name(&format!("p{index}")),
            },
            alias: Some(SmallName::from_name(&format!("a{index}"))),
            source_text: None,
        })
        .collect();
    let order = OrderItem {
        expr: Expr::List(items.iter().map(|item| item.expr.clone()).collect()),
        ascending: true,
    };
    let mut notifications = Vec::new();
    observe_projection(&mut notifications, &items, &[order], None, true);
    assert_eq!(
        notifications
            .iter()
            .filter(|notification| {
                notification.kind() == NotificationKind::ComplexSubclauseReference
            })
            .count(),
        WIDTH,
        "every projected expression is found in the subclause expression"
    );
}

/// A wide projection checked against a wide subclause expression declines
/// at the shared node × item work budget instead of comparing every pair.
#[test]
fn complex_reference_check_declines_past_the_work_budget() {
    const WIDTH: usize = 3000;
    let items: Vec<Projection> = (0..WIDTH)
        .map(|index| Projection {
            expr: Expr::Property {
                key_id: Default::default(),
                expr: Box::new(Expr::Variable("n".into())),
                key: SmallName::from_name(&format!("p{index}")),
            },
            alias: Some(SmallName::from_name(&format!("a{index}"))),
            source_text: None,
        })
        .collect();
    let candidates: Vec<(usize, &Projection)> = items.iter().enumerate().collect();
    let wide = Expr::List(items.iter().map(|item| item.expr.clone()).collect());
    // 2 × WIDTH visited nodes × WIDTH items is past the budget.
    assert!(
        complex_references(&wide, &candidates).is_empty(),
        "a wide nodes × items pair declines instead of comparing every pair"
    );
}

/// The complex-reference deprecation only applies when the projection
/// groups (`DISTINCT` or an aggregation); a plain non-grouping
/// projection keeps its pre-projection scope visible to `ORDER BY`.
#[test]
fn complex_reference_requires_grouping_or_distinct() {
    let item = Projection {
        expr: Expr::Property {
            key_id: Default::default(),
            expr: Box::new(Expr::Variable("n".into())),
            key: "name".into(),
        },
        alias: Some("x".into()),
        source_text: None,
    };
    let order = OrderItem {
        expr: Expr::Binary {
            op: relate_ir::BinaryOp::Add,
            left: Box::new(item.expr.clone()),
            right: Box::new(Expr::Int(1)),
        },
        ascending: true,
    };
    let mut plain = Vec::new();
    observe_projection(
        &mut plain,
        std::slice::from_ref(&item),
        &[order.clone()],
        None,
        false,
    );
    assert!(plain.is_empty(), "a non-grouping projection does not warn");
    let mut grouped = Vec::new();
    observe_projection(&mut grouped, &[item], &[order], None, true);
    assert_eq!(grouped.len(), 1, "DISTINCT makes the reference deprecated");
}

/// A scope-less call's legacy leading `WITH` imports bare variables and
/// the `*` star — one set per UNION arm.
#[test]
fn call_imports_read_each_arms_leading_with() {
    let query = relate_front::parse_and_lower("CALL { WITH *, a RETURN a AS x } RETURN x").unwrap();
    let Clause::CallSubquery {
        query: body, scope, ..
    } = &query.parts[0].clauses[0]
    else {
        panic!("expected a CALL subquery");
    };
    let outer = BTreeSet::from(["a".to_string(), "b".to_string()]);
    let imports = call_imports(scope, body, &outer);
    assert_eq!(imports.len(), body.parts.len());
    assert!(imports[0].contains("a"), "bare variable imported");
    assert!(imports[0].contains("b"), "star imports the outer scope");
}

/// A named subpath binds the same path value under its own name, so a
/// `length(…)` over the subpath name forces the exhaustive evaluation
/// just as one over the outer path variable does.
#[test]
fn named_subpath_length_forces_exhaustive_shortest_path() {
    let query = relate_front::parse_and_lower(
        "MATCH p = shortestPath((q = (a)-[r*]-(b))) WHERE length(q) > 1 RETURN p",
    )
    .unwrap();
    let Clause::Match {
        patterns,
        where_clause,
        ..
    } = &query.parts[0].clauses[0]
    else {
        panic!("expected a MATCH clause");
    };
    let mut notifications = Vec::new();
    observe_match(&mut notifications, patterns, where_clause.as_ref(), false);
    assert_eq!(
        notifications
            .iter()
            .filter(|notification| {
                notification.kind() == NotificationKind::ExhaustiveShortestPath
            })
            .count(),
        1,
        "a length over the named subpath forces the exhaustive evaluation"
    );
}

/// A quantifier over the shortest path's relationship variable forces the
/// exhaustive evaluation too (the single-relationship shape is the only one
/// a `shortestPath()` pattern admits).
#[test]
fn relationship_quantifier_forces_exhaustive_shortest_path() {
    let query = relate_front::parse_and_lower(
        "MATCH p = shortestPath((a)-[r*]-(b)) WHERE any(x IN r WHERE x IS NOT NULL) RETURN p",
    )
    .unwrap();
    let Clause::Match {
        patterns,
        where_clause,
        ..
    } = &query.parts[0].clauses[0]
    else {
        panic!("expected a MATCH clause");
    };
    let mut notifications = Vec::new();
    observe_match(&mut notifications, patterns, where_clause.as_ref(), false);
    assert_eq!(
        notifications
            .iter()
            .filter(|notification| {
                notification.kind() == NotificationKind::ExhaustiveShortestPath
            })
            .count(),
        1,
        "a quantifier over a later relationship variable forces the exhaustive evaluation"
    );
}

/// A `CALL { … }` inside an `EXISTS`/`COUNT`/`COLLECT` expression body
/// is checked against the names that body binds before it. Live Neo4j
/// 2026.10.1 raises 03N60 for all three shapes (the `COLLECT`/`COUNT`
/// controller-command acceptance suites); the detectors used to stop at
/// the clause tree and never see them.
#[test]
fn subquery_shadowing_sees_a_call_inside_an_expression_subquery() {
    let collect = relate_front::parse_and_lower(
        "MATCH (person:Person) \
         WITH COLLECT { \
           MATCH (person)-[:FRIEND]->(friend:Person) \
           CALL (person) { \
             MATCH (person)-[:FRIEND]->(friend:Person) \
             RETURN friend.name AS name \
             UNION ALL \
             MATCH (person)-[:FRIEND]->(otherPerson:Person) \
             RETURN otherPerson.name AS name \
           } \
           RETURN DISTINCT friend.name AS friendNames \
         } AS foo RETURN foo",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&collect),
        1,
        "the CALL re-binds the COLLECT body's `friend` without importing it"
    );

    let count = relate_front::parse_and_lower(
        "MATCH (person:Person) \
         WITH COUNT { \
           MATCH (person)-[:FOLLOWS]->(friend:Person) \
           CALL (person) { \
             MATCH (person)-[:FOLLOWS]->(friend:Person) \
             RETURN friend.name AS name \
           } \
           RETURN friend AS notHumans \
         } AS foo RETURN foo",
    )
    .unwrap();
    assert_eq!(subquery_shadowing_count(&count), 1);

    let exists = relate_front::parse_and_lower(
        "MATCH (person:Person) \
         WHERE EXISTS { \
           MATCH (person)-[:FRIEND]->(friend:Person) \
           CALL (person) { \
             MATCH (person)-[:FRIEND]->(friend:Person) \
             RETURN friend AS f \
           } \
         } RETURN person",
    )
    .unwrap();
    assert_eq!(subquery_shadowing_count(&exists), 1);
}

/// An expression body's leading `WITH` binds a name the nested CALL then
/// shadows (ProjectedPathsAcceptance `Path Expression descoped in COLLECT
/// in CALL`, both `WITH p0 AS p0` examples).
#[test]
fn subquery_shadowing_sees_a_with_inside_a_collect_expression() {
    for with in ["WITH p0 AS p0", "WITH p0 AS p0, 1 AS x"] {
        let query = relate_front::parse_and_lower(&format!(
            "MATCH p0 = (n7)-[]->() RETURN COLLECT {{ {with} \
             CALL () {{ MATCH p0 = (n7)-[]->() RETURN length(p0) AS a }} \
             RETURN a }} AS x"
        ))
        .unwrap();
        assert_eq!(
            subquery_shadowing_count(&query),
            1,
            "the CALL re-binds the COLLECT body's `p0` ({with})"
        );
    }
}

/// The expression body's shadow test starts from an empty frame: a name
/// bound only outside the expression subquery is not in its "outer
/// query", so the nested CALL rebinding it raises nothing (live Neo4j
/// 2026.10.1). Importing the body's own name silences the shadow too.
#[test]
fn subquery_shadowing_in_an_expression_body_starts_from_an_empty_frame() {
    let outside_only = relate_front::parse_and_lower(
        "MATCH (x:X) RETURN COLLECT { \
           CALL () { MATCH (x)-[]->(y) RETURN y AS z } RETURN z } AS c",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&outside_only),
        0,
        "a name bound outside the expression subquery is not its outer scope"
    );

    let imported = relate_front::parse_and_lower(
        "MATCH (friend:Person) RETURN COLLECT { \
           MATCH (friend)-[:FRIEND]->(other:Person) \
           CALL (friend) { MATCH (friend)-[]->(x) RETURN x AS y } RETURN y } AS c",
    )
    .unwrap();
    assert_eq!(
        subquery_shadowing_count(&imported),
        0,
        "an imported body name is not shadowed"
    );
}
