//! `bar(value, min, max, width)`: an engine-only text bar. It renders the
//! position of `value` inside `[min, max]` as block characters, `width`
//! cells wide at full scale, with 1/8-cell resolution (the fractional left
//! blocks U+258F..U+2589 and the full block U+2588). The name is callable in
//! every build but unlisted by `SHOW FUNCTIONS` (it sits in the front's
//! `UNADVERTISED_FUNCTIONS`); it is not part of the Cypher-25 corpus.

use relate_core::{Error, ErrorKind, Result, Value};

use crate::overlay::ExecGraph;

/// The widest bar a call may request, in cells. It bounds the output
/// (at most 3 bytes per cell) so a call can never size an allocation from
/// an unchecked argument.
const BAR_MAX_WIDTH: i64 = 1024;

/// The eight fill levels in order: `FILL[i]` is a cell filled `i + 1`
/// eighths from the left, ending in the full block.
const FILL: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// A numeric argument as a finite `f64`, or the typed error naming it.
fn finite_number(arg: &Value, what: &str) -> Result<f64> {
    let number = arg
        .as_number()
        .ok_or_else(|| Error::invalid_argument_type(format!("bar() {what} must be a number")))?;
    if number.is_finite() {
        Ok(number)
    } else {
        Err(Error::argument_error_kind(
            ErrorKind::InvalidArgumentValue,
            format!("bar() {what} must be finite"),
        ))
    }
}

pub(super) fn bi_bar(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    let [value, min, max, width] = vals.as_slice() else {
        // The row's exactly-four arity gate fires before dispatch.
        unreachable!("bar() is registered with an exactly-four-arguments arity");
    };
    if vals.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let value = finite_number(value, "value")?;
    let min = finite_number(min, "min")?;
    let max = finite_number(max, "max")?;
    // Strictly INTEGER: `Value::as_int` would also accept a whole float.
    let Value::Int(width) = *width else {
        return Err(Error::invalid_argument_type(
            "bar() width must be an integer",
        ));
    };
    if !(0..=BAR_MAX_WIDTH).contains(&width) {
        return Err(Error::argument_error_kind(
            ErrorKind::NumberOutOfRange,
            format!("bar() width must be between 0 and {BAR_MAX_WIDTH}"),
        ));
    }
    let span = max - min;
    if min >= max || !span.is_finite() {
        return Err(Error::argument_error_kind(
            ErrorKind::InvalidArgumentValue,
            "bar() min must be less than max, and their span finite",
        ));
    }
    let fraction = (value.clamp(min, max) - min) / span;
    // `fraction` is in [0, 1] and `width` is at most the cap, so the
    // product is in [0, 8 * cap]: the cast cannot truncate.
    let eighths = (fraction * width as f64 * 8.0).floor() as usize;
    let (full, rest) = (eighths / 8, eighths % 8);
    let mut bar = String::with_capacity((full + 1) * FILL[7].len_utf8());
    bar.extend(std::iter::repeat_n(FILL[7], full));
    if rest > 0 {
        bar.push(FILL[rest - 1]);
    }
    Ok(Value::String(bar.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::test_support::int;

    fn bar(args: Vec<Value>) -> Result<Value> {
        let g = Graph::default();
        bi_bar(&ExecGraph::read(&g), "bar", args)
    }

    fn call(v: Value, min: Value, max: Value, width: i64) -> Result<Value> {
        bar(vec![v, min, max, Value::Int(width)])
    }

    fn text(v: Result<Value>) -> String {
        match v.unwrap() {
            Value::String(s) => s.to_string(),
            other => panic!("expected a string, got {other:?}"),
        }
    }

    #[test]
    fn half_scale_fills_half_the_width() {
        assert_eq!(text(call(int(5), int(0), int(10), 10)), "█████");
    }

    #[test]
    fn values_clamp_to_the_range() {
        assert_eq!(text(call(int(-5), int(0), int(10), 4)), "");
        assert_eq!(text(call(int(99), int(0), int(10), 4)), "████");
    }

    #[test]
    fn partial_cells_use_eighths_floored() {
        // 1/8 of a one-cell bar through the full block.
        for (i, expected) in FILL.iter().enumerate() {
            assert_eq!(
                text(call(int(i as i64 + 1), int(0), int(8), 1)),
                expected.to_string()
            );
        }
        // 0.99 of an eighth floors to nothing; 1.99 to one eighth.
        assert_eq!(text(call(Value::Float(0.99), int(0), int(8), 1)), "");
        assert_eq!(text(call(Value::Float(1.99), int(0), int(8), 1)), "▏");
        // 2.5 cells: two full cells and a half.
        assert_eq!(text(call(Value::Float(2.5), int(0), int(4), 4)), "██▌");
    }

    #[test]
    fn width_zero_is_the_empty_string() {
        assert_eq!(text(call(int(5), int(0), int(10), 0)), "");
    }

    #[test]
    fn integer_and_float_arguments_mix() {
        assert_eq!(
            text(call(Value::Float(2.5), Value::Float(0.0), int(5), 2)),
            "█"
        );
        assert_eq!(
            text(call(int(1), Value::Float(0.5), Value::Float(1.5), 2)),
            "█"
        );
    }

    #[test]
    fn any_null_argument_is_null() {
        for i in 0..4 {
            let mut args = vec![int(5), int(0), int(10), int(10)];
            args[i] = Value::Null;
            assert_eq!(bar(args).unwrap(), Value::Null, "argument {i}");
        }
        // Null wins over a type error in a later position.
        assert_eq!(
            bar(vec![Value::Null, Value::String("x".into()), int(1), int(1)]).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn the_width_cap_bounds_the_output() {
        assert_eq!(
            text(call(int(10), int(0), int(10), BAR_MAX_WIDTH))
                .chars()
                .count(),
            BAR_MAX_WIDTH as usize
        );
        for width in [BAR_MAX_WIDTH + 1, -1, i64::MAX, i64::MIN] {
            let error = call(int(5), int(0), int(10), width)
                .unwrap_err()
                .to_string();
            assert!(error.contains("NumberOutOfRange"), "{width}: {error}");
        }
    }

    #[test]
    fn an_empty_or_inverted_range_is_an_error() {
        for (min, max) in [(5, 5), (10, 0)] {
            let error = call(int(1), int(min), int(max), 4).unwrap_err().to_string();
            assert!(error.contains("InvalidArgumentValue"), "{error}");
        }
        // A finite pair whose span overflows is refused, not rendered.
        let error = call(int(0), Value::Float(-f64::MAX), Value::Float(f64::MAX), 4)
            .unwrap_err()
            .to_string();
        assert!(error.contains("InvalidArgumentValue"), "{error}");
    }

    #[test]
    fn nan_and_infinity_are_errors() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for position in 0..3 {
                let mut args = vec![int(5), int(0), int(10), int(4)];
                args[position] = Value::Float(bad);
                let error = bar(args).unwrap_err().to_string();
                assert!(error.contains("InvalidArgumentValue"), "{bad}: {error}");
            }
        }
    }

    #[test]
    fn non_numeric_arguments_are_type_errors() {
        for position in 0..4 {
            let mut args = vec![int(5), int(0), int(10), int(4)];
            args[position] = Value::String("x".into());
            let error = bar(args).unwrap_err().to_string();
            assert!(error.contains("InvalidArgumentType"), "{position}: {error}");
        }
        // The width is an INTEGER: a float width is a type error.
        let error = bar(vec![int(5), int(0), int(10), Value::Float(4.0)])
            .unwrap_err()
            .to_string();
        assert!(error.contains("InvalidArgumentType"), "{error}");
    }
}
