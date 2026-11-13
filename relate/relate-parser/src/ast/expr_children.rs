//! The one edge table of [`Expr`]: its expression children and its
//! structural (non-expression) children in one visit-ordered enumeration,
//! in a shared and a mutable spelling generated from a single definition.

use super::{Expr, InterpPart, LabelExpr, MapProjectionItem, PatternPath, Statement};

/// One direct child of an [`Expr`] node, in the table's visit order.
///
/// The visit order is part of the contract: a pattern or subquery edge comes
/// before the node's expression children and a label edge after them, so a
/// walker that visits edges as they arrive reproduces the documented order
/// without a second pass.
#[derive(Debug)]
pub enum ExprEdge<'a> {
    /// An expression child: operand, argument, list/map item, CASE arm,
    /// comprehension or reduce body, subscript part, interpolation embed.
    Expr(&'a Expr),
    /// The pattern of a pattern comprehension, pattern predicate or
    /// `shortestPath()`.
    Pattern(&'a PatternPath),
    /// The nested statement of an `EXISTS`/`COUNT`/`COLLECT` subquery.
    Subquery(&'a Statement),
    /// The label expression of a `HasLabel`.
    Label(&'a LabelExpr),
}

/// Mutable twin of [`ExprEdge`].
#[derive(Debug)]
pub enum ExprEdgeMut<'a> {
    /// See [`ExprEdge::Expr`].
    Expr(&'a mut Expr),
    /// See [`ExprEdge::Pattern`].
    Pattern(&'a mut PatternPath),
    /// See [`ExprEdge::Subquery`].
    Subquery(&'a mut Statement),
    /// See [`ExprEdge::Label`].
    Label(&'a mut LabelExpr),
}

/// Expands the one exhaustive `Expr` match into the shared or the mutable
/// edge enumeration: `$r` is the borrow (`&` or `&mut`), `$iter` the matching
/// iterator method, `$edge` the output enum (`ExprEdge` or `ExprEdgeMut`), `$f` the
/// fallible sink. Every edge, expression or structural, is listed once and
/// in visit order, so the two spellings cannot drift and nothing is dropped.
macro_rules! edge_table {
    (@expr $f:ident, $edge:ident, $e:expr) => { $f($edge::Expr($e))? };
    (@exprs $f:ident, $edge:ident, $e:expr) => {
        for x in $e {
            $f($edge::Expr(x))?;
        }
    };
    (@opt $f:ident, $edge:ident, $e:expr) => {
        if let Some(x) = $e {
            $f($edge::Expr(x))?;
        }
    };
    ($self:expr, [$($r:tt)+], $iter:ident, $edge:ident, $f:ident) => {
        match $self {
            Expr::Literal(_)
            | Expr::Variable(_)
            | Expr::Parameter(_)
            | Expr::OptionValuePlaceholder
            | Expr::CountStar
            | Expr::Star => {}
            Expr::List(items) => edge_table!(@exprs $f, $edge, items.$iter()),
            Expr::Map(pairs) => {
                edge_table!(@exprs $f, $edge, pairs.$iter().map(|p| $($r)+ p.value))
            }
            Expr::StringInterp(parts) => {
                for part in parts.$iter() {
                    match part {
                        InterpPart::Interp(e) => edge_table!(@expr $f, $edge, $($r)+ **e),
                        InterpPart::Text(_) => {}
                    }
                }
            }
            Expr::Property(p) => {
                edge_table!(@expr $f, $edge, $($r)+ p.expr);
                edge_table!(@opt $f, $edge, $($r)+ p.key_expr);
            }
            Expr::FunctionCall(fc) => edge_table!(@exprs $f, $edge, fc.args.$iter()),
            Expr::Binary(b) => edge_table!(@exprs $f, $edge, [$($r)+ b.left, $($r)+ b.right]),
            Expr::Unary(u) => edge_table!(@expr $f, $edge, $($r)+ u.expr),
            Expr::Comparison(c) => {
                edge_table!(@exprs $f, $edge, [$($r)+ c.left, $($r)+ c.right])
            }
            Expr::In(i) => edge_table!(@exprs $f, $edge, [$($r)+ i.expr, $($r)+ i.list]),
            Expr::IsNull(x) => edge_table!(@expr $f, $edge, $($r)+ x.expr),
            Expr::IsType(x) => edge_table!(@expr $f, $edge, $($r)+ x.expr),
            Expr::IsNormalized(x) => edge_table!(@expr $f, $edge, $($r)+ x.expr),
            Expr::ChronologicalCompare(c) => {
                edge_table!(@exprs $f, $edge, [$($r)+ c.left, $($r)+ c.right])
            }
            Expr::StringMatch(m) => {
                edge_table!(@exprs $f, $edge, [$($r)+ m.left, $($r)+ m.right])
            }
            Expr::Case(c) => {
                for w in c.whens.$iter() {
                    edge_table!(@exprs $f, $edge, [$($r)+ w.condition, $($r)+ w.result]);
                }
                edge_table!(@opt $f, $edge, $($r)+ c.else_expr);
            }
            Expr::ListComprehension(lc) => {
                edge_table!(@expr $f, $edge, $($r)+ lc.list_expr);
                edge_table!(@opt $f, $edge, $($r)+ lc.where_expr);
                edge_table!(@opt $f, $edge, $($r)+ lc.map_expr);
            }
            Expr::MapComprehension(mc) => {
                edge_table!(@expr $f, $edge, $($r)+ mc.source_expr);
                edge_table!(@opt $f, $edge, $($r)+ mc.where_expr);
                edge_table!(@exprs $f, $edge, [$($r)+ mc.key_expr, $($r)+ mc.value_expr]);
            }
            Expr::PatternComprehension(pc) => {
                $f($edge::Pattern($($r)+ pc.pattern))?;
                edge_table!(@opt $f, $edge, $($r)+ pc.where_expr);
                edge_table!(@expr $f, $edge, $($r)+ pc.map_expr);
            }
            Expr::Reduce(r) => edge_table!(@exprs $f, $edge,
                [$($r)+ r.init_expr, $($r)+ r.list_expr, $($r)+ r.expression]),
            Expr::AllReduce(r) => edge_table!(@exprs $f, $edge,
                [$($r)+ r.init_expr, $($r)+ r.list_expr, $($r)+ r.expression, $($r)+ r.predicate]),
            Expr::Subscript(s) => {
                edge_table!(@expr $f, $edge, $($r)+ s.expr);
                edge_table!(@opt $f, $edge, $($r)+ s.index);
                edge_table!(@opt $f, $edge, $($r)+ s.from);
                edge_table!(@opt $f, $edge, $($r)+ s.to);
            }
            Expr::Quantifier(q) => {
                edge_table!(@expr $f, $edge, $($r)+ q.list);
                edge_table!(@opt $f, $edge, $($r)+ q.predicate);
            }
            Expr::HasLabel(h) => {
                edge_table!(@expr $f, $edge, $($r)+ h.expr);
                $f($edge::Label($($r)+ h.label_expr))?;
            }
            Expr::Paren(inner) => edge_table!(@expr $f, $edge, $($r)+ **inner),
            Expr::MapProjection(mp) => {
                edge_table!(@expr $f, $edge, $($r)+ mp.expr);
                for item in mp.items.$iter() {
                    match item {
                        MapProjectionItem::KeyValue(_, v) => edge_table!(@expr $f, $edge, v),
                        MapProjectionItem::AllProperties
                        | MapProjectionItem::Property(_)
                        | MapProjectionItem::Variable(_) => {}
                    }
                }
            }
            Expr::ExistsSubquery(s) => $f($edge::Subquery($($r)+ s.statement))?,
            Expr::CountSubquery(s) => $f($edge::Subquery($($r)+ s.statement))?,
            Expr::CollectSubquery(s) => $f($edge::Subquery($($r)+ s.statement))?,
            Expr::ShortestPath(s) => $f($edge::Pattern($($r)+ s.pattern))?,
            Expr::PatternPredicate(p) => $f($edge::Pattern($($r)+ p.pattern))?,
        }
    };
}

impl Expr {
    /// Feeds every direct child of this node to `f`, in visit order, stopping
    /// at the first error. The one enumeration of the model's edges: a
    /// pattern or subquery edge precedes the expression children, a label
    /// edge follows them, and expression children appear in source order
    /// (operands, arguments, list/map items, CASE arms, comprehension and
    /// reduce bodies, subscript parts, interpolation embeds). A walker that
    /// consumes edges as they arrive needs no allocation and no second pass.
    /// The underlying match is exhaustive (no catch-all), so a later variant
    /// fails compilation.
    pub fn try_for_each_edge<'a, E>(
        &'a self,
        mut f: impl FnMut(ExprEdge<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        edge_table!(self, [&], iter, ExprEdge, f);
        Ok(())
    }

    /// Mutable twin of [`Self::try_for_each_edge`], generated from the same
    /// table.
    pub fn try_for_each_edge_mut<'a, E>(
        &'a mut self,
        mut f: impl FnMut(ExprEdgeMut<'a>) -> Result<(), E>,
    ) -> Result<(), E> {
        edge_table!(self, [&mut], iter_mut, ExprEdgeMut, f);
        Ok(())
    }
}
