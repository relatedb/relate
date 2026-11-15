//! Output safety: one home for the escaping that keeps untrusted values from
//! injecting terminal control sequences, breaking markdown/CSV grids, or
//! producing an unparseable Cypher statement.
//!
//! The control-character → `\u` escape mapping is defined **once** here
//! ([`control_escape`]) and parameterized by the surface's [`ControlEscape`]
//! style: table/markdown/identifiers use the braced uppercase form `\u{XXXX}`,
//! while re-parseable Cypher string literals use the four-digit uppercase form
//! `\uXXXX`. CSV fields escape controls with the braced form too, but keep LF
//! and CR raw so RFC-4180 quoting leaves multi-line values intact.

use relate_core::is_unsafe_output_char;

/// The `\u` escape spelling for a control character, which differs by surface.
#[derive(Clone, Copy)]
pub(crate) enum ControlEscape {
    /// `\u{XXXX}` — uppercase, minimal hex, braces. Used by table/markdown
    /// output and by backticked Cypher identifiers, which the lexer reads
    /// byte-for-byte.
    Braced,
    /// `\uXXXX` — uppercase, four zero-padded hex digits. Used by Cypher string
    /// literals, whose lexer decodes the escape back to the codepoint.
    Cypher,
}

/// The one canonical control-character → escape mapping. The caller picks the
/// surface's spelling via [`ControlEscape`]; `c` is expected to be unsafe
/// (see [`relate_core::is_unsafe_output_char`]).
pub(crate) fn control_escape(c: char, style: ControlEscape) -> String {
    match style {
        ControlEscape::Braced => format!("\\u{{{:X}}}", c as u32),
        ControlEscape::Cypher => format!("\\u{:04X}", c as u32),
    }
}

/// Escape characters that a terminal would interpret or that would corrupt the
/// row structure, for human-facing table/markdown output.
///
/// A literal backslash becomes `\\` so the escapes are unambiguous; tab, LF and
/// CR get their short forms `\t` / `\n` / `\r`; every other character for which
/// [`is_unsafe_output_char`] is true — Cc controls (C0, DEL, C1) and Cf format
/// characters such as U+202E — becomes `\u{XXXX}` with uppercase hex (e.g.
/// `\u{1B}`, `\u{202E}`). When `escape_pipe` is set (markdown), `|` becomes
/// `\|` so a value cannot inject table columns.
///
/// This is deliberately *not* applied to JSON/JSONL: those are a byte-exact
/// machine format and serde already escapes control characters correctly.
pub(crate) fn escape_control(s: &str, escape_pipe: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '|' if escape_pipe => out.push_str("\\|"),
            c if is_unsafe_output_char(c) => {
                out.push_str(&control_escape(c, ControlEscape::Braced));
            }
            c => out.push(c),
        }
    }
    out
}

/// Remove every character for which [`is_unsafe_output_char`] is true.
///
/// This is the **strip** counterpart to [`escape_control`], not a replacement
/// for it. Strip when the value's exact bytes do not matter and only its
/// recognizability does — human-facing diagnostics such as stderr messages,
/// error text, and logs — so a value cannot inject a terminal control sequence
/// or reorder surrounding text. Escape (via [`escape_control`], [`csv_field`],
/// or the `.mode cypher` literals) whenever the output is structured and the
/// information must survive.
pub fn strip_unsafe_output_chars(s: &str) -> String {
    s.chars().filter(|c| !is_unsafe_output_char(*c)).collect()
}

/// Sanitize text for a single-line stderr diagnostic: like
/// [`strip_unsafe_output_chars`], but an unsafe character that is whitespace
/// (LF, CR, tab, vertical tab, form feed) becomes a single space instead of
/// vanishing, so a multi-line error text renders legibly on one line
/// (`"a.\nb"` → `"a. b"`, not `"a.b"`). Every other unsafe character (ESC,
/// bidi overrides, zero-width marks, …) is still stripped, so no raw control
/// character reaches the terminal.
pub fn sanitize_error_text(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_whitespace() && is_unsafe_output_char(c) {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !is_unsafe_output_char(*c))
        .collect()
}

/// Guard a CSV field against spreadsheet formula injection: a value starting
/// with `=`, `+`, `-`, `@`, or CR would be evaluated as a formula by
/// Excel/LibreOffice. Prefixing a single quote renders it as literal text.
///
/// Callers run [`escape_csv_controls`] first, which rewrites every tab to the
/// two characters `\t`, so a leading tab never reaches this guard.
fn guard_csv_formula(val: &str) -> String {
    if matches!(val.chars().next(), Some('=' | '+' | '-' | '@' | '\r')) {
        format!("'{val}")
    } else {
        val.to_string()
    }
}

/// Neutralize control characters in a CSV field while keeping RFC-4180
/// round-tripping: LF and CR stay raw (the quoting step makes them structural,
/// so multi-line values keep their exact bytes), tab becomes the two characters
/// `\t`, and every other character for which [`is_unsafe_output_char`] is true
/// becomes the braced `\u{XXXX}` escape (e.g. `\u{1B}`, `\u{202E}`).
///
/// A literal backslash is left untouched: CSV is a data export, and this is
/// documented as display-safe rather than byte-exact, so the escapes are not
/// made reversible (which would require doubling `\`).
fn escape_csv_controls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | '\r' => out.push(c),
            '\t' => out.push_str("\\t"),
            c if is_unsafe_output_char(c) => {
                out.push_str(&control_escape(c, ControlEscape::Braced));
            }
            c => out.push(c),
        }
    }
    out
}

/// Quote a CSV field per RFC 4180 when it contains the delimiter, a quote, or a
/// line break; doubles embedded quotes. Applied identically to header and value
/// cells.
///
/// The pipeline is **escape controls first** ([`escape_csv_controls`], so a
/// leading tab no longer reads as a control byte), **then the formula guard**
/// ([`guard_csv_formula`]), **then quoting**. Because escaping precedes the
/// guard, a leading tab no longer triggers it, but a leading raw CR still does:
/// a value that is exactly `\r` becomes `"'\r"` (quoted).
pub(crate) fn csv_field(val: &str) -> String {
    let val = guard_csv_formula(&escape_csv_controls(val));
    if val.contains(',') || val.contains('"') || val.contains('\n') || val.contains('\r') {
        format!("\"{}\"", val.replace('"', "\"\""))
    } else {
        val
    }
}
