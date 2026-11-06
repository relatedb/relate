//! `.mode cypher` and the canonical Cypher literal rendering
//! ([`cypher_literal`]) shared by every surface that emits re-parseable
//! Cypher text.

use std::io::Write;

use indexmap::IndexMap;
use relate_model::CypherValue;
use relate_parser::token::{identifier_needs_quotes, quote_identifier};

use crate::escape::{control_escape, is_unsafe_output_char, ControlEscape};
use crate::{cell, Formatter};

impl Formatter {
    /// `.mode cypher`: emit one replayable `CREATE ({...})` per row.
    ///
    /// This is a **scalar-only** form. A column whose value has no Cypher
    /// literal representation (NODE / RELATIONSHIP / PATH / GEOMETRY / VECTOR,
    /// or a list/map containing one) cannot appear as a node property, so it is
    /// omitted and a `//` comment naming the column and its type is written
    /// instead of silently corrupting the output with an unparseable
    /// `{col: (0:Person{...})}`. All scalar columns (including temporals and
    /// points, which round-trip through `date(...)`/`point(...)` constructors)
    /// are emitted through the shared [`cypher_literal`]. Property keys are
    /// escaped with [`cypher_output_identifier`].
    pub(crate) fn format_cypher(
        &self,
        w: &mut dyn Write,
        columns: &[String],
        rows: &[IndexMap<String, CypherValue>],
    ) -> std::io::Result<()> {
        for row in rows {
            let mut props: Vec<String> = Vec::new();
            for col in columns {
                let val = cell(row, col);
                if is_cypher_scalar(val) {
                    props.push(format!(
                        "{}: {}",
                        cypher_output_identifier(col),
                        cypher_literal(val)
                    ));
                } else {
                    writeln!(
                        w,
                        "// column {} ({}) omitted: no Cypher literal form",
                        cypher_output_identifier(col),
                        val.type_name()
                    )?;
                }
            }
            if props.is_empty() {
                writeln!(w, "CREATE ()")?;
            } else {
                writeln!(w, "CREATE ({{{}}})", props.join(", "))?;
            }
        }
        Ok(())
    }
}

/// Render a [`CypherValue`] as a parseable Cypher literal.
///
/// This is the **one** canonical literal used by every surface that renders
/// engine values as re-parseable Cypher (`.mode cypher` in the CLI).
/// Recordings ([`recording`]) render wire JSON instead, keeping
/// numbers as the wire spells them, with the same identifier and string
/// escaping ([`cypher_identifier`], [`quoted_literal`]). It fixes the
/// historical bugs of the two duplicate copies it replaces:
///
/// * **Float type is preserved**: `Float(1.0)` renders as `1.0`, not `1`, so a
///   round-trip re-parses as a FLOAT rather than an INTEGER. Non-finite floats
///   become the parseable keyword literals `NaN` / `Infinity` / `-Infinity`
///   (CIP-55), never a bare `inf`/`NaN` Display token in a value position that
///   might not round-trip.
/// * **Strings escape both `\` and `'`** (backslash first), plus newline/tab/CR,
///   using only escapes the lexer understands, so the literal is single-line and
///   re-parseable.
/// * **Map keys / identifiers are backticked when needed** via
///   [`cypher_output_identifier`], which also neutralizes control characters.
/// * **Temporals and points round-trip** through their constructor calls
///   (`date(...)`, `time(...)`, `datetime(...)`, `localdatetime(...)`,
///   `duration(...)`, `point({...})`).
///
/// Values with no Cypher literal form (NODE / RELATIONSHIP / PATH / GEOMETRY /
/// VECTOR) have no scalar spelling; as a documented, lossy last resort they are
/// emitted as a single-quoted string of their display form. Callers that must
/// not lose such values (e.g. `.mode cypher`) should pre-filter with
/// [`is_cypher_scalar`].
pub fn cypher_literal(val: &CypherValue) -> String {
    match val {
        CypherValue::Null => "null".to_string(),
        CypherValue::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        CypherValue::Int(i) => i.to_string(),
        CypherValue::Float(f) => cypher_float_literal(*f),
        CypherValue::Str(s) => cypher_string_literal(s),
        CypherValue::List(items) => {
            let parts: Vec<String> = items.iter().map(cypher_literal).collect();
            format!("[{}]", parts.join(", "))
        }
        CypherValue::Map(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", cypher_output_identifier(k), cypher_literal(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        CypherValue::Date(d) => format!("date('{d}')"),
        CypherValue::Time(..) => format!("time('{val}')"),
        CypherValue::DateTime(..) => format!("datetime('{val}')"),
        CypherValue::LocalDateTime(_) => format!("localdatetime('{val}')"),
        CypherValue::Duration(_) => format!("duration('{val}')"),
        CypherValue::Point(p) => {
            let mut fields = format!(
                "srid: {}, x: {}, y: {}",
                p.srid,
                cypher_float_literal(p.x),
                cypher_float_literal(p.y)
            );
            if let Some(z) = p.z {
                fields.push_str(&format!(", z: {}", cypher_float_literal(z)));
            }
            format!("point({{{fields}}})")
        }
        // NODE / RELATIONSHIP / PATH / GEOMETRY / VECTOR: no scalar literal form.
        // Documented lossy fallback — a quoted string of the display form.
        _ => cypher_string_literal(&val.to_string()),
    }
}

/// Escape control characters in an identifier bound for `.mode cypher` output.
///
/// Unlike [`escape_control`] this leaves backslashes untouched: a backticked
/// identifier is read byte-for-byte by the lexer (it decodes no escapes), so
/// escaping a backslash would corrupt the name. Ordinary, control-free names
/// pass through unchanged, preserving the normal backtick behavior.
fn sanitize_identifier(name: &str) -> String {
    if !name.chars().any(is_unsafe_output_char) {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if is_unsafe_output_char(c) {
            out.push_str(&control_escape(c, ControlEscape::Braced));
        } else {
            out.push(c);
        }
    }
    out
}

/// Render an identifier for `.mode cypher` output: control characters are
/// neutralized first (via [`sanitize_identifier`]) so an alias containing a
/// newline can never terminate the `//` comment or split the `CREATE`
/// statement, then the normal backtick quoting is applied.
fn cypher_output_identifier(name: &str) -> String {
    cypher_identifier(&sanitize_identifier(name))
}

/// Render `name` as a Cypher identifier (variable, label, or map key) via the
/// parser's one quoting policy ([`quote_identifier`]): bare when the name is
/// a plain non-keyword identifier, otherwise wrapped in backticks with every
/// embedded backtick doubled (the Cypher escape rule). The policy is
/// deliberately ASCII-only, so non-ASCII names, names with
/// spaces/punctuation, digit-leading names, and the empty string all render
/// quoted.
pub fn cypher_identifier(name: &str) -> String {
    quote_identifier(name)
}

/// True when `name` needs no backticks, i.e. the negation of the parser's
/// [`identifier_needs_quotes`]. Keyword names are not bare (see that policy),
/// and non-ASCII names render quoted even though the lexer accepts them bare
/// (ADR-0182), so generated Cypher stays ASCII and readable by versions that
/// predate Unicode identifiers.
pub(crate) fn is_bare_identifier(name: &str) -> bool {
    !identifier_needs_quotes(name)
}

/// Render an `f64` as a Cypher float literal, keeping the decimal point on
/// whole-valued floats so a round-trip re-parses as FLOAT, and spelling the
/// three non-finite values as their parseable keyword literals.
fn cypher_float_literal(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        if f < 0.0 { "-Infinity" } else { "Infinity" }.to_string()
    } else if f == f.trunc() {
        // Whole-valued finite float — force a fractional part so it is not read
        // back as an integer. Rust's f64 Display never uses exponent form, so
        // there is no `1e300.0` ambiguity to worry about.
        format!("{f}.0")
    } else {
        format!("{f}")
    }
}

/// Render a string as a single-quoted Cypher string literal ([`quoted_literal`]).
fn cypher_string_literal(s: &str) -> String {
    quoted_literal(s, '\'')
}

/// Render `s` between two `quote` characters, escaping backslash and `quote`
/// (backslash first) plus every unsafe character, so the result is a single,
/// re-parseable line that cannot emit a raw terminal escape sequence or a raw
/// bidi override. Cypher string literals use `'`; recordings ([`recording`])
/// use `"` for strings and `` ` `` for labels and tagged literals.
///
/// Newline/tab get their short forms; CR and every other unsafe C0/DEL/C1
/// control character (and BMP `Cf` format character such as U+202E) becomes
/// `\uXXXX` (four uppercase hex digits), which the lexer decodes back to exactly
/// that codepoint — so the literal round-trips. `Cf` codepoints **above** the
/// BMP (e.g. U+E0001) cannot be spelled as a four-digit `\uXXXX` and are left
/// raw; any other spelling would corrupt the literal, since the lexer decodes
/// exactly four hex digits and rejects surrogates.
pub(crate) fn quoted_literal(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\u000D"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            // Only the BMP fits the lexer's fixed 4-hex-digit `\u` escape.
            c if is_unsafe_output_char(c) && (c as u32) <= 0xFFFF => {
                out.push_str(&control_escape(c, ControlEscape::Cypher));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// True when `val` has a Cypher literal representation that can appear as a node
/// property (scalars, temporals, points, and lists/maps recursively made of
/// those). NODE / RELATIONSHIP / PATH / GEOMETRY / VECTOR are not scalars.
pub fn is_cypher_scalar(val: &CypherValue) -> bool {
    match val {
        // The one non-scalar variant set lives in [`crate::is_graph_value`];
        // `json`'s display-string fallback reads the same set (plus its
        // deliberate local Point exception).
        _ if crate::is_graph_value(val) => false,
        CypherValue::List(items) => items.iter().all(is_cypher_scalar),
        CypherValue::Map(map) => map.values().all(is_cypher_scalar),
        _ => true,
    }
}
