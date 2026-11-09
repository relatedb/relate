//! `Expr::try_for_each_edge` / `try_for_each_edge_mut` enumerate the same
//! edges in the same order: expression children, patterns, label expressions
//! and subquery statements, each as its own `Edge` variant.

use std::convert::Infallible;

use relate_parser::{parse, Clause, Edge, EdgeMut, Expr};

fn return_expr(query: &str) -> Expr {
    let stmt = parse(query).expect("parse");
    match stmt.clauses.last() {
        Some(Clause::Return(rc)) => rc.items[0].expr.clone(),
        other => panic!("expected a trailing RETURN, got {other:?}"),
    }
}

/// The expression-typed edges of `expr`, in visit order.
fn children(expr: &Expr) -> Vec<&Expr> {
    let mut out = Vec::new();
    let Ok(()) = expr.try_for_each_edge::<Infallible>(|edge| {
        if let Edge::Expr(child) = edge {
            out.push(child);
        }
        Ok(())
    });
    out
}

fn children_mut(expr: &mut Expr) -> Vec<&mut Expr> {
    let mut out = Vec::new();
    let Ok(()) = expr.try_for_each_edge_mut::<Infallible>(|edge| {
        if let EdgeMut::Expr(child) = edge {
            out.push(child);
        }
        Ok(())
    });
    out
}

/// Pre-order count of every node reachable through the expression edges.
fn count(expr: &Expr) -> usize {
    1 + children(expr).into_iter().map(count).sum::<usize>()
}

fn count_mut(expr: &mut Expr) -> usize {
    1 + children_mut(expr).into_iter().map(count_mut).sum::<usize>()
}

#[test]
fn shared_and_mutable_edges_agree_across_every_expression_shape() {
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
        let shared: Vec<*const Expr> = children(&e).into_iter().map(|c| c as *const Expr).collect();
        let exclusive: Vec<*const Expr> = children_mut(&mut e)
            .into_iter()
            .map(|c| &*c as *const Expr)
            .collect();
        assert_eq!(shared, exclusive, "{query}");
    }
}

#[test]
fn expr_edges_list_exactly_the_expression_edges() {
    let n = |q: &str| children(&return_expr(q)).len();
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
fn mutable_edge_edits_reach_the_tree() {
    let mut e = return_expr("RETURN a + b");
    for child in children_mut(&mut e) {
        *child = Expr::Star;
    }
    assert!(children(&e).iter().all(|c| matches!(c, Expr::Star)));
}

#[test]
fn pattern_label_and_subquery_edges_cover_patterns_labels_and_subqueries() {
    let kinds = |q: &str| {
        let e = return_expr(q);
        let mut out = Vec::new();
        let Ok(()) = e.try_for_each_edge::<Infallible>(|edge| {
            match edge {
                Edge::Expr(_) => {}
                Edge::Pattern(_) => out.push("pattern"),
                Edge::Subquery(_) => out.push("subquery"),
                Edge::Label(_) => out.push("label"),
            }
            Ok(())
        });
        out
    };
    assert_eq!(kinds("RETURN [(a)-->(b) | b.y]"), ["pattern"]);
    assert_eq!(kinds("RETURN (a)-->(b)"), ["pattern"]);
    assert_eq!(kinds("RETURN n:A"), ["label"]);
    assert_eq!(kinds("RETURN EXISTS { MATCH (a) RETURN a }"), ["subquery"]);
    assert!(kinds("RETURN a + b").is_empty());
    let mut e = return_expr("RETURN n:A");
    let mut mut_kinds = Vec::new();
    let Ok(()) = e.try_for_each_edge_mut::<Infallible>(|edge| {
        mut_kinds.push(matches!(edge, EdgeMut::Label(_)));
        Ok(())
    });
    assert_eq!(mut_kinds, [false, true]);
}

#[test]
fn edges_arrive_in_the_documented_visit_order() {
    let order = |q: &str| {
        let e = return_expr(q);
        let mut out = Vec::new();
        let Ok(()) = e.try_for_each_edge::<Infallible>(|edge| {
            out.push(match edge {
                Edge::Expr(_) => 'e',
                Edge::Pattern(_) => 'p',
                Edge::Subquery(_) => 's',
                Edge::Label(_) => 'l',
            });
            Ok(())
        });
        out.into_iter().collect::<String>()
    };
    // Patterns precede the expression children; a label follows them.
    assert_eq!(order("RETURN [(a)-->(b) WHERE a.x > 1 | b.y]"), "pee");
    assert_eq!(order("RETURN n:A"), "el");
    assert_eq!(order("RETURN EXISTS { MATCH (a) RETURN a }"), "s");
}
