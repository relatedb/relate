//! 8.12: unicode-width table layout + `.width` truncation — CJK/emoji
//! alignment and ellipsis truncation at user-set column widths.

use super::*;

#[test]
fn test_wide_and_narrow_cells_stay_grid_aligned() {
    // A wide (CJK/emoji) cell and a narrow (ASCII) cell in the same column
    // must still produce a grid whose every border line has equal display
    // width — in Table mode and in Markdown, which pads by display width.
    for (mode, wide, narrow) in [
        (Mode::Table, "\u{4f60}\u{597d}\u{4e16}\u{754c}", "hi"), // 你好世界 (width 8)
        (Mode::Table, "\u{1f389}\u{1f389}", "x"),                // 🎉🎉 (width 4)
        (Mode::Markdown, "\u{4f60}\u{597d}", "hi"),              // 你好 (width 4)
    ] {
        let cols = vec!["c".to_string()];
        let s = render(
            mode.clone(),
            &cols,
            &[
                vec![CypherValue::Str(wide.into())],
                vec![CypherValue::Str(narrow.into())],
            ],
        );
        let widths: Vec<usize> = s.lines().map(line_width).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "misaligned ({mode:?}): {s}"
        );
    }
}

#[test]
fn test_budget_wraps_a_long_cell() {
    let cols = vec!["v".to_string()];
    // One column: the frame costs 4 display columns, so a budget of 12
    // leaves the column 8 — the 12-column cell wraps instead of overflowing.
    let mut f = Formatter::new(Mode::Table);
    f.width = Some(12);
    let s = render_with(f, &cols, &[vec![CypherValue::Str("abcdefghij".into())]]);
    assert!(s.contains("abcdefg"), "expected the wrapped head: {s}");
    assert!(s.contains("hij\""), "expected the wrapped tail: {s}");
    assert!(
        s.lines().all(|l| line_width(l) <= 12),
        "the grid stays within the budget: {s}"
    );
    let widths: Vec<usize> = s.lines().map(line_width).collect();
    assert!(widths.windows(2).all(|w| w[0] == w[1]), "misaligned: {s}");
}

#[test]
fn test_budget_wrap_never_splits_a_wide_char() {
    // 你好世 is 6 display columns; a budget of 8 leaves the column 4, so the
    // wrap breaks between the wide chars, never through one.
    let cols = vec!["v".to_string()];
    let mut f = Formatter::new(Mode::Table);
    f.width = Some(8);
    let s = render_with(
        f,
        &cols,
        &[vec![CypherValue::Str("\u{4f60}\u{597d}\u{4e16}".into())]],
    );
    // The rendered cell is `"你好世"` (quoted), so at width 4 it wraps as
    // `"你` / `好世` / `"` — the wide chars stay whole.
    assert!(s.contains("\"\u{4f60}"), "expected the wrapped head: {s}");
    assert!(s.contains("\u{597d}\u{4e16}"), "expected 好世 whole: {s}");
    assert!(
        s.lines().all(|l| line_width(l) <= 8),
        "the grid stays within the budget: {s}"
    );
}

#[test]
fn test_an_over_long_cell_is_elided_in_table_and_markdown_only() {
    // ADR-0691: a grid cell past MAX_CELL_CHARS shows its head and an
    // ellipsis; a cell exactly at the limit is untouched, and JSON keeps
    // the whole value. A string renders quoted, so its display is two
    // characters longer than its content.
    let max = crate::table::MAX_CELL_CHARS;
    let cols = vec!["v".to_string()];
    let long = "a".repeat(max + 10);
    let exact = "b".repeat(max - 2);
    for mode in [Mode::Table, Mode::Markdown] {
        let s = render(
            mode.clone(),
            &cols,
            &[
                vec![CypherValue::Str(long.clone())],
                vec![CypherValue::Str(exact.clone())],
            ],
        );
        assert!(
            s.contains(&format!("{}\u{2026}", "a".repeat(max - 1))),
            "{mode:?}"
        );
        assert!(!s.contains(&"a".repeat(max)), "{mode:?}");
        assert!(
            s.contains(&exact) && !s.contains(&format!("{exact}\"\u{2026}")),
            "{mode:?}"
        );
    }
    let json = render(Mode::Json, &cols, &[vec![CypherValue::Str(long.clone())]]);
    assert!(json.contains(&long));
}
