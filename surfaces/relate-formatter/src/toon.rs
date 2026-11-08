//! The TOON renderer (ADR-0239): one `rows[N]{col,…}:` header plus one
//! comma-separated line per row.

use std::io::{self, Write};

use indexmap::IndexMap;
use relate_model::wire;
use relate_model::CypherValue;

use crate::{cell, Formatter, RowCap, StatementMeta};

impl Formatter {
    /// TOON tabular-array rendering (ADR-0239): one header line
    /// `rows[N]{col1,col2,…}:` then N two-space-indented lines of
    /// comma-separated values. Scalars print bare (strings only when they
    /// are TOON-safe; non-finite floats as their `"NaN"`/`"Inf"`/`"-Inf"`
    /// JSON strings, like the other machine modes); nested values print as
    /// inline compact JSON — the same projection `jsonl` uses. The column
    /// list is structural, so `headers` does not apply.
    ///
    /// A plan-carrying statement (ADR-0345) renders the structured plan
    /// tree as one `plan:` entry with the tree beneath it, rendered by this
    /// module's own nested-value rule (inline compact JSON) — the tree
    /// itself, not the pre-rendered prose. The complete notification list
    /// (ADR-0363 decision 9) follows as one `notifications:` entry beside
    /// `plan:`, the same nested-value rule.
    pub(crate) fn format_toon(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        // The rows block first, then the plan block (LABS-373): a PROFILE
        // run keeps its data in the TOON document; an EXPLAIN run has no
        // columns (ADR-0359), so only the plan block prints.
        if !columns.is_empty() {
            let cols: Vec<String> = columns
                .iter()
                .map(|c| toon_token(c))
                .collect::<io::Result<_>>()?;
            writeln!(w, "rows[{}]{{{}}}:", rows.len(), cols.join(","))?;
            for row in rows {
                let vals: Vec<String> = columns
                    .iter()
                    .map(|col| toon_value(cell(row, col)))
                    .collect::<io::Result<_>>()?;
                writeln!(w, "  {}", vals.join(","))?;
            }
        }
        if let Some(view) = meta.plan {
            writeln!(w, "plan:")?;
            writeln!(w, "  {}", toon_value(view.wire)?)?;
            writeln!(w, "notifications:")?;
            writeln!(
                w,
                "  {}",
                serde_json::to_string(&wire::notifications_to_wire_json(view.notifications))
                    .map_err(io::Error::other)?
            )?;
        }
        // ADR-0403: the cut's terminal entry — `rowCap:` with the counts
        // as one inline compact JSON value, TOON's nested-value rule, so
        // a capped result never reads as complete.
        if let Some(cap) = meta.row_cap {
            writeln!(w, "{}:", RowCap::WIRE_KEY)?;
            writeln!(
                w,
                "  {}",
                serde_json::to_string(&cap.to_wire_json(rows.len())).map_err(io::Error::other)?
            )?;
        }
        // LABS-1120: the timing block as terminal entries — `timings:`
        // with the same nested object every machine mode carries (TOON's
        // nested-value rule: inline compact JSON), and the plan-cache
        // outcome beside it as a bare TOON token.
        if let Some(timings) = meta.timings {
            writeln!(w, "timings:")?;
            writeln!(
                w,
                "  {}",
                serde_json::to_string(&timings.to_timings_json()).map_err(io::Error::other)?
            )?;
            writeln!(w, "planCache:")?;
            writeln!(w, "  {}", toon_token(timings.plan_cache)?)?;
        }
        Ok(())
    }
}

/// One TOON cell (ADR-0239): scalars bare, nested values inline compact JSON
/// (the wire-encoder projection, ADR-0367), strings through [`toon_token`].
/// Serialization failures surface as `io::Error`, like every other mode.
fn toon_value(val: &CypherValue) -> io::Result<String> {
    // The token policy discriminates on the typed variant (never the
    // derived wire JSON's shape): strings AND the temporal scalars — whose
    // Display is their wire form, e.g. 2024-03-14T07:30:00Z — render as
    // bare TOON tokens (ADR-0239's scalars-bare contract), everything else
    // projects through the one wire encoder.
    match val {
        CypherValue::Str(s) => toon_token(s),
        temporal if temporal.is_temporal() => toon_token(&temporal.to_string()),
        other => serde_json::to_string(&wire::value_to_wire_json(other)).map_err(io::Error::other),
    }
}

/// A bare TOON token when `s` needs no quoting, else the JSON-quoted form.
/// Quoting is required for the empty string, a token containing a structural
/// character (`,` `"` `\` or any unsafe output character — the shared
/// [`crate::escape::is_unsafe_output_char`] policy, so a raw bidi override
/// never reaches machine-readable output bare), leading/trailing whitespace,
/// or a token that would re-read as a number, boolean, or null.
fn toon_token(s: &str) -> io::Result<String> {
    let ambiguous = s.parse::<f64>().is_ok() || matches!(s, "true" | "false" | "null");
    let safe = !s.is_empty()
        && !ambiguous
        && s.trim() == s
        && !s
            .chars()
            .any(|c| matches!(c, ',' | '"' | '\\') || crate::escape::is_unsafe_output_char(c));
    if safe {
        Ok(s.to_string())
    } else {
        serde_json::to_string(s).map_err(io::Error::other)
    }
}
