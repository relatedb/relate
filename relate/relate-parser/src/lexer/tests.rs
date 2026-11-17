use super::string::decode_char;
use super::*;

fn tok_types(input: &str) -> Vec<TokenType> {
    tokenize(input).iter().map(|t| t.ty).collect()
}

fn tok_literals(input: &str) -> Vec<String> {
    tokenize(input)
        .iter()
        .map(|t| t.literal.to_string())
        .collect()
}

// The parameter-name tests live in their own topic module; every
// other topic stays in this file until it earns its own.
mod parameters;
#[test]
fn test_decode_char_bounds_validation_to_its_sequence() {
    // PR #61 review: `decode_char` must validate only the 2–4 byte sequence
    // it decodes, not the whole remaining input. A trailing invalid byte
    // must not invalidate a leading well-formed character — otherwise
    // `skip_whitespace` re-validates the tail at every token boundary,
    // making lexing quadratic on Unicode-whitespace-heavy input.
    assert_eq!(decode_char(&[0xC2, 0xA0, 0xFF]), Some(('\u{00A0}', 2)));
    assert_eq!(
        decode_char(&[0xF0, 0x9F, 0x98, 0x80, 0xFF]),
        Some(('\u{1F600}', 4))
    );
    // A truncated sequence at end of input is still reported as malformed.
    assert_eq!(decode_char(&[0xC2]), None);
    // A multibyte non-whitespace char decodes to itself and is classified
    // as non-whitespace, so it terminates whitespace skipping.
    let (c, len) = decode_char("\u{00E9}".as_bytes()).unwrap();
    assert_eq!((c, len), ('\u{00E9}', 2));
    assert!(!c.is_whitespace());
}

#[test]
fn test_unicode_whitespace_between_tokens() {
    // NBSP (U+00A0, 2-byte) between tokens is skipped like ASCII whitespace.
    assert_eq!(
        tok_types("RETURN\u{00A0}1"),
        vec![TokenType::RETURN, TokenType::Integer]
    );
    // EM SPACE (U+2003, 3-byte) as well.
    assert_eq!(
        tok_types("MATCH\u{2003}RETURN"),
        vec![TokenType::MATCH, TokenType::RETURN]
    );
}

#[test]
fn test_worst_case_unicode_whitespace_run_tokenizes() {
    // `RETURN` followed by a long run of NBSP lexes correctly; each decode
    // validates only its own 2-byte sequence, not the remaining tail.
    let mut q = String::from("RETURN");
    q.extend(std::iter::repeat_n('\u{00A0}', 4096));
    q.push('1');
    assert_eq!(tok_types(&q), vec![TokenType::RETURN, TokenType::Integer]);
}

#[test]
fn test_multibyte_non_whitespace_is_not_skipped() {
    // U+00A7 is multibyte but not whitespace and not an identifier
    // character, so whitespace skipping must stop there rather than consume
    // it; the lexer then flags the stray byte(s) instead of silently
    // dropping them.
    let tokens = tokenize("\u{00A0}\u{00A7}");
    assert!(!tokens.is_empty());
    assert!(tokens.iter().all(|t| t.ty == TokenType::Error));
}

#[test]
fn test_invalid_multibyte_char_is_one_error_with_detail() {
    // LABS-188: a non-ASCII character outside strings/comments/whitespace
    // that is not an identifier start is ONE invalid character, not one
    // mojibake error token per UTF-8 byte, and carries the TCK
    // `InvalidUnicodeCharacter` detail. The probes stay invalid once
    // Unicode identifiers land (dashes, emoji, section sign, BOM).
    for c in [
        '\u{2014}',  // em dash
        '\u{2013}',  // en dash
        '\u{2212}',  // minus sign
        '\u{2010}',  // hyphen
        '\u{1F600}', // emoji
        '\u{00A7}',  // section sign
        '\u{FEFF}',  // byte-order mark (not whitespace)
    ] {
        let owned = c.to_string();
        let tokens = tokenize(&owned);
        assert_eq!(tokens.len(), 1, "expected one token for {c:?}: {tokens:?}");
        assert_eq!(tokens[0].ty, TokenType::Error, "{c:?}");
        assert_eq!(
            tokens[0].detail,
            Some("InvalidUnicodeCharacter"),
            "expected InvalidUnicodeCharacter for {c:?}, got {:?}",
            tokens[0].detail
        );
        assert!(
            tokens[0].literal.contains(c),
            "literal must contain the decoded {c:?}: {:?}",
            tokens[0].literal
        );
    }
}

#[test]
fn test_invalid_unicode_hyphen_lexes_following_integer() {
    // The Mathematical3 [1] query: the em dash is one error token and lexing
    // resumes after all three of its bytes, so `41` is still an Integer
    // token at its own (byte) column.
    let tokens = tokenize("RETURN 42 \u{2014} 41");
    let types: Vec<_> = tokens.iter().map(|t| t.ty).collect();
    assert_eq!(
        types,
        vec![
            TokenType::RETURN,
            TokenType::Integer,
            TokenType::Error,
            TokenType::Integer,
        ]
    );
    let err = &tokens[2];
    assert_eq!(err.detail, Some("InvalidUnicodeCharacter"));
    assert!(err.literal.contains('\u{2014}'));
    assert_eq!(err.col, 11);
    assert_eq!(tokens[3].literal, "41");
    assert_eq!(tokens[3].col, 15);
}

#[test]
fn test_ascii_fallback_keeps_unexpected_character_without_detail() {
    // ASCII characters that reach the fallback arm keep the historical
    // untagged `unexpected character:` error.
    for (query, ch) in [("RETURN 1 # 2", '#'), ("RETURN 1 ? 2", '?')] {
        let tokens = tokenize(query);
        let err = tokens
            .iter()
            .find(|t| t.ty == TokenType::Error)
            .unwrap_or_else(|| panic!("expected an error token for {query:?}"));
        assert_eq!(err.detail, None, "{query:?}");
        assert_eq!(err.literal, format!("unexpected character: {ch}"));
    }
}

#[test]
fn test_simple_punctuation() {
    assert_eq!(
        tok_types("()[]{}:,;|*+/%^="),
        vec![
            TokenType::LeftParen,
            TokenType::RightParen,
            TokenType::LeftBracket,
            TokenType::RightBracket,
            TokenType::LeftBrace,
            TokenType::RightBrace,
            TokenType::Colon,
            TokenType::Comma,
            TokenType::Semicolon,
            TokenType::Pipe,
            TokenType::Star,
            TokenType::Plus,
            TokenType::Slash,
            TokenType::Percent,
            TokenType::Caret,
            TokenType::Equals,
        ]
    );
}

#[test]
fn test_arrows_and_comparisons() {
    assert_eq!(
        tok_types("-> <- <> <= >= < > - ."),
        vec![
            TokenType::ArrowRight,
            TokenType::ArrowLeft,
            TokenType::NotEquals,
            TokenType::LessEqual,
            TokenType::GreaterEqual,
            TokenType::LessThan,
            TokenType::GreaterThan,
            TokenType::Dash,
            TokenType::Dot,
        ]
    );
}

#[test]
fn test_dot_dot() {
    assert_eq!(tok_types(".."), vec![TokenType::DotDot]);
    assert_eq!(
        tok_types("1..5"),
        vec![TokenType::Integer, TokenType::DotDot, TokenType::Integer]
    );
}

#[test]
fn test_keywords() {
    assert_eq!(
        tok_types("MATCH RETURN WHERE CREATE"),
        vec![
            TokenType::MATCH,
            TokenType::RETURN,
            TokenType::WHERE,
            TokenType::CREATE,
        ]
    );
    // Case-insensitive
    assert_eq!(
        tok_types("match return where create"),
        vec![
            TokenType::MATCH,
            TokenType::RETURN,
            TokenType::WHERE,
            TokenType::CREATE,
        ]
    );
}

#[test]
fn test_identifiers() {
    let tokens = tokenize("n foo bar123 _x");
    assert_eq!(tokens.len(), 4);
    for t in &tokens {
        assert_eq!(t.ty, TokenType::Identifier);
    }
    assert_eq!(tokens[0].literal, "n");
    assert_eq!(tokens[1].literal, "foo");
    assert_eq!(tokens[2].literal, "bar123");
    assert_eq!(tokens[3].literal, "_x");
}

#[test]
fn test_integers() {
    assert_eq!(tok_literals("42 0 100"), vec!["42", "0", "100"]);
    assert_eq!(tok_types("42"), vec![TokenType::Integer]);
}

#[test]
fn test_hex_octal() {
    assert_eq!(tok_types("0xFF"), vec![TokenType::Integer]);
    assert_eq!(tok_literals("0xFF"), vec!["0xFF"]);
    assert_eq!(tok_types("0o77"), vec![TokenType::Integer]);
    assert_eq!(tok_literals("0o77"), vec!["0o77"]);
}

#[test]
fn test_floats() {
    assert_eq!(tok_types("3.14"), vec![TokenType::Float]);
    assert_eq!(tok_types("1e10"), vec![TokenType::Float]);
    assert_eq!(tok_types("2.5E-3"), vec![TokenType::Float]);
    assert_eq!(tok_types(".5"), vec![TokenType::Float]);
    // An explicit double may carry a trailing `d`/`D` suffix.
    assert_eq!(tok_types(".4d"), vec![TokenType::Float]);
    assert_eq!(tok_types("1e4D"), vec![TokenType::Float]);
}

#[test]
fn test_strings() {
    let tokens = tokenize("'hello' \"world\"");
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].ty, TokenType::String);
    assert_eq!(tokens[0].literal, "hello");
    assert_eq!(tokens[1].ty, TokenType::String);
    assert_eq!(tokens[1].literal, "world");
}

#[test]
fn test_string_escapes() {
    let tokens = tokenize(r"'hello\nworld'");
    assert_eq!(tokens[0].literal, "hello\nworld");

    let tokens = tokenize(r"'tab\there'");
    assert_eq!(tokens[0].literal, "tab\there");

    let tokens = tokenize(r"'escaped\\slash'");
    assert_eq!(tokens[0].literal, "escaped\\slash");
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
fn test_backtick_identifier() {
    let tokens = tokenize("`my label`");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].ty, TokenType::Identifier);
    assert_eq!(tokens[0].literal, "my label");
}

#[test]
fn test_empty_backtick_identifier_lexes_as_today() {
    // An empty backtick-quoted identifier emits an Identifier with an
    // empty literal — the historical behavior, deliberately pinned so the
    // divergence from the parameter path stays a recorded fact, not a
    // silent default: the parameter path's empty literal is the reserved
    // `$(expr)` sentinel and is rejected, while an identifier's empty
    // name has no reserved meaning to collide with. Changing this
    // acceptance (Neo4j's verdict on `` `` `` is unverified) is its own
    // change, not a side effect of the parameter work.
    let tokens = tokenize("``");
    assert_eq!(tokens.len(), 1, "one identifier token: {tokens:?}");
    assert_eq!(tokens[0].ty, TokenType::Identifier);
    assert_eq!(tokens[0].literal, "");
}

#[test]
fn test_backtick_identifier_doubled_backtick_is_escaped() {
    // A doubled backtick inside a backtick-quoted identifier collapses to a
    // single literal backtick, mirroring the formatter's `cypher_identifier`
    // escaping so `.dump` / `.mode cypher` output round-trips (the bug: the
    // lexer used to terminate at the first backtick, so `` `a``b` `` lexed as
    // `a` followed by a stray `b`).
    let tokens = tokenize("`a``b`");
    assert_eq!(tokens.len(), 1, "one identifier token: {tokens:?}");
    assert_eq!(tokens[0].ty, TokenType::Identifier);
    assert_eq!(tokens[0].literal, "a`b");

    // A trailing escaped pair, and a leading one.
    assert_eq!(tokenize("`x```")[0].literal, "x`");
    assert_eq!(tokenize("```y`")[0].literal, "`y");

    // A lone backtick still terminates: `` `a`b `` is `a` then bare `b`.
    let two = tokenize("`a`b");
    assert_eq!(two.len(), 2);
    assert_eq!(two[0].literal, "a");
    assert_eq!(two[1].literal, "b");
}

#[test]
fn test_line_comment() {
    let _tokens = tokenize("MATCH // this is a comment\nRETURN");
    assert_eq!(
        tok_types("MATCH // comment\nRETURN"),
        vec![TokenType::MATCH, TokenType::RETURN]
    );
}

#[test]
fn test_block_comment() {
    assert_eq!(
        tok_types("MATCH /* block */ RETURN"),
        vec![TokenType::MATCH, TokenType::RETURN]
    );
}

#[test]
fn test_position_tracking() {
    let tokens = tokenize("MATCH (n)\nRETURN n");
    assert_eq!(tokens[0].line, 1);
    assert_eq!(tokens[0].col, 1);
    // RETURN should be on line 2
    let ret_tok = tokens.iter().find(|t| t.ty == TokenType::RETURN).unwrap();
    assert_eq!(ret_tok.line, 2);
    assert_eq!(ret_tok.col, 1);
}

#[test]
fn test_full_query() {
    let tokens = tokenize("MATCH (n:Person {name: 'Alice'})-[r:KNOWS]->(m) RETURN m.name");
    assert!(tokens.len() > 10);
    assert_eq!(tokens[0].ty, TokenType::MATCH);
}

#[test]
fn test_unterminated_string() {
    let tokens = tokenize("'hello");
    assert_eq!(tokens[0].ty, TokenType::Error);
    assert_eq!(tokens[0].literal, "unterminated string");
}

#[test]
fn test_interpolation_embed_escapes_pass_through_raw() {
    // Inside a `{...}` embed the body is re-lexed as an expression, so
    // escapes must reach it verbatim: `\'` stays `\'`, not `'`.
    // (Round-3 review: silently dropped by the test-module move; restored.)
    let tokens = tokenize(r"s'{coalesce('a\'b','c')}'");
    assert_eq!(tokens[0].ty, TokenType::InterpString);
    assert_eq!(tokens[0].literal, r"{coalesce('a\'b','c')}");

    // `\{`/`\}` inside an embed also pass through raw.
    let tokens = tokenize(r"s'{coalesce('x\{y','z')}'");
    assert_eq!(tokens[0].literal, r"{coalesce('x\{y','z')}");
}

#[test]
fn test_interpolation_escapes_pass_through_raw() {
    // `read_interpolation` leaves *every* escape unresolved in the token
    // body -- the interpolation parser decodes text segments later. That
    // is what keeps `\\` + `{` distinguishable from `\{` (a lexer-resolved
    // `\\` would look like the start of `\{`).
    let tokens = tokenize(r"s'a\nb\tc'");
    assert_eq!(tokens[0].ty, TokenType::InterpString);
    assert_eq!(tokens[0].literal, r"a\nb\tc");

    let tokens = tokenize(r"s'\u0041{1}'");
    assert_eq!(tokens[0].literal, r"\u0041{1}");

    let tokens = tokenize(r"s'\{x\}'");
    assert_eq!(tokens[0].literal, r"\{x\}");

    // An unknown escape is also preserved verbatim.
    let tokens = tokenize(r"s'\q'");
    assert_eq!(tokens[0].literal, r"\q");
}

#[test]
fn test_interpolation_escaped_backslash_before_embed() {
    // Round-3 review: `\\` + `{` is an escaped backslash followed by an
    // embed, distinct from `\{` (an escaped literal brace). The lexer must
    // keep them apart in the raw body.
    assert_eq!(tokenize(r"s'\\{1}'")[0].literal, r"\\{1}");
    assert_eq!(tokenize(r"s'\{1}'")[0].literal, r"\{1}");
    assert_eq!(tokenize(r"s'a\\b{x}'")[0].literal, r"a\\b{x}");
}

#[test]
fn test_interpolation_backtick_name_in_embed() {
    // A backtick-quoted identifier inside an embed is inert: braces and
    // quotes within it must not close the embed or open a string.
    assert_eq!(tokenize(r"s'{`a}b`}'")[0].ty, TokenType::InterpString);
    assert_eq!(tokenize(r"s'{`a}b`}'")[0].literal, r"{`a}b`}");
    assert_eq!(tokenize(r#"s'{`x'y`}'"#)[0].literal, r"{`x'y`}");
}

#[test]
fn test_backtick_identifier_non_ascii_roundtrip() {
    // Non-ASCII bytes must be decoded as UTF-8, not widened per-byte
    // (which would mojibake `Größe` into `GrÃ¶ÃŸe` and break the
    // dump/re-parse round-trip).
    let tokens = tokenize("`Größe`");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].ty, TokenType::Identifier);
    assert_eq!(tokens[0].literal, "Größe");
    assert_eq!(tokens[0].literal.as_bytes(), "Größe".as_bytes());
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

#[test]
fn test_unterminated_block_comment_is_error() {
    // An unterminated `/*` must not silently swallow the rest of the
    // input — `RETURN 1 /* DETACH DELETE n` must be rejected, not lexed
    // as plain `RETURN 1`.
    let tokens = tokenize("RETURN 1 /* DETACH DELETE n");
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].ty, TokenType::RETURN);
    assert_eq!(tokens[1].ty, TokenType::Integer);
    assert_eq!(tokens[2].ty, TokenType::Error);
    assert_eq!(tokens[2].literal, "unterminated block comment");

    // A comment terminated at EOF is still fine.
    assert_eq!(
        tok_types("RETURN 1 /* done */"),
        vec![TokenType::RETURN, TokenType::Integer]
    );
}

#[test]
fn test_unicode_identifier_lexes_as_one_token() {
    // ADR-0182: a non-ASCII identifier start opens one identifier token and
    // identifier parts (letters, `Pc`, `Mn`/`Mc`, `Nd`, and `Sc` outside
    // `Pattern_Syntax`) continue it -- the whole run is a single token,
    // never split per UTF-8 byte.
    for (query, ident) in [
        ("RETURN 1 AS Éx", "Éx"),
        ("RETURN 1 AS xÉ", "xÉ"),
        ("RETURN 1 AS xé", "xé"),
        ("RETURN 1 AS x€", "x€"),               // Sc outside Pattern_Syntax
        ("RETURN 1 AS x名前", "x名前"),         // CJK letters
        ("RETURN 1 AS x\u{0301}", "x\u{0301}"), // combining mark part
        ("RETURN 1 AS x\u{08BE}", "x\u{08BE}"), // Unicode 13 addition
        ("RETURN 1 AS \u{203F}x", "\u{203F}x"), // Pc start (not XID_Start)
        ("RETURN 1 AS \u{017F}KIP", "\u{017F}KIP"), // never a keyword
    ] {
        let tokens = tokenize(query);
        let last = tokens.last().expect("at least one token");
        assert_eq!(last.ty, TokenType::Identifier, "{query:?}: {tokens:?}");
        assert_eq!(last.literal, ident, "{query:?}");
    }

    // Parameter names follow the same rule.
    let tokens = tokenize("RETURN $名前 AS v");
    let param = tokens
        .iter()
        .find(|t| t.ty == TokenType::Parameter)
        .unwrap_or_else(|| panic!("expected a parameter token: {tokens:?}"));
    assert_eq!(param.literal, "名前");
}

#[test]
fn test_unicode_identifier_start_rejects_non_start_characters() {
    // ADR-0182: 😀 (So, supplementary plane), § (Po ∩ Pattern_Syntax), €
    // (Sc: a part, never a start) and U+00B7 (Other_ID_Continue) are not
    // identifier starts; each is ONE InvalidUnicodeCharacter error token.
    for (query, ch) in [
        ("RETURN 1 AS 😀x", '😀'),
        ("RETURN 1 AS §x", '§'),
        ("RETURN 1 AS €x", '€'),
        ("RETURN 1 AS ·x", '·'),
        ("RETURN 1 AS x·", '·'), // not a part either
    ] {
        let tokens = tokenize(query);
        let err = tokens
            .iter()
            .find(|t| t.ty == TokenType::Error)
            .unwrap_or_else(|| panic!("{query:?}: expected an error token"));
        assert_eq!(err.detail, Some("InvalidUnicodeCharacter"), "{query:?}");
        assert!(err.literal.contains(ch), "{query:?}: {:?}", err.literal);
        // The whole multi-byte sequence is consumed: the `x` on the other
        // side of the rejected character still lexes as an identifier.
        assert!(
            tokens
                .iter()
                .any(|t| t.ty == TokenType::Identifier && t.literal.contains('x')),
            "{query:?}: {tokens:?}"
        );
    }
}

// ---- ASCII fast path (SWAR batches, ADR-0505) ----

#[test]
fn test_ascii_fast_path_long_identifier_runs() {
    // Runs of every batch shape stay one token with exact text: the
    // single-byte tail, the exactly-one-batch run, and runs that cross
    // the eight-byte boundary (two full batches plus a tail).
    for name in [
        "n",
        "abcdefgh",
        "abcdefghi",
        "abcdefghijklmnopqrst",
        "a1_b2_c3_d4_e5_f6_g7",
    ] {
        let query = format!("RETURN 1 AS {name}");
        let tokens = tokenize(&query);
        let last = tokens
            .last()
            .unwrap_or_else(|| panic!("{query:?}: at least one token"));
        assert_eq!(last.ty, TokenType::Identifier, "{query:?}: {tokens:?}");
        assert_eq!(last.literal, name, "{query:?}");
    }
}

#[test]
fn test_ascii_fast_path_stops_at_non_ascii_and_resumes() {
    // The batch consumes the ASCII stretch, the first byte >= 0x80 hands
    // the character to the decoded path, and batching resumes after it —
    // one token, exact text, positions unchanged (ADR-0182 behavior on
    // the slow path, byte-for-byte).
    for name in [
        "aaaaaaaaéaaaaaaaa",
        "éaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaé",
        "aé_bÉ1€x",
    ] {
        let query = format!("RETURN 1 AS {name}");
        let tokens = tokenize(&query);
        let last = tokens
            .last()
            .unwrap_or_else(|| panic!("{query:?}: at least one token"));
        assert_eq!(last.ty, TokenType::Identifier, "{query:?}: {tokens:?}");
        assert_eq!(last.literal, name, "{query:?}");
    }

    // Parameter names take the same run scanner.
    let tokens = tokenize("RETURN $aaaaaaaaébbbb AS v");
    let param = tokens
        .iter()
        .find(|t| t.ty == TokenType::Parameter)
        .unwrap_or_else(|| panic!("expected a parameter token: {tokens:?}"));
    assert_eq!(param.literal, "aaaaaaaaébbbb");
}

#[test]
fn test_ascii_fast_path_positions_after_batched_runs() {
    // A long whitespace run (two batches) with newlines, tabs, spaces and
    // a CR: the batched skip must leave line/column exactly where
    // byte-at-a-time advance did.
    let tokens = tokenize("MATCH (n)\n  \t \r\n   RETURN n.name");
    let ret = tokens
        .iter()
        .find(|t| t.ty == TokenType::RETURN)
        .unwrap_or_else(|| panic!("expected RETURN: {tokens:?}"));
    assert_eq!((ret.line, ret.col), (3, 4), "{tokens:?}");

    let tokens = tokenize("MATCH (n)\n          RETURN");
    let ret = tokens
        .iter()
        .find(|t| t.ty == TokenType::RETURN)
        .unwrap_or_else(|| panic!("expected RETURN: {tokens:?}"));
    assert_eq!((ret.line, ret.col), (2, 11), "{tokens:?}");

    // The token after a long identifier batch: the column counts bytes,
    // one per identifier-part byte, exactly as per-byte advance did.
    let tokens = tokenize("RETURN abcdefghijklmnop, 1");
    let int = tokens
        .iter()
        .find(|t| t.ty == TokenType::Integer)
        .unwrap_or_else(|| panic!("expected the integer: {tokens:?}"));
    assert_eq!((int.line, int.col), (1, 26), "{tokens:?}");

    // A run that went through the decoded path advances the column by the
    // character's byte length, same as before the fast path.
    let tokens = tokenize("RETURN aé, 1");
    let int = tokens
        .iter()
        .find(|t| t.ty == TokenType::Integer)
        .unwrap_or_else(|| panic!("expected the integer: {tokens:?}"));
    assert_eq!((int.line, int.col), (1, 13), "{tokens:?}");
}

#[test]
fn test_ascii_fast_path_heavy_whitespace_matches_single_spaces() {
    // Whitespace amount and kind never change the token stream: types and
    // literals of the wide-run form equal the single-space form (only
    // positions differ).
    let thin = tokenize("MATCH (n:Person)-[:KNOWS]->(m) WHERE n.age > 21 RETURN m.name");
    let wide = tokenize(
        "MATCH   (n:Person)\n\t-\t[:KNOWS]->\r\n  (m)   WHERE\tn.age\t>  21  RETURN m.name",
    );
    let shape = |ts: &[Token<'_>]| {
        ts.iter()
            .map(|t| (t.ty, t.literal.to_string()))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&thin), shape(&wide));
    assert!(
        !wide.iter().any(|t| t.ty == TokenType::Error),
        "the wide form must lex clean: {wide:?}"
    );
}
