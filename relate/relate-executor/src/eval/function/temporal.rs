//! Temporal and duration builtins: date/time/datetime construction,
//! `*.truncate`, `datetime.fromepoch[Millis]`, `duration.between`/`in*`, and
//! `format`.

use relate_core::{temporal, Error, Result, Value};

use crate::overlay::ExecGraph;

pub(super) fn bi_format(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // The one-or-two-argument count is the registry row's contract,
    // enforced before any argument is evaluated.
    Ok(if vals.iter().any(Value::is_null) {
        Value::Null
    } else {
        let Value::Temporal(t) = &vals[0] else {
            return Err(Error::ty("format() requires a temporal value"));
        };
        match vals.get(1) {
            // The no-pattern default renders the same full-precision
            // form toString does (ADR-0448): one temporal rendering
            // for every string-producing surface.
            None => Value::String(t.full_seconds_string().into()),
            Some(Value::String(pattern)) => {
                Value::String(temporal::format_pattern(t, pattern)?.into())
            }
            _ => return Err(Error::ty("format() pattern must be a string")),
        }
    })
}

/// `date()`/`time()`/`datetime()`/`localtime()`/`localdatetime()`/
/// `duration()`: the one shared construction policy
/// (`temporal::construct_named`) behind the builtin dispatch, the
/// front-end's literal folding, and the scan-predicate specialization.
pub(super) fn bi_date(_graph: &ExecGraph<'_>, n: &str, vals: Vec<Value>) -> Result<Value> {
    temporal::construct_named(n, &vals)
}
pub(super) fn bi_datetime_fromepoch(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok({
        let secs = vals
            .first()
            .and_then(|v| v.as_int())
            .ok_or_else(|| Error::ty("datetime.fromepoch seconds"))?;
        let nanos = vals.get(1).and_then(|v| v.as_int()).unwrap_or(0);
        Value::Temporal(temporal::datetime_from_epoch(secs, nanos)?)
    })
}
pub(super) fn bi_datetime_fromepochmillis(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok({
        let ms = vals
            .first()
            .and_then(|v| v.as_int())
            .ok_or_else(|| Error::ty("datetime.fromepochmillis"))?;
        Value::Temporal(temporal::datetime_from_epoch_millis(ms)?)
    })
}
/// `timestamp()`: the statement-pinned instant as epoch milliseconds.
pub(super) fn bi_timestamp(_graph: &ExecGraph<'_>, _n: &str, _vals: Vec<Value>) -> Result<Value> {
    Ok(Value::Int(temporal::now_epoch_millis()))
}
/// `duration.between` and its `duration.in*` siblings: the unit comes from
/// the row (`BuiltinImplementation::Between`), never from the call name, and
/// the two operands are the whole argument list — the dispatcher enforced the
/// row's exactly-two contract, so the signature states it. `n` is only the
/// called spelling for the messages.
pub(super) fn between_call(
    unit: temporal::BetweenUnit,
    n: &str,
    [a, b]: [Value; 2],
) -> Result<Value> {
    Ok(match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Temporal(a), Value::Temporal(b)) => {
            Value::Temporal(temporal::duration_between_unit(&a, &b, unit)?)
        }
        _ => {
            return Err(Error::syntax(format!(
                "Invalid call signature: {n} needs temporal values"
            )))
        }
    })
}

/// `<kind>.truncate(...)`: a unit, an optional value, and an optional fields
/// map. The dispatcher enforced the row's `ARITY_ONE_TO_THREE` contract, so
/// the splits below are invariant reads — the arity authority is the row,
/// never a second copy of the contract here.
pub(super) fn truncate_call(kind: temporal::ClockKind, vals: Vec<Value>) -> Result<Value> {
    let (unit, rest) = vals
        .split_first()
        .expect("the truncate row requires a unit argument");
    let unit = match unit {
        Value::String(unit) => &**unit,
        Value::Null => return Ok(Value::Null),
        _ => return Err(Error::ty("truncate unit must be a string")),
    };
    let (value, map) = match rest {
        [] => return Ok(Value::Temporal(temporal::truncate_now(kind, unit)?)),
        [value] => (value, None),
        // At most two further arguments reach here; the pattern stays total.
        [value, map, ..] => (value, Some(map)),
    };
    match value {
        Value::Temporal(value) => Ok(Value::Temporal(temporal::truncate(kind, unit, value, map)?)),
        Value::Null => Ok(Value::Null),
        _ => Err(Error::syntax(format!(
            "Invalid call signature: {}.truncate needs a temporal value",
            kind.name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;

    /// The one-argument `truncate` form truncates the current value of its
    /// kind, so it equals the explicit `<kind>()` form over the same pinned
    /// clock; the two- and three-argument forms keep the explicit value.
    #[test]
    fn truncate_without_a_value_uses_the_pinned_clock() {
        let _clock = relate_core::QueryClockGuard::new();
        let unit = Value::String("millennium".into());
        let implicit = truncate_call(temporal::ClockKind::Date, vec![unit.clone()]).unwrap();
        let explicit = truncate_call(
            temporal::ClockKind::Date,
            vec![unit, temporal::construct_named("date", &[]).unwrap()],
        )
        .unwrap();
        assert_eq!(implicit, explicit);
        assert!(!implicit.is_null());
    }

    /// A fourth argument is an arity error, never silently dropped — the
    /// row's `ARITY_ONE_TO_THREE` contract, enforced by the dispatcher
    /// before the implementation runs (the implementation itself takes the
    /// row's guarantee).
    #[test]
    fn truncate_refuses_a_fourth_argument() {
        let _clock = relate_core::QueryClockGuard::new();
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let row = crate::row::Row::new();
        let params = std::collections::BTreeMap::new();
        let err = super::super::function(
            &graph,
            &row,
            &params,
            "date.truncate",
            false,
            &[
                relate_ir::Expr::String("day".into()),
                relate_ir::Expr::String("2024-01-01".into()),
                relate_ir::Expr::Null,
                relate_ir::Expr::Null,
            ],
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("date.truncate() requires one to three arguments"),
            "{err}"
        );
    }

    /// The clock-access row reads the pinned instant and honors the timezone
    /// argument through the dispatcher (the core clock tests cover the zone
    /// matrix). A numeric timezone is refused by the implementation, and two
    /// arguments by the row's one-argument contract — both named with the
    /// spelling the caller wrote.
    #[test]
    fn clock_access_rows_dispatch_through_the_table() {
        let _clock = relate_core::QueryClockGuard::new();
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let row = crate::row::Row::new();
        let params = std::collections::BTreeMap::new();
        let call = |name: &str, args: &[relate_ir::Expr]| {
            super::super::function(&graph, &row, &params, name, false, args, None)
        };

        let out = call("date.realtime", &[relate_ir::Expr::String("+14:00".into())]).unwrap();
        assert!(matches!(out, Value::Temporal(_)));

        let numeric = call("date.realtime", &[relate_ir::Expr::Int(42)])
            .unwrap_err()
            .to_string();
        assert!(
            numeric.contains("Invalid call signature: date.realtime()"),
            "{numeric}"
        );

        let two = call(
            "date.transaction",
            &[
                relate_ir::Expr::String("UTC".into()),
                relate_ir::Expr::String("UTC".into()),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(
            two.contains(
                "InvalidNumberOfArguments: date.transaction() requires at most one argument"
            ),
            "{two}"
        );
    }

    #[test]
    fn timestamp_reads_the_pinned_statement_clock() {
        let _clock = relate_core::QueryClockGuard::new();
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let result = bi_timestamp(&graph, "timestamp", vec![]).unwrap();
        let Value::Int(ms) = result else {
            panic!("timestamp() must return an INTEGER, got {result:?}");
        };
        assert_eq!(ms, temporal::now_epoch_millis());
    }
}
