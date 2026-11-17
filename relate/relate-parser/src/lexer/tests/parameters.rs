//! The parameter-name family: bare `$name` (identifier start or ASCII
//! digit, ADR-0182 / LABS-209), the sigil-adjacent spellings LABS-1602
//! and LABS-1612 added — whitespace between `$` and the name, and the
//! backtick-quoted `$`name`` form — and the rejections that keep the
//! empty-literal `$(expr)` dynamic-opener sentinel exclusive.

use super::*;

/// The clean-lex atom: `query` tokenizes with no Error token.
fn assert_lexes_clean(query: &str) -> Vec<Token<'_>> {
    let tokens = tokenize(query);
    assert!(
        !tokens.iter().any(|t| t.ty == TokenType::Error),
        "{query:?} must lex clean: {tokens:?}"
    );
    tokens
}

/// The parameter-token atom: the exactly-one Parameter token of `tokens`.
fn the_parameter<'t>(tokens: &'t [Token<'t>], query: &str) -> &'t Token<'t> {
    let mut params = tokens.iter().filter(|t| t.ty == TokenType::Parameter);
    let param = params
        .next()
        .unwrap_or_else(|| panic!("{query:?}: the parameter token: {tokens:?}"));
    assert!(
        params.next().is_none(),
        "{query:?}: exactly one parameter token: {tokens:?}"
    );
    param
}

/// The error-token atom: the query's exactly-one Error token (the
/// `the_parameter` sibling's shape — a cascade would fail here, not at
/// whichever site happens to read the first).
fn the_error<'t>(tokens: &'t [Token<'t>], query: &str) -> &'t Token<'t> {
    let mut errors = tokens.iter().filter(|t| t.ty == TokenType::Error);
    let err = errors
        .next()
        .unwrap_or_else(|| panic!("{query:?} must produce an error token"));
    assert!(
        errors.next().is_none(),
        "{query:?}: exactly one error token: {tokens:?}"
    );
    err
}

/// The InvalidParameterName rejection atom (LABS-209's reference error
/// shape), the one spelling every rejection test of that policy shares:
/// the query produces an Error token with the detail code and the message
/// shape `reject_parameter_name` emits. Per-site extras (naming the
/// offending run) assert on `the_error`'s token at the call site.
fn assert_invalid_parameter_name(query: &str) {
    let tokens = tokenize(query);
    let err = the_error(&tokens, query);
    assert_eq!(err.detail, Some("InvalidParameterName"), "{query:?}");
    assert!(
        err.literal
            .contains("expected an identifier or an integer value"),
        "{query:?}: message shape: {:?}",
        err.literal
    );
}

#[test]
fn test_parameter() {
    let tokens = tokenize("$name $value");
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].ty, TokenType::Parameter);
    assert_eq!(tokens[0].literal, "name");
    assert_eq!(tokens[1].literal, "value");
}

#[test]
fn test_parameter_start_rejects_part_only_characters() {
    // A parameter's first character is an identifier start or an ASCII digit
    // (ADR-0182's Consequences prose, confirmed against the live Neo4j
    // reference — LABS-209): `€`-class part-only starts are rejected with the
    // reference's error shape, and the offending run is consumed as one error
    // token, not a cascade.
    for (query, bad) in [("RETURN $€ AS v", "€"), ("RETURN $€a AS v", "€a")] {
        let tokens = tokenize(query);
        assert!(
            the_error(&tokens, query).literal.contains(bad),
            "{query:?}: {tokens:?}"
        );
    }
}

#[test]
fn test_parameter_start_accepts_digits_and_identifier_starts() {
    // Neo4j 5.26 accepts `$1`, `$1x` and Unicode identifier starts
    // (`$名前`, `$café`) bare (LABS-209's probe); backticks are only needed
    // for names that are not identifier-shaped at all.
    for query in [
        "RETURN $1 AS v",
        "RETURN $1x AS v",
        "RETURN $名前 AS v",
        "RETURN $café AS v",
    ] {
        assert_lexes_clean(query);
    }
}

#[test]
fn test_parameter_dynamic_opener_lexes_clean() {
    // The `(` after `$` is the dynamic-expression opener (`$(expr)`, e.g.
    // `n IS LABELED $('A')`) — a separate contract from the
    // identifier-start/digit acceptance above: the lexer emits the
    // intentionally empty parameter token whose expansion the parser owns.
    assert_lexes_clean("MATCH (n:$(x)) RETURN n");
}

#[test]
fn test_bare_parameter_rejected() {
    // A `$` with no name at end of input is an error token naming what was
    // expected, never the old silent empty-named parameter. Whitespace after
    // the `$` no longer rejects on its own (LABS-1612: `$ p2` names `p2`) —
    // the whitespace-then-EOF and whitespace-then-non-name rejections are
    // pinned in `test_parameter_after_whitespace_still_rejects_non_names`.
    assert_invalid_parameter_name("RETURN $");
}

#[test]
fn test_backtick_parameter_lexes_the_unescaped_name() {
    // LABS-1602: `$`name`` — the parameter twin of the backtick identifier —
    // is one Parameter token carrying the unescaped name, so a name that is
    // not identifier-shaped at all (a space in it, an embedded backtick) is
    // spellable. The literal convention matches the backtick identifier's:
    // a doubled backtick collapses to one literal backtick.
    for (query, name) in [
        ("RETURN $`p1` AS x", "p1"),
        ("RETURN $`p 1` AS x", "p 1"),
        ("RETURN $`a``b` AS x", "a`b"),
    ] {
        let tokens = assert_lexes_clean(query);
        assert_eq!(the_parameter(&tokens, query).literal, name, "{query:?}");
    }
}

#[test]
fn test_backtick_parameter_after_whitespace_lexes_the_name() {
    // The two spellings combine (LABS-1602 + LABS-1612): a whitespace-
    // separated sigil in front of a backticked name.
    let tokens = assert_lexes_clean("RETURN $ `p 1` AS x");
    assert_eq!(the_parameter(&tokens, "RETURN $ `p 1` AS x").literal, "p 1");
}

#[test]
fn test_parameter_sigil_accepts_separating_whitespace() {
    // LABS-1612: whitespace between the `$` and the name — a space, a tab,
    // a newline — still names `p2`; the token keeps the `$`'s position.
    for (query, line, col) in [
        ("RETURN $ p2 AS x", 1, 8),
        ("RETURN $\tp2 AS x", 1, 8),
        ("RETURN $\np2 AS x", 1, 8),
        ("RETURN\n$ p2 AS x", 2, 1),
    ] {
        let tokens = assert_lexes_clean(query);
        let param = the_parameter(&tokens, query);
        assert_eq!(param.literal, "p2", "{query:?}");
        assert_eq!((param.line, param.col), (line, col), "{query:?}");
    }
}

#[test]
fn test_parameter_after_whitespace_still_rejects_non_names() {
    // Whitespace is transparent, not a name: the character after it still
    // faces the bare start rules — a part-only character (`$ €`) and
    // whitespace to end of input both keep the InvalidParameterName shape.
    for query in ["RETURN $ € AS x", "RETURN $ "] {
        assert_invalid_parameter_name(query);
    }
}

#[test]
fn test_unterminated_backtick_parameter_is_one_error_token() {
    // The backtick form shares the identifier reader's one unterminated
    // error — no parameter-specific cascade.
    let tokens = tokenize("RETURN $`p1");
    assert_eq!(
        the_error(&tokens, "RETURN $`p1").literal,
        "unterminated backtick identifier"
    );
}

#[test]
fn test_empty_backtick_parameter_is_rejected() {
    // The empty Parameter literal is the `$(expr)` dynamic opener's
    // reserved sentinel, never a parameter's name: an empty backtick-
    // quoted name ($``) keeps the bare-$ InvalidParameterName shape —
    // including when whitespace separates the sigil from the backticks,
    // so `$ ``(x)` can never silently lex as the dynamic opener.
    for query in ["RETURN $`` AS v", "RETURN $ ``(x)"] {
        assert_invalid_parameter_name(query);
    }
}

#[test]
fn test_dynamic_opener_is_glued_to_the_sigil() {
    // The `$(expr)` opener is glued only: whitespace between the `$` and
    // the `(` is not the opener — LABS-1612's skip widened the name
    // spellings, never the sentinel's shape. `$(x)` lexes the empty
    // sentinel token the parser expands; `$ (x)` keeps the bare-start
    // rejection.
    let tokens = assert_lexes_clean("MATCH (n:$(x)) RETURN n");
    assert!(the_parameter(&tokens, "MATCH (n:$(x)) RETURN n")
        .literal
        .is_empty());
    assert_invalid_parameter_name("RETURN $ (x)");
}
