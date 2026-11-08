//! The recursive aggregate-expression evaluator: aggregate calls read their
//! group's resolved accumulators, every non-aggregate leaf reads the seed row, and the transparent
//! combinators (map, list, comprehension, CASE, binary, …) recurse through
//! the same scope.

use std::cell::RefCell;
use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::{Expr, MapProjectionItem};
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
    cypher_version: Option<u32>,
) -> Result<Value> {
    let seed = scope.seed;
    match expr {
        Expr::CountStar => scope.aggregate_call(expr),
        Expr::Function {
            name,
            distinct: _,
            args,
        } => {
            // The canonical aggregate classification lives in
            // `relate-ir` (single definition, re-exported by
            // `relate-front::agg`; lowering has already canonicalized
            // alias spellings). One classify answers both questions —
            // whether this is an aggregate at all, and which kind — so the
            // aggregate-call policy below reads the same table, not a second
            // spelling of it. A non-aggregate function keeps the transparent
            // scalar wrapper.
            if name.with_str(relate_ir::classify_aggregate).is_none() {
                return agg_eval_function(graph, scope, name, args, params, cypher_version);
            }
            scope.aggregate_call(expr)
        }
        Expr::Map(es) => {
            let mut m = BTreeMap::new();
            for (k, e) in es {
                m.insert(
                    k.clone(),
                    agg_eval(graph, scope, e, params, cypher_version)?,
                );
            }
            // The aggregate evaluator has its own Map/List arms; the
            // checked constructors here are what make the ADR-0462 cap hold
            // on this second evaluation path too.
            Value::checked_map(m)
        }
        Expr::MapProjection { expr, items } => {
            agg_eval_map_projection(graph, scope, expr, items, params, cypher_version)
        }
        Expr::List(xs) => {
            let mut out = Vec::new();
            for e in xs {
                out.push(agg_eval(graph, scope, e, params, cypher_version)?);
            }
            Value::checked_list(out)
        }
        Expr::ListComp {
            ident,
            list,
            predicate,
            projection,
        } => {
            let mut children = Vec::new();
            let rebuilt = Expr::ListComp {
                ident: ident.clone(),
                list: Box::new(bind_child(&mut children, "__agg_list".into(), list)),
                predicate: predicate.clone(),
                projection: projection.clone(),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        Expr::MapComp {
            key_ident,
            value_ident,
            source,
            predicate,
            key_expr,
            value_expr,
        } => {
            let mut children = Vec::new();
            let rebuilt = Expr::MapComp {
                key_ident: key_ident.clone(),
                value_ident: value_ident.clone(),
                source: Box::new(bind_child(&mut children, "__agg_map".into(), source)),
                predicate: predicate.clone(),
                key_expr: key_expr.clone(),
                value_expr: value_expr.clone(),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        Expr::Quantifier {
            kind,
            ident,
            list,
            predicate,
        } => {
            let mut children = Vec::new();
            let rebuilt = Expr::Quantifier {
                kind: *kind,
                ident: ident.clone(),
                list: Box::new(bind_child(&mut children, "__agg_list".into(), list)),
                predicate: predicate.clone(),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        Expr::Reduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
        } => {
            let mut children = Vec::new();
            let rebuilt = Expr::Reduce {
                accumulator: accumulator.clone(),
                initial: Box::new(bind_child(&mut children, "__agg_initial".into(), initial)),
                ident: ident.clone(),
                list: Box::new(bind_child(&mut children, "__agg_list".into(), list)),
                expression: expression.clone(),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        Expr::AllReduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
            predicate,
        } => {
            let mut children = Vec::new();
            let rebuilt = Expr::AllReduce {
                accumulator: accumulator.clone(),
                initial: Box::new(bind_child(&mut children, "__agg_initial".into(), initial)),
                ident: ident.clone(),
                list: Box::new(bind_child(&mut children, "__agg_list".into(), list)),
                expression: expression.clone(),
                predicate: predicate.clone(),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        Expr::Case {
            when_then,
            else_expr,
        } => agg_eval_case(graph, scope, when_then, else_expr, params, cypher_version),
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
        // The rebuilt expression references only the synthetic `__l`/`__r`
        // variables (both operands were already `agg_eval`'d), so the
        // combinator's first-row seed is a harmless superset of the empty
        // row this arm used to pass explicitly.
        Expr::Binary { op, left, right } => {
            let mut children = Vec::new();
            let rebuilt = Expr::Binary {
                op: *op,
                left: Box::new(bind_child(&mut children, "__l".into(), left)),
                right: Box::new(bind_child(&mut children, "__r".into(), right)),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        // Aggregation-transparent single-wrapper positions (LABS-744): every
        // child folds the group and the wrapper runs over the folded values.
        // Driven off the canonical child walk (`children` / `map_children`,
        // which agree on order), so these positions carry no per-variant
        // field list of their own. The front leaves them open (its
        // `agg_enter_child` vocabulary is narrower); this is the executor's
        // classification.
        Expr::Unary { .. }
        | Expr::Property { .. }
        | Expr::IsType { .. }
        | Expr::IsNormalized { .. }
        | Expr::ChronoCompare { .. } => {
            let kids = expr.children();
            let names: Vec<String> = (0..kids.len()).map(|i| format!("__agg_c{i}")).collect();
            let children: Vec<(&String, &Expr)> = names.iter().zip(kids).collect();
            let mut next = 0;
            let rebuilt = expr.map_children(&mut |_| {
                let var = Expr::Variable(SmallName::from_name(&names[next]));
                next += 1;
                Ok::<_, std::convert::Infallible>(var)
            });
            let Ok(rebuilt) = rebuilt;
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        Expr::Subscript { expr, index } => {
            let mut children = Vec::new();
            let rebuilt = Expr::Subscript {
                expr: Box::new(bind_child(&mut children, "__agg_base".into(), expr)),
                index: index.clone(),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        Expr::Slice { expr, start, end } => {
            let mut children = Vec::new();
            let rebuilt = Expr::Slice {
                expr: Box::new(bind_child(&mut children, "__agg_base".into(), expr)),
                start: start.clone(),
                end: end.clone(),
            };
            agg_eval_combinator(graph, scope, &children, params, cypher_version, || rebuilt)
        }
        _ => match seed {
            Some(row) => eval(graph, row, expr, params, cypher_version),
            None => Ok(Value::Null),
        },
    }
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
    cypher_version: Option<u32>,
    rebuild: impl FnOnce() -> Expr,
) -> Result<Value> {
    let mut row = scope.seed.cloned().unwrap_or_default();
    for (name, child) in children {
        row.insert(
            name.as_ref().into(),
            agg_eval(graph, scope, child, params, cypher_version)?,
        );
    }
    if scope.is_discovery() {
        // The children are walked and recorded; the wrapper runs over the
        // folded values only once a group has them.
        return Ok(Value::Null);
    }
    eval(graph, &row, &rebuild(), params, cypher_version)
}

/// Non-aggregate function wrapper over aggregate arguments: bind each
/// argument under `__agg_arg{i}` and re-run the call as scalar `eval`.
fn agg_eval_function<'e>(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_, 'e>,
    name: &SmallName,
    args: &'e [Expr],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let mut children = Vec::new();
    let rebuilt_args: Vec<Expr> = args
        .iter()
        .enumerate()
        .map(|(i, arg)| bind_child(&mut children, format!("__agg_arg{i}"), arg))
        .collect();
    agg_eval_combinator(graph, scope, &children, params, cypher_version, || {
        Expr::Function {
            name: name.clone(),
            distinct: false,
            args: rebuilt_args,
        }
    })
}

/// ADR-0198 ruling 2: an aggregate call in a CASE arm belongs to the
/// enclosing aggregating projection. Every part that can contain an
/// aggregate (WHEN conditions, THEN results, ELSE) goes through `agg_eval`;
/// the rebuilt CASE references the results via `__agg_*` variables, so
/// scalar `eval_case` picks the arm.
fn agg_eval_case<'e>(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_, 'e>,
    when_then: &'e [(Expr, Expr)],
    else_expr: &'e Option<Box<Expr>>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let mut children: Vec<(String, &Expr)> = Vec::new();
    // Bind every part under its synthetic name and capture the matching
    // variable in the same pass: one name stream, no rebuild-time cursor
    // that could drift.
    let when_then: Vec<(Expr, Expr)> = when_then
        .iter()
        .enumerate()
        .map(|(i, (when, then))| {
            (
                bind_child(&mut children, format!("__agg_when{i}"), when),
                bind_child(&mut children, format!("__agg_then{i}"), then),
            )
        })
        .collect();
    let else_expr = else_expr
        .as_deref()
        .map(|e| Box::new(bind_child(&mut children, "__agg_else".into(), e)));
    agg_eval_combinator(graph, scope, &children, params, cypher_version, || {
        Expr::Case {
            when_then,
            else_expr,
        }
    })
}

/// ADR-0453: aggregate calls inside a map projection's key-value expressions
/// fold the group like any other aggregation sub-expression; the target and
/// the shorthand items stay seed-relative. Rebuilt over synthetic names, the
/// same shape the other transparent combinators use.
fn agg_eval_map_projection<'e>(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_, 'e>,
    target: &'e Expr,
    items: &'e [MapProjectionItem],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let mut children = Vec::new();
    let target_var = bind_child(&mut children, "__agg_target".into(), target);
    let mut rebuilt: Vec<MapProjectionItem> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        match item {
            MapProjectionItem::KeyValue { key, value } => {
                rebuilt.push(MapProjectionItem::KeyValue {
                    key: key.clone(),
                    value: bind_child(&mut children, format!("__agg_mp{i}"), value),
                });
            }
            other => rebuilt.push(other.clone()),
        }
    }
    agg_eval_combinator(graph, scope, &children, params, cypher_version, || {
        Expr::MapProjection {
            expr: Box::new(target_var),
            items: rebuilt,
        }
    })
}
