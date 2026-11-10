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
const DEFAULT_COLUMNS: usize = 80;

/// The row count the eggs lay out to when neither the terminal nor `LINES`
/// can tell: the classic 24-row terminal. The one such default.
#[cfg(feature = "eggs")]
const DEFAULT_ROWS: usize = 24;

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

/// The terminal's size as probed: each axis `None` when neither the
/// terminal nor its variable can tell. The one probe in the product — the
/// human table reads [`terminal_columns`] from it, the hidden eggs resolve
/// the whole of it through [`egg_size`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Size {
    columns: Option<usize>,
    #[cfg_attr(not(feature = "eggs"), allow(dead_code))]
    rows: Option<usize>,
}

/// The terminal's raw size, asked from the terminal itself: one
/// `TIOCGWINSZ` probe on stdout. That is the one source that works by
/// default — `COLUMNS`/`LINES` are shell variables a child process does not
/// inherit — and it doubles as the TTY check (the probe fails on a pipe,
/// which is exactly when the natural width should win). `COLUMNS`/`LINES`
/// remain the per-axis fallback for a caller that exports them over a
/// redirected stdout. One zero policy for every source and both axes: a
/// zero or unparsable answer is "cannot tell" (`None`), never a size.
fn probe() -> Size {
    size_from(ioctl_size(), &|k| std::env::var(k).ok())
}

/// The policy half of [`probe`], pure: per axis, the terminal's answer
/// ([`nonzero`]) wins, then the exported variable (trimmed, [`nonzero`]).
fn size_from(ws: Option<(usize, usize)>, env: &dyn Fn(&str) -> Option<String>) -> Size {
    let axis = |from_tty: Option<usize>, var: &str| {
        from_tty.and_then(nonzero).or_else(|| {
            env(var)
                .and_then(|v| v.trim().parse::<usize>().ok())
                .and_then(nonzero)
        })
    };
    Size {
        columns: axis(ws.map(|w| w.0), "COLUMNS"),
        rows: axis(ws.map(|w| w.1), "LINES"),
    }
}

/// The terminal's raw column count ([`probe`]).
pub(crate) fn terminal_columns() -> Option<usize> {
    probe().columns
}

/// A `TIOCGWINSZ` probe on stdout as `(columns, rows)`; `None` when stdout
/// is not a terminal.
#[cfg(unix)]
fn ioctl_size() -> Option<(usize, usize)> {
    // SAFETY: a plain TIOCGWINSZ probe on the stdout descriptor with a
    // valid out-parameter; the kernel fills it or returns an error.
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let asked = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    asked.then(|| (usize::from(ws.ws_col), usize::from(ws.ws_row)))
}

/// The non-unix twin: only the exported variables answer.
#[cfg(not(unix))]
fn ioctl_size() -> Option<(usize, usize)> {
    None
}

/// The size an egg lays out to, `(columns, rows)`: the [`probe`], each
/// unknown axis taking its one default ([`DEFAULT_COLUMNS`],
/// [`DEFAULT_ROWS`]). The one home of those defaults — the eggs receive
/// these resolved numbers and apply no default of their own (each still
/// applies its own floor or clamp).
#[cfg(feature = "eggs")]
pub(crate) fn egg_size() -> (usize, usize) {
    resolve_size(probe())
}

/// The policy half of [`egg_size`], pure.
#[cfg(feature = "eggs")]
fn resolve_size(probed: Size) -> (usize, usize) {
    (
        probed.columns.unwrap_or(DEFAULT_COLUMNS),
        probed.rows.unwrap_or(DEFAULT_ROWS),
    )
}

/// The one zero policy: a zero column count is "cannot tell".
fn nonzero(n: usize) -> Option<usize> {
    (n > 0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;

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

    #[test]
    fn the_terminal_wins_then_the_trimmed_variable_and_zero_is_unknown() {
        let both = size_from(
            Some((132, 50)),
            &env_of(&[("COLUMNS", "10"), ("LINES", "9")]),
        );
        assert_eq!((both.columns, both.rows), (Some(132), Some(50)));
        let vars = size_from(None, &env_of(&[("COLUMNS", " 100 "), ("LINES", "\t30\n")]));
        assert_eq!((vars.columns, vars.rows), (Some(100), Some(30)));
        // A zero terminal axis falls to the variable; zero or junk variables
        // are unknown.
        let zero = size_from(
            Some((0, 0)),
            &env_of(&[("COLUMNS", "0"), ("LINES", "junk")]),
        );
        assert_eq!((zero.columns, zero.rows), (None, None));
        let mixed = size_from(Some((0, 40)), &env_of(&[("COLUMNS", "90")]));
        assert_eq!((mixed.columns, mixed.rows), (Some(90), Some(40)));
    }

    #[cfg(feature = "eggs")]
    #[test]
    fn eggs_take_the_probe_or_the_defaults() {
        let known = Size {
            columns: Some(132),
            rows: Some(50),
        };
        assert_eq!(resolve_size(known), (132, 50));
        let unknown = Size {
            columns: None,
            rows: None,
        };
        assert_eq!(resolve_size(unknown), (DEFAULT_COLUMNS, DEFAULT_ROWS));
    }
}
