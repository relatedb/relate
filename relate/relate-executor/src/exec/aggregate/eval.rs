//! The recursive aggregate-expression evaluator: aggregate calls read their
//! group's resolved accumulators, every non-aggregate leaf reads the seed row, and the transparent
//! combinators (map, list, comprehension, CASE, binary, …) recurse through
//! the same scope.

use std::cell::RefCell;
use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::{walk_scope_children, ChildScope, Expr};
use relate_names::SmallName;

use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::stream::FoldedGroup;

/// Where an aggregate call's value comes from: the group's resolved
/// accumulators, or — in the discovery pass that finds the call sites the
/// accumulators are built for — nowhere (the call is recorded and reads as
/// null).
#[derive(Clone, Copy)]
enum AggSource<'a, 'e> {
    Folded(&'a FoldedGroup<'e>),
    Discover(&'a RefCell<Vec<&'e Expr>>),
}

/// The two scopes one aggregation evaluation reads, bundled so they travel
/// together through the recursion: the source of every aggregate call's
/// value, and the `seed` row a non-aggregate leaf is asked about.
#[derive(Clone, Copy)]
pub(crate) struct AggScope<'a, 'e> {
    source: AggSource<'a, 'e>,
    seed: Option<&'a Row>,
}

impl<'a, 'e> AggScope<'a, 'e> {
    /// A projection item's scope: aggregate calls read the group's resolved
    /// values, non-aggregate leaves ask about `seed` — the group's first
    /// row, or (ADR-0453) an importing subquery's scope row for the phantom
    /// keyless group, whose imports are per-row constants present even
    /// when the body's rows are gone; `None` for an empty group elsewhere.
    pub(crate) fn of_group(folded: &'a FoldedGroup<'e>, seed: Option<&'a Row>) -> Self {
        Self {
            source: AggSource::Folded(folded),
            seed,
        }
    }

    /// An `ORDER BY` key's scope: aggregate calls still read the group, but
    /// every non-aggregate leaf asks about `row` — the projected output
    /// row, whose names the projection's aliases shadow.
    pub(crate) fn of_row(folded: &'a FoldedGroup<'e>, row: &'a Row) -> Self {
        Self {
            source: AggSource::Folded(folded),
            seed: Some(row),
        }
    }

    /// The discovery pass: aggregate calls are recorded into `found`.
    pub(super) fn discovering(found: &'a RefCell<Vec<&'e Expr>>) -> Self {
        Self {
            source: AggSource::Discover(found),
            seed: None,
        }
    }

    /// The value of the aggregate call at `call`, or its recording.
    fn aggregate_call(&self, call: &'e Expr) -> Result<Value> {
        match self.source {
            AggSource::Folded(folded) => folded.value(call),
            AggSource::Discover(found) => {
                found.borrow_mut().push(call);
                Ok(Value::Null)
            }
        }
    }

    /// Whether this is the discovery pass, whose combinators stop after
    /// their children are walked.
    fn is_discovery(&self) -> bool {
        matches!(self.source, AggSource::Discover(_))
    }
}

/// Bind `child` under its synthetic `name` and return the `Expr::Variable`
/// that reads it back — one pass over both sides, so the name stream the
/// combinator inserts and the names its rebuilt expression references
/// cannot drift apart.
fn bind_child<'e>(children: &mut Vec<(String, &'e Expr)>, name: String, child: &'e Expr) -> Expr {
    children.push((name.clone(), child));
    // The synthetic combinator name enters the IR's name domain here,
    // classified once at construction.
    Expr::Variable(SmallName::from_name(&name))
}

/// Bind every enclosing-scope child of `expr` under a synthetic
/// `__agg_c{i}` name (collected into `children`) and rebuild `expr` so
/// those positions read the synthetic variables — the one bind-and-rebuild
/// every compound agg arm rides. The IR's scope model
/// ([`walk_scope_children`]) is the whole classification: a
/// comprehension's source (and a `reduce`'s initial and list) is
/// enclosing — it folds the group here, bound under its synthetic name —
/// while the per-element bodies (a comprehension's predicate and
/// projection, a quantifier's predicate, a `reduce`'s expression and
/// predicate) stay the original subtrees, evaluated inside the rebuilt
/// form's own scope; every other variant's children are all enclosing.
/// The canonical rebuild ([`Expr::map_children`]) walks the same children
/// in the same order, so the name stream and the rebuild positions cannot
/// drift — the cursor's exhaustion is the invariant that pins it.
fn bind_enclosing_children<'e>(expr: &'e Expr, children: &mut Vec<(String, &'e Expr)>) -> Expr {
    let mut replacements: Vec<Expr> = Vec::new();
    walk_scope_children(expr, &mut |child, scope| match scope {
        ChildScope::Enclosing => replacements.push(bind_child(
            children,
            format!("__agg_c{}", replacements.len()),
            child,
        )),
        ChildScope::Element(_) => replacements.push(child.clone()),
    });
    let mut replacements = replacements.into_iter();
    expr.map_children::<std::convert::Infallible>(&mut |_| {
        Ok(replacements
            .next()
            .expect("map_children rebuilds exactly the children walk_scope_children visited"))
    })
    .expect("the rebuild is infallible when every child position is filled")
}

/// Evaluate `expr` in `scope`. An aggregate call reads the value its
/// group's accumulator resolved to (the group's rows were streamed through
/// the accumulators, never retained); every non-aggregate leaf evaluates
/// against the seed row. No seed means an empty group: there is no row to
/// ask, so a non-aggregate leaf is null.
pub(crate) fn agg_eval<'e>(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_, 'e>,
    expr: &'e Expr,
    params: &BTreeMap<String, Value>,
) -> Result<Value> {
    let seed = scope.seed;
    match expr {
        Expr::CountStar => scope.aggregate_call(expr),
        Expr::Function { name, .. } => {
            // The canonical aggregate classification lives in
            // `relate-ir` (single definition, re-exported by
            // `relate-front::agg`; lowering has already canonicalized
            // alias spellings). One classify answers both questions —
            // whether this is an aggregate at all, and which kind — so the
            // aggregate-call policy below reads the same table, not a second
            // spelling of it. A non-aggregate function keeps the transparent
            // scalar wrapper: every argument is an enclosing child of the
            // compound bind-and-rebuild.
            if name.with_str(relate_ir::classify_aggregate).is_none() {
                return agg_eval_compound(graph, scope, expr, params);
            }
            scope.aggregate_call(expr)
        }
        Expr::Map(es) => {
            let mut m = BTreeMap::new();
            for (k, e) in es {
                m.insert(k.clone(), agg_eval(graph, scope, e, params)?);
            }
            // The aggregate evaluator has its own Map/List arms; the
            // checked constructors here are what make the ADR-0462 cap hold
            // on this second evaluation path too.
            Value::checked_map(m)
        }
        Expr::List(xs) => {
            let mut out = Vec::new();
            for e in xs {
                out.push(agg_eval(graph, scope, e, params)?);
            }
            Value::checked_list(out)
        }
        // Every compound arm rides the one bind-and-rebuild
        // ([`bind_enclosing_children`]): each enclosing-scope child is
        // recursively `agg_eval`'d and bound under its synthetic
        // `__agg_c{i}` name, and the rebuilt expression — which references
        // those names — runs as scalar `eval` over a row seeded from the
        // scope's seed row (a harmless superset of whatever the child
        // leaves read). ADR-0198 ruling 2: an aggregate call in a CASE arm
        // belongs to the enclosing aggregating projection, so every WHEN
        // condition, THEN result and ELSE is an enclosing child. The
        // per-element bodies of the comprehension/quantifier/reduce forms
        // stay their original subtrees (the scope model's classification),
        // and the single-wrapper positions (LABS-744 — unary, property,
        // IS TYPE, IS NORMALIZED, the chrono comparisons, subscripts,
        // slices, map projections, non-aggregate calls) bind every child:
        // the front leaves them open (its `agg_enter_child` vocabulary is
        // narrower); this is the executor's classification.
        Expr::ListComp { .. }
        | Expr::MapComp { .. }
        | Expr::Quantifier { .. }
        | Expr::Reduce { .. }
        | Expr::AllReduce { .. }
        | Expr::Binary { .. }
        | Expr::Unary { .. }
        | Expr::Property { .. }
        | Expr::IsType { .. }
        | Expr::IsNormalized { .. }
        | Expr::ChronoCompare { .. }
        | Expr::Subscript { .. }
        | Expr::Slice { .. }
        | Expr::Case { .. }
        | Expr::MapProjection { .. } => agg_eval_compound(graph, scope, expr, params),
        Expr::Int(i) => Ok(Value::Int(*i)),
        Expr::Float(f) => Ok(Value::Float(*f)),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::String(s) => Ok(Value::String(s.as_str().into())),
        Expr::Null => Ok(Value::Null),
        // A parameter does not need a row. An empty group still resolves
        // it, so `count(a) > $p` and `collect(n) + [3]` keep the literal's
        // result when the literal was auto-parameterized (ADR-0503).
        Expr::Parameter(name) => params
            .get(name)
            .cloned()
            .ok_or_else(|| Error::parameters_missing([name.as_str()])),
        _ => match seed {
            Some(row) => eval(graph, row, expr, params),
            None => Ok(Value::Null),
        },
    }
}

/// The one compound bind-and-rebuild tail: bind every enclosing child of
/// `expr` under its synthetic name, then delegate the rebuild — which
/// references those names via `Expr::Variable` — to scalar `eval` over
/// the seeded row (see [`agg_eval_combinator`]).
fn agg_eval_compound<'e>(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_, 'e>,
    expr: &'e Expr,
    params: &BTreeMap<String, Value>,
) -> Result<Value> {
    let mut children = Vec::new();
    let rebuilt = bind_enclosing_children(expr, &mut children);
    agg_eval_combinator(graph, scope, &children, params, || rebuilt)
}

/// Shared shape of the transparent-combinator arms: recursively `agg_eval`
/// each child, bind the results under their synthetic `__agg_*` names in a
/// row seeded from the scope's seed row (cloned, or default when there is
/// none), and delegate the `rebuild`'d combinator — which references those
/// names via `Expr::Variable` — to scalar `eval`.
fn agg_eval_combinator<'e, S: AsRef<str>>(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_, 'e>,
    children: &[(S, &'e Expr)],
    params: &BTreeMap<String, Value>,
    rebuild: impl FnOnce() -> Expr,
) -> Result<Value> {
    let mut row = scope.seed.cloned().unwrap_or_default();
    for (name, child) in children {
        row.insert(name.as_ref().into(), agg_eval(graph, scope, child, params)?);
    }
    if scope.is_discovery() {
        // The children are walked and recorded; the wrapper runs over the
        // folded values only once a group has them.
        return Ok(Value::Null);
    }
    eval(graph, &row, &rebuild(), params)
}
