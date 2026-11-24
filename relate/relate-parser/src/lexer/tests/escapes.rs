//! The `\uXXXX` escape family: the string-literal escapes (the one
//! window decoder, surrogate pairs, the byte-escaped malformed-window
//! report) and the source-level escapes (ADR-0619) that spell whitespace
//! and identifier text between tokens.

use super::*;

#[test]
fn test_control_character_escapes_decode() {
    // The Cypher escape table's control characters: `\r`, `\b` and `\f` are
    // one character each, like `\n` and `\t` — never the two-character
    // backslash-letter form an unknown escape keeps.
    let tokens = tokenize(r"'\r\b\f\n\t'");
    assert_eq!(tokens[0].ty, TokenType::String);
    assert_eq!(tokens[0].literal, "\r\u{8}\u{c}\n\t");
    for (input, expected) in [(r"'\r'", "\u{d}"), (r"'\b'", "\u{8}"), (r"'\f'", "\u{c}")] {
        assert_eq!(tokenize(input)[0].literal, expected, "{input}");
    }
    // An unknown escape still keeps its literal two-character form.
    assert_eq!(tokenize(r"'\z'")[0].literal, "\\z");
}

#[test]
fn test_unicode_escape() {
    let tokens = tokenize(r"'\u0041'");
    assert_eq!(tokens[0].literal, "A");
}

#[test]
fn test_interpolation_escapes_match_string_escapes() {
    // Round-3 CIP-261 design: the InterpString token carries escapes RAW
    // (both text and embed regions) — `parse_interp_body` resolves text
    // escapes with an unambiguous two-byte lookahead. The lexer-level
    // assertion is therefore the raw body; resolution is covered by the
    // parser tests (`test_interp_embed_escaped_quote_kept_raw` etc.).
    let tokens = tokenize(r"s'a\nb\tc'");
    assert_eq!(tokens[0].ty, TokenType::InterpString);
    assert_eq!(tokens[0].literal, r"a\nb\tc");

    let tokens = tokenize(r"s'\u0041{1}'");
    assert_eq!(tokens[0].literal, r"\u0041{1}");

    let tokens = tokenize(r"s'\{x\}'");
    assert_eq!(tokens[0].literal, r"\{x\}");

    // An unknown escape is preserved verbatim, like a plain string.
    let tokens = tokenize(r"s'\q'");
    assert_eq!(tokens[0].literal, "\\q");
}

#[test]
fn test_unicode_escape_invalid_codepoint_is_error() {
    // Surrogate codepoints are not valid `char`s; they must produce a lex
    // error rather than being silently dropped from the string.
    for input in [r"'\uD800'", r"'\uDFFF'"] {
        let tokens = tokenize(input);
        assert_eq!(tokens[0].ty, TokenType::Error, "{input}");
        assert!(
            tokens[0].literal.contains("invalid unicode escape"),
            "{input}: {}",
            tokens[0].literal
        );
    }
}

#[test]
fn test_unicode_escape_window_cut_mid_character_is_error() {
    // The `\u` operand window is a raw four-byte cut and can end
    // mid-character (`'\u00€'` yields `30 30 E2 82`): it must be one
    // `InvalidUnicodeLiteral` error naming the bytes found — never a
    // fabricated empty operand silently substituted for them.
    let tokens = tokenize("'\u{5c}u00€'");
    assert_eq!(tokens[0].ty, TokenType::Error);
    assert!(
        tokens[0].literal.contains("invalid unicode escape"),
        "{}",
        tokens[0].literal
    );
    assert_eq!(tokens[0].detail, Some("InvalidUnicodeLiteral"));
    // A valid-UTF-8 window of non-hex digits reports the window itself,
    // exactly as the radix rejection always did.
    let tokens = tokenize(r"'\uGGGG'");
    assert_eq!(tokens[0].ty, TokenType::Error);
    assert_eq!(
        tokens[0].literal,
        r"invalid unicode escape sequence: \uGGGG"
    );
    assert_eq!(tokens[0].detail, Some("InvalidUnicodeLiteral"));
}

#[test]
fn test_unicode_escape_surrogate_pair_is_one_codepoint() {
    // A UTF-16 surrogate pair escape names one supplementary code point
    // (U+10000, the TCK's `'𐀀'`); the lone-surrogate rejection above still
    // covers a high surrogate with no low pair.
    let tokens = tokenize("'\\uD800\\uDC00'");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].ty, TokenType::String);
    assert_eq!(tokens[0].literal, "\u{10000}");
    // A high surrogate followed by anything but a low surrogate stays an
    // error (the second escape alone is not a pair).
    for input in [r"'\uD800\uD800'", r"'\uD800x'", r"'\uDC00\uD800'"] {
        let tokens = tokenize(input);
        assert_eq!(tokens[0].ty, TokenType::Error, "{input}");
    }
}

#[test]
fn test_source_escape_spells_whitespace_between_tokens() {
    // A well-formed `\u2028` escape whose character is whitespace is that
    // whitespace (ADR-0619): the reference server's own spelling for the
    // line separators a text corpus cannot write as characters — each of
    // them a line terminator for that corpus's embedded-text format.
    for escape in [r"\u2028", r"\u0085", r"\u000b", r"\u0020"] {
        let input = format!("RETURN{escape}1 AS r");
        assert_eq!(
            tok_types(&input),
            tok_types("RETURN 1 AS r"),
            "escape {escape} is whitespace"
        );
    }

    // A decoded newline (`\u000A`) is whitespace, but it occupies its six
    // source bytes on the line it is spelled on: the token after it stays
    // on line 1 and its column counts those six bytes, so `line`/`col`
    // remain source positions — the invariant `token_start_byte` turns
    // into a byte offset.
    let tokens = tokenize(r"RETURN\u000A1 AS r");
    assert_eq!(
        tokens[1].line, 1,
        "an escape is six source bytes on its line"
    );
    assert_eq!(tokens[1].col, 13, "RETURN is 6, the escape 6, then the 1");
}

/// The escape family keeps every token addressable: `line` and `col` are
/// source positions, so the byte offset `token_start_byte` derives from
/// them is the token's real offset in the text. A decoded whitespace
/// escape could break that by moving the line without moving the cursor —
/// this pins the offset a caller slices the source with.
#[test]
fn a_whitespace_escape_keeps_every_token_addressable() {
    let src = r"RETURN 1\u000A AS r";
    let starts = crate::token::line_starts_of(src);
    let want = src.find("AS").expect("the AS keyword is in the source");
    let tokens = tokenize(src);
    let as_tok = tokens
        .iter()
        .find(|tok| tok.literal == "AS")
        .expect("the AS keyword lexes");
    assert_eq!(
        crate::token::token_start_byte(as_tok, &starts, src.len()),
        Some(want),
        "the derived offset is the token's real byte offset"
    );
}

#[test]
fn test_source_escape_spells_identifier_text() {
    // An escape whose character starts or continues an identifier is that
    // identifier's character (ADR-0619); keywords resolve through the same
    // lookup, so an escaped keyword letter spells the keyword.
    let tokens = tokenize(r"\u0061");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].ty, TokenType::Identifier);
    assert_eq!(tokens[0].literal, "a");

    // Mid-identifier: borrowed run, decoded character, borrowed run — one
    // identifier text, not three tokens.
    assert_eq!(tok_literals(r"ab\u0063d"), ["abcd"]);

    // Escaped keyword letter, and a whole escaped identifier in use.
    let tokens = tokenize(r"\u0052ETURN 1");
    assert_eq!(tokens[0].ty, TokenType::RETURN);
    assert_eq!(
        tok_literals(r"WITH 1 AS \u0078 RETURN \u0078"),
        ["WITH", "1", "AS", "x", "RETURN", "x"]
    );
}

#[test]
fn test_source_escape_boundary_is_the_historical_rejection() {
    // ADR-0619's boundary: an escape may widen whitespace and identifier
    // text only. An escape that would spell any other token — a quote, a
    // digit — and a malformed or surrogate escape all take the
    // historical `unexpected character:` error path, never a silent
    // half-decode. Each case pins that specific rejection, not just the
    // presence of some Error token.
    for input in [
        r"\u0027x\u0027",
        r"RETURN \u0031",
        r"RETURN\uzzzz1",
        r"\uD800",
    ] {
        let tokens = tokenize(input);
        let rejection = tokens
            .iter()
            .find(|t| t.ty == TokenType::Error)
            .unwrap_or_else(|| panic!("{input}: no rejection"));
        assert!(
            rejection.literal.starts_with("unexpected character"),
            "{input}: {rejection:?}"
        );
    }
}

#[test]
fn test_source_escape_does_not_leak_into_neighbouring_tokens() {
    // `AS` stands as its own keyword, the `\u0085` spelling of NEL
    // separates like the whitespace it decodes to, and `r` lexes as its
    // own identifier.
    assert_eq!(tok_types(r"RETURN 1 AS\u0085r"), tok_types("RETURN 1 AS r"));
}

#[test]
fn test_escape_fallback_preserves_non_ascii() {
    // An unknown escape keeps the backslash and the escaped character;
    // a non-ASCII escaped character must survive byte-identically.
    let tokens = tokenize("'\\é'");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].ty, TokenType::String);
    assert_eq!(tokens[0].literal, "\\é");

    // Mixed with a recognized escape.
    let tokens = tokenize("'é\\n'");
    assert_eq!(tokens[0].literal, "é\n");
}
