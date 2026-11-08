//! `Expr::children` / `Expr::children_mut` enumerate the same expression
//! edges, in the same order, and leave patterns, label expressions and
//! subquery statements to the walkers that enter them.

use relate_parser::{parse, Clause, Expr};

fn return_expr(query: &str) -> Expr {
    let stmt = parse(query).expect("parse");
    match stmt.clauses.last() {
        Some(Clause::Return(rc)) => rc.items[0].expr.clone(),
        other => panic!("expected a trailing RETURN, got {other:?}"),
    }
}

/// Pre-order count of every node reachable through `children`.
fn count(expr: &Expr) -> usize {
    1 + expr.children().into_iter().map(count).sum::<usize>()
}

fn count_mut(expr: &mut Expr) -> usize {
    1 + expr
        .children_mut()
        .into_iter()
        .map(count_mut)
        .sum::<usize>()
}

#[test]
fn children_and_children_mut_agree_across_every_expression_shape() {
    let queries = [
        "RETURN [1, 2, 3]",
        "RETURN {a: 1, b: 2 + 3}",
        "RETURN n.p + $k * f(1, 2)",
        "RETURN CASE WHEN a THEN 1 WHEN b THEN 2 ELSE 3 END",
        "RETURN [x IN [1, 2] WHERE x > 1 | x * 2]",
        "RETURN reduce(s = 0, x IN [1, 2] | s + x)",
        "RETURN all(x IN [1] WHERE x > 0)",
        "RETURN l[1], l[1..2], l[..2]",
        "RETURN n {.a, b: 1 + 1, c}",
        "RETURN [(a)-->(b) WHERE b.x > 1 | b.y]",
        "RETURN x IS NULL, x IN y, x STARTS WITH 'a', x:A",
        "RETURN (1 + 2)",
        "RETURN EXISTS { MATCH (a) RETURN a }",
    ];
    for query in queries {
        let mut e = return_expr(query);
        assert_eq!(count(&e), count_mut(&mut e), "{query}");
        let shared: Vec<*const Expr> = e.children().into_iter().map(|c| c as *const Expr).collect();
        let exclusive: Vec<*const Expr> = e
            .children_mut()
            .into_iter()
            .map(|c| &*c as *const Expr)
            .collect();
        assert_eq!(shared, exclusive, "{query}");
    }
}

#[test]
fn children_lists_exactly_the_expression_edges() {
    let n = |q: &str| return_expr(q).children().len();
    assert_eq!(n("RETURN 1"), 0);
    assert_eq!(n("RETURN a + b"), 2);
    assert_eq!(n("RETURN f(1, 2, 3)"), 3);
    assert_eq!(n("RETURN CASE WHEN a THEN 1 WHEN b THEN 2 ELSE 3 END"), 5);
    assert_eq!(n("RETURN [x IN l WHERE x > 1 | x]"), 3);
    assert_eq!(n("RETURN l[1..2]"), 3);
    assert_eq!(n("RETURN n {.a, b: 1, c}"), 2);
    // Patterns, label expressions and subquery statements are not children.
    assert_eq!(n("RETURN [(a)-->(b) | b.y]"), 1);
    assert_eq!(n("RETURN n:A"), 1);
    assert_eq!(n("RETURN EXISTS { MATCH (a) RETURN a }"), 0);
    assert_eq!(n("RETURN (a)-->(b)"), 0);
}

#[test]
fn children_mut_edits_reach_the_tree() {
    let mut e = return_expr("RETURN a + b");
    for child in e.children_mut() {
        *child = Expr::Star;
    }
    assert!(e.children().iter().all(|c| matches!(c, Expr::Star)));
}

#[test]
fn structural_edges_cover_patterns_labels_and_subqueries() {
    use relate_parser::StructuralEdge as E;
    let kinds = |q: &str| {
        return_expr(q)
            .structural_edges()
            .iter()
            .map(|e| match e {
                E::Pattern(_) => "pattern",
                E::Subquery(_) => "subquery",
                E::Label(_) => "label",
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds("RETURN [(a)-->(b) | b.y]"), ["pattern"]);
    assert_eq!(kinds("RETURN (a)-->(b)"), ["pattern"]);
    assert_eq!(kinds("RETURN n:A"), ["label"]);
    assert_eq!(kinds("RETURN EXISTS { MATCH (a) RETURN a }"), ["subquery"]);
    assert!(kinds("RETURN a + b").is_empty());
    let mut e = return_expr("RETURN n:A");
    assert_eq!(e.structural_edges_mut().len(), 1);
}
