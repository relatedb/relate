//! Token resolution (ADR-0541): at the start of planning, every static
//! label, relationship type and property key the read side of a statement
//! mentions is resolved to its store token id once, and the id is carried
//! beside the name on the IR (`NodePattern::labels`, `RelPattern::types`,
//! `Expr::Property::key_id`, `NodePattern::prop_keys`). `Token::Absent` records a name the store has
//! never interned; `Token::Unresolved` is a name planning did not resolve.
//!
//! Covered: MATCH patterns and predicates, projections, ordering, UNWIND
//! and nested `CALL { }` / `WHEN` bodies. Write clauses (CREATE, MERGE,
//! SET, REMOVE, FOREACH) stay unresolved — names a statement creates are
//! interned only at commit, so they keep their string arm. Patterns inside
//! expressions (`EXISTS { }`, pattern predicates) are likewise left
//! unresolved; consumers treat an unresolved element as "look the name up".

use std::convert::Infallible;

use relate_ir::{
    Clause, Expr, NodePattern, OrderItem, PathPattern, PatternElement, Projection, Query,
    RelPattern,
};
use relate_names::{NamedToken, SmallName, Token, TokenResolver};

/// The resolver a statement that commits mid-flight resolves through.
///
/// `Token::Id` is the store's id for the name, assigned once and never
/// reassigned, so a commit cannot invalidate it. `Token::Absent` — "the
/// store has never interned this name" — is the one arm a mid-statement
/// commit can falsify: a batch interns a name an earlier-planned read
/// called absent, and that read then misses the committed data. Reported
/// as `Unresolved`, the name is looked up by text at run time instead, and
/// every already-interned name keeps its id.
pub(crate) struct FreshAbsent<'a>(pub(crate) &'a dyn TokenResolver);

impl TokenResolver for FreshAbsent<'_> {
    fn resolve(&self, name: &str) -> Token {
        match self.0.resolve(name) {
            Token::Absent => Token::Unresolved,
            resolved => resolved,
        }
    }
}

/// Resolve every covered name in `query` against `resolver`.
pub(crate) fn resolve_query(query: &mut Query, resolver: &dyn TokenResolver) {
    for part in &mut query.parts {
        for clause in &mut part.clauses {
            resolve_clause(clause, resolver);
        }
    }
    if let Some(next) = query.next.as_deref_mut() {
        resolve_query(next, resolver);
    }
}

fn token_of(name: &SmallName, resolver: &dyn TokenResolver) -> Token {
    name.with_str(|text| resolver.resolve(text))
}

fn resolve_clause(clause: &mut Clause, resolver: &dyn TokenResolver) {
    match clause {
        Clause::Match {
            patterns,
            where_clause,
            ..
        } => {
            for pattern in patterns {
                resolve_path(pattern, resolver);
            }
            resolve_opt_expr(where_clause.as_mut(), resolver);
        }
        Clause::With {
            group_by,
            items,
            order,
            where_clause,
            ..
        } => {
            resolve_projection(group_by.as_mut(), items, order, resolver);
            resolve_opt_expr(where_clause.as_mut(), resolver);
        }
        Clause::Return {
            group_by,
            items,
            order,
            ..
        } => resolve_projection(group_by.as_mut(), items, order, resolver),
        Clause::Unwind { expression, .. } => resolve_expr_in_place(expression, resolver),
        Clause::CallSubquery { query, .. } => resolve_query(query, resolver),
        Clause::Conditional {
            branches,
            else_query,
        } => {
            for (condition, branch) in branches {
                resolve_expr_in_place(condition, resolver);
                resolve_query(branch, resolver);
            }
            if let Some(branch) = else_query.as_deref_mut() {
                resolve_query(branch, resolver);
            }
        }
        // Writes keep their string arm (ADR-0541 §3); the remaining clauses
        // mention no static token.
        Clause::Catalog(_)
        | Clause::Create { .. }
        | Clause::Merge { .. }
        | Clause::Call { .. }
        | Clause::Delete { .. }
        | Clause::Set { .. }
        | Clause::Remove { .. }
        | Clause::CreateIndex { .. }
        | Clause::DropIndex { .. }
        | Clause::ShowIndexes
        | Clause::Finish
        | Clause::Foreach { .. }
        | Clause::LoadCsv { .. }
        | Clause::Use { .. } => {}
    }
}

fn resolve_projection(
    group_by: Option<&mut Vec<Expr>>,
    items: &mut [Projection],
    order: &mut [OrderItem],
    resolver: &dyn TokenResolver,
) {
    for expr in group_by.into_iter().flatten() {
        resolve_expr_in_place(expr, resolver);
    }
    for item in items {
        resolve_expr_in_place(&mut item.expr, resolver);
    }
    for item in order {
        resolve_expr_in_place(&mut item.expr, resolver);
    }
}

fn resolve_path(path: &mut PathPattern, resolver: &dyn TokenResolver) {
    resolve_elements(&mut path.elements, resolver);
    for node in &mut path.nodes {
        resolve_node(node, resolver);
    }
    for rel in &mut path.rels {
        resolve_rel(rel, resolver);
    }
    resolve_opt_expr(path.where_clause.as_deref_mut(), resolver);
}

fn resolve_elements(elements: &mut [PatternElement], resolver: &dyn TokenResolver) {
    for element in elements {
        match element {
            PatternElement::Node(node) => resolve_node(node, resolver),
            PatternElement::Relationship(rel) => resolve_rel(rel, resolver),
            PatternElement::Quantified(group) => {
                resolve_elements(&mut group.elements, resolver);
                resolve_opt_expr(group.where_clause.as_mut(), resolver);
            }
        }
    }
}

fn resolve_node(node: &mut NodePattern, resolver: &dyn TokenResolver) {
    for (name, token) in &mut node.labels {
        *token = token_of(name, resolver);
    }
    resolve_opt_expr(node.properties.as_mut(), resolver);
    node.prop_keys = map_key_tokens(node.properties.as_ref(), resolver);
    resolve_opt_expr(node.where_clause.as_mut(), resolver);
}

fn resolve_rel(rel: &mut RelPattern, resolver: &dyn TokenResolver) {
    for (name, token) in &mut rel.types {
        *token = token_of(name, resolver);
    }
    resolve_opt_expr(rel.properties.as_mut(), resolver);
    rel.prop_keys = map_key_tokens(rel.properties.as_ref(), resolver);
    resolve_opt_expr(rel.where_clause.as_mut(), resolver);
}

/// The literal keys of a pattern's property map, each with its token.
fn map_key_tokens(properties: Option<&Expr>, resolver: &dyn TokenResolver) -> Vec<NamedToken> {
    let Some(Expr::Map(pairs)) = properties else {
        return Vec::new();
    };
    pairs
        .iter()
        .map(|(key, _)| {
            let name = SmallName::from_name(key);
            let token = token_of(&name, resolver);
            (name, token)
        })
        .collect()
}

fn resolve_opt_expr(expr: Option<&mut Expr>, resolver: &dyn TokenResolver) {
    if let Some(expr) = expr {
        resolve_expr_in_place(expr, resolver);
    }
}

fn resolve_expr_in_place(expr: &mut Expr, resolver: &dyn TokenResolver) {
    *expr = resolve_expr(expr, resolver);
}

/// The expression rebuilt with every property key resolved. Scope-bearing
/// variants (pattern and subquery expressions) are returned unchanged,
/// exactly as [`Expr::map_children`] does.
fn resolve_expr(expr: &Expr, resolver: &dyn TokenResolver) -> Expr {
    let Ok(mut out) =
        expr.map_children::<Infallible>(&mut |child| Ok(resolve_expr(child, resolver)));
    if let Expr::Property { key, key_id, .. } = &mut out {
        *key_id = token_of(key, resolver);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{plan_with_tokens, LogicalPlan, NoPatternStats};
    use relate_names::TokenId;

    /// A store that has interned exactly `Person`, `KNOWS` and `age`.
    struct Interned;

    impl TokenResolver for Interned {
        fn resolve(&self, name: &str) -> Token {
            match ["Person", "KNOWS", "age"]
                .iter()
                .position(|known| *known == name)
            {
                Some(i) => Token::Id(TokenId(i as u32)),
                None => Token::Absent,
            }
        }
    }

    fn plan_of(cypher: &str) -> LogicalPlan {
        let query = relate_front::parse_and_lower(cypher).unwrap();
        plan_with_tokens(query, &mut |_| false, &NoPatternStats, &Interned).unwrap()
    }

    /// The first MATCH pattern's node and rel of a planned one-hop query.
    fn first_hop(plan: &LogicalPlan) -> (&NodePattern, &RelPattern) {
        let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
            panic!("expected MATCH first");
        };
        let path = &patterns[0];
        (&path.nodes[0], &path.rels[0])
    }

    /// Folding a WHERE label into the pattern keeps the labels planning
    /// already resolved: `Person` stays an `Id` beside the folded `Employee`.
    #[test]
    fn where_label_fold_keeps_resolved_label_tokens() {
        let plan = plan_of("MATCH (n:Person) WHERE n:Employee RETURN n");
        let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
            panic!("expected MATCH first");
        };
        let labels = &patterns[0].nodes[0].labels;
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].0, "Person");
        assert_eq!(labels[0].1, Token::Id(TokenId(0)));
        assert_eq!(labels[1].0, "Employee");
    }

    /// A statement that commits mid-flight keeps the ids it can: an
    /// interned name stays an `Id` (assigned once, so a commit cannot
    /// invalidate it), and only a name the store has not interned — the one
    /// arm a mid-statement commit can falsify — is reported `Unresolved`
    /// and looked up by text at run time (LABS-1589).
    #[test]
    fn a_committing_statement_keeps_ids_and_softens_absent() {
        let plan = plan_of(
            "MATCH (n:Person)-[:KNOWS]->(m) WHERE n.age > 1 \
             CALL (n) { CREATE (:Employee) } IN TRANSACTIONS OF 1 ROW RETURN m",
        );
        let (node, rel) = first_hop(&plan);
        assert_eq!(
            node.labels[0].1,
            Token::Id(TokenId(0)),
            "Person is interned"
        );
        assert_eq!(rel.types[0].1, Token::Id(TokenId(1)), "KNOWS is interned");
        let Clause::Match {
            where_clause: Some(Expr::Binary { left, .. }),
            ..
        } = &plan.parts[0][0]
        else {
            panic!("expected a WHERE comparison");
        };
        let Expr::Property { key_id, .. } = left.as_ref() else {
            panic!("expected a property read");
        };
        assert_eq!(*key_id, Token::Id(TokenId(2)), "age is interned");
    }

    /// The same statement without the commit keeps the plain resolution:
    /// the softened arm is the mid-statement commit's, not the default's.
    #[test]
    fn a_statement_without_a_commit_reports_absent_as_absent() {
        let plan = plan_of("MATCH (n:Person)-[:KNOWS]->(m) WHERE n.nope > 1 RETURN m");
        let Clause::Match {
            where_clause: Some(Expr::Binary { left, .. }),
            ..
        } = &plan.parts[0][0]
        else {
            panic!("expected a WHERE comparison");
        };
        let Expr::Property { key_id, .. } = left.as_ref() else {
            panic!("expected a property read");
        };
        assert_eq!(*key_id, Token::Absent, "nope is not interned");
    }

    #[test]
    fn match_names_resolve_to_ids_and_unknown_names_to_absent() {
        let plan = plan_of("MATCH (n:Person)-[:KNOWS]->(m) WHERE n.age > 1 RETURN m");
        let (node, rel) = first_hop(&plan);
        assert_eq!(node.labels[0].1, Token::Id(TokenId(0)));
        assert_eq!(rel.types[0].1, Token::Id(TokenId(1)));
        let Clause::Match {
            where_clause: Some(Expr::Binary { left, .. }),
            ..
        } = &plan.parts[0][0]
        else {
            panic!("expected a WHERE comparison");
        };
        let Expr::Property { key_id, .. } = left.as_ref() else {
            panic!("expected n.age");
        };
        assert_eq!(*key_id, Token::Id(TokenId(2)));

        let absent = plan_of("MATCH (n:Nobody)-[:NEVER]->(m) WHERE n.nope > 1 RETURN m");
        let (node, rel) = first_hop(&absent);
        assert_eq!(node.labels[0].1, Token::Absent);
        assert_eq!(rel.types[0].1, Token::Absent);
        let Clause::Match {
            where_clause: Some(Expr::Binary { left, .. }),
            ..
        } = &absent.parts[0][0]
        else {
            panic!("expected a WHERE comparison");
        };
        assert!(matches!(
            left.as_ref(),
            Expr::Property {
                key_id: Token::Absent,
                ..
            }
        ));
    }

    #[test]
    fn pattern_map_keys_resolve_beside_the_map() {
        let plan = plan_of("MATCH (n {age: 1, nope: 2})-[:KNOWS {age: 3}]->(m) RETURN m");
        let (node, rel) = first_hop(&plan);
        let keys = |keys: &[NamedToken]| -> Vec<(String, Token)> {
            keys.iter().map(|(n, t)| (n.to_string(), *t)).collect()
        };
        assert_eq!(
            keys(&node.prop_keys),
            [
                ("age".to_string(), Token::Id(TokenId(2))),
                ("nope".to_string(), Token::Absent)
            ]
        );
        assert_eq!(
            keys(&rel.prop_keys),
            [("age".to_string(), Token::Id(TokenId(2)))]
        );
    }

    #[test]
    fn plan_without_a_resolver_leaves_patterns_unresolved() {
        let query =
            relate_front::parse_and_lower("MATCH (n:Person)-[:KNOWS]->(m) RETURN m").unwrap();
        let plan = crate::plan(query, &mut |_| false).unwrap();
        let (node, rel) = first_hop(&plan);
        assert_eq!(node.labels[0].1, Token::Unresolved);
        assert_eq!(rel.types[0].1, Token::Unresolved);
    }
}
