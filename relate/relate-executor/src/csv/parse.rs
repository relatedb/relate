//! CSV text parsing: RFC 4180 record and field splitting and the `LOAD CSV`
//! row-value construction. Pure functions over text — no filesystem or
//! policy concerns (those live in [`super::policy`]).

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};

/// The one RFC 4180 quote-state rule (issue #91) shared by
/// [`RecordCursor::next_range`] and [`split_csv_line`], so the rule lives
/// once:
/// a `"` opens a quoted field only at a field start; inside quotes `""` is
/// one literal `"` and a lone `"` closes the field; every other character —
/// delimiter and newline included — is field data.
#[derive(Default)]
struct QuoteScan {
    in_quotes: bool,
    field_started: bool,
}

/// What one input character means under the [`QuoteScan`] rule.
enum QuoteEvent {
    /// `c` is field data. Inside quotes that includes delimiters and
    /// newlines; outside quotes it includes a stray `"` that cannot open a
    /// field. Callers distinguish a record-breaking newline from quoted
    /// data via [`QuoteScan::in_quotes`].
    Data(char),
    /// `c` opened a quoted field.
    EnterQuotes,
    /// `c` closed a quoted field.
    ExitQuotes,
    /// `c` starts a `""` escape inside quotes: one literal `"`; the caller
    /// consumes the second `"`.
    EscapedQuote,
    /// `c` is the field delimiter outside quotes: the field ends here.
    FieldEnd,
}

impl QuoteScan {
    /// Classify `c` and advance the quote state. `next_is_quote` peeks the
    /// following character so a `""` escape is recognized without consuming
    /// it; `delim` is the field terminator.
    fn advance(&mut self, c: char, next_is_quote: bool, delim: char) -> QuoteEvent {
        if self.in_quotes {
            if c == '"' {
                if next_is_quote {
                    return QuoteEvent::EscapedQuote;
                }
                self.in_quotes = false;
                return QuoteEvent::ExitQuotes;
            }
            return QuoteEvent::Data(c);
        }
        if c == '"' && !self.field_started {
            self.in_quotes = true;
            self.field_started = true;
            return QuoteEvent::EnterQuotes;
        }
        if c == delim {
            self.field_started = false;
            return QuoteEvent::FieldEnd;
        }
        self.field_started = true;
        QuoteEvent::Data(c)
    }

    /// Whether the scanner is inside a quoted field — a `Data('\n')` event
    /// in this state is quoted field content, not a record break.
    fn in_quotes(&self) -> bool {
        self.in_quotes
    }
}

/// The record-breaking cursor — this module's one record policy, in its
/// streaming form: one record at a time, so a consumer (the `LOAD CSV`
/// feed, ADR-0555) walks a source's records without ever holding them.
/// The cursor owns its text — an `Arc<str>` from the reader's memo — and
/// one cursor spans one source; the feed drops both when the source is
/// spent.
pub(crate) struct RecordCursor<S> {
    text: S,
    pos: usize,
}

/// The end of the record that breaks at `break_at`, with a `\r`
/// immediately before the break stripped — `str::lines` parity, the one
/// spelling of the record-break rule's `\r` handling. `floor` is the
/// record's start: a break at the very start has no preceding byte to
/// strip.
fn cr_stripped_end(text: &str, break_at: usize, floor: usize) -> usize {
    if break_at > floor && text.as_bytes()[break_at - 1] == b'\r' {
        break_at - 1
    } else {
        break_at
    }
}

impl<S: AsRef<str>> RecordCursor<S> {
    pub(crate) fn new(text: S) -> Self {
        Self { text, pos: 0 }
    }

    /// The next physical record, or `None` once the text is spent. The
    /// record is a slice of the cursor's own text — the consumer copies
    /// only what it keeps, so a record is never cloned on the way out
    /// (the cursor holds the text for the whole source).
    pub(crate) fn next_record(&mut self, delim: char) -> Option<&str> {
        let text = self.text.as_ref();
        let (start, end, next) = Self::next_range(text, self.pos, delim)?;
        self.pos = next;
        Some(&text[start..end])
    }

    /// How many records remain from the cursor's position whose text
    /// `keep` admits — the materializing feed's charge fact (ADR-0463),
    /// counted without materializing any of them. `keep` is the
    /// consumer's own "this record yields a row" policy, so the count
    /// bills exactly what the walk will build.
    pub(crate) fn remaining_records(&self, delim: char, keep: impl Fn(&str) -> bool) -> usize {
        let text = self.text.as_ref();
        let mut count = 0;
        let mut pos = self.pos;
        while let Some((start, end, next)) = Self::next_range(text, pos, delim) {
            if keep(&text[start..end]) {
                count += 1;
            }
            pos = next;
        }
        count
    }

    /// The next record's `(start, end, next_pos)` byte range from `pos` —
    /// the one scan [`Self::next_record`] and [`Self::remaining_records`]
    /// both drive, so the record-breaking policy lives once.
    fn next_range(text: &str, pos: usize, delim: char) -> Option<(usize, usize, usize)> {
        if pos >= text.len() {
            return None;
        }
        // A `\n` field terminator would make every line break a field end,
        // collapsing the whole text into one record — the terminator must
        // not double as the record break. Split records on `\n`
        // unconditionally; each record's cells then split on the terminator
        // (a `\n` terminator yields the pre-quote one-row-per-line,
        // single-cell behavior). `str::lines` semantics exactly: a `\r` is
        // stripped only when it precedes the `\n` — a lone trailing `\r` is
        // data, unlike the general path's final-record strip below.
        if delim == '\n' {
            return match text[pos..].find('\n') {
                Some(i) => {
                    let break_at = pos + i;
                    Some((pos, cr_stripped_end(text, break_at, pos), break_at + 1))
                }
                None => Some((pos, text.len(), text.len())),
            };
        }
        let mut scan = QuoteScan::default();
        let mut iter = text[pos..].char_indices().peekable();
        while let Some((offset, c)) = iter.next() {
            let i = pos + offset;
            match scan.advance(c, iter.peek().is_some_and(|&(_, next)| next == '"'), delim) {
                QuoteEvent::EscapedQuote => {
                    iter.next();
                }
                QuoteEvent::Data('\n') if !scan.in_quotes() => {
                    return Some((pos, cr_stripped_end(text, i, pos), i + 1));
                }
                QuoteEvent::Data(_)
                | QuoteEvent::EnterQuotes
                | QuoteEvent::ExitQuotes
                | QuoteEvent::FieldEnd => {}
            }
        }
        // The final record without a line ending still counts, and its
        // trailing `\r` is stripped exactly like a broken record's —
        // `str::lines` parity (a CRLF file missing its final `\n` must
        // not load a `\r`-suffixed last cell).
        Some((pos, cr_stripped_end(text, text.len(), pos), text.len()))
    }
}

/// Split one CSV record on `delim`, honoring RFC 4180 double-quoting (issue
/// #91): a field starting with `"` runs to the closing `"` with delimiters
/// and newlines literal inside, and `""` inside a quoted field is one `"`.
/// Records come from [`RecordCursor`], which finds the record breaks with
/// the same [`QuoteScan`] rule. An unterminated quote is preserved
/// as-is (no silent data loss) — the row keeps whatever was read.
/// Leading/trailing spaces outside quotes are kept, matching the unquoted
/// splitter's behavior.
pub(crate) fn split_csv_line(line: &str, delim: char) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut scan = QuoteScan::default();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match scan.advance(c, chars.peek() == Some(&'"'), delim) {
            QuoteEvent::Data(c) => cur.push(c),
            QuoteEvent::EscapedQuote => {
                cur.push('"');
                chars.next();
            }
            QuoteEvent::FieldEnd => cells.push(std::mem::take(&mut cur)),
            QuoteEvent::EnterQuotes | QuoteEvent::ExitQuotes => {}
        }
    }
    cells.push(cur);
    cells
}

/// Resolve the `FIELDTERMINATOR` literal to the split delimiter. The
/// terminator must be exactly one character; a longer or empty string is a
/// type error rather than a silent truncation to its first character. The
/// parser already guarantees a string literal (relate-parser
/// `parse_load_csv`), but not its width — that is a property of the value,
/// so the check stays here at execution time rather than in the front-end's
/// lexical-shape validation.
pub(crate) fn field_terminator_char(field_terminator: Option<&str>) -> Result<char> {
    match field_terminator {
        None => Ok(','),
        Some(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(c),
                _ => Err(Error::ty(
                    "LOAD CSV FIELDTERMINATOR must be exactly one character",
                )),
            }
        }
    }
}

/// Zip header names with row cells under the ragged-row width rule
/// (docs/api/cli/spec.md): a row with fewer cells than the header maps the
/// missing names to `null` — Neo4j's `LOAD CSV WITH HEADERS` semantics,
/// adopted deliberately so an absent field is distinguishable from an
/// empty one (this supersedes the earlier empty-string padding) — and
/// extra cells past the header width are dropped, so the row map is keyed
/// by exactly the header names.
fn zip_with_header_nulls<'a>(
    headers: &'a [String],
    cells: &'a [String],
) -> impl Iterator<Item = (&'a String, Value)> + 'a {
    headers.iter().enumerate().map(|(i, key)| {
        let value = match cells.get(i) {
            Some(cell) => Value::String(cell.as_str().into()),
            None => Value::Null,
        };
        (key, value)
    })
}

/// Build the `LOAD CSV` row value for one parsed record: a `Map` keyed by
/// the header row under `WITH HEADERS` (ragged rows follow
/// [`zip_with_header_nulls`]), a `List` of every cell otherwise — list
/// rows keep their natural width, never padded or truncated.
pub(crate) fn record_value(headers: Option<&[String]>, cells: Vec<String>) -> Value {
    match headers {
        Some(headers) => Value::Map(
            zip_with_header_nulls(headers, &cells)
                .map(|(key, value)| (key.clone(), value))
                .collect::<BTreeMap<_, _>>()
                .into(),
        ),
        None => Value::List(
            cells
                .into_iter()
                .map(|c| Value::String(c.into()))
                .collect::<Vec<_>>()
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{field_terminator_char, split_csv_line, RecordCursor};

    /// The cursor's records as one vector — the record-breaking policy's
    /// one test spelling.
    fn split(text: &str, delim: char) -> Vec<String> {
        let mut cursor = RecordCursor::new(text);
        let mut out = Vec::new();
        while let Some(record) = cursor.next_record(delim) {
            out.push(record.to_owned());
        }
        out
    }

    #[test]
    fn final_record_without_line_ending_strips_trailing_cr() {
        let rs = split("a,b\r\nc,d\r", ',');
        assert_eq!(rs, ["a,b", "c,d"]);
    }

    #[test]
    fn newline_field_terminator_keeps_one_row_per_line() {
        let rs = split("1\n2\n3", '\n');
        assert_eq!(rs, ["1", "2", "3"]);
        assert_eq!(split_csv_line("1", '\n'), vec!["1".to_string()]);
    }

    #[test]
    fn quoted_field_with_delimiter_stays_one_cell() {
        let cells = split_csv_line("1,\"Neo4j, Cypher and graphs\",x", ',');
        assert_eq!(cells, vec!["1", "Neo4j, Cypher and graphs", "x"]);
    }

    #[test]
    fn doubled_quote_inside_quoted_field_is_one_quote() {
        let cells = split_csv_line("\"a \"\"quoted\"\" b\",2", ',');
        assert_eq!(cells, vec!["a \"quoted\" b", "2"]);
    }

    #[test]
    fn header_row_is_quote_aware_too() {
        let cells = split_csv_line("id,\"the, header\",n", ',');
        assert_eq!(cells, vec!["id", "the, header", "n"]);
    }

    #[test]
    fn unquoted_lines_are_unchanged() {
        assert_eq!(split_csv_line("a,b,c", ','), vec!["a", "b", "c"]);
        assert_eq!(split_csv_line("a", ','), vec!["a"]);
    }

    #[test]
    fn unterminated_quote_is_preserved_not_dropped() {
        // RFC violation, but the row must not silently lose data.
        let cells = split_csv_line("\"open,ended", ',');
        assert_eq!(cells, vec!["open,ended"]);
    }

    #[test]
    fn quoted_field_with_newline_stays_one_record() {
        let records = split("1,\"line one\nline two\",3\n4,four,5\n", ',');
        assert_eq!(records, vec!["1,\"line one\nline two\",3", "4,four,5"]);
        assert_eq!(
            split_csv_line(&records[0], ','),
            vec!["1", "line one\nline two", "3"]
        );
    }

    #[test]
    fn quoted_record_break_with_crlf_and_blank_lines() {
        let records = split("a,\"b\r\nc\"\r\n\r\n\"d\",e", ',');
        assert_eq!(records, vec!["a,\"b\r\nc\"", "", "\"d\",e"]);
    }

    #[test]
    fn stray_quote_never_swallows_a_record_break() {
        // A `"` not at a field start is literal, so it cannot start a
        // quoted field that eats the following lines.
        let records = split("say \"hi\",1\nnext,2\n", ',');
        assert_eq!(records, vec!["say \"hi\",1", "next,2"]);
    }

    #[test]
    fn custom_field_terminator_respects_quotes() {
        let cells = split_csv_line("a\t\"b\tc\"\td", '\t');
        assert_eq!(cells, vec!["a", "b\tc", "d"]);
    }

    #[test]
    fn empty_fields_and_only_quotes() {
        assert_eq!(split_csv_line(",\"\",x", ','), vec!["", "", "x"]);
        assert_eq!(split_csv_line("a,,", ','), vec!["a", "", ""]);
    }

    #[test]
    fn field_terminator_must_be_exactly_one_char() {
        assert_eq!(field_terminator_char(None).unwrap(), ',');
        assert_eq!(field_terminator_char(Some("\t")).unwrap(), '\t');
        for invalid in ["||", ",;", ""] {
            assert!(
                field_terminator_char(Some(invalid)).is_err(),
                "{invalid:?} must be rejected"
            );
        }
    }

    /// The cursor's counted remainder always equals what walking it yields
    /// — the charge fact the streaming feed reads must not drift from the
    /// records the feed generates. The `\r` asymmetry is pinned here too:
    /// a lone trailing `\r` is stripped by the general path's final record
    /// and kept as data by the `\n`-terminator path's `str::lines`
    /// semantics.
    #[test]
    fn the_cursor_counts_what_it_yields() {
        let cases: &[(&str, char)] = &[
            ("a,b\r\nc,d\r", ','),
            ("1\n2\n3", '\n'),
            ("1,\"line one\nline two\",3\n4,four,5\n", ','),
            ("a,\"b\r\nc\"\r\n\r\n\"d\",e", ','),
            ("say \"hi\",1\nnext,2\n", ','),
            ("a\r", ','),
            ("a\r", '\n'),
            ("\r", ','),
            ("\n", '\n'),
            ("", ','),
            ("no breaks at all", ','),
        ];
        for &(text, delim) in cases {
            let walked = split(text, delim);
            assert_eq!(
                RecordCursor::new(text).remaining_records(delim, |_| true),
                walked.len(),
                "count parity on {text:?} delim {delim:?}"
            );
        }
    }
}
