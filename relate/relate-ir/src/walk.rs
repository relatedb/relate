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

use std::collections::HashSet;

use relate_names::SmallName;

use crate::{Clause, Expr, LabelExpr, MapProjectionItem, PathPattern, Query};

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
            Expr::List(items) | Expr::Function { args: items, .. } => {
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
                else_expr: match else_expr {
                    Some(expr) => Some(Box::new(f(expr)?)),
                    None => None,
                },
            },
            Expr::Subscript { expr, index } => Expr::Subscript {
                expr: Box::new(f(expr)?),
                index: Box::new(f(index)?),
            },
            Expr::Slice { expr, start, end } => Expr::Slice {
                expr: Box::new(f(expr)?),
                start: match start {
                    Some(start) => Some(Box::new(f(start)?)),
                    None => None,
                },
                end: match end {
                    Some(end) => Some(Box::new(f(end)?)),
                    None => None,
                },
            },
            Expr::ListComp {
                ident,
                list,
                predicate,
                projection,
            } => Expr::ListComp {
                ident: ident.clone(),
                list: Box::new(f(list)?),
                predicate: match predicate {
                    Some(predicate) => Some(Box::new(f(predicate)?)),
                    None => None,
                },
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
                predicate: match predicate {
                    Some(predicate) => Some(Box::new(f(predicate)?)),
                    None => None,
                },
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
}

/// Rebuild a child-expression list with `f`.
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

/// The names a map projection's shorthand items read (`{n, .name, k: v}` reads
/// `n`; `k: v` reads only what `v` reads). One policy both free-variable
/// walkers consult — the shorthand item is a name, not an expression, so it is
/// invisible to [`Expr::children`].
fn map_projection_shorthand_names(items: &[MapProjectionItem]) -> impl Iterator<Item = &SmallName> {
    items.iter().filter_map(|item| match item {
        MapProjectionItem::Variable { name } => Some(name),
        MapProjectionItem::KeyValue { .. }
        | MapProjectionItem::Property { .. }
        | MapProjectionItem::AllProperties => None,
    })
}

/// Whether `expr` references any of `names` from *its own* scope.
///
/// One fold over the scope model ([`walk_scope_children`]): a scope-bearing
/// variant ([`Expr::is_scope_bearing`]) is opaque (its body opens its own
/// scope), and an identifier a comprehension/quantifier/`reduce` binds
/// ([`Expr::binds_identifiers`]) is not a reference to an outer name — only
/// the variant's *source* expression is evaluated outside that binding.
/// The one predicate the CALL-subquery importing-WITH rule uses to decide
/// whether a projection reads the enclosing scope.
pub fn references_any(expr: &Expr, names: &HashSet<String>) -> bool {
    fn go(expr: &Expr, names: &HashSet<String>, bound: &mut Vec<SmallName>) -> bool {
        match expr {
            // The String-keyed read set is probed through the allocation-
            // free text bridge (with_str), never a per-lookup to_string.
            Expr::Variable(name) => {
                name.with_str(|text| names.contains(text)) && !bound.iter().any(|b| b == name)
            }
            other if other.is_scope_bearing() => false,
            // A map projection's shorthand items are reads the children walk
            // cannot see (`MapProjectionItem::Variable` is a name, not an
            // expression): test them through the one shared policy, then
            // descend the value expressions as usual.
            Expr::MapProjection { items, .. }
                if map_projection_shorthand_names(items).any(|name| {
                    name.with_str(|text| names.contains(text)) && !bound.iter().any(|b| b == name)
                }) =>
            {
                true
            }
            other => {
                let mut hit = false;
                walk_scope_children(other, &mut |child, scope| {
                    if hit {
                        return;
                    }
                    match scope {
                        ChildScope::Enclosing => hit = go(child, names, bound),
                        ChildScope::Element(bound_names) => {
                            let mark = bound.len();
                            bound.extend(bound_names);
                            hit = go(child, names, bound);
                            bound.truncate(mark);
                        }
                    }
                });
                hit
            }
        }
    }
    go(expr, names, &mut Vec::new())
}

/// Collect the variable names an expression references, comprehension- and
/// reduce-bound identifiers excluded.
///
/// For pattern variants the pattern's own variable names are included: the
/// executor's column analysis treats them as values the expression needs
/// from the surrounding row. Subquery variants contribute every variable
/// name mentioned in the subquery (an over-approximation of the outer
/// references, matching the historical executor behaviour).
///
/// This is [`references_any`]'s scope model over a collecting leaf policy
/// (the two documented differences: scope-bearing variants are descended
/// here rather than treated as opaque, and a map projection's shorthand
/// items count as reads).
pub fn expr_vars(expr: &Expr, out: &mut HashSet<String>) {
    fn go(expr: &Expr, bound: &mut Vec<SmallName>, out: &mut HashSet<String>) {
        // A subquery body contributes every name it mentions (the
        // documented over-approximation) instead of the generic child
        // descent — the one accessor decides which expressions open one.
        if let Some(query) = expr.subquery_body() {
            exists_outer_vars(query, out);
            return;
        }
        match expr {
            Expr::Variable(v) => {
                if !bound.iter().any(|b| b == v) {
                    // Map-bridge: the String-keyed variable sink stays until
                    // the typed expr_vars sink (LABS-936) lands; the text
                    // render happens once per collected name, at this feed.
                    out.insert(v.to_string());
                }
            }
            Expr::PatternPred { path } | Expr::ShortestPath { path, .. } => {
                path_var_names(path, out);
            }
            Expr::PatternComp {
                path,
                predicate,
                projection,
            } => {
                path_var_names(path, out);
                if let Some(predicate) = predicate {
                    go(predicate, bound, out);
                }
                go(projection, bound, out);
            }
            Expr::MapProjection { items, .. } => {
                for name in map_projection_shorthand_names(items) {
                    out.insert(name.to_string());
                }
                expr.each_child(&mut |child| go(child, bound, out));
            }
            other => walk_scope_children(other, &mut |child, scope| match scope {
                ChildScope::Enclosing => go(child, bound, out),
                ChildScope::Element(names) => {
                    let mark = bound.len();
                    bound.extend(names);
                    go(child, bound, out);
                    bound.truncate(mark);
                }
            }),
        }
    }
    go(expr, &mut Vec::new(), out);
}

/// Variables referenced by the dynamic (`$(…)`) parts of a label
/// expression: a fold over `each_label_expr_child`, the one enumeration
/// of the `LabelExpr` structure. The variant vocabulary is matched in
/// that helper alone, so a new `LabelExpr` variant fails compilation
/// there and is picked up here without a second match to keep aligned.
pub fn label_expr_vars(expr: &LabelExpr, out: &mut HashSet<String>) {
    each_label_expr_child(expr, &mut |child| expr_vars(child, out));
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

/// Node variables, then rel variables, of a path pattern's compact view
/// (`nodes`/`rels`) — the path variable excluded. Compact-view only:
/// lowering clears `nodes`/`rels` when an element is non-alternating or
/// quantified (see `walk_query`'s rule), so such a pattern yields nothing.
pub fn path_pattern_vars(path: &PathPattern) -> impl Iterator<Item = &SmallName> {
    path.nodes
        .iter()
        .filter_map(|n| n.variable.as_ref())
        .chain(path.rels.iter().filter_map(|r| r.variable.as_ref()))
}

fn path_var_names(path: &PathPattern, out: &mut HashSet<String>) {
    // Map-bridge: the String-keyed variable sink stays until the typed
    // expr_vars sink (LABS-936) lands; one text render per bound name.
    for name in path.path_names() {
        out.insert(name.to_string());
    }
    // Node and rel variables both come from the shared compact-view
    // iterator above; the payload loops below carry only the property
    // maps and the label/type expressions.
    out.extend(path_pattern_vars(path).map(SmallName::to_string));
    for n in &path.nodes {
        if let Some(p) = &n.properties {
            expr_vars(p, out);
        }
        if let Some(e) = &n.label_expr {
            label_expr_vars(e, out);
        }
    }
    for r in &path.rels {
        if let Some(p) = &r.properties {
            expr_vars(p, out);
        }
        if let Some(e) = &r.type_expr {
            label_expr_vars(e, out);
        }
    }
}

fn exists_outer_vars(query: &Query, out: &mut HashSet<String>) {
    let mut inner = HashSet::new();
    for part in &query.parts {
        for clause in &part.clauses {
            match clause {
                Clause::Match {
                    patterns,
                    where_clause,
                    ..
                } => {
                    for p in patterns {
                        path_var_names(p, &mut inner);
                    }
                    if let Some(w) = where_clause {
                        expr_vars(w, &mut inner);
                    }
                }
                Clause::Unwind { expression, .. } => {
                    expr_vars(expression, &mut inner);
                }
                Clause::With {
                    items,
                    order,
                    skip,
                    limit,
                    where_clause,
                    ..
                } => {
                    for i in items {
                        expr_vars(&i.expr, &mut inner);
                    }
                    for o in order {
                        expr_vars(&o.expr, &mut inner);
                    }
                    if let Some(s) = skip {
                        expr_vars(s, &mut inner);
                    }
                    if let Some(l) = limit {
                        expr_vars(l, &mut inner);
                    }
                    if let Some(w) = where_clause {
                        expr_vars(w, &mut inner);
                    }
                }
                Clause::Return {
                    items,
                    order,
                    skip,
                    limit,
                    ..
                } => {
                    for i in items {
                        expr_vars(&i.expr, &mut inner);
                    }
                    for o in order {
                        expr_vars(&o.expr, &mut inner);
                    }
                    if let Some(s) = skip {
                        expr_vars(s, &mut inner);
                    }
                    if let Some(l) = limit {
                        expr_vars(l, &mut inner);
                    }
                }
                Clause::Call { args, .. } => {
                    for a in args.args() {
                        expr_vars(a, &mut inner);
                    }
                }
                // Clause kinds whose expressions are not part of the
                // outer-reference scan (the historical over-approximation
                // documented on `expr_vars`). Enumerated explicitly rather
                // than through a `_` catch-all so a new `Clause` variant
                // fails compilation here instead of silently skipping its
                // variables.
                Clause::Catalog(_)
                | Clause::Create { .. }
                | Clause::Merge { .. }
                | Clause::Delete { .. }
                | Clause::Set { .. }
                | Clause::Remove { .. }
                | Clause::CreateIndex { .. }
                | Clause::DropIndex { .. }
                | Clause::ShowIndexes
                | Clause::Finish
                | Clause::LoadCsv { .. }
                | Clause::CallSubquery { .. }
                | Clause::Conditional { .. }
                | Clause::Foreach { .. }
                | Clause::Use { .. } => {}
            }
        }
    }
    out.extend(inner);
}

#[cfg(test)]
mod tests;
