//! The one edge table of [`Expr`]: its expression children and its
//! structural (non-expression) children, each in a shared and a mutable
//! spelling generated from a single definition.

use super::{Expr, InterpPart, LabelExpr, MapProjectionItem, PatternPath, Statement};

/// A non-expression child of an [`Expr`] node: what a walker that enters
/// patterns, label expressions or nested statements must visit beyond
/// [`Expr::children`].
#[derive(Debug)]
pub enum StructuralEdge<'a> {
    /// The pattern of a pattern comprehension, pattern predicate or
    /// `shortestPath()`.
    Pattern(&'a PatternPath),
    /// The nested statement of an `EXISTS`/`COUNT`/`COLLECT` subquery.
    Subquery(&'a Statement),
    /// The label expression of a `HasLabel`.
    Label(&'a LabelExpr),
}

/// Mutable twin of [`StructuralEdge`].
#[derive(Debug)]
pub enum StructuralEdgeMut<'a> {
    /// See [`StructuralEdge::Pattern`].
    Pattern(&'a mut PatternPath),
    /// See [`StructuralEdge::Subquery`].
    Subquery(&'a mut Statement),
    /// See [`StructuralEdge::Label`].
    Label(&'a mut LabelExpr),
}

/// Expands the one exhaustive `Expr` match into a shared or a mutable
/// accessor: `$r` is the borrow (`&` or `&mut`), `$iter` the matching
/// iterator method, `$edge` the output enum. `$kind` selects which of the
/// two edge families the expansion lists (`expr` children or `structural`
/// edges); the variant coverage is written once, so the two spellings cannot
/// drift.
macro_rules! edge_table {
    ($self:expr, $kind:ident, [$($r:tt)+], $iter:ident, $edge:ident, $out:ident) => {
        match $self {
            Expr::Literal(_)
            | Expr::Variable(_)
            | Expr::Parameter(_)
            | Expr::OptionValuePlaceholder
            | Expr::CountStar
            | Expr::Star => {}
            Expr::List(items) => edge_table!(@$kind $out, extend items.$iter()),
            Expr::Map(pairs) => {
                edge_table!(@$kind $out, extend pairs.$iter().map(|p| $($r)+ p.value))
            }
            Expr::StringInterp(parts) => edge_table!(@$kind $out, extend
                parts.$iter().filter_map(|part| match part {
                    InterpPart::Interp(e) => Some($($r)+ **e),
                    InterpPart::Text(_) => None,
                })),
            Expr::Property(p) => {
                edge_table!(@$kind $out, push $($r)+ p.expr);
                edge_table!(@$kind $out, extend $($r)+ p.key_expr);
            }
            Expr::FunctionCall(fc) => edge_table!(@$kind $out, extend fc.args.$iter()),
            Expr::Binary(b) => edge_table!(@$kind $out, extend [$($r)+ b.left, $($r)+ b.right]),
            Expr::Unary(u) => edge_table!(@$kind $out, push $($r)+ u.expr),
            Expr::Comparison(c) => {
                edge_table!(@$kind $out, extend [$($r)+ c.left, $($r)+ c.right])
            }
            Expr::In(i) => edge_table!(@$kind $out, extend [$($r)+ i.expr, $($r)+ i.list]),
            Expr::IsNull(x) => edge_table!(@$kind $out, push $($r)+ x.expr),
            Expr::IsType(x) => edge_table!(@$kind $out, push $($r)+ x.expr),
            Expr::IsNormalized(x) => edge_table!(@$kind $out, push $($r)+ x.expr),
            Expr::ChronologicalCompare(c) => {
                edge_table!(@$kind $out, extend [$($r)+ c.left, $($r)+ c.right])
            }
            Expr::StringMatch(m) => {
                edge_table!(@$kind $out, extend [$($r)+ m.left, $($r)+ m.right])
            }
            Expr::Case(c) => {
                for w in c.whens.$iter() {
                    edge_table!(@$kind $out, extend [$($r)+ w.condition, $($r)+ w.result]);
                }
                edge_table!(@$kind $out, extend $($r)+ c.else_expr);
            }
            Expr::ListComprehension(lc) => {
                edge_table!(@$kind $out, push $($r)+ lc.list_expr);
                edge_table!(@$kind $out, extend $($r)+ lc.where_expr);
                edge_table!(@$kind $out, extend $($r)+ lc.map_expr);
            }
            Expr::MapComprehension(mc) => {
                edge_table!(@$kind $out, push $($r)+ mc.source_expr);
                edge_table!(@$kind $out, extend $($r)+ mc.where_expr);
                edge_table!(@$kind $out, extend [$($r)+ mc.key_expr, $($r)+ mc.value_expr]);
            }
            Expr::PatternComprehension(pc) => {
                edge_table!(@$kind $out, extend $($r)+ pc.where_expr);
                edge_table!(@$kind $out, push $($r)+ pc.map_expr);
                edge_table!(@$kind $out, pattern $edge, $($r)+ pc.pattern);
            }
            Expr::Reduce(r) => edge_table!(@$kind $out, extend
                [$($r)+ r.init_expr, $($r)+ r.list_expr, $($r)+ r.expression]),
            Expr::AllReduce(r) => edge_table!(@$kind $out, extend
                [$($r)+ r.init_expr, $($r)+ r.list_expr, $($r)+ r.expression, $($r)+ r.predicate]),
            Expr::Subscript(s) => {
                edge_table!(@$kind $out, push $($r)+ s.expr);
                edge_table!(@$kind $out, extend $($r)+ s.index);
                edge_table!(@$kind $out, extend $($r)+ s.from);
                edge_table!(@$kind $out, extend $($r)+ s.to);
            }
            Expr::Quantifier(q) => {
                edge_table!(@$kind $out, push $($r)+ q.list);
                edge_table!(@$kind $out, extend $($r)+ q.predicate);
            }
            Expr::HasLabel(h) => {
                edge_table!(@$kind $out, push $($r)+ h.expr);
                edge_table!(@$kind $out, label $edge, $($r)+ h.label_expr);
            }
            Expr::Paren(inner) => edge_table!(@$kind $out, push $($r)+ **inner),
            Expr::MapProjection(mp) => {
                edge_table!(@$kind $out, push $($r)+ mp.expr);
                edge_table!(@$kind $out, extend mp.items.$iter().filter_map(|item| match item {
                    MapProjectionItem::KeyValue(_, v) => Some(v),
                    MapProjectionItem::AllProperties
                    | MapProjectionItem::Property(_)
                    | MapProjectionItem::Variable(_) => None,
                }));
            }
            Expr::ExistsSubquery(s) => {
                edge_table!(@$kind $out, subquery $edge, $($r)+ s.statement)
            }
            Expr::CountSubquery(s) => {
                edge_table!(@$kind $out, subquery $edge, $($r)+ s.statement)
            }
            Expr::CollectSubquery(s) => {
                edge_table!(@$kind $out, subquery $edge, $($r)+ s.statement)
            }
            Expr::ShortestPath(s) => edge_table!(@$kind $out, pattern $edge, $($r)+ s.pattern),
            Expr::PatternPredicate(p) => {
                edge_table!(@$kind $out, pattern $edge, $($r)+ p.pattern)
            }
        }
    };
    // Expression-children family: only expression edges are recorded.
    (@expr $out:ident, push $e:expr) => { $out.push($e) };
    (@expr $out:ident, extend $e:expr) => { $out.extend($e) };
    (@expr $out:ident, pattern $edge:ident, $e:expr) => {{ let _ = $e; }};
    (@expr $out:ident, label $edge:ident, $e:expr) => {{ let _ = $e; }};
    (@expr $out:ident, subquery $edge:ident, $e:expr) => {{ let _ = $e; }};
    // Structural family: only the non-expression edges are recorded.
    (@structural $out:ident, push $e:expr) => {{ let _ = $e; }};
    (@structural $out:ident, extend $e:expr) => {{ let _ = $e; }};
    (@structural $out:ident, pattern $edge:ident, $e:expr) => { $out.push($edge::Pattern($e)) };
    (@structural $out:ident, label $edge:ident, $e:expr) => { $out.push($edge::Label($e)) };
    (@structural $out:ident, subquery $edge:ident, $e:expr) => {
        $out.push($edge::Subquery($e))
    };
}

impl Expr {
    /// The expression-typed direct children of this node, in source order:
    /// operands, arguments, list/map items, CASE arms, comprehension and
    /// reduce bodies, subscript parts, interpolation embeds. The one
    /// enumeration of the model's expression->expression edges, so a new
    /// variant costs one edit here instead of one per walker. Patterns, label
    /// expressions and subquery statements come from
    /// [`Self::structural_edges`]. The underlying match is exhaustive (no
    /// catch-all), so a later variant fails compilation.
    pub fn children(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        edge_table!(self, expr, [&], iter, StructuralEdge, out);
        out
    }

    /// Mutable twin of [`Self::children`], generated from the same table.
    pub fn children_mut(&mut self) -> Vec<&mut Expr> {
        let mut out = Vec::new();
        edge_table!(self, expr, [&mut], iter_mut, StructuralEdgeMut, out);
        out
    }

    /// The non-expression children of this node: the pattern of a pattern
    /// comprehension / predicate / `shortestPath()`, a subquery's statement,
    /// a `HasLabel`'s label expression. Generated from the same exhaustive
    /// table as [`Self::children`], so a variant carrying one of these
    /// cannot be dropped by a walker. A walker visits a pattern edge before
    /// the expression children and a label edge after them.
    pub fn structural_edges(&self) -> Vec<StructuralEdge<'_>> {
        let mut out = Vec::new();
        edge_table!(self, structural, [&], iter, StructuralEdge, out);
        out
    }

    /// Mutable twin of [`Self::structural_edges`].
    pub fn structural_edges_mut(&mut self) -> Vec<StructuralEdgeMut<'_>> {
        let mut out = Vec::new();
        edge_table!(self, structural, [&mut], iter_mut, StructuralEdgeMut, out);
        out
    }
}
