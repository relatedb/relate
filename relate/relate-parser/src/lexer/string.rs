//! The string-literal family: quoted-string bodies and their escape
//! resolution, CIP-261 interpolated strings, and backtick-quoted
//! identifiers — plus the decoded-character (`decode_char`) and UTF-8 push
//! helpers they share with the rest of the lexer. Split out of `lexer.rs`
//! by the file-size gate; the methods stay on [`Lexer`].

use std::borrow::Cow;

use crate::token::TokenType;

use super::Lexer;

impl<'a> Lexer<'a> {
    /// If `hi` is a high surrogate and the four hex digits at the cursor are
    /// immediately followed by `\uXXXX` naming a low surrogate, return the
    /// combined supplementary code point and the byte count of that second
    /// escape (6). `None` leaves the lone surrogate to the invalid-literal
    /// path. Ported to the UTF-16 escape form Neo4j's Java strings use.
    fn surrogate_pair_codepoint(&self, hi: u32) -> Option<(u32, usize)> {
        if !(0xD800..=0xDBFF).contains(&hi) {
            return None;
        }
        let rest = self.input.get(self.pos + 4..)?;
        if rest.len() < 6 || rest[0] != b'\\' || rest[1] != b'u' {
            return None;
        }
        let hex = std::str::from_utf8(rest.get(2..6)?).ok()?;
        let lo = u32::from_str_radix(hex, 16).ok()?;
        if !(0xDC00..=0xDFFF).contains(&lo) {
            return None;
        }
        Some((0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), 6))
    }

    /// Read a quoted string literal. The token literal is the *decoded*
    /// text: the input slice between the quotes while the body has no `\`
    /// (the common case, borrowed per ADR-0505), switching to an owned
    /// `String` at the first escape — `push_escape` resolves `\n`,
    /// `\uXXXX`, surrogate pairs, and the two-character unknown-escape form.
    pub(super) fn read_string(&mut self, line: usize, col: usize) {
        let quote = self.advance();
        let body_start = self.pos;
        // Seeded at the first escape with the already-scanned raw prefix;
        // `None` until then means every byte so far is the borrowed body.
        let mut owned: Option<String> = None;
        while self.pos < self.input.len() {
            let ch = self.advance();
            if ch == b'\\' && self.pos < self.input.len() {
                let next = self.advance();
                let s =
                    owned.get_or_insert_with(|| self.slice(body_start, self.pos - 2).to_string());
                if self.push_escape(s, next, line, col).is_err() {
                    return;
                }
            } else if ch == quote {
                let literal = match owned {
                    Some(s) => Cow::Owned(s),
                    None => Cow::Borrowed(self.slice(body_start, self.pos - 1)),
                };
                self.emit(TokenType::String, literal, line, col);
                return;
            } else if let Some(s) = owned.as_mut() {
                push_utf8(self, s, ch);
            }
        }
        self.emit(TokenType::Error, "unterminated string", line, col);
    }

    /// Resolve the byte following a `\` in a plain string body and push its
    /// decoded form onto `s`. Used by [`Lexer::read_string`]; an unknown escape
    /// (including `\{`/`\}`) stays as the literal two-character form. Returns
    /// `Err(())` after emitting an `InvalidUnicodeLiteral` error, which the
    /// caller must treat as a stop.
    ///
    /// [`Lexer::read_interpolation`] deliberately does *not* use this: it
    /// keeps every escape unresolved so the interpolation parser can
    /// distinguish `\\` + `{` (escaped backslash, then an embed) from `\{`
    /// (escaped literal brace).
    fn push_escape(
        &mut self,
        s: &mut String,
        escaped: u8,
        line: usize,
        col: usize,
    ) -> Result<(), ()> {
        match escaped {
            b'n' => s.push('\n'),
            b't' => s.push('\t'),
            b'\\' => s.push('\\'),
            b'\'' => s.push('\''),
            b'"' => s.push('"'),
            b'u' => {
                if self.pos + 4 > self.input.len() {
                    self.emit_error_detail(
                        "invalid unicode escape sequence",
                        line,
                        col,
                        "InvalidUnicodeLiteral",
                    );
                    return Err(());
                }
                let hex = &self.input[self.pos..self.pos + 4];
                // Decide the shape before decoding: the four-byte window is
                // cut from the byte view and can end mid-character
                // (`'\u00€'` yields `30 30 E2 82`), so `from_utf8` can
                // legitimately fail here. The window must be four ASCII
                // hex digits; anything else is one malformed `\u` escape,
                // reported from its byte-escaped text — never a fabricated
                // empty operand.
                let (hex_str, cp) = match std::str::from_utf8(hex)
                    .ok()
                    .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(|s| (s, u32::from_str_radix(s, 16)))
                {
                    Some((s, Ok(cp))) => (s, cp),
                    _ => {
                        self.emit_error_detail(
                            format!(
                                "invalid unicode escape sequence: \\u{}",
                                unicode_window_text(hex)
                            ),
                            line,
                            col,
                            "InvalidUnicodeLiteral",
                        );
                        return Err(());
                    }
                };
                // A high surrogate immediately followed by a low
                // surrogate escape is one supplementary code point
                // encoded as a UTF-16 pair (`'𐀀'` is
                // U+10000) — Neo4j accepts the pair, and the TCK's
                // StringOrdering scenarios pin its code-point order.
                if let Some((combined, extra)) = self.surrogate_pair_codepoint(cp) {
                    if let Some(ch) = char::from_u32(combined) {
                        s.push(ch);
                        for _ in 0..4 + extra {
                            self.advance();
                        }
                        return Ok(());
                    }
                }
                // A lone surrogate (U+D800..=U+DFFF) and codepoints
                // above U+10FFFF are not valid `char`s; dropping them
                // would silently corrupt the string (main's #72
                // hardening).
                match char::from_u32(cp) {
                    Some(ch) => {
                        s.push(ch);
                        for _ in 0..4 {
                            self.advance();
                        }
                    }
                    None => {
                        self.emit_error_detail(
                            format!("invalid unicode escape sequence: \\u{hex_str}"),
                            line,
                            col,
                            "InvalidUnicodeLiteral",
                        );
                        return Err(());
                    }
                }
            }
            other => {
                s.push('\\');
                // An escaped non-ASCII byte must survive byte-identically,
                // not be byte-widened into mojibake (decode the UTF-8
                // sequence like read_string's fallback did).
                push_utf8(self, s, other);
            }
        }
        Ok(())
    }

    /// Read a backtick-quoted name and return its decoded literal — the
    /// one canonical backtick scan, shared by the backtick-quoted
    /// identifier and the backtick-quoted parameter name (`$`name``,
    /// LABS-1602). The literal is the input slice between the backticks
    /// (borrowed, ADR-0505) unless the name contains a doubled backtick —
    /// the one form whose decoded text differs from the raw slice — which
    /// switches to an owned `String` seeded with the prefix before the
    /// first pair. Returns `None` when the name is unterminated: the scan
    /// emits the one unterminated-backtick error token itself, so the
    /// caller has nothing to emit. The caller owns the token type (and any
    /// empty-name policy), so one scan serves every spelling without a
    /// per-flavor wrapper.
    pub(super) fn read_backtick_name(&mut self, line: usize, col: usize) -> Option<Cow<'a, str>> {
        self.advance(); // skip opening backtick
        let body_start = self.pos;
        let mut owned: Option<String> = None;
        while self.pos < self.input.len() {
            let ch = self.advance();
            if ch == b'`' {
                // A doubled backtick inside a backtick-quoted name is an
                // escaped literal backtick, mirroring the formatter's
                // `cypher_identifier` (which doubles every embedded backtick).
                // Peek the next byte: another backtick means "one literal
                // backtick, keep reading"; anything else terminates the
                // name. Without this the `.dump` / `.mode cypher` output
                // for a label/type/key containing a backtick would not re-parse.
                if self.pos < self.input.len() && self.input[self.pos] == b'`' {
                    self.advance(); // consume the second backtick of the pair
                    owned
                        .get_or_insert_with(|| self.slice(body_start, self.pos - 2).to_string())
                        .push('`');
                    continue;
                }
                return Some(match owned {
                    Some(s) => Cow::Owned(s),
                    None => Cow::Borrowed(self.slice(body_start, self.pos - 1)),
                });
            }
            if let Some(s) = owned.as_mut() {
                push_utf8(self, s, ch);
            }
        }
        self.emit(
            TokenType::Error,
            "unterminated backtick identifier",
            line,
            col,
        );
        None
    }

    /// Read a CIP-261 interpolated string literal: the opening `s` and quote
    /// are already consumed positioning-wise (`read_identifier` detected the
    /// shape and the quote is the next byte). Emits one
    /// [`TokenType::InterpString`] token whose payload is the raw body:
    /// unescaped `{`/`}` preserved as expression delimiters and *every* escape
    /// (`\n`, `\\`, `\{`, …) passed through as its original two characters --
    /// resolved later by the interpolation parser. Keeping escapes unresolved
    /// is what lets the parser tell `\\` + `{` (escaped backslash, then an
    /// embed) from `\{` (escaped literal brace); a lexer-resolved `\\` would
    /// collapse to the same byte a resolved `\{` starts with. Inside a
    /// `{ ... }` embed the raw form also makes the re-lexed expression see the
    /// original text (an embed like `{f('a\'b')}` would otherwise be
    /// corrupted).
    ///
    /// Because every byte of the body is passed through raw, the payload is
    /// byte-identical to the input slice between the quotes, so the token
    /// borrows it (ADR-0505) and the scan only tracks the state machine that
    /// finds the true closing quote.
    pub(super) fn read_interpolation(&mut self, line: usize, col: usize) {
        let quote = self.advance();
        let body_start = self.pos;
        // Brace depth and inner-string state: inside `{ ... }` an embedded
        // string -- or a backtick-quoted identifier -- may itself contain
        // braces or the outer quote character.
        let mut brace_depth = 0usize;
        let mut inner_quote: Option<u8> = None;
        let mut in_backtick = false;
        while self.pos < self.input.len() {
            let ch = self.advance();
            if ch == b'\\' && self.pos < self.input.len() {
                // Pass the escape through raw, in both text and embed regions.
                self.advance();
            } else if let Some(q) = inner_quote {
                if ch == q {
                    inner_quote = None;
                }
            } else if in_backtick {
                if ch == b'`' {
                    in_backtick = false;
                }
            } else if brace_depth > 0 {
                match ch {
                    b'{' => brace_depth += 1,
                    b'}' => brace_depth -= 1,
                    b'\'' | b'"' => inner_quote = Some(ch),
                    b'`' => in_backtick = true,
                    _ => {}
                }
            } else if ch == b'{' {
                brace_depth += 1;
            } else if ch == quote {
                let body = self.slice(body_start, self.pos - 1);
                self.emit(TokenType::InterpString, body, line, col);
                return;
            }
        }
        self.emit(TokenType::Error, "unterminated string", line, col);
    }
}

/// Decode the first UTF-8 character of `bytes` as `(char, byte_len)`, or
/// `None` on malformed/truncated input.
///
/// Validation is bounded to the single 2–4 byte sequence being decoded,
/// never the whole remaining slice: `skip_whitespace` calls this at every
/// token boundary, so validating the tail here would make lexing quadratic on
/// input heavy in multi-byte whitespace.
pub(super) fn decode_char(bytes: &[u8]) -> Option<(char, usize)> {
    let first = *bytes.first()?;
    if first < 0x80 {
        return Some((first as char, 1));
    }
    let seq_len = if first >= 0xF0 {
        4
    } else if first >= 0xE0 {
        3
    } else {
        2
    };
    let end = seq_len.min(bytes.len());
    let s = std::str::from_utf8(&bytes[..end]).ok()?;
    let c = s.chars().next()?;
    Some((c, c.len_utf8()))
}

/// Push one raw character of a string or interpolation body, decoding
/// multi-byte UTF-8 via [`decode_char`] and advancing past the continuation
/// bytes. Shared by [`Lexer::read_string`] and [`Lexer::read_interpolation`].
fn push_utf8(lexer: &mut Lexer<'_>, s: &mut String, ch: u8) {
    if ch < 0x80 {
        s.push(ch as char);
        return;
    }
    let start = lexer.pos - 1;
    if let Some((c, len)) = decode_char(&lexer.input[start..]) {
        s.push(c);
        for _ in 1..len {
            lexer.advance();
        }
    } else {
        s.push(ch as char);
    }
}

/// The `\u` escape's four-byte operand window as its error text: a window
/// `from_utf8` accepts is reported as itself (the historical message), and
/// one cut mid-character — the one form `from_utf8` rejects — is escaped
/// per byte, so the report names exactly the bytes found instead of a
/// fabricated empty operand.
fn unicode_window_text(hex: &[u8]) -> String {
    match std::str::from_utf8(hex) {
        Ok(s) => s.to_string(),
        Err(_) => hex.iter().map(|&b| format!("\\x{b:02x}")).collect(),
    }
}
