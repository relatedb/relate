//! Numeric function helpers (`round`, unary int/float adapters).

use relate_core::{ArithmeticFailure, Error, Result, Value};

/// The largest `round()` precision the f64-backed engine supports: an f64
/// carries at most ~15–17 significant decimal digits, so a larger decimal
/// precision cannot express anything the value does not already have
/// (ADR-0336).
const MAX_ROUND_PRECISION: i64 = 15;
/// Once a float's ulp is at least 1 (|x| >= 2^52), every value of that
/// magnitude is already an integer — `Math.round`'s `floor(x + 0.5)`
/// would instead round an odd ulp up, and live returns the identity.
const NO_FRACTION_LIMIT: f64 = 4_503_599_627_370_496.0;

/// The `round()` mode vocabulary (Neo4j's eight names), parsed once at the
/// call boundary so the rounding dispatch matches on a typed mode instead of
/// re-classifying the raw strings (the `TruncUnit::parse` shape in
/// relate-core).
#[derive(Clone, Copy)]
enum RoundingMode {
    Up,
    Down,
    Ceiling,
    Floor,
    /// The wire spelling `HALF_UP`: ties away from zero (Java's
    /// `RoundingMode.HALF_UP`).
    HalfUp,
    HalfDown,
    HalfEven,
    Unnecessary,
}

impl RoundingMode {
    /// Parses a wire spelling; any other spelling is the
    /// `UnsupportedRoundingMode` error.
    fn parse(mode: &str) -> Result<RoundingMode> {
        Ok(match mode {
            "UP" => RoundingMode::Up,
            "DOWN" => RoundingMode::Down,
            "CEILING" => RoundingMode::Ceiling,
            "FLOOR" => RoundingMode::Floor,
            "HALF_UP" => RoundingMode::HalfUp,
            "HALF_DOWN" => RoundingMode::HalfDown,
            "HALF_EVEN" => RoundingMode::HalfEven,
            "UNNECESSARY" => RoundingMode::Unnecessary,
            _ => return Err(Error::execution("UnsupportedRoundingMode")),
        })
    }

    /// The digit run is unsigned (the sign is factored out), so the two
    /// directional modes read the sign once, here: CEILING and FLOOR swap
    /// on a negative value and land on their unsigned UP/DOWN spellings —
    /// ceiling a positive moves away from zero, ceiling a negative stays
    /// at the truncation; floor is the mirror. The result is
    /// sign-resolved: no CEILING/FLOOR state survives, so the digit walk
    /// matches only reachable modes.
    fn for_sign(self, negative: bool) -> AppliedMode {
        match (self, negative) {
            (RoundingMode::Up, _)
            | (RoundingMode::Ceiling, false)
            | (RoundingMode::Floor, true) => AppliedMode::Up,
            (RoundingMode::Down, _)
            | (RoundingMode::Ceiling, true)
            | (RoundingMode::Floor, false) => AppliedMode::Down,
            (RoundingMode::HalfUp, _) => AppliedMode::HalfUp,
            (RoundingMode::HalfDown, _) => AppliedMode::HalfDown,
            (RoundingMode::HalfEven, _) => AppliedMode::HalfEven,
            (RoundingMode::Unnecessary, _) => AppliedMode::Unnecessary,
        }
    }
}

/// A rounding mode with the sign resolved — CEILING/FLOOR have already
/// landed on their unsigned UP/DOWN spellings, so [`increment`] matches
/// only reachable states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppliedMode {
    Up,
    Down,
    HalfUp,
    HalfDown,
    HalfEven,
    Unnecessary,
}

/// The `round()` precision argument, normalized once: an INTEGER and a FLOAT
/// spelling both flow through [`Value::as_number`], so one range-check pair
/// decides `-1` and `-1.0` alike (both a range rejection, not a type one).
/// A fraction truncates toward zero — the reference engine's
/// `round(3.14…, -0.02)` is `round(3.14…, 0)` — and a value that is not a
/// truncatable number keeps the `must be INTEGER` type rejection.
fn round_precision(v: &Value) -> Result<usize> {
    let Some(n) = v.as_number() else {
        return Err(Error::invalid_argument_type(
            "round precision must be INTEGER",
        ));
    };
    if !n.is_finite() {
        return Err(Error::invalid_argument_type(
            "round precision must be INTEGER",
        ));
    }
    let n = n.trunc();
    if n < 0.0 {
        return Err(Error::execution(
            "NumericValueOutOfRange: round precision must be non-negative",
        ));
    }
    if n > MAX_ROUND_PRECISION as f64 {
        return Err(Error::execution(format!(
            "NumericValueOutOfRange: round precision must be at most {MAX_ROUND_PRECISION}"
        )));
    }
    // The range gate above bounds n to 0..=15, so this is the one checked
    // conversion — callers carry a plain `usize` with nothing to cast.
    Ok(n as usize)
}

pub(crate) fn round_function(vals: &[Value]) -> Result<Value> {
    if vals.is_empty() || vals.len() > 3 {
        return Err(Error::syntax(
            "InvalidNumberOfArguments: round() requires one to three arguments",
        ));
    }
    if vals.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let value = vals[0]
        .as_number()
        .ok_or_else(|| Error::invalid_argument_type("round value must be numeric"))?;
    let precision = match vals.get(1) {
        None => 0,
        Some(v) => round_precision(v)?,
    };
    let mode = match vals.get(2) {
        None => None,
        Some(Value::String(mode)) => Some(RoundingMode::parse(mode)?),
        _ => return Err(Error::invalid_argument_type("round mode must be STRING")),
    };
    if !value.is_finite() {
        return Ok(Value::Float(value));
    }
    let rounded = match (mode, precision) {
        // Java's `Math.round`: `floor(x + 0.5)` — ties toward positive
        // infinity, which is why `round(-2.5)` is -2.0 while the wire
        // HALF_UP mode (ties away from zero) gives -3.0. Once the ulp is
        // at least 1 (|x| >= 2^52) every float is already an integer and
        // the addition would round an odd ulp up instead (live Neo4j
        // 2026.10.1: `round(4503599627370497.0)` is the identity).
        (None, 0) if value.abs() >= NO_FRACTION_LIMIT => value,
        (None, 0) => (value + 0.5).floor(),
        // An explicit precision with no mode is HALF_UP on the decimal
        // spelling (away from zero): live Neo4j 2026.10.1 rounds
        // `round(-2.55, 1)` to -2.6.
        (mode, _) => round_decimal(value, precision, mode.unwrap_or(RoundingMode::HalfUp))?,
    };
    Ok(Value::Float(rounded))
}

/// `round()` on the decimal spelling: Java's
/// `BigDecimal.valueOf(value).setScale(precision, mode)`, which rounds the
/// shortest decimal that round-trips (Rust's `Display`, the same digits as
/// Java's `Double.toString`) rather than the binary image. `round(265.335,
/// 2)` is 265.34 because the shortest spelling is `265.335`, although the
/// exact binary value is 265.33499999999997 (live Neo4j 2026.10.1).
fn round_decimal(value: f64, precision: usize, mode: RoundingMode) -> Result<f64> {
    let (negative, digits, scale) = decimal_digits(&value.to_string());
    if scale <= precision {
        return Ok(value);
    }
    let (kept, dropped) = digits.split_at(digits.len() - (scale - precision));
    let last_kept = kept.as_bytes()[kept.len() - 1] - b'0';
    // `scale > precision` keeps at least one dropped digit — the split
    // never hands `increment` an empty run, so the first digit is read
    // here, at the invariant's owner.
    let (first, rest) = (dropped.as_bytes()[0] - b'0', &dropped.as_bytes()[1..]);
    let mode = mode.for_sign(negative);
    let rounded = if increment(first, rest, last_kept, mode)? {
        increment_digits(kept)
    } else {
        kept.to_string()
    };
    render_decimal(negative, &rounded, precision)
        .parse::<f64>()
        .map_err(|_| Error::execution("round() rendered an invalid decimal"))
}

/// Split a finite f64's `Display` spelling into sign, digit run and scale
/// (the fraction digit count). `Display` never uses exponent notation, so
/// the scale is exactly the digits after the decimal point.
fn decimal_digits(text: &str) -> (bool, String, usize) {
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole);
    digits.push_str(fraction);
    (negative, digits, fraction.len())
}

/// Whether the dropped digit run rounds `kept` away from its truncation,
/// under `mode`. `first` is the first dropped digit (the run is non-empty —
/// `round_decimal` owns that invariant) and `rest` the digits behind it;
/// `last_kept` is the last kept digit's value, which HALF_EVEN reads for
/// the tie case. `mode` arrives sign-resolved ([`AppliedMode`], from
/// [`RoundingMode::for_sign`]).
fn increment(first: u8, rest: &[u8], last_kept: u8, mode: AppliedMode) -> Result<bool> {
    let after_nonzero = rest.iter().any(|digit| *digit != b'0');
    let any_nonzero = first != 0 || after_nonzero;
    Ok(match mode {
        AppliedMode::Up => any_nonzero,
        AppliedMode::Down => false,
        AppliedMode::HalfUp => first >= 5,
        AppliedMode::HalfDown => first > 5 || (first == 5 && after_nonzero),
        AppliedMode::HalfEven => first > 5 || (first == 5 && (after_nonzero || last_kept % 2 == 1)),
        AppliedMode::Unnecessary => {
            if any_nonzero {
                return Err(Error::arithmetic(ArithmeticFailure::RoundingNecessary));
            }
            false
        }
    })
}

/// Add one to a run of decimal digits, growing it on an all-nines carry
/// (`"99"` → `"100"`).
fn increment_digits(digits: &str) -> String {
    let mut bytes = digits.as_bytes().to_vec();
    for index in (0..bytes.len()).rev() {
        if bytes[index] == b'9' {
            bytes[index] = b'0';
        } else {
            bytes[index] += 1;
            return String::from_utf8(bytes).expect("decimal digits are ASCII");
        }
    }
    let mut grown = String::with_capacity(bytes.len() + 1);
    grown.push('1');
    grown.push_str(&"0".repeat(bytes.len()));
    grown
}

/// Render a sign, digit run and decimal place count back as a decimal
/// number the f64 parser reads exactly.
fn render_decimal(negative: bool, digits: &str, precision: usize) -> String {
    let mut out = String::with_capacity(digits.len() + precision + 2);
    if negative {
        out.push('-');
    }
    if precision == 0 {
        out.push_str(digits);
    } else if digits.len() > precision {
        let (whole, fraction) = digits.split_at(digits.len() - precision);
        out.push_str(whole);
        out.push('.');
        out.push_str(fraction);
    } else {
        out.push_str("0.");
        out.push_str(&"0".repeat(precision - digits.len()));
        out.push_str(digits);
    }
    out
}

/// Generic numeric unary op. The integer arm must be a checked op (`None`
/// on overflow — e.g. `abs(i64::MIN)`) so extreme integers surface an honest
/// range error instead of unwinding into the host.
pub(crate) fn num_unary(
    vals: &[Value],
    on_int: impl Fn(i64) -> Option<i64>,
    on_float: impl Fn(f64) -> f64,
) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Int(i)) => {
            Value::Int(on_int(*i).ok_or_else(|| Error::ty("integer out of range"))?)
        }
        Some(Value::Float(f)) => Value::Float(on_float(*f)),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("expected number")),
    })
}

pub(crate) fn float_unary(vals: &[Value], f: impl Fn(f64) -> f64) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Int(i)) => Value::Float(f(*i as f64)),
        Some(Value::Float(x)) => Value::Float(f(*x)),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("expected number")),
    })
}

/// The shared FLOAT → INTEGER conversion (ADR-0168, ADR-0175), used by the
/// integer-argument sites (`range()`, `substring()` / `left()` /
/// `right()`, list-slice bounds) and the `toInteger()` /
/// `toIntegerOrNull()` / `toIntegerList()` family. It is Java's
/// `(long) d`, which Neo4j applies through `NumberValue.longValue()`, and
/// Rust's saturating `as` cast has exactly those semantics: NaN converts
/// to 0, values at or beyond the `i64` limits (±Infinity included)
/// saturate to those limits, and every other value truncates toward zero.
/// A float never fails to convert.
pub(crate) fn float_to_i64(f: f64) -> i64 {
    f as i64
}

/// ADR-0175's shared integer-argument coercion (`range()`,
/// `substring()` / `left()` / `right()`, list-slice bounds): an INTEGER
/// passes through, a FLOAT converts via [`float_to_i64`], and anything
/// else — null or non-numeric — is `None` so the call site applies its
/// own policy (type error, range gate, or default fallback). An absent
/// argument is the caller's `None`, never this function's.
pub(crate) fn int_arg(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Float(f) => Some(float_to_i64(*f)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0336: the precision is bounded to 0..=15 — a larger one gets the
    /// same NumericValueOutOfRange treatment as a negative one, never the
    /// NaN the overflow-prone scale computation used to produce.
    #[test]
    fn round_precision_is_bounded() {
        let too_large =
            Error::execution("NumericValueOutOfRange: round precision must be at most 15");
        for precision in [16, 400, i64::MAX] {
            assert_eq!(
                round_function(&[Value::Float(2.5), Value::Int(precision)]).unwrap_err(),
                too_large,
                "precision {precision}"
            );
        }
        // One normalized range check: the INTEGER and FLOAT spellings share
        // both the negative and the upper-bound class (`round(x, -1)` and
        // `round(x, -1.0)` used to diverge into "non-negative" vs "must be
        // INTEGER").
        let negative =
            Error::execution("NumericValueOutOfRange: round precision must be non-negative");
        for precision in [Value::Int(-1), Value::Float(-1.0), Value::Float(-1.5)] {
            assert_eq!(
                round_function(&[Value::Float(2.5), precision.clone()]).unwrap_err(),
                negative,
                "precision {precision:?}"
            );
        }
        for precision in [Value::Int(16), Value::Float(16.0)] {
            assert_eq!(
                round_function(&[Value::Float(2.5), precision.clone()]).unwrap_err(),
                too_large,
                "precision {precision:?}"
            );
        }
        assert_eq!(
            round_function(&[Value::Float(2.5), Value::Int(15)]).unwrap(),
            Value::Float(2.5)
        );
        assert_eq!(
            round_function(&[Value::Float(2.5)]).unwrap(),
            Value::Float(3.0)
        );
        // The implicit form is HALF_UP toward positive infinity, not the
        // wire `HALF_UP` (away from zero): -2.5 rounds to -2.0.
        assert_eq!(
            round_function(&[Value::Float(-2.5)]).unwrap(),
            Value::Float(-2.0)
        );
        assert_eq!(
            round_function(&[Value::Float(2.5), Value::Int(2)]).unwrap(),
            Value::Float(2.5)
        );
        assert_eq!(
            round_function(&[Value::Null, Value::Int(400)]).unwrap(),
            Value::Null
        );
    }

    /// `round()` rounds the shortest decimal spelling, not the binary image
    /// (Java's `BigDecimal.valueOf(...).setScale`), and an explicit
    /// precision with no mode is HALF_UP away from zero. Every pair is
    /// live-verified against Neo4j 2026.10.1.
    #[test]
    fn round_rounds_the_shortest_decimal_spelling() {
        let cases: &[(f64, i64, Option<&str>, f64)] = &[
            (265.335, 2, None, 265.34),
            (1.005, 2, None, 1.01),
            (2.675, 2, None, 2.68),
            (1.15, 1, None, 1.2),
            (1.45, 1, None, 1.5),
            (0.35, 1, None, 0.4),
            (-2.55, 1, None, -2.6),
            (2.55, 1, None, 2.6),
            (-1.25, 1, None, -1.3),
            (1.25, 1, None, 1.3),
            (2.675, 0, Some("HALF_UP"), 3.0),
            (2.675, 0, Some("HALF_EVEN"), 3.0),
            (1.235, 2, Some("HALF_EVEN"), 1.24),
            (1.245, 2, Some("HALF_EVEN"), 1.24),
            (1.005, 2, Some("DOWN"), 1.0),
            (-1.005, 2, Some("UP"), -1.01),
        ];
        for &(value, precision, mode, want) in cases {
            let mut args = vec![Value::Float(value), Value::Int(precision)];
            if let Some(mode) = mode {
                args.push(Value::String(mode.into()));
            }
            assert_eq!(
                round_function(&args).unwrap(),
                Value::Float(want),
                "round({value}, {precision}, {mode:?})"
            );
        }
        // Precision 0 with no mode stays Java's Math.round (floor(x + 0.5),
        // ties toward positive infinity), which is not the wire HALF_UP.
        assert_eq!(
            round_function(&[Value::Float(-2.55), Value::Int(0)]).unwrap(),
            Value::Float(-3.0)
        );
        assert_eq!(
            round_function(&[Value::Float(-2.5), Value::Int(0)]).unwrap(),
            Value::Float(-2.0)
        );
    }

    /// A float precision is accepted and truncated, matching Neo4j:
    /// `round(1.23456, 2.5)` is 1.23, and a non-finite value stays itself.
    #[test]
    fn round_accepts_a_float_precision_by_truncation() {
        assert_eq!(
            round_function(&[Value::Float(1.23456), Value::Float(2.5)]).unwrap(),
            Value::Float(1.23)
        );
        assert_eq!(
            round_function(&[Value::Float(f64::INFINITY), Value::Float(0.0)]).unwrap(),
            Value::Float(f64::INFINITY)
        );
        // Truncation is toward zero, so a negative fraction below one is
        // precision 0: the reference engine's `round(x, -0.02)` is
        // `round(x, 0)`.
        assert_eq!(
            round_function(&[Value::Float(3.7), Value::Float(-0.02)]).unwrap(),
            Value::Float(4.0)
        );
        assert!(round_function(&[Value::Float(1.23), Value::Float(-1.0)]).is_err());
    }

    /// The `UNNECESSARY` mode keeps its typed failure at a bounded
    /// precision.
    #[test]
    fn round_unnecessary_keeps_its_typed_failure() {
        assert_eq!(
            round_function(&[
                Value::Float(2.5),
                Value::Int(0),
                Value::String("UNNECESSARY".into())
            ])
            .unwrap_err(),
            Error::arithmetic(ArithmeticFailure::RoundingNecessary)
        );
        assert_eq!(
            round_function(&[
                Value::Float(2.0),
                Value::Int(0),
                Value::String("UNNECESSARY".into())
            ])
            .unwrap(),
            Value::Float(2.0)
        );
    }

    /// The mode vocabulary parses once at the boundary: every accepted
    /// spelling reaches a typed variant, and any other spelling keeps the
    /// `UnsupportedRoundingMode` error.
    #[test]
    fn round_mode_parses_the_wire_vocabulary_once() {
        for mode in [
            "UP",
            "DOWN",
            "CEILING",
            "FLOOR",
            "HALF_UP",
            "HALF_DOWN",
            "HALF_EVEN",
            "UNNECESSARY",
        ] {
            assert!(RoundingMode::parse(mode).is_ok(), "{mode}");
        }
        assert_eq!(
            round_function(&[
                Value::Float(2.5),
                Value::Int(0),
                Value::String("half_up".into())
            ])
            .unwrap_err(),
            Error::execution("UnsupportedRoundingMode")
        );
    }
}
