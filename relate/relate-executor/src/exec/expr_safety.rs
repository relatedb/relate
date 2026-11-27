//! Which expressions cannot raise: the one place the executor decides that an
//! expression's evaluation may be skipped on a row it will not keep. The
//! top-k window keys a candidate before it projects it, and an id-ordered
//! scan stops reading rows once its window is full; both would hide an error
//! the skipped evaluation raises, so each admits only what these predicates
//! prove cannot.

use relate_core::Value;
use relate_ir::{BinaryOp, Expr, PathPattern};
use relate_names::SmallName;

use crate::overlay::ExecGraph;
use crate::row::Row;

/// The variable of a bare `id(var)` call, the one spelling an id-ordered
/// scan recognizes (the planner's `scan_order` reads the same call).
pub(crate) fn id_call_variable(expr: &Expr) -> Option<&SmallName> {
    match expr {
        Expr::Function {
            name,
            distinct: false,
            args,
        } if name.with_str(|text| text.eq_ignore_ascii_case("id")) => match args.as_slice() {
            [Expr::Variable(var)] => Some(var),
            _ => None,
        },
        _ => None,
    }
}

/// A literal, or a property of `var`: the operands of a comparison that
/// cannot raise on a node `var` binds.
fn literal_or_property_of(expr: &Expr, var: &SmallName) -> bool {
    match expr {
        Expr::Null | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => true,
        Expr::Property { expr, .. } => {
            matches!(expr.as_ref(), Expr::Variable(name) if name == var)
        }
        _ => false,
    }
}

/// Whether `expr` cannot raise on any row of a scan that binds `var` to a
/// node: a literal, `var`, a property of `var`, or `id(var)`. No row in hand,
/// so the one variable the scan itself binds is the only one trusted.
pub(crate) fn cannot_fail_over_node(expr: &Expr, var: &SmallName) -> bool {
    literal_or_property_of(expr, var)
        || matches!(expr, Expr::Variable(name) if name == var)
        || id_call_variable(expr) == Some(var)
}

/// Whether a `MATCH … WHERE` predicate cannot raise on any node `var` scans:
/// a conjunction of comparisons between literals and properties of `var` (a
/// comparison of incomparable values is null, never an error).
pub(crate) fn where_cannot_fail(predicate: &Expr, var: &SmallName) -> bool {
    match predicate {
        Expr::Binary {
            op: BinaryOp::And,
            left,
            right,
        } => where_cannot_fail(left, var) && where_cannot_fail(right, var),
        Expr::Binary {
            op:
                BinaryOp::Eq
                | BinaryOp::Neq
                | BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::Lte
                | BinaryOp::Gte,
            left,
            right,
        } => literal_or_property_of(left, var) && literal_or_property_of(right, var),
        _ => false,
    }
}

/// The one eligibility test for stopping a node scan early: the clause is a
/// single bare node pattern binding `var`, and its `WHERE`, if any, cannot
/// raise on a row past the page. The ladder reads it to route the clause to
/// the walker, and the walker's stop reads it again, so a clause the walker
/// runs for another reason never stops over a filter that could raise.
pub(crate) fn is_bare_node_scan_of(
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    var: &SmallName,
) -> bool {
    let [path] = patterns else {
        return false;
    };
    let [node] = path.nodes.as_slice() else {
        return false;
    };
    node.variable.as_ref() == Some(var)
        && path.rels.is_empty()
        && path.variable.is_none()
        && path.where_clause.is_none()
        && path.selector.is_none()
        && path.mode.is_none()
        && node.properties.is_none()
        && node.where_clause.is_none()
        && where_clause.is_none_or(|predicate| where_cannot_fail(predicate, var))
}

/// Whether evaluating `expr` against `row` cannot raise: a literal, a bound
/// variable, a property read of a node, relationship, map or null, or `id()`
/// of a node, relationship or null. The one list the key-before-projection
/// path trusts to skip an expression on a row it rejects.
pub(crate) fn cannot_fail_on(expr: &Expr, row: &Row, graph: &ExecGraph<'_>) -> bool {
    // A property read raises on an entity deleted earlier in the statement, so
    // a bound node or relationship must still exist.
    let readable = |value: Option<&Value>| match value {
        Some(Value::Node(node)) => graph.node_exists(node.id),
        Some(Value::Relationship(rel)) => graph.rel_exists(rel.id),
        Some(Value::Map(_) | Value::Null) => true,
        _ => false,
    };
    match expr {
        Expr::Null | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => true,
        Expr::Variable(name) => row.get_name(name).is_some(),
        Expr::Property { expr, .. } => match expr.as_ref() {
            Expr::Variable(name) => readable(row.get_name(name)),
            _ => false,
        },
        Expr::Function { .. } => id_call_variable(expr).is_some_and(|arg| {
            matches!(
                row.get_name(arg),
                Some(Value::Node(_) | Value::Relationship(_) | Value::Null)
            )
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use relate_front::parse_and_lower;
    use relate_ir::Clause;

    use super::*;

    /// Whether the lone `MATCH` of `cypher` is a bare node scan of `n` the
    /// walker may stop early.
    fn stoppable(cypher: &str) -> bool {
        let query = parse_and_lower(cypher).unwrap();
        let Clause::Match {
            patterns,
            where_clause,
            ..
        } = &query.parts[0].clauses[0]
        else {
            panic!("the first clause is a MATCH");
        };
        is_bare_node_scan_of(patterns, where_clause.as_ref(), &SmallName::from("n"))
    }

    #[test]
    fn a_bare_scan_with_a_comparison_filter_may_stop() {
        assert!(stoppable("MATCH (n:N) RETURN n"));
        assert!(stoppable("MATCH (n:N) WHERE n.k > 3 AND n.k <> 9 RETURN n"));
    }

    #[test]
    fn a_filter_that_can_raise_never_stops_the_scan() {
        assert!(!stoppable("MATCH (n:N) WHERE 1 / (n.k - 1) > 0 RETURN n"));
        assert!(!stoppable("MATCH (n:N) WHERE n.k + 1 > 3 RETURN n"));
    }

    #[test]
    fn an_element_pattern_where_keeps_its_own_path() {
        // The engine fork sends it to the GPM walk, which has no node-scan
        // stop, so the clause is not a scan the walker may stop.
        assert!(!stoppable("MATCH (n:N WHERE n.k > 3) RETURN n"));
        assert!(!stoppable("MATCH (n:N WHERE 1 / (n.k - 1) > 0) RETURN n"));
    }

    #[test]
    fn a_pattern_beyond_one_bare_node_never_stops_the_scan() {
        assert!(!stoppable("MATCH (n:N {k: 1}) RETURN n"));
        assert!(!stoppable("MATCH (n:N), (m:M) RETURN n"));
        assert!(!stoppable("MATCH (n:N)-[]->(m) RETURN n"));
        assert!(!stoppable("MATCH p = (n:N) RETURN n"));
    }
}
