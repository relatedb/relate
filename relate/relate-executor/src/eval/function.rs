//! The scalar function dispatcher (`function()`), the BUILTINS
//! name-to-implementation table, and the helpers shared by the per-cluster
//! builtin modules under `function/`.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_ir::Expr;

use crate::eval_walk::{eval_shortest, ShortestWant};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::eval;

mod bar;
mod convert;
mod entity;
mod list;
pub(crate) use list::{RangePlan, RangeValues};
mod math;
mod property;
mod registry;
mod scalar;
mod spatial;
mod string;
mod temporal;
mod vector;

pub(super) use entity::entity_backing_map;
pub(crate) use registry::is_nondeterministic;
use registry::{arity_rejection, lookup_builtin, Applied, BuiltinImplementation, BuiltinRow};

/// The one resolution of a call against the builtin table, shared by both
/// call spellings — the scalar [`function`] dispatcher and the UNWIND
/// [`lazy_range`] — so lazy_range's resolves-exactly-as promise holds by
/// construction, never by mirroring the steps. `n` is the call's
/// lower-cased name (each caller derives it once); the row's arity
/// contract is enforced here, before any argument is evaluated.
fn resolve_call(n: &str, args: &[Expr]) -> Result<Option<&'static BuiltinRow>> {
    let Some(builtin) = lookup_builtin(n) else {
        return Ok(None);
    };
    if let Some(requirement) = builtin.arity.violation(args.len()) {
        return Err(Error::syntax(arity_rejection(n, requirement, args.len())));
    }
    Ok(Some(builtin))
}

/// The `range(...)` call an `UNWIND` can walk without building its list:
/// resolves the call exactly as [`function`] does — through the one
/// [`resolve_call`] both spellings read, then the arguments in order —
/// and returns the plan instead of the materialized list. `None` for
/// every other call.
pub(crate) fn lazy_range(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    name: &str,
    args: &[Expr],
) -> Result<Option<RangePlan>> {
    let n = name.to_ascii_lowercase();
    let Some(builtin) = resolve_call(&n, args)? else {
        return Ok(None);
    };
    // The resolved row's registered name is the registry's own canonical
    // spelling, so this is a table-identity question, not call-text
    // sniffing: only the range row answers a plan.
    if builtin.name != "range" {
        return Ok(None);
    }
    let vals = eval_args(graph, row, params, args)?;
    list::range_plan(&vals).map(Some)
}

pub(crate) fn function(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    name: &str,
    _distinct: bool,
    args: &[Expr],
) -> Result<Value> {
    let n = name.to_ascii_lowercase();
    if n == "length" {
        if let Some(Expr::ShortestPath {
            all: false, path, ..
        }) = args.first()
        {
            return eval_shortest(graph, row, params, false, path, ShortestWant::Length);
        }
    }
    // Resolve the row and validate the call's shape before evaluating any
    // argument: a malformed call reports its arity (LABS-667, Neo4j 42I13:
    // "does not provide the required number of arguments"), never an
    // argument's own error, and the evaluator does no work for it. The row
    // owns the contract, so registering a builtin is the one decision
    // point.
    let Some(builtin) = resolve_call(&n, args)? else {
        if relate_front::agg::is_aggregate_name(&n) {
            // 42I24: an aggregate call outside an aggregation context is a
            // compile-time `SyntaxError` in Neo4j, not a semantic error — the
            // front end rejects the contexts it owns, and this scalar-eval
            // backstop keeps the same class for the aggregation-transparent
            // positions it does not (LABS-744).
            return Err(Error::invalid_aggregation(format!(
                "{n}() is an aggregating function"
            )));
        }
        // 42N48 "no such function" is a SyntaxError-class error in Neo4j,
        // not a semantic one; the kind token carries the class.
        return Err(Error::unknown_function(&n));
    };
    match builtin.implementation {
        // The form rows receive the *unevaluated* arguments: their
        // trailing argument is syntax (a property-name identifier, a
        // distance-metric literal), never a value, so no argument is
        // evaluated before the form's own lowering.
        BuiltinImplementation::Form(form) => form(graph, row, params, args),
        // Every value-taking shape: the arguments are evaluated exactly
        // once, in order, at this one site, before any implementation
        // runs.
        BuiltinImplementation::Applied(applied) => {
            let vals = eval_args(graph, row, params, args)?;
            match applied {
                Applied::Plain(implementation) => implementation(graph, n.as_str(), vals),
                // The clock-access spelling's kind and mode come from the
                // row, so the implementation never re-parses the call
                // name; its argument is a timezone (never a value), and
                // the row's one-argument contract was enforced above.
                Applied::Clock(kind, mode) => {
                    relate_core::temporal::construct_clock(kind, mode, vals.first())
                }
                Applied::Truncate(kind) => temporal::truncate_call(kind, vals),
                // The between family's unit comes from the row and its two
                // operands are the row's arity contract; `n` is the called
                // spelling its messages name.
                Applied::Between(unit) => {
                    let [a, b] = <[Value; 2]>::try_from(vals)
                        .expect("the between row's arity is exactly two");
                    temporal::between_call(unit, n.as_str(), [a, b])
                }
            }
        }
    }
}

/// Evaluate a call's arguments in order — the shared prelude of every
/// value-taking implementation shape (the form rows take their arguments
/// unevaluated and never reach this).
fn eval_args(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    args: &[Expr],
) -> Result<Vec<Value>> {
    let mut vals = Vec::new();
    for a in args {
        vals.push(eval(graph, row, a, params)?);
    }
    Ok(vals)
}

/// The accepted scalar set's rendering, stated once: a STRING passes
/// through as the same shared payload (no copy); every other accepted
/// scalar renders through [`Value::to_display_string`] (the one display
/// policy, temporals at full precision per ADR-0448). Both
/// [`render_scalar`] and `toString`'s exhaustive scalar tail call this,
/// so the rendering policy has one spelling; each caller still spells the
/// ACCEPTANCE set itself, because that is what makes a new `Value`
/// variant fail compilation at every routing site.
pub(crate) fn render_accepted_scalar(v: &Value) -> Arc<str> {
    match v {
        // A STRING passes through as the same shared payload (no copy).
        Value::String(s) => Arc::clone(s),
        // Every other caller-verified accepted scalar renders through the
        // one display policy (temporals at full precision, ADR-0448).
        _ => v.to_display_string().into(),
    }
}

fn render_scalar(v: &Value) -> Option<Arc<str>> {
    match v {
        // A STRING passes through as the same shared payload (no copy).
        Value::String(_)
        // Every other accepted scalar renders through the one display
        // policy (temporals at full precision, ADR-0448).
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_) => Some(render_accepted_scalar(v)),
        // The declined vocabulary, spelled: the containers and graph
        // values each caller routes on its own, and the Vector the two
        // toString paths render differently.
        Value::Null
        | Value::Bytes(_)
        | Value::List(_)
        | Value::Map(_)
        | Value::Node(_)
        | Value::Relationship(_)
        | Value::Path(_)
        | Value::Vector { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::convert::{bi_tostring, bi_tostringornull};
    use super::registry::{
        builtins, Arity, ARITY_AT_LEAST_ONE, ARITY_AT_MOST_ONE, ARITY_EXACTLY_FOUR_ENGINE_ONLY,
    };
    use super::string::bi_string_interp;
    use super::*;
    use crate::graph::Graph;

    fn registered(arity: Arity) -> Vec<&'static str> {
        builtins()
            .iter()
            .filter(|b| b.arity == arity)
            .map(|b| b.name)
            .collect()
    }

    /// The lockstep test: every distinct special arity registered in the
    /// table must carry exactly the names its constant's verified set
    /// declares (the spec verified against live Neo4j 2026.09, riding the
    /// constant the way its requirement does) -- the independent fact the
    /// behavior test alone cannot supply. The three arities with an empty
    /// `verified` set are the one typed table below.
    #[test]
    fn special_arity_rows_match_the_verified_sets() {
        /// How an arity with no verified set is pinned instead.
        enum Specless {
            /// The floor: every-row behavior is checked by the per-row test.
            Floor,
            /// Rows generated from the spelling registry, pinned to it.
            Clock,
            /// Engine-only: no live spec exists, so the one name is pinned.
            EngineOnly(&'static str),
        }
        let specless = |arity: &Arity| {
            if *arity == ARITY_AT_LEAST_ONE {
                Some(Specless::Floor)
            } else if *arity == ARITY_AT_MOST_ONE {
                Some(Specless::Clock)
            } else if *arity == ARITY_EXACTLY_FOUR_ENGINE_ONLY {
                Some(Specless::EngineOnly("bar"))
            } else {
                None
            }
        };
        // The distinct arities actually registered (sorted before dedup --
        // the table is sorted by name, not arity).
        let mut registered_arities: Vec<Arity> = builtins().iter().map(|b| b.arity).collect();
        registered_arities.sort_by_key(|a| (a.min, a.max));
        registered_arities.dedup();
        for arity in registered_arities {
            let mut expected: Vec<&str> = match specless(&arity) {
                Some(Specless::Floor) => continue,
                Some(Specless::Clock) => relate_core::temporal::clock_spellings().collect(),
                Some(Specless::EngineOnly(name)) => vec![name],
                None => {
                    assert!(
                        !arity.verified.is_empty(),
                        "{arity:?}: only the floor, the clock arity and the engine-only bar arity may carry an empty verified set"
                    );
                    arity.verified.to_vec()
                }
            };
            let mut actual = registered(arity);
            actual.sort_unstable();
            expected.sort_unstable();
            assert_eq!(actual, expected, "{arity:?}");
        }
    }

    // LABS-667: a call outside its row's contract is an arity error on
    // both sides (Neo4j 42I13 shape: it names the function, the
    // requirement, and the count); a call inside it reaches the
    // implementation. The special variants are checked at every count they
    // refuse; AtLeastOne checks its floor.
    #[test]
    fn calls_outside_their_rows_contract_are_arity_errors() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let row = Row::new();
        let params = BTreeMap::new();
        // The gate fires before any argument is evaluated, so only the
        // count reaches it: one (refused count, expected requirement) case.
        for builtin in builtins() {
            // The refused counts, derived from the row's own bounds: below
            // the floor (if any) and above the ceiling (if any).
            let mut counts: Vec<usize> = Vec::new();
            if builtin.arity.min > 0 {
                counts.push(0);
            }
            if let Some(max) = builtin.arity.max {
                counts.push(max + 1);
            }
            for count in counts {
                let requirement = builtin.arity.violation(count).unwrap();
                let args = vec![Expr::Int(1); count];
                let text = function(&graph, &row, &params, builtin.name, false, &args)
                    .unwrap_err()
                    .to_string();
                let expected = arity_rejection(builtin.name, requirement, args.len());
                assert!(text.contains(&expected), "{}: {text}", builtin.name);
            }
        }
    }

    #[test]
    fn builtins_table_is_strictly_sorted() {
        assert!(
            builtins().windows(2).all(|w| w[0].name < w[1].name),
            "the builtin table must stay strictly sorted for binary_search_by_key"
        );
    }

    /// The clock and truncate row shapes sample the clock whatever their
    /// argument is, so the determinism classification derives from the shape
    /// rather than a per-row flag.
    #[test]
    fn temporal_row_shapes_are_nondeterministic() {
        for name in [
            "date.realtime",
            "time.transaction",
            "localdatetime.statement",
            "date.truncate",
            "datetime.truncate",
        ] {
            assert!(is_nondeterministic(name), "{name}");
        }
        assert!(!is_nondeterministic("datetime.fromepoch"));
        assert!(!is_nondeterministic("tostring"));
    }

    #[test]
    fn to_string_and_interp_of_a_string_share_its_payload() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let payload: Arc<str> = "añ😀".into();
        let input = Value::String(Arc::clone(&payload));
        let shared = |out: Value| matches!(out, Value::String(s) if Arc::ptr_eq(&s, &payload));

        assert!(Arc::ptr_eq(&render_scalar(&input).unwrap(), &payload));
        assert!(shared(
            bi_tostring(&graph, "toString", vec![input.clone()]).unwrap()
        ));
        assert!(shared(
            bi_tostringornull(&graph, "toStringOrNull", vec![input.clone()]).unwrap()
        ));
        assert!(shared(
            bi_string_interp(&graph, "string.interp", vec![input.clone()]).unwrap()
        ));
    }
}
