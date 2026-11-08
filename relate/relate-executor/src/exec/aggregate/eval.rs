//! The recursive aggregate-expression evaluator: aggregate calls fold the
//! group, every non-aggregate leaf reads the seed row, and the transparent
//! combinators (map, list, comprehension, CASE, binary, …) recurse through
//! the same scope.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::{AggregateArity, AggregateKind, Expr, MapProjectionItem};
use relate_names::SmallName;

use crate::eval::{eval, fold_aggregate, FoldInput};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// The two scopes one aggregation evaluation reads, bundled so they travel
/// together through the recursion: `group` is what an aggregate call folds,
/// `seed` the row a non-aggregate leaf is asked about.
#[derive(Clone, Copy)]
pub(crate) struct AggScope<'a> {
    group: &'a [Row],
    seed: Option<&'a Row>,
}

impl<'a> AggScope<'a> {
    /// A projection item's scope: its expression runs over the group and
    /// asks about the group's first row. ADR-0453: an importing subquery's
    /// scope row is the empty-group fallback — the phantom keyless group
    /// keeps reading the imports (per-row constants) when the body's rows
    /// are gone; `None` at every other scope.
    pub(crate) fn of_group_with_import(group: &'a [Row], import: Option<&'a Row>) -> Self {
        Self {
            group,
            seed: group.first().or(import),
        }
    }

    /// An `ORDER BY` key's scope: aggregate calls still fold the group, but
    /// every non-aggregate leaf asks about `row` — the projected output row,
    /// whose names the projection's aliases shadow.
    pub(crate) fn of_row(group: &'a [Row], row: &'a Row) -> Self {
        Self {
            group,
            seed: Some(row),
        }
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

/// Evaluate `expr` in `scope`. Aggregate calls fold the group's rows; every
/// non-aggregate leaf evaluates against the seed row. No seed means an empty
/// group: there is no row to ask, so a non-aggregate leaf is null.
pub(crate) fn agg_eval(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_>,
    expr: &Expr,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let AggScope { group, seed } = scope;
    match expr {
        Expr::CountStar => Ok(Value::Int(group.len() as i64)),
        Expr::Function {
            name,
            distinct,
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
            let Some(kind) = name.with_str(relate_ir::classify_aggregate) else {
                return agg_eval_function(graph, scope, name, args, params, cypher_version);
            };
            fold_aggregate_call(
                graph,
                group,
                kind,
                name,
                *distinct,
                args,
                params,
                cypher_version,
            )
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

/// One aggregate call's policy, held apart from [`agg_eval`]'s Expr-variant
/// dispatch: the kind's arity contract, the percentile's second argument,
/// the retained-container row loop, the distinct dedup, and then the shared
/// per-kind fold ([`fold_aggregate`]).
#[allow(clippy::too_many_arguments)]
fn fold_aggregate_call(
    graph: &ExecGraph<'_>,
    group: &[Row],
    kind: AggregateKind,
    name: &SmallName,
    distinct: bool,
    args: &[Expr],
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    // LABS-276 / ADR-0225/0217: every aggregate's arity contract comes from
    // the kind's row in the single `relate-ir` contracts table, checked
    // before the row loop so an empty group cannot slip a bad call through
    // and the loop's `args[0]` index cannot panic. The contract class owns
    // the error shape: the single-input class matches the front's
    // `check_aggregate_arity` (the shared `SINGLE_INPUT_AGGREGATES` table);
    // `count` is guarded here too — a backtick-escaped `` `count`() ``
    // bypasses both the parser's keyword path and front validation — and the
    // percentiles keep their own two-argument message, now exact: over-arity
    // calls raise the same error instead of silently dropping args[2..].
    let arity = kind.arity();
    if args.len() != arity.required() {
        let rejection = name.with_str(|text| arity.rejection(text, args.len()));
        return Err(match arity {
            AggregateArity::SingleInput => Error::syntax(rejection),
            AggregateArity::PercentilePair => Error::ty(rejection),
        });
    }
    // The arity class decides whether the call has a percentile:
    // `PercentilePair` evaluates it and asks `FoldInput::percentile` — the
    // one exhaustive pairing beside the vocabulary — for the paired input;
    // every single-input kind builds `Many`. The pairing is carried by the
    // input variant, so `fold_aggregate` never re-derives it.
    // The percentile argument depends only on the group's first row, so it is
    // evaluated and validated here, before the retain guard and the per-row
    // loop: a malformed percentile fails before any per-row work.
    let input = match arity {
        AggregateArity::SingleInput => FoldInput::Many(kind),
        AggregateArity::PercentilePair => {
            // An empty group has no percentile to report and no row to ask
            // about: the fold below yields null. The scope's own seed policy
            // (`AggScope::of_group_with_import`: `group.first().or(import)`)
            // never fabricates a row, so neither does this evaluation.
            let Some(first) = group.first() else {
                return Ok(Value::Null);
            };
            // Arity (two arguments) is checked ahead of the row loop, above;
            // the percentile itself must be a number in [0, 1] — one
            // rejection site covers both failure modes, so the message
            // cannot drift between them.
            let p = eval(graph, first, &args[1], params, cypher_version)?;
            let Some(p) = p.as_number().filter(|p| (0.0..=1.0).contains(p)) else {
                return Err(Error::ty("NumberOutOfRange: percentile"));
            };
            name.with_str(|text| FoldInput::percentile(kind, p, text))?
        }
    };
    // The `vals` vector is one retaining container for every aggregate (not
    // just `collect`), and the guard's construction owns both the slot-buffer
    // charge and the running retained total — charging the buffer here as
    // well would double-count the figure and run the cap check twice. The
    // values' own payloads were charged by the expression that built them;
    // `collect` additionally retains them in the result.
    //
    // LABS-802 / ADR-0499: the exact `owned_heap` charge stays for every
    // fold — a transient fold's vals hold the payloads until the fold
    // completes, and a constructed payload nested one level deep
    // (`count([$p + $p])`, each element under the per-allocation cap)
    // escapes any fixed-width estimate, so the cumulative bound must be
    // the exact walk (the security round's finding). The per-row overhead
    // the profile measured — 46.5% of a plain count — was the walk's
    // non-inlined recursive call chain, not its semantics: the chain is
    // now `#[inline]` (`owned_heap` and its helpers, the core string and
    // byte formulas), so a shared payload costs one `Arc` count read, a
    // flat payload inline arithmetic, and the deep walk is paid only by a
    // row whose own operand construction was that deep — proportional, at
    // the construction site, never re-walked per row for moved values.
    let mut retained = graph.retain_slots(group.len())?;
    let mut vals = Vec::new();
    for row in group {
        graph.cancel_tick()?;
        let value = eval(graph, row, &args[0], params, cypher_version)?;
        retained.add_owned(graph, &value)?;
        vals.push(value);
    }
    if distinct {
        // First-seen emission under the one `dedup_first_seen` policy, whose
        // set charge is internal; the surviving values stay retained here.
        vals = crate::distinct_set::dedup_first_seen(vals)?;
    }
    name.with_str(|text| fold_aggregate(input, text, vals))
}

/// Shared shape of the transparent-combinator arms: recursively `agg_eval`
/// each child, bind the results under their synthetic `__agg_*` names in a
/// row seeded from the scope's seed row (cloned, or default when there is
/// none), and delegate the `rebuild`'d combinator — which references those
/// names via `Expr::Variable` — to scalar `eval`.
fn agg_eval_combinator<S: AsRef<str>>(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_>,
    children: &[(S, &Expr)],
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
    eval(graph, &row, &rebuild(), params, cypher_version)
}

/// Non-aggregate function wrapper over aggregate arguments: bind each
/// argument under `__agg_arg{i}` and re-run the call as scalar `eval`.
fn agg_eval_function(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_>,
    name: &SmallName,
    args: &[Expr],
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
    scope: AggScope<'_>,
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
fn agg_eval_map_projection(
    graph: &ExecGraph<'_>,
    scope: AggScope<'_>,
    target: &Expr,
    items: &[MapProjectionItem],
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
