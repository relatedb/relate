use std::borrow::Cow;

/// Token types for the Cypher lexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenType {
    // Special tokens
    EOF,
    Error,
    Identifier,
    Integer,
    Float,
    String,
    /// CIP-261 interpolated string literal `s'...'` / `s"..."`. Payload is
    /// the body between the quotes with standard escapes resolved; `\{` and
    /// `\}` stay as two characters so the parser can tell an escaped literal
    /// brace from an expression delimiter, and raw `{`/`}` are kept for the
    /// parser to split into text/expression parts.
    InterpString,
    /// The Neo4j internal obfuscated literal `******` (exactly six stars),
    /// lexed only under the `obfuscated-literals` preview gate: a dummy
    /// `false` for semantic analysis, never meant to execute. Without the
    /// gate the run lexes as ordinary `*` tokens and the query fails with
    /// the parser's own syntax error, Neo4j's disabled-form behavior.
    ObfuscatedLiteral,
    Parameter, // $param

    // Punctuation
    LeftParen,    // (
    RightParen,   // )
    LeftBracket,  // [
    RightBracket, // ]
    LeftBrace,    // {
    RightBrace,   // }
    Colon,        // :
    DoubleColon,  // ::
    Comma,        // ,
    Dot,          // .
    Semicolon,    // ;
    Pipe,         // |
    DoublePipe,   // ||
    Dash,         // -
    ArrowRight,   // ->
    ArrowLeft,    // <-
    FatArrow,     // => (CIP-176/CIP-187 graph type declaration syntax)
    Star,         // *
    Plus,         // +
    Slash,        // /
    Percent,      // %
    Caret,        // ^
    DotDot,       // ..
    Bang,         // ! (CIP-102 NOT NULL type shorthand; also GQL label negation, CIP-14)
    Ampersand,    // & (GQL label expression conjunction, CIP-14)

    // Comparison operators
    Equals,       // =
    NotEquals,    // <>
    LessThan,     // <
    GreaterThan,  // >
    LessEqual,    // <=
    GreaterEqual, // >=
    RegexMatch,   // =~

    // Keywords
    MATCH,
    OPTIONAL,
    CREATE,
    INSERT,
    MERGE,
    DELETE,
    DETACH,
    SET,
    REMOVE,
    RETURN,
    WITH,
    UNWIND,
    WHERE,
    ORDER,
    BY,
    ASC,
    DESC,
    LIMIT,
    SKIP,
    AS,
    DISTINCT,
    AND,
    OR,
    NOT,
    XOR,
    IN,
    STARTS,
    ENDS,
    CONTAINS,
    IS,
    NULL,
    TRUE,
    FALSE,
    CASE,
    WHEN,
    THEN,
    ELSE,
    END,
    FOREACH,
    CALL,
    YIELD,
    UNION,
    ALL,
    ON,
    INDEX,
    CONSTRAINT,
    FOR,
    REQUIRE,
    UNIQUE,
    EXISTS,
    IF,
    DROP,
    VECTOR,
    POINT,
    OPTIONS,
    NODE,
    REL,
    COUNT,
    COLLECT,
    LOAD,
    FROM,
    CSV,
    REDUCE,
    SPATIAL,
    EXPLAIN,
    PROFILE,
    INFINITY, // Infinity/Inf literal (CIP-55)
    NAN,      // NaN literal (CIP-55)
}

impl TokenType {
    /// Returns true if this token type is a keyword (MATCH..PROFILE range).
    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            TokenType::MATCH
                | TokenType::OPTIONAL
                | TokenType::CREATE
                | TokenType::INSERT
                | TokenType::MERGE
                | TokenType::DELETE
                | TokenType::DETACH
                | TokenType::SET
                | TokenType::REMOVE
                | TokenType::RETURN
                | TokenType::WITH
                | TokenType::UNWIND
                | TokenType::WHERE
                | TokenType::ORDER
                | TokenType::BY
                | TokenType::ASC
                | TokenType::DESC
                | TokenType::LIMIT
                | TokenType::SKIP
                | TokenType::AS
                | TokenType::DISTINCT
                | TokenType::AND
                | TokenType::OR
                | TokenType::NOT
                | TokenType::XOR
                | TokenType::IN
                | TokenType::STARTS
                | TokenType::ENDS
                | TokenType::CONTAINS
                | TokenType::IS
                | TokenType::NULL
                | TokenType::TRUE
                | TokenType::FALSE
                | TokenType::CASE
                | TokenType::WHEN
                | TokenType::THEN
                | TokenType::ELSE
                | TokenType::END
                | TokenType::FOREACH
                | TokenType::CALL
                | TokenType::YIELD
                | TokenType::UNION
                | TokenType::ALL
                | TokenType::ON
                | TokenType::INDEX
                | TokenType::CONSTRAINT
                | TokenType::FOR
                | TokenType::REQUIRE
                | TokenType::UNIQUE
                | TokenType::EXISTS
                | TokenType::IF
                | TokenType::DROP
                | TokenType::VECTOR
                | TokenType::POINT
                | TokenType::OPTIONS
                | TokenType::NODE
                | TokenType::REL
                | TokenType::COUNT
                | TokenType::COLLECT
                | TokenType::LOAD
                | TokenType::FROM
                | TokenType::CSV
                | TokenType::REDUCE
                | TokenType::SPATIAL
                | TokenType::EXPLAIN
                | TokenType::PROFILE
                | TokenType::INFINITY
                | TokenType::NAN
        )
    }

    /// Returns true if the token can be used as an identifier (variable,
    /// alias, map key, ...): `Identifier` or any keyword except the
    /// [`CLAUSE_KEYWORDS`].
    pub fn is_ident_like(self) -> bool {
        self.is_schema_name() && !CLAUSE_KEYWORDS.contains(&self)
    }

    /// Returns true if the token is `Identifier` or any keyword: openCypher's
    /// `SchemaName ::= SymbolicName | ReservedWord`, for positions where the
    /// grammar already requires a name, so no keyword there starts a clause
    /// -- property keys after `.`, the LET binding variable, and label and
    /// relationship-type names. ADR-0169: a strict superset of
    /// `is_ident_like()`, so `(o:Order)` and `-[:ORDER]->` parse unquoted
    /// with the name kept as written.
    ///
    /// This token class is unrelated to, and deliberately wider than, the
    /// narrower thing this crate also calls a schema name: the
    /// index/constraint name `ast::SchemaName` that
    /// `parse_optional_schema_name` / `parse_required_schema_name` read
    /// (`Identifier`, `INDEX`, `CONSTRAINT` or a `$param` only).
    pub fn is_schema_name(self) -> bool {
        self == TokenType::Identifier || self.is_keyword()
    }
}

/// The clause keywords: keyword tokens that start or continue a clause
/// (`MATCH`, `ORDER BY`, `AS`, `DISTINCT`, ...). `is_ident_like()` rejects
/// them as variables, aliases and map keys, so `(WHERE ...)` does not read
/// `WHERE` as the element variable. ADR-0169: they are still label and
/// relationship-type names (`is_schema_name()`).
///
/// `RETURN` is not one of them. Verified
/// against live Neo4j 2026.05 (both CYPHER 5 and CYPHER 25): `RETURN` is a
/// non-reserved word usable as an ordinary variable/alias name (e.g.
/// `WITH 1 AS RETURN RETURN RETURN AS r`) -- `is_ident_like()` is only
/// checked in positions the grammar already knows unambiguously expect an
/// identifier (after AS, as a pattern/FOREACH variable, ...), so it never
/// shadows RETURN's role starting a new clause.
pub const CLAUSE_KEYWORDS: [TokenType; 22] = [
    TokenType::MATCH,
    TokenType::OPTIONAL,
    TokenType::CREATE,
    TokenType::INSERT,
    TokenType::MERGE,
    TokenType::DELETE,
    TokenType::DETACH,
    TokenType::SET,
    TokenType::REMOVE,
    TokenType::WITH,
    TokenType::UNWIND,
    TokenType::WHERE,
    TokenType::ORDER,
    TokenType::BY,
    TokenType::LIMIT,
    TokenType::SKIP,
    TokenType::AS,
    TokenType::DISTINCT,
    TokenType::FOREACH,
    TokenType::UNION,
    TokenType::REQUIRE,
    TokenType::DROP,
];

/// The contextual `FIELDTERMINATOR` word of LOAD CSV: an identifier-spelled
/// keyword the clause parser discriminates on by spelling. The one
/// constant, shared with the statement normalizer's grammar guard (a
/// `String` after this word is a literal-only position there) — the
/// spelling is a centralized fact, never re-derived at a second site.
pub(crate) const FIELDTERMINATOR_WORD: &str = "FIELDTERMINATOR";

/// Whether an identifier's literal spells the LOAD CSV
/// [`FIELDTERMINATOR_WORD`] (case-insensitively, as the clause grammar
/// reads it).
pub(crate) fn is_field_terminator_word(literal: &str) -> bool {
    literal.eq_ignore_ascii_case(FIELDTERMINATOR_WORD)
}

/// The reserved-keyword clause-starter tokens: the keyword tokens that
/// start a new clause at clause position (`MATCH`, `CREATE`, `UNWIND`,
/// `FOR` (CIP-250), ...). The grammar authority is the clause dispatch
/// (`Parser::parse_clause_inner`); this table is the derived vocabulary
/// every *lookahead* over the same fact consults instead of re-spelling a
/// second hand-maintained list -- through [`is_clause_boundary`], by
/// `Parser::group_by_ends_clause` (whose round-5 comment records the
/// LOAD/DROP drift that motivated it) and the simple-CASE lookahead -- and
/// the dispatch-coverage pin test. A new clause-start dispatch arm
/// extends this one table. `OPTIONAL` is a member: it starts a clause
/// only immediately before `MATCH`/`CALL` (the arm that checks what
/// follows). The contextual (identifier-spelled) starters are the
/// separate [`CONTEXTUAL_CLAUSE_WORDS`] below.
pub const CLAUSE_STARTER_TOKENS: &[TokenType] = &[
    TokenType::MATCH,
    TokenType::OPTIONAL,
    TokenType::CREATE,
    TokenType::INSERT,
    TokenType::MERGE,
    TokenType::DELETE,
    TokenType::DETACH,
    TokenType::SET,
    TokenType::REMOVE,
    TokenType::UNWIND,
    // `FOR <variable> IN <expression>`.
    TokenType::FOR,
    TokenType::FOREACH,
    TokenType::CALL,
    TokenType::UNION,
    TokenType::LOAD,
    TokenType::DROP,
];

/// The one derived "this token ends the enclosing clause" predicate: a
/// reserved clause keyword ([`CLAUSE_KEYWORDS`]) or a reserved clause
/// starter ([`CLAUSE_STARTER_TOKENS`]). Every lookahead that must stop at a
/// clause boundary consults this instead of re-spelling the vocabulary —
/// `Parser::group_by_ends_clause` and the simple-CASE lookahead
/// (`ends_case_test_scan`), each adding only its own statement/item
/// terminals.
pub(crate) fn is_clause_boundary(ty: TokenType) -> bool {
    CLAUSE_KEYWORDS.contains(&ty) || CLAUSE_STARTER_TOKENS.contains(&ty)
}

/// The contextual (non-reserved, identifier-spelled) clause keywords:
/// words that start or continue a clause when they appear as a plain
/// `Identifier` at clause position, recognized by spelling -- the same
/// role as [`CLAUSE_KEYWORDS`], but these stay usable as ordinary
/// variable/label names everywhere else (`MATCH (show) RETURN show`).
/// Single-sourced here so the clause dispatcher
/// (`Parser::parse_clause_inner`) and the `SHOW SETTINGS`-style
/// filter-expression starter (`Parser::peek_starts_show_filter_expr`,
/// which excludes exactly this set so a following composed clause is not
/// swallowed as a filter expression) can never drift apart.
///
/// `NEXT` (CIP-213) and `CYPHER` (preamble) are members even though they
/// never start a clause inside `parse_clause_inner` -- they are
/// recognized at statement-block boundaries and in the pre-parser
/// preamble respectively. `START`/`STOP`/`NODETACH`/`OFFSET` are
/// deliberately *not* members: they only act clause-like before a
/// specific following word (`DATABASE`/`DELETE`/paging), so a bare
/// `SHOW SETTINGS start` still reads `start` as a name filter.
pub const CONTEXTUAL_CLAUSE_WORDS: [&str; 9] = [
    "alter",
    "cypher",
    "filter",
    "finish",
    "let",
    "next",
    "show",
    "terminate",
    "use",
];

/// True if `literal` (any case) spells one of the
/// [`CONTEXTUAL_CLAUSE_WORDS`].
pub fn is_contextual_clause_keyword(literal: &str) -> bool {
    CONTEXTUAL_CLAUSE_WORDS
        .iter()
        .any(|w| literal.eq_ignore_ascii_case(w))
}

/// A lexical token with its type, literal value, and source position.
///
/// The literal text borrows from the lexer's input (`'a`): identifiers,
/// keywords, numbers, parameters and escape-free string literals are slices
/// of the query text, and punctuation/operators and fixed error texts are
/// `&'static str` (which coerces to `'a`). Only synthesized text is owned —
/// escape-decoded string literals and `format!`-built error messages — so
/// the token stream costs no heap allocation per token beyond the
/// `Vec<Token>` itself (ADR-0505). Tokens never outlive the input: the only
/// consumer is the [`crate::parser::Parser`], built and dropped inside the
/// parse entry points.
#[derive(Debug, Clone, PartialEq)]
pub struct Token<'a> {
    pub ty: TokenType,
    pub literal: Cow<'a, str>,
    pub line: usize,
    pub col: usize,
    /// Optional symbolic openCypher-TCK error-*detail* name carried by a
    /// [`TokenType::Error`] token (e.g. `"InvalidNumberLiteral"`,
    /// `"InvalidUnicodeLiteral"`). The parser propagates it onto the
    /// [`crate::ParseError`] it raises when it surfaces the error token, so a
    /// malformed number/unicode literal reports its specific TCK detail rather
    /// than the generic `UnexpectedSyntax`. `None` for every non-error token.
    pub detail: Option<&'static str>,
}

impl<'a> Token<'a> {
    /// Build a token whose text is borrowed (`&'a str` / `&'static str`) or
    /// owned (`String`), per the ADR-0505 literal rule — `Into<Cow>` accepts
    /// all three without a hidden copy.
    pub fn new(ty: TokenType, literal: impl Into<Cow<'a, str>>, line: usize, col: usize) -> Self {
        Token {
            ty,
            literal: literal.into(),
            line,
            col,
            detail: None,
        }
    }

    /// Build a [`TokenType::Error`] token tagged with a TCK error-detail name
    /// (see [`Token::detail`]).
    pub fn error_with_detail(
        literal: impl Into<Cow<'a, str>>,
        line: usize,
        col: usize,
        detail: &'static str,
    ) -> Self {
        Token {
            ty: TokenType::Error,
            literal: literal.into(),
            line,
            col,
            detail: Some(detail),
        }
    }
}

/// The one declaration of the keyword vocabulary [`lookup_keyword`]
/// accepts: each `spelling => variant` pair (aliases included) generates
/// the stack-buffer match cascade, the derived [`LONGEST_KEYWORD_BYTES`]
/// bound and the test's spelling table, so the cascade, the bound and the
/// test can never drift apart — a spelling added here is classified,
/// bounded and exercised in the same change, and a spelling forgotten
/// cannot exist (there is no second list to forget it from).
macro_rules! declare_keywords {
    ($( $spelling:literal => $variant:expr ),+ $(,)?) => {
        /// The longest keyword spelling accepted by [`lookup_keyword`] in
        /// bytes (`CONSTRAINT`, `DESCENDING`) — derived from the one
        /// vocabulary declaration below, never re-spelled at the
        /// short-circuit or the stack buffer size, so no arm can fall
        /// outside the bound.
        const LONGEST_KEYWORD_BYTES: usize = {
            let lengths = [$($spelling.len()),+];
            let mut longest = 0;
            let mut i = 0;
            while i < lengths.len() {
                if lengths[i] > longest {
                    longest = lengths[i];
                }
                i += 1;
            }
            longest
        };

        /// Look up a keyword from an identifier string (case-insensitive
        /// uppercase).
        ///
        /// No heap copy (ADR-0505): identifiers longer than
        /// [`LONGEST_KEYWORD_BYTES`] short-circuit to `Identifier`; shorter
        /// ones are ASCII-uppercased into a fixed stack buffer and matched
        /// against byte literals. Non-ASCII bytes pass through untouched
        /// (only `make_ascii_uppercase` is applied), so a Unicode
        /// identifier (ADR-0182) never matches a keyword — the same
        /// behavior `to_ascii_uppercase` gave.
        pub fn lookup_keyword(ident: &str) -> TokenType {
            if ident.len() > LONGEST_KEYWORD_BYTES {
                return TokenType::Identifier;
            }
            let mut buf = [0u8; LONGEST_KEYWORD_BYTES];
            let bytes = ident.as_bytes();
            buf[..bytes.len()].copy_from_slice(bytes);
            buf[..bytes.len()].make_ascii_uppercase();
            match &buf[..bytes.len()] {
                $( $spelling => $variant, )+
                _ => TokenType::Identifier,
            }
        }

        /// Every `(spelling, variant)` pair of the vocabulary, aliases
        /// included — generated from the same declaration as the cascade,
        /// so the test below pins the cascade itself, not a hand-copied
        /// twin of it.
        #[cfg(test)]
        const KEYWORD_SPELLINGS: &[(&[u8], TokenType)] = &[$(($spelling, $variant)),+];
    };
}

declare_keywords! {
    b"MATCH" => TokenType::MATCH,
    b"OPTIONAL" => TokenType::OPTIONAL,
    b"CREATE" => TokenType::CREATE,
    b"INSERT" => TokenType::INSERT,
    b"MERGE" => TokenType::MERGE,
    b"DELETE" => TokenType::DELETE,
    b"DETACH" => TokenType::DETACH,
    b"SET" => TokenType::SET,
    b"REMOVE" => TokenType::REMOVE,
    b"RETURN" => TokenType::RETURN,
    b"WITH" => TokenType::WITH,
    b"UNWIND" => TokenType::UNWIND,
    b"WHERE" => TokenType::WHERE,
    b"ORDER" => TokenType::ORDER,
    b"BY" => TokenType::BY,
    b"ASC" => TokenType::ASC,
    b"ASCENDING" => TokenType::ASC,
    b"DESC" => TokenType::DESC,
    b"DESCENDING" => TokenType::DESC,
    b"LIMIT" => TokenType::LIMIT,
    b"SKIP" => TokenType::SKIP,
    b"AS" => TokenType::AS,
    b"DISTINCT" => TokenType::DISTINCT,
    b"AND" => TokenType::AND,
    b"OR" => TokenType::OR,
    b"NOT" => TokenType::NOT,
    b"XOR" => TokenType::XOR,
    b"IN" => TokenType::IN,
    b"STARTS" => TokenType::STARTS,
    b"ENDS" => TokenType::ENDS,
    b"CONTAINS" => TokenType::CONTAINS,
    b"IS" => TokenType::IS,
    b"NULL" => TokenType::NULL,
    b"TRUE" => TokenType::TRUE,
    b"FALSE" => TokenType::FALSE,
    b"CASE" => TokenType::CASE,
    b"WHEN" => TokenType::WHEN,
    b"THEN" => TokenType::THEN,
    b"ELSE" => TokenType::ELSE,
    b"END" => TokenType::END,
    b"FOREACH" => TokenType::FOREACH,
    b"CALL" => TokenType::CALL,
    b"YIELD" => TokenType::YIELD,
    b"UNION" => TokenType::UNION,
    b"ALL" => TokenType::ALL,
    b"ON" => TokenType::ON,
    b"INDEX" => TokenType::INDEX,
    b"CONSTRAINT" => TokenType::CONSTRAINT,
    b"FOR" => TokenType::FOR,
    b"REQUIRE" => TokenType::REQUIRE,
    b"UNIQUE" => TokenType::UNIQUE,
    b"EXISTS" => TokenType::EXISTS,
    b"IF" => TokenType::IF,
    b"DROP" => TokenType::DROP,
    b"VECTOR" => TokenType::VECTOR,
    b"POINT" => TokenType::POINT,
    b"OPTIONS" => TokenType::OPTIONS,
    b"NODE" => TokenType::NODE,
    b"REL" => TokenType::REL,
    b"COUNT" => TokenType::COUNT,
    b"COLLECT" => TokenType::COLLECT,
    b"LOAD" => TokenType::LOAD,
    b"FROM" => TokenType::FROM,
    b"CSV" => TokenType::CSV,
    b"REDUCE" => TokenType::REDUCE,
    b"SPATIAL" => TokenType::SPATIAL,
    b"EXPLAIN" => TokenType::EXPLAIN,
    b"PROFILE" => TokenType::PROFILE,
    b"INFINITY" => TokenType::INFINITY,
    b"INF" => TokenType::INFINITY,
    b"NAN" => TokenType::NAN,
}

/// True when `name` must be backtick-quoted to round-trip as a Cypher name.
///
/// The DDL / canonical identifier-quoting policy: a name is bare only when it
/// has the plain identifier shape
/// ([`relate_model::identifier::has_plain_identifier_shape`]) **and**
/// [`lookup_keyword`] does not classify it as a keyword.
///
/// Keyword names need quotes even in positions that accept them bare (a
/// property key after `.`, a label or relationship type per ADR-0169) because
/// the same spelling is a keyword in the positions that call
/// [`TokenType::is_ident_like`] — a variable, alias, map key, or the
/// `ON (n.prop)` property name of `CREATE INDEX` — so the unquoted form
/// would not parse everywhere. Quoting every keyword keeps one rendered
/// statement runnable in every position instead of deciding per site.
pub fn identifier_needs_quotes(name: &str) -> bool {
    !relate_model::identifier::has_plain_identifier_shape(name) || lookup_keyword(name).is_keyword()
}

/// Renders `name` as a Cypher identifier for generated Cypher text: bare
/// when [`identifier_needs_quotes`] is false, otherwise through
/// [`relate_model::identifier::quote_identifier_always`] (the one
/// spelling of the quoting escape).
pub fn quote_identifier(name: &str) -> String {
    if identifier_needs_quotes(name) {
        relate_model::identifier::quote_identifier_always(name)
    } else {
        name.to_string()
    }
}

/// Byte offset of the first byte of each 1-based line; O(source) once.
/// The one spelling of the line table, shared by the parser's
/// `token_byte_offset` and the statement normalizer (ADR-0503).
pub(crate) fn line_starts_of(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// The byte offset of `tok`'s first byte in its source, given that
/// source's [`line_starts_of`] table and its byte length: the lexer
/// counts columns in bytes (`Lexer::advance` does `col += 1` per byte),
/// so `line_start + col - 1` is exactly the token's byte offset — no
/// per-token source rescan, and no char/byte-column mismatch after
/// non-ASCII text. `None` when the position does not land inside the
/// source — the one spelling of that half of the contract, so no caller
/// re-checks it.
pub(crate) fn token_start_byte(
    tok: &Token<'_>,
    line_starts: &[usize],
    source_len: usize,
) -> Option<usize> {
    let line_idx = tok.line.checked_sub(1)?;
    let col_bytes = tok.col.checked_sub(1)?;
    let offset = line_starts.get(line_idx)? + col_bytes;
    (offset <= source_len).then_some(offset)
}

#[cfg(test)]
mod tests {
    use relate_model::identifier::{quote_identifier_always, quote_identifier_shaped};

    use super::*;

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
        for &(spelling, ty) in KEYWORD_SPELLINGS {
            // The declaration is byte-literal — the cascade's own arm form;
            // every spelling is ASCII by construction.
            let spelling = std::str::from_utf8(spelling).expect("keyword spellings are ASCII");
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
        // `RETURN` is a
        // non-reserved word usable as an ordinary variable/alias name.
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
        for ty in CLAUSE_KEYWORDS {
            assert!(ty.is_keyword() && !ty.is_ident_like(), "{ty:?}");
        }
        assert!(!TokenType::LeftParen.is_ident_like());
    }

    #[test]
    fn test_is_schema_name() {
        // ADR-0169: every clause keyword is a label name.
        for ty in CLAUSE_KEYWORDS {
            assert!(ty.is_schema_name(), "{ty:?}");
        }
        assert!(TokenType::Identifier.is_schema_name());
        assert!(TokenType::RETURN.is_schema_name());
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
        for name in ["order", "Order", "match", "WHERE", "return", "count"] {
            let rendered = quote_identifier(name);
            assert_eq!(rendered, format!("`{name}`"), "keyword names are quoted");
            assert_eq!(one_identifier(&rendered), name);
        }
        for ty in CLAUSE_KEYWORDS {
            let spelling = format!("{ty:?}");
            assert!(
                identifier_needs_quotes(&spelling),
                "{spelling} is a clause keyword"
            );
            assert_eq!(one_identifier(&quote_identifier(&spelling)), spelling);
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
}
