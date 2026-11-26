//! `--timeout DURATION` (ADR-0241, LABS-234): a per-statement wall-clock
//! bound. The CLI parses the value; the engine owns the deadline and the
//! cancel points (ADR-0692, `Connection::set_statement_timeout`).

use std::time::Duration;

/// The one invalid-value message, shared by both rejection paths so the
/// wording cannot drift.
fn not_a_duration(value: &str) -> String {
    format!("'{value}' is not a duration (try 500ms, 10s, 2m)")
}

/// Parse a `--timeout` value: a bare number is seconds, the suffixes `ms`,
/// `s` and `m` scale it, and one fractional part is allowed (`1.5s`, `0.5`).
/// `0` (in any unit) is accepted and means no bound, matching `--max-rows 0`.
///
/// The domain is integer milliseconds, not floats: exponent spellings
/// (`1e3s`) fail closed, and overflow is a checked multiply returning the
/// same usage error, never a `from_secs_f64` panic.
pub(crate) fn parse_timeout(value: &str) -> Result<Duration, String> {
    let (digits, unit_millis) = if let Some(d) = value.strip_suffix("ms") {
        (d, 1u64)
    } else if let Some(d) = value.strip_suffix('s') {
        (d, 1_000)
    } else if let Some(d) = value.strip_suffix('m') {
        (d, 60_000)
    } else {
        (value, 1_000)
    };
    let digits = digits.trim();
    // `whole[.frac]`, both plain decimal; the fraction reads as milliseconds
    // at whatever scale the unit implies (`.5s` = 500ms, `.5ms` = 0.5ms → 0).
    let (whole, frac) = match digits.split_once('.') {
        Some((w, f)) => (w, f),
        None => (digits, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(not_a_duration(value));
    }
    let parse_digits = |s: &str| -> Result<u64, String> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(not_a_duration(value));
        }
        s.parse().map_err(|_| not_a_duration(value))
    };
    let whole_ms = if whole.is_empty() {
        0
    } else {
        parse_digits(whole)?
            .checked_mul(unit_millis)
            .ok_or_else(|| not_a_duration(value))?
    };
    // The fraction's first three digits are its milliseconds at second
    // scale; scale them by the unit (for `m`, one decimal is 6 seconds).
    // The `10^digits` divisor is part of the checked chain: a fraction of
    // 20+ digits overflows the pow and is the same usage error as any other
    // overflow, never a panic.
    let frac_ms = if frac.is_empty() {
        0
    } else {
        parse_digits(frac)?
            .checked_mul(unit_millis)
            .and_then(|scaled| {
                10u64
                    .checked_pow(frac.len() as u32)
                    .and_then(|divisor| scaled.checked_div(divisor))
            })
            .ok_or_else(|| not_a_duration(value))?
    };
    // The final add is checked too: `18446744073709551.999s` passes the
    // whole-part multiply and must still be a usage error, not a wrap.
    Ok(Duration::from_millis(
        whole_ms
            .checked_add(frac_ms)
            .ok_or_else(|| not_a_duration(value))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_units_and_fractions() {
        assert_eq!(parse_timeout("10").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_timeout("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_timeout("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_timeout("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_timeout("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_timeout("0.5").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_timeout(".5s").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_timeout("1.5m").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_timeout("0").unwrap(), Duration::ZERO);
    }

    #[test]
    fn parse_rejects_garbage_and_never_panics() {
        for bad in [
            "",
            "abc",
            "10h",
            "-5s",
            "1..5",
            "ms",
            "NaN",
            "inf",
            "1e3s",
            "1e300",
            "99999999999999999999999m",
            "1.5.2s",
            ".",
            // A 20+-digit fraction overflows the 10^digits divisor.
            "0.00000000000000000001s",
            "0.12345678901234567890s",
            "1.99999999999999999999999",
            // whole_ms passes checked_mul, but adding the fraction
            // overflows u64.
            "18446744073709551.999s",
        ] {
            assert!(parse_timeout(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parse_accepts_the_u64_millisecond_boundary() {
        // The largest representable whole part, with no fraction to add.
        assert_eq!(
            parse_timeout("18446744073709551").unwrap(),
            Duration::from_millis(u64::MAX - 615)
        );
        assert_eq!(
            parse_timeout("18446744073709551615ms").unwrap(),
            Duration::from_millis(u64::MAX)
        );
        // One millisecond past the boundary is a usage error, not a wrap.
        assert!(parse_timeout("18446744073709551616ms").is_err());
        assert!(parse_timeout("18446744073709552").is_err());
    }
}
