//! `--width` (LABS-399): the total display width the human table wraps
//! to — its parsing, its floor, and the terminal-width auto-detect. The
//! one home of the width policy: `main` reads
//! [`effective_width`] and nothing else.

/// The narrowest width worth wrapping to: a narrower terminal is not a
/// grid worth laying out, and the natural width is the honest fallback.
/// One floor for every source — the explicit flag rejects below it loudly
/// ([`parse_width`]), the auto-detect drops a narrower answer quietly.
const MIN_EFFECTIVE_WIDTH: usize = 20;

/// The width the eggs lay out to when neither the terminal nor `COLUMNS`
/// can tell: the classic 80-column terminal. The one such default; the
/// human table is unaffected (a pipe keeps the natural width).
#[cfg(feature = "eggs")]
pub(crate) const DEFAULT_COLUMNS: usize = 80;

/// Parse a `--width` value: a terminal width in columns. Below
/// [`MIN_EFFECTIVE_WIDTH`] — like `0` — is rejected loudly with the
/// remediation, never silently discarded: an explicit caller gets an
/// error, not a run that ignored them.
pub(crate) fn parse_width(raw: &str) -> Result<usize, String> {
    let value: usize = raw
        .trim()
        .parse()
        .map_err(|_| format!("`{raw}` is not a column count"))?;
    if value < MIN_EFFECTIVE_WIDTH {
        return Err(format!(
            "a width under {MIN_EFFECTIVE_WIDTH} columns cannot hold a table; omit --width for the terminal default"
        ));
    }
    Ok(value)
}

/// The width a run lays out to: an explicit `--width` wins; otherwise the
/// terminal is asked. `None` (a pipe, no answer) keeps the natural width —
/// piped output stays byte-stable.
pub(crate) fn effective_width(flag: Option<usize>) -> Option<usize> {
    flag.or_else(terminal_width)
}

/// The terminal width a run lays out to: [`terminal_columns`], dropped when
/// narrower than the floor (a terminal that small is not a grid worth
/// laying out).
fn terminal_width() -> Option<usize> {
    terminal_columns().filter(|w| *w >= MIN_EFFECTIVE_WIDTH)
}

/// The width an egg lays out to: [`terminal_columns`], or
/// [`DEFAULT_COLUMNS`] when neither the terminal nor `COLUMNS` can tell.
/// The one home of that default — the eggs receive this resolved number and
/// apply no default of their own (each still applies its own floor or
/// clamp to a known width).
#[cfg(feature = "eggs")]
pub(crate) fn egg_columns() -> usize {
    resolve_columns(terminal_columns())
}

/// The policy half of [`egg_columns`], pure: a probed width stands, an
/// unknown one (`None`) takes [`DEFAULT_COLUMNS`]. Zero never reaches here —
/// the probe maps it to `None` ([`nonzero`]).
#[cfg(feature = "eggs")]
fn resolve_columns(probed: Option<usize>) -> usize {
    probed.unwrap_or(DEFAULT_COLUMNS)
}

/// The terminal's raw column count, asked from the terminal itself: a
/// `TIOCGWINSZ` probe on stdout. That is the one source that works by
/// default — `COLUMNS` is a shell variable that a child process does not
/// inherit — and it doubles as the TTY check (the probe fails on a pipe,
/// which is exactly when the natural width should win). `COLUMNS` remains
/// the fallback for a caller that exports it over a redirected stdout. The
/// one probe in the product ([`egg_columns`] resolves it for the hidden
/// eggs). A zero answer — `TIOCGWINSZ` reporting 0 columns, `COLUMNS=0` — is
/// "cannot tell" (`None`), one policy for both sources.
#[cfg(unix)]
pub(crate) fn terminal_columns() -> Option<usize> {
    // SAFETY: a plain TIOCGWINSZ probe on the stdout descriptor with a
    // valid out-parameter; the kernel fills it or returns an error.
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let asked = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    asked
        .then(|| nonzero(usize::from(ws.ws_col)))
        .flatten()
        .or_else(columns_env)
}

/// The non-unix fallback: only an exported `COLUMNS` answers.
#[cfg(not(unix))]
pub(crate) fn terminal_columns() -> Option<usize> {
    columns_env()
}

/// The exported `COLUMNS` fallback, unfloored but never zero — one spelling
/// for both cfg variants.
fn columns_env() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()?
        .parse::<usize>()
        .ok()
        .and_then(nonzero)
}

/// The one zero policy: a zero column count is "cannot tell".
fn nonzero(n: usize) -> Option<usize> {
    (n > 0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_sub_floor_widths_loudly() {
        // An explicit caller gets the error, never a silently ignored value.
        assert!(parse_width("0").is_err());
        assert!(parse_width("10").is_err());
        assert_eq!(parse_width("20"), Ok(MIN_EFFECTIVE_WIDTH));
        assert_eq!(parse_width("120"), Ok(120));
        assert!(parse_width("wide").is_err());
    }

    #[test]
    fn effective_width_prefers_the_flag_then_the_terminal() {
        // The flag wins when present; the terminal answers on a pipe only
        // through COLUMNS (absent here) — so None means natural width.
        assert_eq!(effective_width(Some(50)), Some(50));
    }

    #[test]
    fn zero_is_unknown() {
        assert_eq!(nonzero(0), None);
        assert_eq!(nonzero(132), Some(132));
    }

    #[cfg(feature = "eggs")]
    #[test]
    fn eggs_take_the_probe_or_the_default() {
        assert_eq!(resolve_columns(Some(132)), 132);
        assert_eq!(resolve_columns(None), DEFAULT_COLUMNS);
    }
}
