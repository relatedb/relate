//! The table/markdown family: both grid renderers, laying out through the
//! shared grid engine in [`crate::layout`] and materializing their cells
//! here ([`materialize_cells`], [`format_cell`]).

use std::io::Write;

use indexmap::IndexMap;
use relate_model::CypherValue;

use crate::escape::escape_control;
use crate::layout::{compute_layout, separator_line, write_row_lines};
use crate::notifications::format_complete_notifications;
use crate::{Formatter, StatementMeta};

impl Formatter {
    pub(crate) fn format_table(
        &self,
        w: &mut dyn Write,
        diagnostics: Option<&mut dyn Write>,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        // The plan presentation ACCOMPANIES the data (LABS-373): a PROFILE
        // result keeps the wrapped statement's own rows, so the data table
        // prints first and the plan table follows it, separated by a blank
        // line. EXPLAIN carries no rows (ADR-0359), so its plan table
        // prints alone — and a zero-column PROFILE (bare CREATE/SET/DELETE)
        // shows its plan without an empty data table.
        // This is the one owner of table-mode plan routing: the data table
        // goes to `w`, the plan table and its notification block go to the
        // caller's diagnostic writer (or `w` itself when there is none).
        match meta.plan {
            Some(view) => {
                if !columns.is_empty() {
                    self.write_data_table(w, columns, rows)?;
                    // The separator belongs to the shared stream only; a
                    // separate diagnostic writer starts with its own plan
                    // table.
                    if diagnostics.is_none() {
                        writeln!(w)?;
                    }
                }
                let plan_w: &mut dyn Write = match diagnostics {
                    Some(diagnostics) => diagnostics,
                    None => w,
                };
                write_plan_presentation(plan_w, &view, self.width)
            }
            None => {
                if columns.is_empty() {
                    return Ok(());
                }
                // A plain run carries no plan and therefore no notification
                // list (`StatementMeta::notifications` derives the empty
                // list from the absent view), so no block follows the data.
                self.write_data_table(w, columns, rows)?;
                Ok(())
            }
        }
    }

    /// The data-table rendering (the rows the statement returned) — split
    /// out of [`Formatter::format_table`] so the plan presentation can
    /// follow it (LABS-373).
    fn write_data_table(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
    ) -> std::io::Result<()> {
        // Escape every header and cell exactly once; width measurement and
        // rendering both read from these materialized strings.
        let (headers, cells) = materialize_cells(columns, rows, false);
        let widths = compute_layout(&headers, &cells, self.width);

        // Build separator
        let separator = separator_line(&widths);

        // Header
        writeln!(w, "{separator}")?;
        if self.headers {
            write_row_lines(w, &headers, &widths)?;
            writeln!(w, "{separator}")?;
        }

        // Rows: each logical row may render as several physical lines when
        // its cells wrap to fit the width budget (LABS-399).
        for cell in &cells {
            write_row_lines(w, cell, &widths)?;
        }
        writeln!(w, "{separator}")?;

        Ok(())
    }

    pub(crate) fn format_markdown(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
        meta: StatementMeta<'_>,
    ) -> std::io::Result<()> {
        if !columns.is_empty() {
            let (headers, cells) = materialize_cells(columns, rows, true);
            // A GFM table has no wrapped cells, so markdown lays out at its
            // natural width and ignores the terminal budget (LABS-399).
            let widths = compute_layout(&headers, &cells, None);

            // Header
            write_row_lines(w, &headers, &widths)?;

            // Separator
            let sep_parts: Vec<String> = widths.iter().map(|w| "-".repeat(w + 2)).collect();
            writeln!(w, "|{}|", sep_parts.join("|"))?;

            // Rows: the widths were computed with no budget, so each row
            // is exactly one line — the same builder, unwrapped output.
            for cell in &cells {
                write_row_lines(w, cell, &widths)?;
            }
        }

        // The notification block after the table (ADR-0363 decision 8);
        // markdown pipes are escaped at this module's materialization
        // boundary like every cell. Markdown never renders the plan table,
        // so the three plan-warning kinds are not rendered anywhere else
        // and the block carries the complete list.
        format_complete_notifications(w, meta.notifications())?;

        // ADR-0403: the cut's in-band footer — one blockquote line after the
        // table, so a capped result never reads as complete (markdown is
        // the human transcript format; the line renders visibly there).
        // Counts only: the opt-out naming the `--max-rows` flag is the
        // CLI's stderr warning, the surface that owns the flag — never a
        // spelling in this shared crate (http/bolt/wasm/load have no such
        // flag to name).
        if let Some(cap) = meta.row_cap {
            writeln!(w, "> truncated: {} of {} rows shown", rows.len(), cap.total)?;
        }
        Ok(())
    }
}

/// Format a CypherValue for display in table/markdown cells.
///
/// Returns the raw display form with no escaping. Escaping is applied *after*
/// formatting by [`materialize_cells`] via [`escape_control`], which is the one
/// place the `escape_pipe` contract (markdown pipe escaping) lives. Escaping
/// happens before any width math — [`materialize_cells`] produces the escaped
/// strings once and [`compute_layout`] then measures them, so a raw control char
/// (width 0) cannot slip through the column budget.
fn format_cell(val: &CypherValue) -> String {
    match val {
        CypherValue::Null => "<null>".to_string(),
        v => format!("{v}"),
    }
}

/// The longest cell, in characters, a table or markdown grid renders
/// (ADR-0691). A shared-payload value can expand to megabytes of text
/// inside one cell; a grid cell that wraps over thousands of lines reads
/// as noise, so the grid shows the head and an ellipsis. Presentation
/// only: the JSON, CSV and wire formats carry the whole value.
pub(crate) const MAX_CELL_CHARS: usize = 4096;

/// `cell`, cut to [`MAX_CELL_CHARS`] characters with a trailing `…` when it
/// is longer; the cell unchanged otherwise.
fn elide_cell(cell: &str) -> std::borrow::Cow<'_, str> {
    match cell.char_indices().nth(MAX_CELL_CHARS) {
        Some((end, _)) => format!("{}…", &cell[..end]).into(),
        None => cell.into(),
    }
}

/// Escape every header and cell for a table/markdown grid **once**.
///
/// Returns the escaped header strings and, for each row, the escaped cell
/// strings in column order. The `escape_pipe` flag lives only here — the single
/// materialization boundary — and is never threaded into layout measurement or
/// rendering, both of which consume the returned strings as-is.
fn materialize_cells(
    columns: &[String],
    rows: &[IndexMap<String, CypherValue>],
    escape_pipe: bool,
) -> (Vec<String>, Vec<Vec<String>>) {
    let headers = columns
        .iter()
        .map(|c| escape_control(c, escape_pipe))
        .collect();
    let cells = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|col| {
                    let cell = format_cell(crate::cell(row, col));
                    escape_control(&elide_cell(&cell), escape_pipe)
                })
                .collect()
        })
        .collect();
    (headers, cells)
}

// The plan presentation lives in `plan_table` — one entry point back to
// this module's table path.
use crate::plan_table::write_plan_presentation;
