//! The scope-aware expression walk: the visitor trait, the shared binder
//! stack, and the walker — split from the collector (the LABS-311 size
//! gate) as one self-contained family.

use relate_error::EngineError;
use relate_names::SmallName;
use relate_parser::{Expr, ExprEdge, Statement};

use super::{distance_metric_arg_index, pattern_vars};
use crate::ast::tree::nesting_depth_error;
use crate::ast::MAX_NESTING_DEPTH;
use crate::ident_hash::IdentHashMap;

/// The scope-aware expression visit: the consumer owns what a *read* and a
/// *binder* mean for its rule — which reads count, what a binder shadows,
/// how a nested statement's own scope is modeled — while this module owns
/// the one table of which construct binds which name and where each scope
/// begins and ends. The scope-tracking validators implement the visit and
/// ride [`walk_expr_scoped`] instead of re-listing the expression child
/// edges beside the walker family (ADR-0513's 42I58 collector is the
/// first consumer).
pub(crate) trait ScopedExprVisitor {
    /// Pre-order visit of an expression node — the visit itself, so every
    /// consumer implements it. An `Err` stops the whole walk and
    /// propagates.
    fn enter_expr(&mut self, expr: &Expr) -> Result<(), EngineError>;
    /// A name read outside expression position: a pattern variable of a
    /// pattern predicate, `shortestPath()` or pattern comprehension, or a
    /// map projection's bare variable selector.
    fn read_name(&mut self, _name: &SmallName) -> Result<(), EngineError> {
        Ok(())
    }
    /// One binder's scope begins: the name is in scope until the matching
    /// [`unbind_name`](Self::unbind_name).
    fn bind_name(&mut self, _name: &SmallName) -> Result<(), EngineError> {
        Ok(())
    }
    /// The most recently bound, still-open scope ends. The event stream is
    /// well-nested: this is always the youngest open binder.
    fn unbind_name(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
    /// An `EXISTS`/`COUNT`/`COLLECT { … }` body. The consumer owns the
    /// statement-level scope model; the walker does not descend further.
    fn subquery_body(&mut self, _statement: &Statement) -> Result<(), EngineError> {
        Ok(())
    }
}

/// The binder bookkeeping every shadow-aware visitor shares: the names the
/// walker's [`bind_name`](ScopedExprVisitor::bind_name) /
/// [`unbind_name`](ScopedExprVisitor::unbind_name) events have opened,
/// innermost last, with the O(1) membership query — the count-map shape,
/// because a rule that consults the stack per name read over a linear
/// scan is the admission-time quadratic (#741). One home for the
/// well-nesting invariant the walker's event stream guarantees.
#[derive(Default)]
pub(crate) struct BinderStack {
    /// Binder names in scope, innermost last — the frame stack `bind` and
    /// `unbind_to` own; the map below is its index, never edited beside
    /// them.
    shadowed: Vec<SmallName>,
    /// Binder name -> number of live frames binding it (nested binders of
    /// one name stack up).
    live: IdentHashMap<SmallName, usize>,
}

impl BinderStack {
    /// Bind `name` in the current frame.
    pub(crate) fn bind(&mut self, name: &SmallName) {
        self.shadowed.push(name.clone());
        *self.live.entry(name.clone()).or_insert(0) += 1;
    }

    /// Unbind back to the frame boundary `len` (a previously captured
    /// [`len`](Self::len)), reversing exactly the binds pushed above it.
    /// A boundary above the current depth is a stale capture, not a
    /// no-op: the walker's binder events are well-nested, so a captured
    /// length never exceeds the depth it was taken at.
    pub(crate) fn unbind_to(&mut self, len: usize) {
        if len > self.shadowed.len() {
            panic!("a captured frame boundary never exceeds the stack depth");
        }
        while self.shadowed.len() > len {
            self.unbind();
        }
    }

    /// The most recently bound, still-open scope ends — the walker's
    /// well-nested unbind event.
    pub(crate) fn unbind(&mut self) {
        let name = self
            .shadowed
            .pop()
            .expect("the walker's binder events are well-nested");
        let count = self
            .live
            .get_mut(&name)
            .expect("every stacked binder is live");
        if *count == 1 {
            self.live.remove(&name);
        } else {
            *count -= 1;
        }
    }

    /// The current frame boundary, for a later
    /// [`unbind_to`](Self::unbind_to).
    pub(crate) fn len(&self) -> usize {
        self.shadowed.len()
    }

    /// Whether a binder in scope shadows `name`.
    pub(crate) fn shadows(&self, name: &SmallName) -> bool {
        self.live.contains_key(name)
    }
}

/// Walk one expression with [`ScopedExprVisitor`] semantics: every
/// expression child edge the plain expression walker reaches, plus the
/// scope-introducing constructs' binder events — a comprehension's binders
/// scope their WHERE and projection (the list is outside), a map
/// comprehension's key/value binders scope theirs, `reduce`/`allReduce`
/// scope their body (and `allReduce`'s predicate), a quantifier's binder
/// scopes its predicate, and a pattern comprehension's pattern names are
/// read and then bound around its WHERE and projection. A subquery body is
/// handed to [`subquery_body`](ScopedExprVisitor::subquery_body) instead
/// of being descended. Three edges are deliberately not reads of an
/// entity, so they are not walked: the distance-metric keyword argument of
/// `vector_distance`/`vector_norm` (the one
/// [`distance_metric_arg_index`] table every variable reach consults), a
/// property's dynamic-key expression, and `hasLabel`'s label expression.
/// A pattern predicate's and `shortestPath()`'s embedded expressions are
/// likewise the pattern walk's own reach — only their pattern variables
/// are read here. Nesting is capped at [`MAX_NESTING_DEPTH`] from the
/// walk's root with the one shared
/// [`nesting_depth_error`](super::tree::nesting_depth_error); a nested
/// statement's clause roots each start a fresh walk, so a body's
/// expressions restart the count exactly as the per-expression walks
/// always did.
pub(crate) fn walk_expr_scoped(
    expr: &Expr,
    visitor: &mut dyn ScopedExprVisitor,
) -> Result<(), EngineError> {
    walk_expr_scoped_at(expr, 0, visitor)
}

/// The binder scope a node opens over its expression children: the first
/// `outer` expression edges (the source list, the reduce init and list) are
/// outside it, every later one inside it; `binders` open in order and close
/// in reverse once the children are done. The one table of which construct
/// binds which name — everything else is a plain child edge of the parser's
/// [`Expr::try_for_each_edge`].
struct BinderScope<'a> {
    outer: usize,
    binders: Vec<&'a SmallName>,
}

fn binder_scope(expr: &Expr) -> Option<BinderScope<'_>> {
    match expr {
        Expr::ListComprehension(c) => Some(BinderScope {
            outer: 1,
            binders: vec![&c.variable],
        }),
        // The value binder always, the key binder beside it on a map
        // source.
        Expr::MapComprehension(c) => Some(BinderScope {
            outer: 1,
            binders: c
                .key_var
                .as_ref()
                .into_iter()
                .chain(std::iter::once(&c.value_var))
                .collect(),
        }),
        Expr::Reduce(r) => Some(BinderScope {
            outer: 2,
            binders: vec![&r.accumulator, &r.variable],
        }),
        Expr::AllReduce(r) => Some(BinderScope {
            outer: 2,
            binders: vec![&r.accumulator, &r.variable],
        }),
        Expr::Quantifier(q) => Some(BinderScope {
            outer: 1,
            binders: vec![&q.variable],
        }),
        // The pattern edge comes first in the table: its names are read
        // there, then bound around the WHERE and projection.
        Expr::PatternComprehension(c) => Some(BinderScope {
            outer: 0,
            binders: pattern_vars(std::slice::from_ref(&c.pattern)),
        }),
        _ => None,
    }
}

/// The expression-child edge (by position among the node's expression
/// edges) that is deliberately not a read of an entity: a property's
/// dynamic-key expression, and the distance-metric keyword argument of
/// `vector_distance`/`vector_norm`.
fn skipped_expr_edge(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Property(_) => Some(1),
        Expr::FunctionCall(call) => call.name.with_str(distance_metric_arg_index),
        _ => None,
    }
}

fn walk_expr_scoped_at(
    expr: &Expr,
    depth: usize,
    visitor: &mut dyn ScopedExprVisitor,
) -> Result<(), EngineError> {
    use relate_parser::MapProjectionItem;
    if depth > MAX_NESTING_DEPTH {
        return Err(nesting_depth_error());
    }
    visitor.enter_expr(expr)?;
    let scope = binder_scope(expr);
    let skip = skipped_expr_edge(expr);
    let mut opened = false;
    let mut expr_edges = 0usize;
    // Open the binders once the node's outer edges are behind us.
    let open =
        |visitor: &mut dyn ScopedExprVisitor, opened: &mut bool| -> Result<(), EngineError> {
            if !*opened {
                *opened = true;
                if let Some(scope) = &scope {
                    for name in &scope.binders {
                        visitor.bind_name(name)?;
                    }
                }
            }
            Ok(())
        };
    expr.try_for_each_edge(|edge| match edge {
        ExprEdge::Expr(child) => {
            let index = expr_edges;
            expr_edges += 1;
            if scope.as_ref().is_some_and(|s| index >= s.outer) {
                open(visitor, &mut opened)?;
            }
            if skip == Some(index) {
                return Ok(());
            }
            walk_expr_scoped_at(child, depth + 1, visitor)
        }
        // A pattern predicate's, `shortestPath()`'s and a pattern
        // comprehension's embedded expressions are the pattern walk's own
        // reach; only their pattern variables are reads here. A variable
        // that is already bound is a reference, not a fresh binding.
        ExprEdge::Pattern(pattern) => {
            for name in pattern_vars(std::slice::from_ref(pattern)) {
                visitor.read_name(name)?;
            }
            Ok(())
        }
        // A subquery body reads the enclosing scope: a name it mentions is
        // the outer entity, exactly as a `MATCH` on it inside the subquery
        // would be. The body's own scope model is the visitor's.
        ExprEdge::Subquery(statement) => visitor.subquery_body(statement),
        // The label expression is the dynamic-label half's own walk.
        ExprEdge::Label(_) => Ok(()),
    })?;
    if let Expr::MapProjection(projection) = expr {
        // A bare `Variable` selector pulls a same-named variable from the
        // surrounding scope, so it is a read of that variable.
        for item in &projection.items {
            if let MapProjectionItem::Variable(name) = item {
                visitor.read_name(name)?;
            }
        }
    }
    if let Some(scope) = &scope {
        open(visitor, &mut opened)?;
        for _ in &scope.binders {
            visitor.unbind_name()?;
        }
    }
    Ok(())
}
