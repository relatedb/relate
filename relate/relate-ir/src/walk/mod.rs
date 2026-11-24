//! Shared expression walkers over the planner IR.
//!
//! Every expression walker is a fold over [`Expr::children`], so variant
//! coverage can no longer drift between the planner and the executor.
//! [`walk_scope_children`] is the one scope model: it reports each child
//! together with the identifiers its variant binds ([`ChildScope`]), and
//! the scope-aware folds ([`expr_vars`], [`references_any`], the planner's
//! aggregation leaf walks) ride it. The pattern/subquery variants carry
//! their own structure (`PathPattern` / `Query`), which `children()`
//! deliberately does not flatten; each fold states how it treats them.
//!
//! The clause-tree counterpart ([`crate::walk_query`]) descends into nested
//! queries (`CALL { … }`, `WHEN` branches, `FOREACH` bodies, `NEXT` stages
//! and the pattern/subquery variants `children()` skips) with exhaustive
//! matches, so a new clause or expression variant fails compilation there
//! instead of silently skipping a subtree.
//!
//! The name-collection folds live in [`vars`](self::vars); this module
//! keeps the `Expr` traversal core and the scope model.

use relate_names::SmallName;

use crate::{Expr, LabelExpr, MapProjectionItem};

mod vars;

// The name-collection family's one path out of the private `vars`
// module: the crate re-export in `lib.rs` reads these names from here,
// so the public spelling (`relate_ir::expr_vars` &c.) never changed.
pub use vars::{
    correlated_reads, exists_outer_vars, expr_row_vars, expr_vars, label_expr_vars,
    path_pattern_vars, references_any,
};

impl Expr {
    /// Direct child expressions, one canonical callback traversal.
    ///
    /// Uniform traversal used by every walker. Label expressions contribute
    /// the expressions inside their dynamic (`$(…)`) forms. Pattern-bearing
    /// variants (`PatternPred`, `PatternComp`, `ShortestPath`) and subquery
    /// variants (`ExistsSubquery`, `CountSubquery`, `CollectSubquery`)
    /// introduce their own scope and structure, so they report no children;
    /// walkers that need their internals (e.g. [`expr_vars`]) handle those
    /// variants explicitly.
    ///
    /// This is the one spelling of the child enumeration:
    /// [`children`](Expr::children) is its collecting form (it allocates a
    /// `Vec` per node — walkers on a hot path ride the callback instead),
    /// and the rebuilt-expression counterpart is
    /// [`map_children`](Expr::map_children).
    pub fn each_child<'a>(&'a self, visit: &mut impl FnMut(&'a Expr)) {
        match self {
            Expr::RuntimeValue(_)
            | Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Parameter(_)
            | Expr::Variable(_)
            | Expr::Star
            | Expr::CountStar
            | Expr::DistanceMetric(_) => {}
            Expr::Property { expr, .. }
            | Expr::Unary { expr, .. }
            | Expr::IsType { expr, .. }
            | Expr::IsNormalized { expr, .. } => visit(expr.as_ref()),
            Expr::ChronoCompare { left, right, .. } => {
                visit(left.as_ref());
                visit(right.as_ref());
            }
            Expr::LabelPredicate {
                expr, label_expr, ..
            } => {
                visit(expr.as_ref());
                each_label_expr_child(label_expr, visit);
            }
            Expr::DynamicProperty { expr, key } => {
                visit(expr.as_ref());
                visit(key.as_ref());
            }
            Expr::Map(items) => {
                for (_, value) in items {
                    visit(value);
                }
            }
            Expr::List(items)
            | Expr::PathConstruct { elements: items }
            | Expr::Function { args: items, .. } => {
                for item in items {
                    visit(item);
                }
            }
            Expr::MapComp {
                source,
                predicate,
                key_expr,
                value_expr,
                ..
            } => {
                visit(source.as_ref());
                if let Some(p) = predicate {
                    visit(p.as_ref());
                }
                visit(key_expr.as_ref());
                visit(value_expr.as_ref());
            }
            Expr::Binary { left, right, .. } => {
                visit(left.as_ref());
                visit(right.as_ref());
            }
            Expr::Case {
                when_then,
                else_expr,
            } => {
                for (when, then) in when_then {
                    visit(when);
                    visit(then);
                }
                if let Some(else_expr) = else_expr {
                    visit(else_expr);
                }
            }
            Expr::Subscript { expr, index } => {
                visit(expr.as_ref());
                visit(index.as_ref());
            }
            Expr::Slice { expr, start, end } => {
                visit(expr.as_ref());
                if let Some(start) = start {
                    visit(start);
                }
                if let Some(end) = end {
                    visit(end);
                }
            }
            Expr::ListComp {
                list,
                predicate,
                projection,
                ..
            } => {
                visit(list.as_ref());
                if let Some(predicate) = predicate {
                    visit(predicate);
                }
                visit(projection.as_ref());
            }
            Expr::Quantifier {
                list, predicate, ..
            } => {
                visit(list.as_ref());
                visit(predicate.as_ref());
            }
            Expr::Reduce {
                initial,
                list,
                expression,
                ..
            } => {
                visit(initial.as_ref());
                visit(list.as_ref());
                visit(expression.as_ref());
            }
            Expr::AllReduce {
                initial,
                list,
                expression,
                predicate,
                ..
            } => {
                visit(initial.as_ref());
                visit(list.as_ref());
                visit(expression.as_ref());
                visit(predicate.as_ref());
            }
            Expr::MapProjection { expr, items } => {
                visit(expr.as_ref());
                for item in items {
                    if let MapProjectionItem::KeyValue { value, .. } = item {
                        visit(value);
                    }
                }
            }
            Expr::PatternPred { .. }
            | Expr::PatternComp { .. }
            | Expr::ShortestPath { .. }
            | Expr::ExistsSubquery { .. }
            | Expr::CountSubquery { .. }
            | Expr::CollectSubquery { .. } => {}
        }
    }

    /// [`each_child`](Expr::each_child)'s collecting form: the same
    /// traversal rendered into a fresh `Vec`. Allocation-free walkers ride
    /// the callback directly; this form serves the callers that genuinely
    /// need an owned list (aggregating into another collection, indexing
    /// by position).
    pub fn children(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        self.each_child(&mut |child| out.push(child));
        out
    }

    /// [`each_child`](Expr::each_child)'s fallible form: the first child
    /// whose visitor errors stops the walk, matching a `?`-loop over
    /// `children()`'s order — children after the first error are not
    /// visited (the first error is the answer either way, and a caller
    /// accumulating per-child diagnostics must not rely on the rest).
    pub fn try_each_child<'a, E>(
        &'a self,
        visit: &mut impl FnMut(&'a Expr) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let mut err = Ok(());
        self.each_child(&mut |child| {
            if err.is_ok() {
                err = visit(child);
            }
        });
        err
    }

    /// Rebuild the expression with every direct child (the same set
    /// [`children`](Expr::children) reports) replaced by `f`'s result.
    /// Scope-bearing variants (`PatternPred`, `PatternComp`,
    /// `ShortestPath`, the subquery forms) carry their own structure and
    /// are returned unchanged, exactly matching `children()`'s skip set —
    /// the exhaustive match here is the rebuild counterpart of that walk,
    /// so a walker recurses over every traversable subtree instead of a
    /// hand-listed few variants.
    pub fn map_children<E>(
        &self,
        f: &mut impl FnMut(&Expr) -> std::result::Result<Expr, E>,
    ) -> std::result::Result<Expr, E> {
        Ok(match self {
            Expr::RuntimeValue(_)
            | Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Parameter(_)
            | Expr::Variable(_)
            | Expr::Star
            | Expr::CountStar
            | Expr::DistanceMetric(_) => self.clone(),
            Expr::Property { expr, key, key_id } => Expr::Property {
                key_id: *key_id,
                expr: Box::new(f(expr)?),
                key: key.clone(),
            },
            Expr::DynamicProperty { expr, key } => Expr::DynamicProperty {
                expr: Box::new(f(expr)?),
                key: Box::new(f(key)?),
            },
            Expr::LabelPredicate {
                expr,
                label_expr,
                strict,
            } => Expr::LabelPredicate {
                expr: Box::new(f(expr)?),
                label_expr: map_label_expr_children(label_expr, f)?,
                strict: *strict,
            },
            Expr::Map(items) => Expr::Map(
                items
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), f(value)?)))
                    .collect::<std::result::Result<_, E>>()?,
            ),
            Expr::List(items) => Expr::List(map_expr_children(items, f)?),
            Expr::PathConstruct { elements } => Expr::PathConstruct {
                elements: map_expr_children(elements, f)?,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op: *op,
                expr: Box::new(f(expr)?),
            },
            Expr::Binary { op, left, right } => Expr::Binary {
                op: *op,
                left: Box::new(f(left)?),
                right: Box::new(f(right)?),
            },
            Expr::Function {
                name,
                distinct,
                args,
            } => Expr::Function {
                name: name.clone(),
                distinct: *distinct,
                args: map_expr_children(args, f)?,
            },
            Expr::Case {
                when_then,
                else_expr,
            } => Expr::Case {
                when_then: when_then
                    .iter()
                    .map(|(when, then)| Ok((f(when)?, f(then)?)))
                    .collect::<std::result::Result<_, E>>()?,
                else_expr: map_opt_child(else_expr.as_deref(), f)?,
            },
            Expr::Subscript { expr, index } => Expr::Subscript {
                expr: Box::new(f(expr)?),
                index: Box::new(f(index)?),
            },
            Expr::Slice { expr, start, end } => Expr::Slice {
                expr: Box::new(f(expr)?),
                start: map_opt_child(start.as_deref(), f)?,
                end: map_opt_child(end.as_deref(), f)?,
            },
            Expr::ListComp {
                ident,
                list,
                predicate,
                projection,
            } => Expr::ListComp {
                ident: ident.clone(),
                list: Box::new(f(list)?),
                predicate: map_opt_child(predicate.as_deref(), f)?,
                projection: Box::new(f(projection)?),
            },
            Expr::MapComp {
                key_ident,
                value_ident,
                source,
                predicate,
                key_expr,
                value_expr,
            } => Expr::MapComp {
                key_ident: key_ident.clone(),
                value_ident: value_ident.clone(),
                source: Box::new(f(source)?),
                predicate: map_opt_child(predicate.as_deref(), f)?,
                key_expr: Box::new(f(key_expr)?),
                value_expr: Box::new(f(value_expr)?),
            },
            Expr::Quantifier {
                kind,
                ident,
                list,
                predicate,
            } => Expr::Quantifier {
                kind: *kind,
                ident: ident.clone(),
                list: Box::new(f(list)?),
                predicate: Box::new(f(predicate)?),
            },
            Expr::Reduce {
                accumulator,
                initial,
                ident,
                list,
                expression,
            } => Expr::Reduce {
                accumulator: accumulator.clone(),
                initial: Box::new(f(initial)?),
                ident: ident.clone(),
                list: Box::new(f(list)?),
                expression: Box::new(f(expression)?),
            },
            Expr::AllReduce {
                accumulator,
                initial,
                ident,
                list,
                expression,
                predicate,
            } => Expr::AllReduce {
                accumulator: accumulator.clone(),
                initial: Box::new(f(initial)?),
                ident: ident.clone(),
                list: Box::new(f(list)?),
                expression: Box::new(f(expression)?),
                predicate: Box::new(f(predicate)?),
            },
            Expr::IsType {
                expr,
                type_expr,
                negated,
            } => Expr::IsType {
                expr: Box::new(f(expr)?),
                type_expr: type_expr.clone(),
                negated: *negated,
            },
            Expr::IsNormalized {
                expr,
                form,
                negated,
            } => Expr::IsNormalized {
                expr: Box::new(f(expr)?),
                form: form.clone(),
                negated: *negated,
            },
            Expr::ChronoCompare {
                left,
                op,
                right,
                negated,
            } => Expr::ChronoCompare {
                left: Box::new(f(left)?),
                op: *op,
                right: Box::new(f(right)?),
                negated: *negated,
            },
            Expr::MapProjection { expr, items } => Expr::MapProjection {
                expr: Box::new(f(expr)?),
                items: items
                    .iter()
                    .map(|item| match item {
                        MapProjectionItem::KeyValue { key, value } => {
                            Ok(MapProjectionItem::KeyValue {
                                key: key.clone(),
                                value: f(value)?,
                            })
                        }
                        other => Ok(other.clone()),
                    })
                    .collect::<std::result::Result<_, E>>()?,
            },
            // Scope-bearing variants: `children()` deliberately reports no
            // children for these, so the rebuild keeps them intact.
            Expr::PatternPred { .. }
            | Expr::PatternComp { .. }
            | Expr::ShortestPath { .. }
            | Expr::ExistsSubquery { .. }
            | Expr::CountSubquery { .. }
            | Expr::CollectSubquery { .. } => self.clone(),
        })
    }

    /// Whether the variant introduces its own scope/structure that
    /// [`children`](Expr::children) deliberately does not flatten: pattern
    /// predicates/comprehensions, `shortestPath`, and the
    /// `EXISTS`/`COUNT`/`COLLECT` subquery forms. This is the one
    /// vocabulary for that class — `children()`'s skip set is the
    /// compile-forced enumeration; walkers that must refuse or specially
    /// handle these variants consult this predicate instead of re-listing
    /// them. A new `Expr` variant fails compilation in `children()` first,
    /// and the `scope_bearing_variants_match_children_skip_set` test keeps
    /// the two aligned.
    pub fn is_scope_bearing(&self) -> bool {
        // The subquery half derives from the one accessor
        // ([`subquery_body`](Expr::subquery_body)) instead of re-listing
        // its variants here, so the two vocabularies cannot drift.
        matches!(
            self,
            Expr::PatternPred { .. } | Expr::PatternComp { .. } | Expr::ShortestPath { .. }
        ) || self.subquery_body().is_some()
    }

    /// Whether the variant binds its own identifiers (a comprehension's or
    /// quantifier's iteration variable, a map comprehension's key/value
    /// idents, a reduce's accumulator and iteration variable). Their bodies
    /// evaluate once per element rather than once per row, so an aggregate
    /// has no group to fold there. The one vocabulary for that class:
    /// walkers that must treat the bodies specially (grouping-key leaves,
    /// nested aggregates) consult this predicate instead of re-listing the
    /// variants. The `every_identifier_binding_variant_opens_an_element_scope`
    /// and `walk_scope_children_classifies_every_binding_variant` tests (in
    /// `walk/tests.rs`) keep this predicate and [`walk_scope_children`]'s
    /// arms aligned — a new binding variant must grow both.
    pub fn binds_identifiers(&self) -> bool {
        matches!(
            self,
            Expr::ListComp { .. }
                | Expr::MapComp { .. }
                | Expr::Quantifier { .. }
                | Expr::Reduce { .. }
                | Expr::AllReduce { .. }
        )
    }

    /// Pre-order traversal: `f` sees `self`, then every descendant.
    pub fn visit(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        self.each_child(&mut |child| child.visit(&mut *f));
    }

    /// Whether `pred` holds for this expression or any descendant expression.
    pub fn any(&self, pred: &mut impl FnMut(&Expr) -> bool) -> bool {
        if pred(self) {
            return true;
        }
        let mut hit = false;
        self.each_child(&mut |child| {
            if !hit && child.any(&mut *pred) {
                hit = true;
            }
        });
        hit
    }

    /// Whether `self` contains `needle` as itself or as a subexpression —
    /// an equality test over the crate's one deep pre-order descent
    /// (`walk_query`'s `walk_expr`), which reaches the pattern- and
    /// subquery-bearing variants [`each_child`](Expr::each_child) skips
    /// because they carry their own structure (`COUNT { RETURN a.p + 1 }`
    /// holds the read of `a`). No private second descent: the reach and
    /// its scope model live in the walker every deep reader rides.
    pub fn contains_deep(&self, needle: &Expr) -> bool {
        let mut hit = false;
        crate::walk_query::walk_expr(
            self,
            crate::walk_query::ExprRole::Read,
            &mut |_, _| {},
            &mut |expr, _| hit |= expr == needle,
        );
        hit
    }
}

/// Rebuild a child-expression list with `f`.
/// Remap one optional child, boxing the result (the `Option` counterpart
/// of the `Box::new(f(child)?)` arms in [`Expr::map_children`]).
fn map_opt_child<E>(
    child: Option<&Expr>,
    f: &mut impl FnMut(&Expr) -> std::result::Result<Expr, E>,
) -> std::result::Result<Option<Box<Expr>>, E> {
    child.map(|c| f(c).map(Box::new)).transpose()
}

fn map_expr_children<E>(
    items: &[Expr],
    f: &mut impl FnMut(&Expr) -> std::result::Result<Expr, E>,
) -> std::result::Result<Vec<Expr>, E> {
    items.iter().map(f).collect()
}

fn map_label_expr_children<E>(
    label_expr: &LabelExpr,
    f: &mut impl FnMut(&Expr) -> std::result::Result<Expr, E>,
) -> std::result::Result<LabelExpr, E> {
    Ok(match label_expr {
        LabelExpr::Dynamic { expr, quantifier } => LabelExpr::Dynamic {
            expr: Box::new(f(expr)?),
            quantifier: *quantifier,
        },
        LabelExpr::Not(inner) => LabelExpr::Not(Box::new(map_label_expr_children(inner, f)?)),
        LabelExpr::And(items) => LabelExpr::And(
            items
                .iter()
                .map(|item| map_label_expr_children(item, f))
                .collect::<std::result::Result<_, E>>()?,
        ),
        LabelExpr::Or(items) => LabelExpr::Or(
            items
                .iter()
                .map(|item| map_label_expr_children(item, f))
                .collect::<std::result::Result<_, E>>()?,
        ),
        LabelExpr::Label(_) | LabelExpr::Wildcard => label_expr.clone(),
    })
}

/// The scope class of a child expression reported by
/// [`walk_scope_children`].
pub enum ChildScope {
    /// Evaluated once per row, in the enclosing scope.
    Enclosing,
    /// A per-element body (a comprehension's predicate/projection, a
    /// quantifier's predicate, a reduce's expression/predicate) that
    /// evaluates once per element with these identifiers bound. The bound
    /// identifiers ride the IR's own name representation (ADR-0509 §1) —
    /// copies, never re-classified text.
    Element(Vec<SmallName>),
}

/// Whether `name` is an unbound read — the one test the free-variable
/// walkers' bare-variable and map-projection shorthand arms share (the
/// `bound` set is [`ChildScope::Element`]'s bound names, the identifiers
/// the traversal is inside), so the two arms cannot answer the same
/// question differently.
fn reads(name: &SmallName, bound: &[SmallName]) -> bool {
    !bound.iter().any(|b| b == name)
}

/// The variable an expression is rooted in, descending only through the
/// access chain — a bare variable, a property access (`n.k`), a dynamic
/// property (`n[k]`) and a subscript (`n[x][y]`) — or `None` for any other
/// shape (a literal, a function call, a `CASE`). The one walk of the
/// target-shape fact the write-target class decision reads (REL8-1813):
/// a variable-rooted SET/REMOVE target is the runtime type-error shape,
/// every other target the pinned compile-time one.
pub fn root_variable(expr: &Expr) -> Option<&SmallName> {
    match expr {
        Expr::Variable(name) => Some(name),
        Expr::Property { expr, .. }
        | Expr::DynamicProperty { expr, .. }
        | Expr::Subscript { expr, .. } => root_variable(expr),
        _ => None,
    }
}

/// Visit `expr`'s children with their scope class.
///
/// [`Expr::children`] is the descent for every variant outside
/// [`Expr::binds_identifiers`]; the arms below are the one place that knows,
/// per identifier-binding variant, which child is the source (evaluated once
/// per row in the enclosing scope) and which are the bodies evaluated once
/// per element with the variant's identifiers bound. Every scope-aware
/// walker ([`expr_vars`], [`references_any`], the planner's aggregation
/// leaf walks) classifies through this helper instead of re-listing the
/// binding variants.
///
/// The `debug_assert!` in the fallback arm is the drift alarm: a variant
/// added to [`Expr::binds_identifiers`] without an arm here would otherwise
/// have its per-element bodies walked as per-row reads.
pub fn walk_scope_children<'a>(expr: &'a Expr, f: &mut impl FnMut(&'a Expr, ChildScope)) {
    match expr {
        Expr::ListComp {
            ident,
            list,
            predicate,
            projection,
        } => {
            f(list, ChildScope::Enclosing);
            let bound = vec![ident.clone()];
            if let Some(predicate) = predicate {
                f(predicate, ChildScope::Element(bound.clone()));
            }
            f(projection, ChildScope::Element(bound));
        }
        Expr::MapComp {
            key_ident,
            value_ident,
            source,
            predicate,
            key_expr,
            value_expr,
        } => {
            f(source, ChildScope::Enclosing);
            let mut bound = Vec::new();
            if let Some(key_ident) = key_ident {
                bound.push(key_ident.clone());
            }
            bound.push(value_ident.clone());
            if let Some(predicate) = predicate {
                f(predicate, ChildScope::Element(bound.clone()));
            }
            f(key_expr, ChildScope::Element(bound.clone()));
            f(value_expr, ChildScope::Element(bound));
        }
        Expr::Quantifier {
            ident,
            list,
            predicate,
            ..
        } => {
            f(list, ChildScope::Enclosing);
            f(predicate, ChildScope::Element(vec![ident.clone()]));
        }
        Expr::Reduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
        } => {
            f(initial, ChildScope::Enclosing);
            f(list, ChildScope::Enclosing);
            f(
                expression,
                ChildScope::Element(vec![accumulator.clone(), ident.clone()]),
            );
        }
        Expr::AllReduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
            predicate,
        } => {
            f(initial, ChildScope::Enclosing);
            f(list, ChildScope::Enclosing);
            let bound = vec![accumulator.clone(), ident.clone()];
            f(expression, ChildScope::Element(bound.clone()));
            f(predicate, ChildScope::Element(bound));
        }
        other => {
            debug_assert!(
                !other.binds_identifiers(),
                "identifier-binding variant missing a walk_scope_children arm: {other:?}"
            );
            other.each_child(&mut |child| f(child, ChildScope::Enclosing));
        }
    }
}

/// The one enumeration of a label expression's dynamic (`$(…)`) child
/// expressions — the `LabelExpr` counterpart of
/// [`Expr::each_child`](Expr::each_child)'s canonical callback traversal.
pub(crate) fn each_label_expr_child<'a>(
    label_expr: &'a LabelExpr,
    visit: &mut impl FnMut(&'a Expr),
) {
    match label_expr {
        LabelExpr::Dynamic { expr, .. } => visit(expr),
        LabelExpr::Not(inner) => each_label_expr_child(inner, visit),
        LabelExpr::And(items) | LabelExpr::Or(items) => {
            for item in items {
                each_label_expr_child(item, visit);
            }
        }
        LabelExpr::Label(_) | LabelExpr::Wildcard => {}
    }
}

#[cfg(test)]
mod tests;
