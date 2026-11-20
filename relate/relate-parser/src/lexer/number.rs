//! The numeric-literal scanner: `read_number` (hex/octal prefixes, the
//! Cypher 25 `_` separators, the float forms, the FLOAT32 suffix and its
//! glued-run rejection) and the malformed-number classification they
//! share. Split out of `lexer.rs` by the file-size gate; the methods
//! stay on [`Lexer`].

use std::borrow::Cow;

use crate::token::TokenType;

use super::Lexer;

impl<'a> Lexer<'a> {
    /// Scan one numeric literal starting at the cursor (a digit, or a `.`
    /// followed by one) and emit its `Integer`/`Float` token, or a single
    /// `InvalidNumberLiteral` error token covering the whole glued run.
    pub(super) fn read_number(&mut self, line: usize, col: usize) {
        let start = self.pos;
        let mut is_float = false;

        // Check for hex or octal prefix
        if self.pos < self.input.len() && self.input[self.pos] == b'0' {
            if let Some(next) = self.peek_at(1) {
                match next {
                    // Cypher 25 removed the uppercase prefixes; the run is
                    // one malformed number, exactly like the parser's other
                    // removed-spelling rejections used to report it.
                    b'X' | b'O' => {
                        self.advance(); // 0
                        self.advance(); // X / O
                        self.emit_malformed_number(start, line, col);
                        return;
                    }
                    b'x' => {
                        self.read_radix_number(|b| b.is_ascii_hexdigit(), start, line, col);
                        return;
                    }
                    b'o' => {
                        self.read_radix_number(
                            |b| b.is_ascii_digit() && b <= b'7',
                            start,
                            line,
                            col,
                        );
                        return;
                    }
                    // Any other byte after the leading `0` takes the
                    // decimal / deprecated-separator paths below.
                    _ => {}
                }
            }
        }

        // A `_` directly after a leading zero (`0_66_44_22`, `0_xFF`) is
        // the deprecated octal separator position Cypher 25 rejects
        // (LiteralAcceptance [25]/[19]): stop before it so the run is
        // reported as one malformed number, exactly like a trailing
        // identifier would be. Underscores after any other leading digit
        // stay valid separators (`10_000`, `1_000.000_1`).
        if self.input[start] == b'0' && self.peek_at(1) == Some(b'_') {
            self.emit_malformed_number(start, line, col);
            return;
        }

        self.scan_digit_run(|b| b.is_ascii_digit(), false);

        if self.pos < self.input.len()
            && self.input[self.pos] == b'.'
            && self.peek_at(1) != Some(b'.')
            && self.pos + 1 < self.input.len()
            && self.input[self.pos + 1].is_ascii_digit()
        {
            is_float = true;
            self.advance(); // skip dot
            self.scan_digit_run(|b| b.is_ascii_digit(), false);
        }

        // Scientific notation
        if self.pos < self.input.len()
            && (self.input[self.pos] == b'e' || self.input[self.pos] == b'E')
        {
            is_float = true;
            self.advance();
            if self.pos < self.input.len()
                && (self.input[self.pos] == b'+' || self.input[self.pos] == b'-')
            {
                self.advance();
            }
            self.scan_digit_run(|b| b.is_ascii_digit(), false);
        }

        // A float literal may carry one trailing suffix: `f`/`F` (the
        // narrower FLOAT32 coordinate precision) or `d`/`D` (an explicit
        // double, `.4d` / `1e4D`). The engine's only scalar floating value
        // is FLOAT, so both lex the same FLOAT token and only have to be
        // accepted — and kept out of the literal text the parser parses as
        // `f64`. The value text ends where the suffix begins, so the
        // boundary is carried instead of recomputed.
        let value_end = self.pos;
        if is_float && matches!(self.peek(), b'f' | b'F' | b'd' | b'D') {
            self.advance();
        }

        // The one glue policy and its class rule live on
        // `number_has_trailing_ident`; a positive check here reports the
        // whole run as one `InvalidNumberLiteral` token.
        if self.number_has_trailing_ident() {
            self.emit_malformed_number(start, line, col);
            return;
        }

        let literal = self.number_literal(start, value_end);
        // Cypher 25 removed the legacy leading-zero decimal spelling
        // (`01277`, `00` — live-verified: a lone `0` is still ordinary).
        // The lexer owns the whole removed-spelling policy, so
        // `parse_int_literal` stays a pure decoder.
        if !is_float && literal.len() > 1 && literal.starts_with('0') {
            self.emit_malformed_number(start, line, col);
            return;
        }
        if is_float {
            self.emit(TokenType::Float, literal, line, col);
        } else {
            self.emit(TokenType::Integer, literal, line, col);
        }
    }

    /// Scan one `0x`/`0o`-prefixed integer (the prefix still at the cursor)
    /// and emit its `Integer` token — the one body shape both radix forms
    /// share, parameterised only by the digit test. An empty body (`0x`,
    /// Literals3 [12]) or trailing identifier characters (`0x1A2b3j...`, a
    /// non-radix letter continuing the run — Literals3 [13]/[14]) make the
    /// run one invalid number literal, not a valid radix integer.
    fn read_radix_number(
        &mut self,
        is_digit: fn(u8) -> bool,
        start: usize,
        line: usize,
        col: usize,
    ) {
        self.advance(); // 0
        self.advance(); // x / o
        let body_start = self.pos;
        self.scan_digit_run(is_digit, true);
        if self.pos == body_start || self.number_has_trailing_ident() {
            self.emit_malformed_number(start, line, col);
            return;
        }
        let literal = self.number_literal(start, self.pos);
        self.emit(TokenType::Integer, literal, line, col);
    }

    /// Scan one run of digits for a numeric literal, accepting the Cypher 25
    /// `_` digit separator between digits (`1_000`, `0x_2A_40_7F`,
    /// `1.0E1_0`). A `_` is consumed only when a digit was already scanned
    /// and another digit follows, so a doubled (`1__0`), trailing (`1000_`),
    /// or otherwise misplaced separator stops the scan at the `_` — which is
    /// an identifier-start character, so the caller's trailing-identifier
    /// classification reports the whole glued run as one malformed number.
    /// `leading_separator` admits the one `_` directly after the `0x`/`0o`
    /// prefix (`0x_2A`, `0o_66`), the one position a separator may start a
    /// run.
    fn scan_digit_run(&mut self, is_digit: fn(u8) -> bool, leading_separator: bool) {
        if leading_separator && self.peek() == b'_' && self.peek_at(1).is_some_and(is_digit) {
            self.advance();
        }
        loop {
            while self.pos < self.input.len() && is_digit(self.input[self.pos]) {
                self.advance();
            }
            // A separator sits BETWEEN digits: the byte before it must be
            // one, so `1E_1` (no exponent digit yet) stops at the `_`.
            if self.pos < self.input.len()
                && self.input[self.pos] == b'_'
                && self.pos > 0
                && is_digit(self.input[self.pos - 1])
                && self.peek_at(1).is_some_and(is_digit)
            {
                self.advance();
                continue;
            }
            return;
        }
    }

    /// The scanned numeric literal from `start` to `end` with its `_` digit
    /// separators removed — the value text every consumer of an
    /// `Integer`/`Float` token parses. The separators are spelling only;
    /// keeping them in the token literal would make every parse site strip
    /// them again (a re-split of this one fact). A separator-free run is the
    /// input slice itself, borrowed (ADR-0505); only a separated run needs
    /// the owned, stripped copy.
    fn number_literal(&self, start: usize, end: usize) -> Cow<'a, str> {
        if self.input[start..end].contains(&b'_') {
            Cow::Owned(self.slice(start, end).replace('_', ""))
        } else {
            Cow::Borrowed(self.slice(start, end))
        }
    }

    /// True when the character immediately after a just-scanned number
    /// continues an identifier — the identifier-*part* class, so no byte
    /// that could continue an identifier run may continue a numeric one,
    /// whether it starts one (`9223372h54775808`, Literals2 [11]) or only
    /// continues it (`1.0f32`, REL8-1064; a combining mark glued to a
    /// plain number). Cypher requires whitespace before any following
    /// identifier/keyword. The character is decoded so a non-ASCII
    /// identifier part counts too (ADR-0182).
    fn number_has_trailing_ident(&self) -> bool {
        self.at_ident_part()
    }

    /// Consume any trailing identifier characters that are glued to a malformed
    /// number so the whole run becomes a single error token, then emit it tagged
    /// with the `InvalidNumberLiteral` TCK detail.
    fn emit_malformed_number(&mut self, start: usize, line: usize, col: usize) {
        let literal = self.take_ident_run(start);
        self.emit_error_detail(
            format!("invalid number literal: {literal}"),
            line,
            col,
            "InvalidNumberLiteral",
        );
    }
}
