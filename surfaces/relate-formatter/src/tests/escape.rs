//! LABS-84: terminal escape / control-char injection — table, markdown,
//! and string-literal output must never emit a raw ESC, Cf format
//! character, or delimiter-breaking pipe.

use super::*;

#[test]
fn test_table_escapes_esc_and_osc52_sequence() {
    // OSC-52 can rewrite the user's clipboard and ESC starts any control
    // sequence; neither byte may reach the terminal raw.
    let payload = "\u{1b}]52;c;ZXZhbA==\u{7}";
    let cols = vec!["v".to_string()];
    let s = render(
        Mode::Table,
        &cols,
        &[vec![CypherValue::Str(payload.into())]],
    );
    assert!(s.contains("\\u{1B}"), "{s}");
    assert!(s.contains("\\u{7}"), "{s}");
    assert!(!s.contains('\u{1b}'), "raw ESC leaked: {s:?}");
    assert!(!s.contains('\u{7}'), "raw BEL leaked: {s:?}");
}

#[test]
fn test_table_header_alias_newline_stays_on_one_physical_line() {
    let cols = vec!["a\nb".to_string()];
    let s = render(Mode::Table, &cols, &[vec![CypherValue::Int(1)]]);
    assert!(s.contains("a\\nb"), "{s}");
    assert!(!s.contains("\nb"), "raw newline in header: {s:?}");
    // separator + header + separator + row + separator
    assert_eq!(s.lines().count(), 5, "{s:?}");
}

#[test]
fn test_table_escapes_cr_and_backslash() {
    let cols = vec!["v".to_string()];
    let s = render(
        Mode::Table,
        &cols,
        &[vec![CypherValue::Str("x\\y\rz".into())]],
    );
    // Display wraps the string in quotes; the inner `\` and CR are escaped.
    assert!(s.contains(r#""x\\y\rz""#), "{s}");
    assert!(!s.contains('\r'), "raw CR leaked: {s:?}");
}

#[test]
fn test_budget_counts_escaped_control_chars() {
    // A raw ESC has display width 0, so before escaping the cell measured 2
    // columns (`""`); after escaping it measures 8 (`"\u{1B}"`), which the
    // budget then wraps.
    let cols = vec!["v".to_string()];
    let mut f = Formatter::new(Mode::Table);
    f.width = Some(9);
    let s = render_with(f, &cols, &[vec![CypherValue::Str("\u{1b}".into())]]);
    assert!(s.contains("\"\\u{1"), "expected the escaped head: {s}");
    assert!(s.contains("B}\""), "expected the wrapped tail: {s}");
    let widths: Vec<usize> = s.lines().map(line_width).collect();
    assert!(widths.windows(2).all(|w| w[0] == w[1]), "misaligned: {s}");
}

#[test]
fn test_markdown_escapes_control_chars() {
    let cols = vec!["v".to_string()];
    let s = render(
        Mode::Markdown,
        &cols,
        &[vec![CypherValue::Str("\u{1b}".into())]],
    );
    assert!(s.contains("\\u{1B}"), "{s}");
    assert!(!s.contains('\u{1b}'), "raw ESC leaked: {s:?}");
}

#[test]
fn test_strip_unsafe_output_chars_removes_cc_and_cf() {
    // Cc (ESC) and Cf (U+202E) are removed, leaving the printable remainder.
    assert_eq!(strip_unsafe_output_chars("a\u{1b}\u{202e}b"), "ab");
    // Tab is a Cc control and is stripped too.
    assert_eq!(strip_unsafe_output_chars("a\tb"), "ab");
    // Printable text, including non-ASCII, survives untouched.
    assert_eq!(
        strip_unsafe_output_chars("a\u{e9}\u{4f60}b"),
        "a\u{e9}\u{4f60}b"
    );
    // A control-free value is the identity.
    assert_eq!(strip_unsafe_output_chars("plain"), "plain");
}

#[test]
fn test_sanitize_error_text_joins_lines_and_strips() {
    // Whitespace controls become a single space, so a multi-line error text
    // stays legible on one line instead of gluing words together.
    assert_eq!(sanitize_error_text("a.\nb"), "a. b");
    assert_eq!(sanitize_error_text("a\rb\tc\u{b}d\u{c}e"), "a b c d e");
    // Non-whitespace unsafe characters (ESC, bidi override) are still
    // stripped: no raw control character reaches the terminal.
    assert_eq!(sanitize_error_text("a\u{1b}\u{202e}b"), "ab");
    // Printable text, including non-ASCII, survives untouched.
    assert_eq!(sanitize_error_text("a\u{e9}\u{4f60}b"), "a\u{e9}\u{4f60}b");
    // A control-free value is the identity.
    assert_eq!(sanitize_error_text("plain"), "plain");
}

#[test]
fn test_table_and_markdown_escape_format_characters() {
    // U+202E (RLO), U+200E (LRM), U+FEFF (BOM) are `Cf`, not `Cc`; they must
    // be escaped so a cell cannot reorder or hide the surrounding grid.
    for mode in [Mode::Table, Mode::Markdown] {
        let payload = "a\u{202e}b\u{200e}c\u{feff}d";
        let cols = vec!["v".to_string()];
        let s = render(
            mode.clone(),
            &cols,
            &[vec![CypherValue::Str(payload.into())]],
        );
        assert!(s.contains("\\u{202E}"), "{s}");
        assert!(s.contains("\\u{200E}"), "{s}");
        assert!(s.contains("\\u{FEFF}"), "{s}");
        for c in ['\u{202e}', '\u{200e}', '\u{feff}'] {
            assert!(!s.contains(c), "raw Cf {c:?} leaked ({mode:?}): {s:?}");
        }
    }
}

#[test]
fn test_cypher_string_literal_escapes_format_characters() {
    // A `Cf` such as U+202E must not reach terminal/`.mode cypher` output raw,
    // and the four-digit escape must still round-trip through the lexer.
    let original = "a\u{202e}b\u{200e}c\u{feff}d";
    let lit = cypher_literal(&CypherValue::Str(original.into()));
    assert!(lit.contains("\\u202E"), "{lit}");
    assert!(lit.contains("\\u200E"), "{lit}");
    assert!(lit.contains("\\uFEFF"), "{lit}");
    for c in ['\u{202e}', '\u{200e}', '\u{feff}'] {
        assert!(!lit.contains(c), "raw Cf {c:?} leaked: {lit:?}");
    }
    let toks = relate_parser::lexer::tokenize(&lit);
    assert_eq!(toks[0].ty, relate_parser::TokenType::String);
    assert_eq!(toks[0].literal, original);
}

#[test]
fn test_markdown_escapes_pipe_and_keeps_column_count() {
    let cols = vec!["v".to_string()];
    let s = render(
        Mode::Markdown,
        &cols,
        &[vec![CypherValue::Str("a|b".into())]],
    );
    assert!(s.contains("a\\|b"), "{s}");
    for line in s.lines() {
        assert_eq!(
            count_unescaped_pipes(line),
            2,
            "column count drifted: {line:?}"
        );
    }
}
