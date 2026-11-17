use std::borrow::Cow;

use crate::token::{lookup_keyword, Token, TokenType};
use crate::unicode_identifier;

mod ascii;
mod number;
mod string;

/// Parse-affecting preview options: the ADR-0371 preview gates that widen
/// what the parser accepts (the lexer's token stream and the parser's own
/// grammar), resolved by the caller (the front derives them from the
/// store's `RELATE_PREVIEW` set). The default is every gate closed --
/// the dialect floor. One struct so a speculative or nested parse inherits
/// its caller's dialect by plain field copy; the flags are `Copy`, never
/// re-derived per site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseOptions {
    /// The `obfuscated-literals` gate: a run of exactly six `*` lexes as
    /// one [`TokenType::ObfuscatedLiteral`] instead of six `Star` tokens.
    pub obfuscated_literals: bool,
    /// The `mixed-label-syntax` gate (Neo4j's
    /// mixed-label-syntax extra semantic feature):
    /// legacy colon-conjunction (`:A:B`), GQL label-expression symbols
    /// (`|`, `&`, `!`, `%`) and the `IS` introducer may mix within one
    /// clause and within one label expression (`(n:A|B:C)`,
    /// `(n IS S:R)`), with `:` parsed as an `&`-level conjunction
    /// operator. Off (the floor), every CIP-40/CIP-14 mixing rejection
    /// keeps firing.
    pub mixed_label_syntax: bool,
}

/// Tokenizes a Cypher query string into a list of tokens.
///
/// The tokens borrow `input` (ADR-0505): they stay valid exactly as long as
/// the query text does.
pub fn tokenize(input: &str) -> Vec<Token<'_>> {
    tokenize_with_options(input, ParseOptions::default())
}

/// Tokenizes with explicit [`ParseOptions`]; [`tokenize`] is the same call
/// with every gate closed.
pub fn tokenize_with_options(input: &str, options: ParseOptions) -> Vec<Token<'_>> {
    let mut lexer = Lexer::new(input, options);
    lexer.tokenize();
    lexer.tokens
}

/// What [`skip_comment`] found at a position in the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentSkip {
    /// No comment starts here.
    NotComment,
    /// A comment ended just before this byte position.
    Skipped(usize),
    /// A block comment opened without a closing `*/` — the lexer's error
    /// case.
    Unterminated,
}

/// The Cypher comment starting at `input[pos..]` — the one spelling of the
/// comment rule, shared by the lexer and by callers that re-read a literal
/// token's text through the same grammar.
///
/// A `//` line comment runs through (and includes) its terminating newline;
/// a `/* */` block comment does not nest and ends at the first `*/`.
pub fn skip_comment(input: &[u8], pos: usize) -> CommentSkip {
    if input.get(pos) == Some(&b'/') {
        if input.get(pos + 1) == Some(&b'/') {
            let mut end = pos + 2;
            while end < input.len() && input[end] != b'\n' {
                end += 1;
            }
            return CommentSkip::Skipped((end + 1).min(input.len()));
        }
        if input.get(pos + 1) == Some(&b'*') {
            let mut end = pos + 2;
            while end < input.len() {
                if input[end] == b'*' && input.get(end + 1) == Some(&b'/') {
                    return CommentSkip::Skipped(end + 2);
                }
                end += 1;
            }
            return CommentSkip::Unterminated;
        }
    }
    CommentSkip::NotComment
}

struct Lexer<'a> {
    /// The input as `&str` — the borrowed-literal slice source (ADR-0505).
    /// Slicing it costs one O(1) character-boundary check, where
    /// `from_utf8` over the byte view would re-validate the whole token
    /// text; an all-ASCII run classified by the fast path is valid by
    /// construction and never re-validated.
    text: &'a str,
    /// `text` as bytes — the scanning view. One input, two immutable
    /// borrowed views of the same data, created together in [`Lexer::new`].
    input: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    tokens: Vec<Token<'a>>,
    options: ParseOptions,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str, options: ParseOptions) -> Self {
        Lexer {
            text: input,
            input: input.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
            options,
        }
    }

    fn tokenize(&mut self) {
        while self.pos < self.input.len() {
            self.skip_whitespace();
            if self.pos >= self.input.len() {
                break;
            }

            let ch = self.peek();

            let line = self.line;
            let col = self.col;

            match ch {
                b'(' => {
                    self.advance();
                    self.emit(TokenType::LeftParen, "(", line, col);
                }
                b')' => {
                    self.advance();
                    self.emit(TokenType::RightParen, ")", line, col);
                }
                b'[' => {
                    self.advance();
                    self.emit(TokenType::LeftBracket, "[", line, col);
                }
                b']' => {
                    self.advance();
                    self.emit(TokenType::RightBracket, "]", line, col);
                }
                b'{' => {
                    self.advance();
                    self.emit(TokenType::LeftBrace, "{", line, col);
                }
                b'}' => {
                    self.advance();
                    self.emit(TokenType::RightBrace, "}", line, col);
                }
                b':' => {
                    if self.peek_at(1) == Some(b':') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::DoubleColon, "::", line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::Colon, ":", line, col);
                    }
                }
                b',' => {
                    self.advance();
                    self.emit(TokenType::Comma, ",", line, col);
                }
                b';' => {
                    self.advance();
                    self.emit(TokenType::Semicolon, ";", line, col);
                }
                b'|' => {
                    if self.peek_at(1) == Some(b'|') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::DoublePipe, "||", line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::Pipe, "|", line, col);
                    }
                }
                b'*' => {
                    // The obfuscated literal is the one gated spelling:
                    // exactly six stars under the preview gate lexes as one
                    // dummy literal; any other run (or the gate closed)
                    // keeps the ordinary `*` tokens.
                    if self.options.obfuscated_literals && self.star_run() == 6 {
                        for _ in 0..6 {
                            self.advance();
                        }
                        self.emit(TokenType::ObfuscatedLiteral, "******", line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::Star, "*", line, col);
                    }
                }
                b'+' => {
                    self.advance();
                    self.emit(TokenType::Plus, "+", line, col);
                }
                b'/' => {
                    // The comment rule lives once in `skip_comment`.
                    // `Unterminated` must not silently swallow the rest of
                    // the input (which could hide trailing statements such
                    // as `RETURN 1 /* DETACH DELETE n`).
                    match skip_comment(self.input, self.pos) {
                        CommentSkip::Skipped(end) => self.advance_by(end - self.pos),
                        CommentSkip::Unterminated => {
                            self.advance_by(self.input.len() - self.pos);
                            self.emit(TokenType::Error, "unterminated block comment", line, col);
                        }
                        CommentSkip::NotComment => {
                            self.advance();
                            self.emit(TokenType::Slash, "/", line, col);
                        }
                    }
                }
                b'%' => {
                    self.advance();
                    self.emit(TokenType::Percent, "%", line, col);
                }
                b'^' => {
                    self.advance();
                    self.emit(TokenType::Caret, "^", line, col);
                }
                b'=' => {
                    if self.peek_at(1) == Some(b'~') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::RegexMatch, "=~", line, col);
                    } else if self.peek_at(1) == Some(b'>') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::FatArrow, "=>", line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::Equals, "=", line, col);
                    }
                }
                b'.' => {
                    if self.peek_at(1) == Some(b'.') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::DotDot, "..", line, col);
                    } else if matches!(self.peek_at(1), Some(b'0'..=b'9')) {
                        self.read_number(line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::Dot, ".", line, col);
                    }
                }
                b'-' => {
                    if self.peek_at(1) == Some(b'>') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::ArrowRight, "->", line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::Dash, "-", line, col);
                    }
                }
                b'<' => {
                    if self.peek_at(1) == Some(b'-') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::ArrowLeft, "<-", line, col);
                    } else if self.peek_at(1) == Some(b'>') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::NotEquals, "<>", line, col);
                    } else if self.peek_at(1) == Some(b'=') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::LessEqual, "<=", line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::LessThan, "<", line, col);
                    }
                }
                b'>' => {
                    if self.peek_at(1) == Some(b'=') {
                        self.advance();
                        self.advance();
                        self.emit(TokenType::GreaterEqual, ">=", line, col);
                    } else {
                        self.advance();
                        self.emit(TokenType::GreaterThan, ">", line, col);
                    }
                }
                b'$' => {
                    self.advance();
                    self.read_parameter(line, col);
                }
                b'!' => {
                    self.advance();
                    self.emit(TokenType::Bang, "!", line, col);
                }
                b'&' => {
                    self.advance();
                    self.emit(TokenType::Ampersand, "&", line, col);
                }
                b'\'' | b'"' => {
                    self.read_string(line, col);
                }
                b'`' => {
                    // The one canonical backtick scan (string.rs): it
                    // returns the decoded name and owns the
                    // unterminated-backtick error; this arm owns the token
                    // type. An empty backtick-quoted name (` `` `) emits an
                    // Identifier with an empty literal — the historical
                    // behavior, unchanged here and pinned by
                    // `test_empty_backtick_identifier_lexes_as_today` —
                    // unlike the parameter path, whose empty literal is the
                    // reserved `$(expr)` sentinel and therefore rejected.
                    if let Some(name) = self.read_backtick_name(line, col) {
                        self.emit(TokenType::Identifier, name, line, col);
                    }
                }
                b'0'..=b'9' => {
                    self.read_number(line, col);
                }
                // The remaining bytes open either an identifier or one
                // invalid character; the decoded-character decision lives in
                // `at_ident_start`, so `read_identifier` has exactly this one
                // call site (ADR-0182).
                _ if self.at_ident_start() => {
                    self.read_identifier(line, col);
                }
                _ => {
                    // A decoded non-ASCII character that is not an identifier
                    // start is ONE invalid Unicode character, not one mojibake
                    // error per UTF-8 byte (LABS-188).
                    self.read_invalid_char(line, col);
                }
            }
        }
    }

    fn peek(&self) -> u8 {
        if self.pos >= self.input.len() {
            0
        } else {
            self.input[self.pos]
        }
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        let p = self.pos + offset;
        if p >= self.input.len() {
            None
        } else {
            Some(self.input[p])
        }
    }

    fn advance(&mut self) -> u8 {
        if self.pos >= self.input.len() {
            return 0;
        }
        let ch = self.input[self.pos];
        self.pos += 1;
        if ch == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        ch
    }

    /// Decode the character at the cursor as `(char, byte_len)` without
    /// consuming it; `None` only on malformed UTF-8. ASCII is decided on the
    /// byte; a byte >= 0x80 decodes its full sequence (via the string
    /// module's [`string::decode_char`]).
    fn peek_char(&self) -> Option<(char, usize)> {
        string::decode_char(&self.input[self.pos..])
    }

    /// Consume `len` bytes — the byte length [`Lexer::peek_char`] reported for
    /// the character at the cursor — so a multi-byte character is never split.
    fn advance_by(&mut self, len: usize) {
        for _ in 0..len {
            self.advance();
        }
    }

    /// The input slice `[start, end)` as `&'a str` — the borrowed-literal
    /// source of ADR-0505. Both bounds sit on character boundaries by
    /// construction (token starts, and ends at an ASCII delimiter or a
    /// decoded-character edge), and the input itself came from a `&str`,
    /// so slicing it is an O(1) boundary check that panics on a broken
    /// invariant — never a fallback, and never a re-validation of the
    /// bytes between the bounds (interior bytes are valid UTF-8 because
    /// the input is).
    fn slice(&self, start: usize, end: usize) -> &'a str {
        &self.text[start..end]
    }

    /// The eight bytes at the cursor as one little-endian `u64` word for
    /// the `ascii` fast path, zero-padded past the end of the input. The
    /// padding bytes classify as none of the ASCII classes, so an
    /// overhanging word never claims bytes the input does not have.
    fn load_word(&self) -> u64 {
        let end = (self.pos + 8).min(self.input.len());
        let mut buf = [0u8; 8];
        buf[..end - self.pos].copy_from_slice(&self.input[self.pos..end]);
        u64::from_le_bytes(buf)
    }

    /// Emit a token. `literal` follows the ADR-0505 rule: an input slice
    /// (identifiers, numbers, parameters, escape-free strings), a
    /// `&'static str` (punctuation, fixed error texts), or an owned `String`
    /// (escape-decoded / synthesized text) — `Into<Cow>` takes each without
    /// a hidden copy, so nothing here allocates on the borrowed paths.
    fn emit(&mut self, ty: TokenType, literal: impl Into<Cow<'a, str>>, line: usize, col: usize) {
        self.tokens.push(Token::new(ty, literal, line, col));
    }

    /// Emit a [`TokenType::Error`] token carrying a symbolic openCypher-TCK
    /// error-*detail* name (see [`Token::detail`]). The parser propagates the
    /// detail onto the raised `ParseError` so a malformed literal reports its
    /// specific TCK detail (`InvalidNumberLiteral`, `InvalidUnicodeLiteral`)
    /// instead of the generic `UnexpectedSyntax`. `literal` takes the same
    /// borrowed/owned forms as [`Lexer::emit`]; the error path is the one
    /// place owned text is expected (`format!`-built messages).
    fn emit_error_detail(
        &mut self,
        literal: impl Into<Cow<'a, str>>,
        line: usize,
        col: usize,
        detail: &'static str,
    ) {
        self.tokens
            .push(Token::error_with_detail(literal, line, col, detail));
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len() {
            // ASCII whitespace (CIP-88 adds vertical tab and form feed to
            // the previous \t \n \r + space set; verified on 2026.09.0) is
            // consumed eight bytes per classification step (`ascii`,
            // ADR-0505), the batch settling line/column exactly as
            // byte-at-a-time `advance` would.
            let w = self.load_word();
            let n = ascii::ws_prefix(w);
            if n > 0 {
                self.advance_ws_run(w, n);
                continue;
            }
            // CIP-88: the GQL/Java Unicode whitespace set (Zs + Zl + Zp
            // categories plus separators, i.e. Rust's char::is_whitespace)
            // is also accepted between tokens — e.g. NBSP U+00A0. Verified
            // against Neo4j 2026.09.0. The byte at the cursor is not ASCII
            // whitespace, so only a non-ASCII character can still qualify;
            // the decoded path decides.
            if let Some((c, len)) = self.peek_char() {
                if c.is_whitespace() {
                    self.advance_by(len);
                    continue;
                }
            }
            break;
        }
    }

    /// Consume the `n`-byte ASCII whitespace run `[0, n)` of word `w`'s
    /// lanes, settling line and column in one step — exactly `n` calls to
    /// [`Lexer::advance`] would leave: the line advances by the run's
    /// newline count, and the column restarts after the last one. The
    /// `ascii` module hands back that settled run summary; the per-lane
    /// bit layout never crosses the module boundary.
    fn advance_ws_run(&mut self, w: u64, n: usize) {
        let (newlines, after_last_newline) = ascii::ws_run_summary(w, n);
        self.line += newlines;
        if newlines == 0 {
            self.col += n;
        } else {
            // The column after the last newline is the run's bytes past
            // it, +1.
            self.col = after_last_newline + 1;
        }
        self.pos += n;
    }

    /// The length of the run of consecutive `*` starting at the cursor.
    /// The obfuscated-literal gate asks for exactly six.
    fn star_run(&self) -> usize {
        let mut n = 0usize;
        while self.peek_at(n) == Some(b'*') {
            n += 1;
        }
        n
    }

    /// True when the character at the cursor is an identifier start: an
    /// ASCII `[A-Za-z_]` byte, decided from the class table with no
    /// decoding, or a non-ASCII `char` accepted by the hand-written
    /// Unicode predicate (ADR-0182) — only a byte >= 0x80 is decoded.
    fn at_ident_start(&self) -> bool {
        let b = self.peek();
        if b < 0x80 {
            return ascii::ascii_ident_start(b);
        }
        self.peek_char()
            .is_some_and(|(c, _)| unicode_identifier::is_identifier_start(c))
    }

    /// Consume the remaining identifier-part run from `start` and return it
    /// as the run's literal — the one canonical consume-and-derive for
    /// identifier runs, consulted by `emit_malformed_number`,
    /// `read_parameter`'s pass-through, and its part-only-start error block
    /// (which differ only in message and detail tag). The run is a slice of
    /// the input (borrowed, ADR-0505) — the decoded text never differs from
    /// the raw bytes, so [`Lexer::slice`]'s O(1) boundary-checked slice is
    /// the literal for ASCII and non-ASCII runs alike, and the one place
    /// the text view is indexed.
    ///
    /// Pure-ASCII runs are consumed eight bytes per classification step
    /// (`ascii`, ADR-0505); the first byte >= 0x80 hands the scan back to
    /// the decoded-character path, which stays authoritative for the whole
    /// non-ASCII stretch (ADR-0182).
    fn take_ident_run(&mut self, start: usize) -> &'a str {
        loop {
            let n = ascii::ident_part_prefix(self.load_word());
            if n > 0 {
                // No identifier-part byte is a newline, so the batch
                // advance only moves the column.
                self.pos += n;
                self.col += n;
            }
            if n == 8 {
                continue;
            }
            // The byte at the cursor is not an ASCII identifier part: the
            // run's terminator, or a non-ASCII character — the decoded
            // path decides which, consuming a whole character if it
            // continues the run.
            if !self.take_ident_part() {
                break;
            }
        }
        self.slice(start, self.pos)
    }

    /// If the character at the current position is an identifier part,
    /// consume it — all of its UTF-8 bytes — and return true. The character
    /// is decoded through [`Lexer::peek_char`] (ASCII on the byte; a byte at
    /// or above 0x80 as its full sequence) and classified with the
    /// hand-written Unicode predicate, so a multi-byte character is never
    /// split (ADR-0182).
    fn take_ident_part(&mut self) -> bool {
        let Some((c, len)) = self.peek_char() else {
            return false;
        };
        if !unicode_identifier::is_identifier_part(c) {
            return false;
        }
        self.advance_by(len);
        true
    }

    /// Consume and report one character that opens neither a token nor an
    /// identifier: a decoded non-ASCII character is ONE
    /// `InvalidUnicodeCharacter` error token covering all of its UTF-8 bytes
    /// (LABS-188); an ASCII byte keeps the historical untagged
    /// `unexpected character:` error.
    fn read_invalid_char(&mut self, line: usize, col: usize) {
        if let Some((c, len)) = self.peek_char() {
            if !c.is_ascii() {
                self.advance_by(len);
                self.emit_error_detail(
                    format!("invalid unicode character: {c}"),
                    line,
                    col,
                    "InvalidUnicodeCharacter",
                );
                return;
            }
        }
        // An ASCII byte and a malformed byte take the same path: one byte
        // consumed, one untagged `unexpected character:` token. For ASCII the
        // decoded `char` and `char::from(byte)` are the same thing; malformed
        // UTF-8 cannot occur in `&str` input — every cursor sits on a complete
        // sequence — but keep the historical byte-wise report for the
        // defensive branch.
        let byte = self.advance();
        // The one formatted (owned) form of the untagged ASCII error text.
        self.emit(
            TokenType::Error,
            format!("unexpected character: {}", char::from(byte)),
            line,
            col,
        );
    }

    fn read_identifier(&mut self, line: usize, col: usize) {
        let start = self.pos;
        let literal = self.take_ident_run(start);
        // CIP-261: a lone `s`/`S` immediately followed by a quote starts an
        // interpolated string literal (`s'...'` / `s"..."`). Anything else
        // stays a normal identifier/keyword.
        if literal.len() == 1
            && literal.as_bytes()[0].eq_ignore_ascii_case(&b's')
            && matches!(self.peek(), b'\'' | b'"')
        {
            self.read_interpolation(line, col);
            return;
        }
        let tok_type = lookup_keyword(literal);
        self.emit(tok_type, literal, line, col);
    }

    fn read_parameter(&mut self, line: usize, col: usize) {
        // A parameter's first character is an identifier start or an ASCII
        // digit — ADR-0182's Consequences prose, confirmed against the live
        // Neo4j reference (LABS-209): a part-only character such as `€` is
        // rejected with the reference's error shape; an arbitrary name (spaces,
        // part-only characters) is spelled through the backtick form
        // $`name`, offered through the shared backtick scan below (LABS-1602
        // delivering LABS-209's acceptance criteria).
        // `(` glued to the `$` is the dynamic-expression opener (`$(expr)`,
        // e.g. `n IS LABELED $('A')`): it lexes as an intentionally empty
        // Parameter token whose expansion the parser owns. Glued only —
        // whitespace between the sigil and the `(` is not this opener (the
        // empty-literal sentinel keeps exactly the shape it had before
        // LABS-1612's whitespace skip existed). The `$all(...)`/`$any(...)`
        // quantifier openers are names followed by `(` — they follow the
        // name rule, whitespace-transparent like any parameter name
        // (pinned in the label-expression quantifier table).
        if self.peek() == b'(' {
            self.emit(TokenType::Parameter, "", line, col);
            return;
        }
        // Whitespace may sit between the `$` and the name (LABS-1612): a
        // space, tab or newline — `$ p2` names `p2` — so the whitespace
        // skip runs before the start rules; whatever follows it still faces
        // them (`$ €`, or whitespace to end of input, keeps the rejection).
        self.skip_whitespace();
        if self.peek() == b'`' {
            // The backtick form: the one canonical backtick scan, emitted
            // as a Parameter token with the unescaped name (a doubled
            // backtick collapses to one), so a name that is not
            // identifier-shaped at all — $`p 1` — is one Parameter token.
            // An empty backtick-quoted name ($``) is rejected: the empty
            // Parameter literal is the reserved sentinel of the `$(expr)`
            // dynamic opener, never a parameter's name.
            if let Some(name) = self.read_backtick_name(line, col) {
                if name.is_empty() {
                    self.reject_parameter_name("", line, col);
                } else {
                    self.emit(TokenType::Parameter, name, line, col);
                }
            }
            return;
        }
        let start_ok =
            self.at_ident_start() || self.peek_char().is_some_and(|(c, _)| c.is_ascii_digit());
        // One consume-and-derive for both outcomes: the accepted name and
        // the rejected run are the same identifier-part run from the same
        // position; the guard above only classifies its first character.
        let start = self.pos;
        let run = self.take_ident_run(start);
        if !start_ok {
            self.reject_parameter_name(run, line, col);
            return;
        }
        self.emit(TokenType::Parameter, run, line, col);
    }

    /// The one InvalidParameterName rejection (LABS-209's reference error
    /// shape): the offending run — empty for a bare `$` or an empty
    /// backtick-quoted name — becomes one error token, not a cascade (the
    /// `emit_malformed_number` shape). The message's two spellings (the
    /// bare-`$` and the run-naming one) and the detail code live here, so
    /// every rejection path in `read_parameter` emits the same shape.
    fn reject_parameter_name(&mut self, offending: &str, line: usize, col: usize) {
        const EXPECTED: &str = "expected an identifier or an integer value";
        let literal = if offending.is_empty() {
            format!("Invalid input after '$': {EXPECTED}")
        } else {
            format!("Invalid input '{offending}': {EXPECTED}")
        };
        self.emit_error_detail(Cow::Owned(literal), line, col, "InvalidParameterName");
    }
}

#[cfg(test)]
mod tests;
