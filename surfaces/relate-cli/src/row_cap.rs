//! The row cap for agent-driven runs (ADR-0196): the default-cap policy
//! and the `RELATE_MAX_ROWS` variable — the detection vocabulary and
//! the agent-mode resolution (ADR-0407's ladder) live in `agent_vars`,
//! consumed here as one decided fact. The cut's record is the
//! formatter's one `RowCap` bundle (ADR-0403) — the machine modes render
//! it in-band; the stderr warning here is the human's channel and the
//! only one the `csv`, `cypher` and `table` modes have.

use relate_formatter::RowCap;

/// The cap applied when agent mode is in effect and nothing else says
/// otherwise (ADR-0196).
pub(crate) const DEFAULT_AGENT_MAX_ROWS: usize = 100;

/// The effective row cap for a run (ADR-0196/0405): `None` is unlimited.
///
/// An explicit `--max-rows` wins, then `RELATE_MAX_ROWS`; with
/// neither, the cap is [`DEFAULT_AGENT_MAX_ROWS`] when the agent mode
/// (already resolved — detection gated on the terminal, or an explicit
/// `--agent-mode`/`RELATE_AGENT_MODE=1` override, ADR-0407) is on.
/// `0` always means unlimited.
pub(crate) fn effective_max_rows(
    flag: Option<usize>,
    env: &dyn Fn(&str) -> Option<String>,
    agent_on: bool,
) -> Result<Option<usize>, String> {
    let explicit = match flag {
        Some(n) => Some(n),
        None => env_max_rows(env)?,
    };
    Ok(match explicit {
        Some(0) => None,
        Some(n) => Some(n),
        None => agent_on.then_some(DEFAULT_AGENT_MAX_ROWS),
    })
}

/// `RELATE_MAX_ROWS`, if set to something other than blanks.
fn env_max_rows(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<usize>, String> {
    let Some(raw) = env("RELATE_MAX_ROWS").filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    parse_max_rows(raw.trim())
        .map(Some)
        .map_err(|e| format!("invalid RELATE_MAX_ROWS: {e}"))
}

/// [`effective_max_rows`] against the real environment, over the
/// already-resolved agent mode (ADR-0407 — one resolution, consumed).
pub(crate) fn effective_max_rows_from_env(
    flag: Option<usize>,
    agent_on: bool,
) -> Result<Option<usize>, String> {
    effective_max_rows(flag, &|k| std::env::var(k).ok(), agent_on)
}

/// The stderr warning for a capped result (ADR-0196): it names both counts
/// and the opt-out, so a short result is never mistaken for a complete one.
/// The machine modes carry the same fact in-band as the `rowCap` record
/// (ADR-0403 — kept is the rendered rows themselves); this warning stays
/// the human's channel and the only one the `csv`, `cypher` and `table`
/// modes have.
pub(crate) fn truncation_warning(cap: RowCap, kept: usize) -> String {
    format!(
        "warning: result truncated to {kept} rows (the statement returned {}); pass --max-rows 0 for all rows, or page with SKIP/LIMIT",
        cap.total
    )
}

/// The cap decision (ADR-0196), one derivation: the rows to render and the
/// cut's record — `None` in the pair means no cut (the rendered rows are
/// all of them). Decided from the count alone, before any row is
/// converted, so the cap bounds the conversion work. The record is the
/// formatter's one row-cap fact (ADR-0403): the total it cannot derive;
/// the machine modes read kept off their rows.
pub(crate) fn truncation(total: usize, max_rows: Option<usize>) -> (usize, Option<RowCap>) {
    let Some(cap) = max_rows else {
        return (total, None);
    };
    (cap.min(total), (total > cap).then_some(RowCap { total }))
}

/// Parse a `--max-rows` / `RELATE_MAX_ROWS` value: a non-negative integer.
pub(crate) fn parse_max_rows(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("'{value}' is not a non-negative integer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;

    #[test]
    fn default_cap_follows_the_resolved_agent_mode() {
        // The mode arrives decided (ADR-0407): on — from detection gated on
        // the terminal, or an explicit override — caps; off does not.
        assert_eq!(effective_max_rows(None, &env_of(&[]), true), Ok(Some(100)));
        assert_eq!(effective_max_rows(None, &env_of(&[]), false), Ok(None));
    }

    #[test]
    fn explicit_values_win_and_zero_is_unlimited() {
        assert_eq!(effective_max_rows(Some(5), &env_of(&[]), true), Ok(Some(5)));
        assert_eq!(effective_max_rows(Some(0), &env_of(&[]), true), Ok(None));
        // An explicit cap applies with the agent mode off too.
        assert_eq!(
            effective_max_rows(Some(5), &env_of(&[]), false),
            Ok(Some(5))
        );
    }

    #[test]
    fn env_var_sets_the_default_and_the_flag_overrides_it() {
        let env = env_of(&[("RELATE_MAX_ROWS", "7")]);
        assert_eq!(effective_max_rows(None, &env, true), Ok(Some(7)));
        assert_eq!(effective_max_rows(Some(3), &env, true), Ok(Some(3)));
        let zero = env_of(&[("RELATE_MAX_ROWS", "0")]);
        assert_eq!(effective_max_rows(None, &zero, true), Ok(None));
        // A blank value falls through to the resolved mode's default.
        let blank = env_of(&[("RELATE_MAX_ROWS", "  ")]);
        assert_eq!(effective_max_rows(None, &blank, true), Ok(Some(100)));
    }

    #[test]
    fn truncation_is_decided_from_the_count_alone() {
        assert!(truncation(5, None).1.is_none());
        assert!(truncation(5, Some(5)).1.is_none());
        assert!(truncation(5, Some(9)).1.is_none());
        let (kept, cut) = truncation(5, Some(2));
        assert_eq!(kept, 2);
        let cut = cut.expect("5 > 2");
        assert_eq!(cut.total, 5);
        assert_eq!(
            truncation_warning(cut, kept),
            "warning: result truncated to 2 rows (the statement returned 5); pass --max-rows 0 for all rows, or page with SKIP/LIMIT"
        );
    }

    #[test]
    fn a_bad_env_value_is_an_error_naming_the_variable() {
        let env = env_of(&[("RELATE_MAX_ROWS", "lots")]);
        let err = effective_max_rows(None, &env, false).unwrap_err();
        assert!(err.contains("RELATE_MAX_ROWS"), "{err}");
        assert!(parse_max_rows("-1").is_err());
        assert_eq!(parse_max_rows("12"), Ok(12));
    }
}
