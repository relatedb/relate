//! The token module's tests, offloaded by the file-size gate (the
//! normalize.rs / type_normalize.rs `#[path]` extraction shape): keyword
//! lookup, the token-class predicates, and name rendering.

use super::*;

use relate_model::identifier::{quote_identifier_always, quote_identifier_shaped};

// The spelling iterator every vocabulary test consults is the one beside
// the declaration itself (`token.rs`'s `keyword_spellings`, in scope
// through `use super::*`) — no test site decodes the byte-literal table
// inline.

#[test]
fn test_lookup_keyword() {
    assert_eq!(lookup_keyword("MATCH"), TokenType::MATCH);
    assert_eq!(lookup_keyword("match"), TokenType::MATCH);
    assert_eq!(lookup_keyword("Match"), TokenType::MATCH);
    assert_eq!(lookup_keyword("RETURN"), TokenType::RETURN);
    assert_eq!(lookup_keyword("foo"), TokenType::Identifier);
    assert_eq!(lookup_keyword("ASCENDING"), TokenType::ASC);
    assert_eq!(lookup_keyword("DESCENDING"), TokenType::DESC);
}

/// ADR-0505's stack-buffer lookup: the length short-circuit and the
/// untouched non-ASCII bytes keep every classification identical to the
/// old `to_ascii_uppercase` cascade.
#[test]
fn test_lookup_keyword_stack_buffer_paths() {
    // The longest keywords still classify at exactly the bound.
    assert_eq!(lookup_keyword("constraint"), TokenType::CONSTRAINT);
    assert_eq!(lookup_keyword("descending"), TokenType::DESC);
    // One byte past the bound short-circuits, keyword or not.
    assert_eq!(lookup_keyword("constraints"), TokenType::Identifier);
    assert_eq!(lookup_keyword("descendingx"), TokenType::Identifier);
    // A full-width buffer that is not a keyword.
    assert_eq!(lookup_keyword("abcdefghij"), TokenType::Identifier);
    // Non-ASCII bytes are uppercased never, so a Unicode identifier
    // (ADR-0182) cannot collide with an ASCII keyword spelling.
    assert_eq!(lookup_keyword("mätch"), TokenType::Identifier);
    assert_eq!(lookup_keyword("ÖR"), TokenType::Identifier);
}

/// Every keyword spelling of the one vocabulary declaration, aliases
/// included: each must classify to its variant and stay
/// case-insensitive. The table is generated from the same declaration
/// as `lookup_keyword`'s cascade (and `LONGEST_KEYWORD_BYTES` is
/// derived from it), so this pins the cascade itself — a spelling added
/// to the declaration is exercised here in the same change, and a
/// longer keyword raises the derived bound instead of silently
/// short-circuiting to `Identifier` in the lexer.
#[test]
fn test_every_keyword_spelling_classifies_case_insensitively() {
    for (spelling, ty) in keyword_spellings() {
        assert!(ty.is_keyword(), "{spelling}");
        assert_eq!(lookup_keyword(spelling), ty, "{spelling}");
        // The cascade is case-insensitive; the lowercase spelling
        // classifies identically.
        assert_eq!(
            lookup_keyword(&spelling.to_ascii_lowercase()),
            ty,
            "{spelling}"
        );
    }
}

#[test]
fn test_is_ident_like() {
    assert!(TokenType::Identifier.is_ident_like());
    // The keyword half is pinned by explicit samples, not by re-deriving
    // `is_schema_name() && !CLAUSE_KEYWORDS` over `keyword_spellings()`:
    // that formula loop asserts the definition against itself and stays
    // green over any partition of the vocabulary. `RETURN` is a
    // non-reserved word usable as an ordinary variable/alias name; the
    // rest are named non-clause keywords a variable spelling can collide
    // with.
    for ty in [
        TokenType::RETURN,
        TokenType::NODE,
        TokenType::COUNT,
        TokenType::ALL,
        TokenType::NOT,
        TokenType::IN,
        TokenType::IS,
        TokenType::NULL,
        TokenType::TRUE,
        TokenType::INFINITY,
    ] {
        assert!(ty.is_ident_like(), "{ty:?}");
    }
    // The reserved half: every clause keyword is a keyword (the
    // membership drift guard) and never ident-like.
    for ty in CLAUSE_KEYWORDS {
        assert!(ty.is_keyword() && !ty.is_ident_like(), "{ty:?}");
    }
    assert!(!TokenType::LeftParen.is_ident_like());
}

#[test]
fn test_is_schema_name() {
    // ADR-0169: every keyword is a label/relationship-type name
    // (`SchemaName ::= SymbolicName | ReservedWord`) -- the positive
    // half derived from the one vocabulary declaration, so the whole
    // keyword vocabulary is pinned (NULL included), not just the clause
    // keywords and a hand-picked sample.
    for (spelling, ty) in keyword_spellings() {
        assert!(ty.is_schema_name(), "{spelling}");
    }
    assert!(TokenType::Identifier.is_schema_name());
    for ty in [
        TokenType::LeftParen,
        TokenType::Colon,
        TokenType::Pipe,
        TokenType::Parameter,
        TokenType::String,
        TokenType::Integer,
    ] {
        assert!(!ty.is_schema_name(), "{ty:?}");
    }
}

#[test]
fn test_is_keyword() {
    assert!(TokenType::MATCH.is_keyword());
    assert!(TokenType::RETURN.is_keyword());
    assert!(TokenType::PROFILE.is_keyword());
    assert!(!TokenType::Identifier.is_keyword());
    assert!(!TokenType::LeftParen.is_keyword());
}

#[test]
fn test_identifier_needs_quotes_shape() {
    for bare in ["a", "_", "_a1", "Person", "ship_date", "x9"] {
        assert!(
            !identifier_needs_quotes(bare),
            "{bare} is a bare identifier"
        );
        assert_eq!(quote_identifier(bare), bare);
    }
    for quoted in ["", "1abc", "a-b", "a b", "n.prop", "Ünïcode", "a`b"] {
        assert!(
            identifier_needs_quotes(quoted),
            "{quoted:?} must be backtick-quoted"
        );
    }
    assert_eq!(
        quote_identifier("we`ird"),
        "`we``ird`",
        "an embedded backtick is doubled"
    );
}

/// The display policy is shape-only: a keyword spelling renders bare (the
/// CIP-257 display corpus writes `when :: STRING`), while a name that
/// fails the plain identifier shape keeps its backticks. The DDL/canonical
/// policy stays keyword-aware: `quote_identifier("when")` is `` `when` ``,
/// which is what a generated statement needs in variable/alias positions.
#[test]
fn test_quote_identifier_shaped_is_shape_only() {
    assert_eq!(quote_identifier_shaped("when"), "when");
    assert_eq!(quote_identifier_shaped("order"), "order");
    assert_eq!(quote_identifier_shaped("prop-3"), "`prop-3`");
    assert_eq!(quote_identifier_shaped("a b"), "`a b`");
    assert_eq!(quote_identifier_shaped(""), "``");
    assert_eq!(
        quote_identifier_shaped("we`ird"),
        "`we``ird`",
        "the display quoter reuses the one escape"
    );
    assert_eq!(
        quote_identifier_shaped("prop-3"),
        quote_identifier_always("prop-3")
    );
    assert_eq!(
        quote_identifier("when"),
        "`when`",
        "the DDL/canonical policy stays keyword-aware"
    );
}

/// The forced-quoted constructor always quotes, and shares the one
/// escape rule with [`quote_identifier`]'s quoting arm: an embedded
/// backtick is doubled in both spellings.
#[test]
fn test_quote_identifier_always_forces_the_quoting_arm() {
    assert_eq!(quote_identifier_always("Person"), "`Person`");
    assert_eq!(quote_identifier_always("ship_date"), "`ship_date`");
    assert_eq!(quote_identifier_always("we`ird"), "`we``ird`");
    assert_eq!(
        quote_identifier_always("we`ird"),
        quote_identifier("we`ird")
    );
}

/// The rendering gate is ASCII-only even though the lexer accepts Unicode
/// identifiers (ADR-0182): a non-ASCII name that parses bare still renders
/// backticked, keeping generated Cypher ASCII and independent of the
/// pinned Unicode table version. The quoted form round-trips exactly.
#[test]
fn test_identifier_needs_quotes_is_ascii_only() {
    for name in ["café", "Ünïcode", "名前"] {
        assert_eq!(one_identifier(name), name, "{name} lexes bare");
        assert!(identifier_needs_quotes(name), "{name} must render quoted");
        let rendered = quote_identifier(name);
        assert_eq!(rendered, format!("`{name}`"));
        assert_eq!(one_identifier(&rendered), name);
    }
}

/// The policy is keyword-aware: a keyword spelling is quoted even where
/// the grammar would take it bare (a property key after `.`, a label per
/// ADR-0169), because `CREATE INDEX ... ON (n.<prop>)` reads the property
/// through `is_ident_like()` and rejects a clause keyword. Every quoted
/// form lexes back to one `Identifier` token with the original spelling.
#[test]
fn test_identifier_quoting_is_keyword_aware_and_round_trips() {
    // One declaration-driven loop over every keyword spelling (and its
    // lowercase form — the cascade is case-insensitive) pins both halves:
    // every keyword spelling needs quotes, renders backticked, and
    // round-trips to one Identifier with the original spelling.
    for (spelling, _) in keyword_spellings() {
        for name in [spelling, &spelling.to_ascii_lowercase()] {
            assert!(identifier_needs_quotes(name), "{name} is a keyword");
            let rendered = quote_identifier(name);
            assert_eq!(rendered, format!("`{name}`"), "{name} is quoted");
            assert_eq!(one_identifier(&rendered), name, "{name} round-trips");
        }
    }
}

/// The single `Identifier` token a rendered name lexes to, with its
/// literal as written — the round-trip half of the quoting policy.
fn one_identifier(rendered: &str) -> String {
    let tokens = crate::lexer::tokenize(rendered);
    let [token] = tokens.as_slice() else {
        panic!("{rendered} must lex to one identifier token, got {tokens:?}");
    };
    assert_eq!(token.ty, TokenType::Identifier, "{rendered}");
    token.literal.to_string()
}
