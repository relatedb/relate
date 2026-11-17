//! The shared fixed-width grid layout engine: column widths, separator
//! lines, and piped rows — the primitives the table and plan-table
//! presenters both lay out through.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The narrowest a column may be shrunk to when fitting a budget: wide
/// enough for a few characters plus the padding, so a wrapped cell stays
/// readable rather than becoming a vertical ribbon.
const MIN_COLUMN_WIDTH: usize = 4;

/// One column's frame cost: the two padding columns [`cell_string`] adds
/// (one space each side) plus the `|` between columns — so a column of
/// width `w` costs `w + 3`, and the grid adds the closing `|`. The one
/// spelling [`frame_width`], [`cell_string`] and [`separator_line`] derive
/// from.
const CELL_PADDING: usize = 2;
const CELL_FRAME: usize = CELL_PADDING + 1;

/// The grid's total display width for these columns: every column's frame
/// plus the closing `|`.
fn frame_width(widths: &[usize]) -> usize {
    widths.iter().map(|w| w + CELL_FRAME).sum::<usize>() + 1
}

/// Compute per-column display widths (unicode-width aware).
///
/// `headers` and `cells` are the already-escaped strings from
/// [`materialize_cells`]; this function never re-renders a value.
///
/// `budget` is the total display width the grid must fit (the terminal
/// width, LABS-399): when the natural widths exceed it, the widest columns
/// give up width first, down to [`MIN_COLUMN_WIDTH`], and the cell renderer
/// wraps their content over several lines rather than overflowing the
/// terminal. `None` keeps the natural width — the stable choice for piped
/// output.
pub(crate) fn compute_layout(
    headers: &[String],
    cells: &[Vec<String>],
    budget: Option<usize>,
) -> Vec<usize> {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|h| UnicodeWidthStr::width(h.as_str()))
        .collect();
    for cell in cells {
        for (i, val) in cell.iter().enumerate() {
            let w = UnicodeWidthStr::width(val.as_str());
            if w > widths[i] {
                widths[i] = w;
            }
        }
    }
    fit_budget(&mut widths, budget);
    widths
}

/// Shrink the widest columns until the grid fits `budget` display columns
/// (the frame is `|` + one space each side of every column, plus the `+`
/// joins), never below [`MIN_COLUMN_WIDTH`]. The widest column gives up
/// width first, so a long `Details` column wraps instead of pushing the
/// grid past the terminal.
fn fit_budget(widths: &mut [usize], budget: Option<usize>) {
    let Some(budget) = budget else {
        return;
    };
    while frame_width(widths) > budget {
        let Some((i, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > MIN_COLUMN_WIDTH)
            .max_by_key(|(_, w)| **w)
        else {
            break; // every column is at the floor: the grid cannot shrink further
        };
        widths[i] -= 1;
    }
}

/// Render one padded table cell: `" <content> "`, padded to `width` display
/// columns. Padding is computed from display width, not `char`/byte count, so
/// CJK and emoji stay aligned. Content wider than the column is wrapped by
/// [`pipe_lines`] (or shrunk to fit by [`fit_budget`]) before it reaches
/// here, so this pads rather than truncates.
fn cell_string(raw: &str, width: usize) -> String {
    // One space each side — [`CELL_PADDING`] split across the two edges.
    format!(" {} ", pad_display(raw, width))
}

/// One logical row as its physical grid lines (LABS-399): each cell's
/// content wrapped over as many lines as its column width needs, the cells'
/// lines zipped (a shorter cell pads with blanks), every line fenced and
/// padded the same way. A row whose cells all fit yields exactly one line —
/// the pre-wrap behaviour. The one `|`-delimited builder behind the table
/// and markdown header and row lines, so the two renderers cannot drift.
pub(crate) fn pipe_lines(cells: &[String], widths: &[usize]) -> Vec<String> {
    let wrapped: Vec<Vec<String>> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| wrap_display(c, widths[i]))
        .collect();
    // `wrap_display` always returns at least one line, so the max is
    // `Some(>= 1)`; `unwrap_or(1)` covers an empty row.
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    (0..height)
        .map(|line| {
            let parts: Vec<String> = wrapped
                .iter()
                .enumerate()
                .map(|(i, lines)| {
                    let raw = lines.get(line).map(String::as_str).unwrap_or("");
                    cell_string(raw, widths[i])
                })
                .collect();
            format!("|{}|", parts.join("|"))
        })
        .collect()
}

/// Write one logical row as its physical grid lines (LABS-399): the one
/// row-emission policy — wrap, fence, pad — every table presenter calls,
/// never a per-presenter copy of the loop.
pub(crate) fn write_row_lines(
    w: &mut dyn std::io::Write,
    cells: &[String],
    widths: &[usize],
) -> std::io::Result<()> {
    for line in pipe_lines(cells, widths) {
        writeln!(w, "{line}")?;
    }
    Ok(())
}

/// Wrap `s` over lines of at most `width` display columns: words are packed
/// greedily, and a word wider than the column is hard-broken at the column
/// edge (never splitting a multi-column character). A cell that fits is one
/// line, unchanged. Lines come back unpadded; the caller pads through
/// [`cell_string`].
fn wrap_display(s: &str, width: usize) -> Vec<String> {
    if width == 0 || UnicodeWidthStr::width(s) <= width {
        return vec![s.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    for word in s.split_whitespace() {
        // A word that does not fit after the current line starts a new one.
        if line_w > 0 && line_w + 1 + UnicodeWidthStr::width(word) > width {
            lines.push(std::mem::take(&mut line));
            line_w = 0;
        }
        if line_w > 0 {
            line.push(' ');
            line_w += 1;
        }
        // Pack the word, hard-breaking it whenever it overruns the column.
        let mut rest = word;
        loop {
            let avail = width - line_w;
            let rest_w = UnicodeWidthStr::width(rest);
            if rest_w <= avail {
                line.push_str(rest);
                line_w += rest_w;
                break;
            }
            let cut = width_prefix_end(rest, avail);
            let (head, tail) = rest.split_at(cut);
            line.push_str(head);
            lines.push(std::mem::take(&mut line));
            line_w = 0;
            rest = tail;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// The byte index of the widest prefix of `s` whose display width is at most
/// `width`, never splitting a multi-column character. At least one character
/// is taken when `width` allows one.
fn width_prefix_end(s: &str, width: usize) -> usize {
    let mut acc = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if acc + cw > width {
            return i;
        }
        acc += cw;
        end = i + c.len_utf8();
    }
    end
}

/// Right-pad `s` with spaces until its display width reaches `width`.
fn pad_display(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

/// Left-pad `s` with spaces until its display width reaches `width`.
fn pad_display_start(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    format!("{}{}", " ".repeat(width.saturating_sub(w)), s)
}

/// Which side of its column a cell hugs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnAlign {
    Left,
    Right,
}

/// A borderless text grid: `rows` (the first is conventionally the
/// header) laid out in columns two spaces apart, every width measured and
/// padded in display columns — the same measure the table presenters use,
/// so wide characters (CJK, emoji) stay aligned. `aligns` names each
/// column's side; a row with fewer cells than `aligns` is padded as if
/// its missing cells were empty.
pub fn text_grid_lines(rows: &[Vec<String>], aligns: &[ColumnAlign]) -> Vec<String> {
    // A missing cell reads as empty; cells are borrowed, never copied.
    fn cell(row: &[String], i: usize) -> &str {
        row.get(i).map_or("", String::as_str)
    }
    let widths: Vec<usize> = (0..aligns.len())
        .map(|i| {
            rows.iter()
                .map(|row| UnicodeWidthStr::width(cell(row, i)))
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            aligns
                .iter()
                .zip(&widths)
                .enumerate()
                .map(|(i, (align, &width))| match align {
                    ColumnAlign::Left => pad_display(cell(row, i), width),
                    ColumnAlign::Right => pad_display_start(cell(row, i), width),
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

/// The `+---+` border line for a grid of column `widths` — the one
/// builder behind the table mode's header/row fences and the EXPLAIN plan
/// boxes, so the border policy lives once.
pub(crate) fn separator_line(widths: &[usize]) -> String {
    let parts: Vec<String> = widths
        .iter()
        .map(|w| "-".repeat(w + CELL_PADDING))
        .collect();
    format!("+{}+", parts.join("+"))
}
