//! Where a syntax error points (ADR-0290).
//!
//! The engine hands the CLI a syntax error as text (`Error::Syntax(String)`),
//! built from the parser's `Display` (the `syntax error: ` prefix is added by
//! the error's own `Display`, so it is not part of the text handled here). That text has two fixed shapes, and this
//! module recovers the position from them so stderr can show the caret under the
//! offending token again and the error envelope can carry `column`/`source`
//! (or `line`/`column`) as data. Text that matches neither shape yields no
//! position, and the caller reports the error exactly as before.

use relate_core::is_unsafe_output_char;

/// Where in the statement a syntax error points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyntaxPosition {
    /// The parser echoed the offending line and a caret under the token.
    Excerpt {
        /// The echoed source line, sanitized to one line.
        source: String,
        /// 1-based character column of the caret within `source`.
        column: usize,
    },
    /// The parser reported only a position (`... at line L, col C`).
    Line { line: usize, column: usize },
}

impl SyntaxPosition {
    /// Where `e` points, if it is a syntax error whose text has a known shape.
    /// The one place that decides which errors carry a position.
    pub(crate) fn from_error(e: &relate::Error) -> Option<Self> {
        match e {
            relate::Error::Syntax(text) => Self::from_text(text),
            _ => None,
        }
    }

    /// The recovered position of the syntax error whose text is `text` (what
    /// `Error::Syntax` holds), or `None` when it has neither known shape.
    fn from_text(text: &str) -> Option<Self> {
        Self::excerpt(text).or_else(|| Self::line_and_column(text))
    }

    /// `[<Detail>: ]parse error: <message>\n  <excerpt>\n  <caret>`: the parser's
    /// source echo.
    fn excerpt(text: &str) -> Option<Self> {
        let mut lines = text.split('\n');
        let (headline, excerpt, caret) = (lines.next()?, lines.next()?, lines.next()?);
        if lines.next().is_some() || !headline.contains("parse error:") {
            return None;
        }
        let excerpt = excerpt.strip_prefix("  ")?;
        let caret = caret.strip_prefix("  ")?;
        // The caret line is spaces and one `^`; its offset is the column.
        let offset = caret.strip_suffix('^')?;
        if !offset.chars().all(|c| c == ' ') {
            return None;
        }
        let (source, column) = sanitize_with_column(excerpt, offset.chars().count() + 1);
        Some(Self::Excerpt { source, column })
    }

    /// `... at line L, col C` at the end of a one-line message. A position of
    /// `0, 0` means the parser did not know it, so it is not reported.
    fn line_and_column(text: &str) -> Option<Self> {
        let (_, tail) = text.rsplit_once(" at line ")?;
        let (line, column) = tail.split_once(", col ")?;
        let (line, column): (usize, usize) = (line.parse().ok()?, column.parse().ok()?);
        (line >= 1 && column >= 1).then_some(Self::Line { line, column })
    }

    /// The two stderr lines under an excerpt's headline: the indented source and
    /// the caret, aligned under it. `None` for a position with no echo.
    pub(crate) fn echo_lines(&self) -> Option<[String; 2]> {
        let Self::Excerpt { source, column } = self else {
            return None;
        };
        Some([
            format!("  {source}"),
            format!("  {}^", " ".repeat(column - 1)),
        ])
    }
}

/// `source` with its unsafe characters removed, and the 1-based character
/// `column` moved left by the number removed before it, so the caret still lines
/// up under the same character of the sanitized text.
fn sanitize_with_column(source: &str, column: usize) -> (String, usize) {
    let mut clean = String::with_capacity(source.len());
    let mut removed_before = 0;
    for (i, c) in source.chars().enumerate() {
        if is_unsafe_output_char(c) {
            if i + 1 < column {
                removed_before += 1;
            }
        } else {
            clean.push(c);
        }
    }
    (clean, column - removed_before)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine text for `query`, from the real parser through the real
    /// front end, so a change to how the parser renders a position fails here
    /// instead of silently dropping it.
    fn engine_text(query: &str) -> String {
        let db = relate::Database::open(":memory:").expect("open");
        let mut conn = db.connect_typed(relate::Namespace::Neo4j).expect("connect");
        match conn.query(query).unwrap_err() {
            relate::Error::Syntax(text) => text,
            other => panic!("not a syntax error: {other}"),
        }
    }

    #[test]
    fn the_real_parsers_excerpt_gives_a_column_under_the_token() {
        let text = engine_text("RETRN 1");
        let Some(SyntaxPosition::Excerpt { source, column }) = SyntaxPosition::from_text(&text)
        else {
            panic!("not an excerpt: {text}");
        };
        assert_eq!(source, "RETRN 1");
        // The caret sits under the first character of `RETRN`.
        assert_eq!(column, 1, "{text}");
    }

    #[test]
    fn the_column_counts_characters_not_bytes() {
        let text = engine_text("RETURN 1 AS é, é RETRN 5");
        let Some(SyntaxPosition::Excerpt { source, column, .. }) = SyntaxPosition::from_text(&text)
        else {
            panic!("not an excerpt: {text}");
        };
        let under: String = source.chars().skip(column - 1).collect();
        assert!(
            under.starts_with("RETRN"),
            "caret column {column} in {source:?}"
        );
    }

    #[test]
    fn the_caret_lines_up_under_the_source() {
        let text = engine_text("RETURN 1 AS x, 2 AS y RETRN 3");
        let position = SyntaxPosition::from_text(&text).expect("excerpt");
        let SyntaxPosition::Excerpt { source, .. } = &position else {
            panic!("not an excerpt: {text}");
        };
        // Both lines carry the same two-space indent, so the caret sits under
        // the token exactly when its offset is the token's offset in `source`.
        let [echoed, caret] = position.echo_lines().expect("echo lines");
        assert_eq!(echoed, format!("  {source}"));
        // Offsets are characters, like `column`; the two-space indent is shared.
        let at = caret.chars().position(|c| c == '^').expect("caret") - 2;
        let under: String = source.chars().skip(at).collect();
        assert!(under.starts_with("RETRN"), "{source:?} vs {caret:?}");
    }

    #[test]
    fn a_detail_tag_before_parse_error_is_still_recognized() {
        let text = "IntegerOverflow: parse error: bad\n  RETURN 99\n         ^";
        assert_eq!(
            SyntaxPosition::from_text(text),
            Some(SyntaxPosition::Excerpt {
                source: "RETURN 99".to_string(),
                column: 8
            })
        );
    }

    #[test]
    fn a_position_only_message_gives_line_and_column() {
        let text = "parse error: oops at line 3, col 7";
        assert_eq!(
            SyntaxPosition::from_text(text),
            Some(SyntaxPosition::Line { line: 3, column: 7 })
        );
        assert_eq!(
            SyntaxPosition::from_text(text).and_then(|p| p.echo_lines()),
            None
        );
    }

    #[test]
    fn an_unknown_position_and_other_text_give_none() {
        for text in [
            // the parser's own "unknown position" rendering
            "parse error: unexpected token \"\" (EOF) at line 0, col 0 at line 0, col 0",
            "parse error: oops at line 0, col 0",
            "something else entirely",
            "parse error: x\n  src\n  not a caret",
            "parse error: x\n  src\n    ^\n  extra",
            "database is read-only",
            "",
        ] {
            assert_eq!(SyntaxPosition::from_text(text), None, "{text:?}");
        }
    }

    #[test]
    fn removing_a_control_character_before_the_caret_moves_the_column_with_it() {
        // `RETRN` starts at column 23 of the raw line, 22 once the ESC is gone.
        let raw = "RETURN 'a\u{1b}[31mb' AS x RETRN 2";
        let text = format!("parse error: bad\n  {raw}\n  {}^", " ".repeat(22));
        let Some(SyntaxPosition::Excerpt { source, column, .. }) = SyntaxPosition::from_text(&text)
        else {
            panic!("no excerpt");
        };
        assert!(!source.contains('\u{1b}'));
        assert_eq!(column, 22);
        assert!(source
            .chars()
            .skip(column - 1)
            .collect::<String>()
            .starts_with("RETRN"));
        // A control character after the caret does not move it.
        let text = "parse error: bad\n  AB\u{1b}CD\n  ^";
        let Some(SyntaxPosition::Excerpt { column, .. }) = SyntaxPosition::from_text(text) else {
            panic!("no excerpt");
        };
        assert_eq!(column, 1);
    }

    #[test]
    fn control_characters_in_the_echoed_source_are_removed() {
        let text = "parse error: x\n  RE\u{1b}[31mTURN\n  ^";
        let Some(SyntaxPosition::Excerpt { source, .. }) = SyntaxPosition::from_text(text) else {
            panic!("no excerpt");
        };
        assert!(!source.contains('\u{1b}'), "{source:?}");
    }
}
