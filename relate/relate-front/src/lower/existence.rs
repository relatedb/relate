//! The lowering-time emptiness rewrite (REL8-2033): a comparison that only
//! asks whether a counted set is empty — `COUNT { q } > 0`, `= 0`, … — is
//! `EXISTS { q }` / `NOT EXISTS { q }`, which stops at the first match. The
//! rewrite is IR→IR and runs over the already-lowered `mdb::Expr`, so it has
//! its own module beside the AST→IR lowering.

use super::expr::literal_value;
use super::*;

/// A comparison of a row-count to a literal threshold, when it only asks
/// whether the counted set is empty: `COUNT { q }` is a non-null integer
/// `>= 0`, so `> 0`, `>= 1`, `<> 0` mean "at least one row" and `= 0`,
/// `< 1`, `<= 0` mean "none" — exactly `EXISTS { q }` / `NOT EXISTS { q }`,
/// which stops at the first match where the count runs the body to
/// completion for every outer row (REL8-2033). The same holds for `size()`
/// of a pattern comprehension whose projection is a literal: the projection
/// is evaluated per match only for its value, so dropping it changes no
/// answer and no error.
pub(super) fn comparison_expr(op: mdb::BinaryOp, left: mdb::Expr, right: mdb::Expr) -> mdb::Expr {
    // The one two-sided orientation policy (`relate_ir::orient`): the counted
    // operand may be written on either side and the op mirrors with it.
    let rewritten = relate_ir::orient(counting_expr, &left, &right, op)
        .and_then(|(counted, other, op)| existence_test(op, other, counted));
    rewritten.unwrap_or_else(|| mdb::Expr::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    })
}

/// A row-counting expression's decoded shape — the recognizer's result, so
/// the shape match runs once and the body builder consumes it.
enum Counted<'a> {
    /// `COUNT { q }`, its body borrowed (cloned only on an accepted rewrite).
    Count(&'a mdb::Query),
    /// `size()` of a pattern comprehension whose projection is a literal
    /// (checked when the shape is classified).
    Comprehension {
        path: &'a mdb::PathPattern,
        predicate: Option<&'a mdb::Expr>,
    },
}

/// The two shapes an emptiness comparison can probe.
fn counting_expr(expr: &mdb::Expr) -> Option<Counted<'_>> {
    match expr {
        // A CIP-213 `NEXT` body is declined: the existence evaluator answers
        // from the first stage (corpus issue 80), so rewriting a count that
        // spans the chain would change the answer.
        mdb::Expr::CountSubquery { query } if query.next.is_none() => Some(Counted::Count(query)),
        mdb::Expr::Function {
            name,
            distinct: false,
            args,
        } if args.len() == 1 && name.with_str(|n| n.eq_ignore_ascii_case("size")) => {
            let mdb::Expr::PatternComp {
                path,
                predicate,
                projection,
            } = &args[0]
            else {
                return None;
            };
            // A non-literal projection is evaluated per match; dropping it
            // could drop an error, so only a literal qualifies.
            literal_value(projection)?;
            Some(Counted::Comprehension {
                path,
                predicate: predicate.as_deref(),
            })
        }
        _ => None,
    }
}

/// `Some(existence test)` when `op` against the literal `other` is one of
/// the emptiness spellings.
fn existence_test(op: mdb::BinaryOp, other: &mdb::Expr, counted: Counted<'_>) -> Option<mdb::Expr> {
    let mdb::Expr::Int(threshold) = other else {
        return None;
    };
    let positive = match (op, *threshold) {
        (mdb::BinaryOp::Gt, 0) | (mdb::BinaryOp::Gte, 1) | (mdb::BinaryOp::Neq, 0) => true,
        (mdb::BinaryOp::Eq, 0) | (mdb::BinaryOp::Lt, 1) | (mdb::BinaryOp::Lte, 0) => false,
        _ => return None,
    };
    let exists = mdb::Expr::ExistsSubquery {
        query: exists_body(counted),
    };
    Some(if positive {
        exists
    } else {
        mdb::Expr::Unary {
            op: mdb::UnaryOp::Not,
            expr: Box::new(exists),
        }
    })
}

/// The `EXISTS` body a counted shape names. Called only for an accepted
/// rewrite, so the count body's clone is never wasted; whether a shape can
/// be served at all is `counting_expr`'s decision, so there is nothing left
/// to decline here.
fn exists_body(counted: Counted<'_>) -> mdb::Query {
    match counted {
        Counted::Count(query) => query.clone(),
        Counted::Comprehension { path, predicate } => mdb::Query {
            parts: vec![mdb::SingleQuery {
                clauses: vec![mdb::Clause::Match {
                    optional: false,
                    match_mode: mdb::MatchMode::DifferentRelationships,
                    patterns: vec![path.clone()],
                    where_clause: predicate.cloned(),
                    using_index: Vec::new(),
                    using_join: Vec::new(),
                    search: None,
                }],
            }],
            union_all: Vec::new(),
            next: None,
        },
    }
}
