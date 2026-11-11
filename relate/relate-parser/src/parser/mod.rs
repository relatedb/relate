use crate::ast::*;
use crate::error::{ParseError, ParseResult};
use crate::lexer::tokenize_with_options;
use crate::token::{is_clause_boundary, is_contextual_clause_keyword, Token, TokenType};
use relate_names::SmallName;
// `pub(crate)` beyond the parser module for the statement normalizer
// (ADR-0503), which extracts the same values the parser would bind.
pub(crate) use literals::parse_int_literal;
use literals::{IntLiteralError, IntLiteralSite};

/// `parse_constraint_for_target_ext`'s result: the `FOR <pattern>` target of
/// a `CREATE CONSTRAINT`.
struct ConstraintForTarget {
    entity_type: SchemaEntityType,
    /// The flat label/relationship-type list (empty for a label-less
    /// target).
    labels: Vec<SmallName>,
    /// The FOR pattern's own variable, `""` when it has none (the shape
    /// `CreateConstraintClause::variable` stores).
    variable: SmallName,
    /// A relationship pattern's source endpoint alias (CIP-177); `None`
    /// for a node target or an alias-less endpoint.
    source_alias: Option<SmallName>,
    /// A relationship pattern's target endpoint alias, like `source_alias`.
    target_alias: Option<SmallName>,
}

/// `parse_graph_type_constraint_verb`'s return shape: the constraint kind,
/// the parsed `<value type>` for a `PropertyType` kind, and the `OPTIONS`
/// tail.
struct GraphTypeConstraintVerbSpec {
    kind: GraphTypeConstraintVerb,
    /// The `<value type>` of a `PropertyType` kind, `None` otherwise.
    property_type: Option<TypeExpr>,
    options: OptionsForm,
}

/// ADR-0257: maximum size of a Cypher query text accepted by [`parse`], in
/// bytes (4 MiB). Enforced *before lexing* so that adversarial inputs cannot
/// amplify into unbounded token allocations (LABS-81: 8 MiB of commas used to
/// become >=520 MB of tokens). Every query path -- the Rust API, the C ABI,
/// the CLI, the TCK, and Bolt -- funnels through the parser's entry points,
/// each of which constructs through `Parser::new`, the one site the cap is
/// enforced, so this single check covers them all.
pub const MAX_QUERY_BYTES: usize = 4 * 1024 * 1024; // 4 MiB = 4 194 304

/// ADR-0257: maximum nesting depth accepted while parsing. Enforced on
/// expression nesting (every nested expression -- parenthesized constructs,
/// list/map literals, function-call arguments, subqueries, ... -- descends
/// through [`Parser::parse_expression`]; `NOT` chains recurse through
/// [`Parser::parse_not`]) and on statement nesting (subquery bodies).
/// Recursive-descent parsing beyond this depth risks a stack-overflow abort,
/// which -- when the engine is embedded via the C ABI -- kills the host
/// process instead of surfacing a recoverable error (LABS-69). The front-end
/// lowering boundary enforces the same limit on its own walkers.
pub const MAX_NESTING_DEPTH: u32 = 500;

/// ADR-0116 §2: the [`ParseError::detail`] tag on a parse failure for a
/// recognized-but-unsupported database administration command (everything
/// outside CE's supported set on Relate's fixed namespaces). The
/// error's `message` carries the verbatim statement text (without a leading
/// `CYPHER n` preamble), so `relate-front` can raise CE's
/// `UnsupportedAdministrationCommand` (51N27) echoing it. The tag name
/// matches `ErrorKind::UnsupportedAdministrationCommand` in
/// `relate-error`, so even an unmapped parse error keeps the right TCK
/// kind/class.
pub const UNSUPPORTED_ADMINISTRATION_DETAIL: &str = "UnsupportedAdministrationCommand";

/// The [`ParseError::detail`] tag naming the undefined-variable condition
/// (Neo4j 42N62): every parser site that rejects an unbound name carries it,
/// so `EngineError::kind()` surfaces `UndefinedVariable` instead of the
/// generic `UnexpectedSyntax` fallback. The tag name matches
/// `ErrorKind::UndefinedVariable` in `relate-error`.
pub const UNDEFINED_VARIABLE_DETAIL: &str = "UndefinedVariable";

/// Parse a Cypher query string into a Statement AST.
pub fn parse(input: &str) -> ParseResult<Statement> {
    parse_with_options(input, crate::lexer::ParseOptions::default())
}

/// Parse a Cypher query string into a Statement AST under
/// parse-affecting preview opt-ins ([`crate::lexer::ParseOptions`]: the
/// ADR-0371 gate set the caller's store resolved from
/// `RELATE_PREVIEW`, threaded here by `relate-front`'s preview
/// admission). [`crate::lexer::ParseOptions::default`] is the dialect
/// floor: every option widens acceptance of a form the floor rejects,
/// never narrows it, so the ungated [`parse`] is exactly
/// `parse_with_options` with all options off. The ADR-0257 size cap rides
/// the constructor (`Parser::new`'s first statement), the parser's one
/// tokenizing site -- not this entry point.
pub fn parse_with_options(
    input: &str,
    options: crate::lexer::ParseOptions,
) -> ParseResult<Statement> {
    let mut parser = Parser::new(input, "query text", options)?;
    parser.parse()
}

/// Parse a bare value expression under explicit parse-affecting preview
/// opt-ins ([`crate::lexer::ParseOptions`]). Used for CIP-261 interpolation
/// embeds: the `{...}` body of an interpolated string literal is a full
/// expression, sub-parsed by [`parse_interp_body`] with the options of the
/// parse that lexed the enclosing statement, so an embed observes the
/// caller's dialect gates instead of silently re-parsing under the floor.
/// `pub(crate)`: its only callers are in this crate (the public floor
/// wrapper [`parse_expression_str`] and the embed sub-parse); it is
/// promoted to `pub` -- with a root re-export beside
/// [`parse`]/[`parse_with_options`] -- when a second crate needs gated
/// expression parsing. The ADR-0257 size cap rides the constructor
/// (`Parser::new`'s first statement), the parser's one tokenizing site --
/// not this entry point.
pub(crate) fn parse_expression_str_with_options(
    input: &str,
    options: crate::lexer::ParseOptions,
) -> ParseResult<Expr> {
    let mut parser = Parser::new(input, "expression text", options)?;
    let expr = parser.parse_expression()?;
    if parser.peek().ty != TokenType::EOF {
        return Err(parser
            .error_at_token(parser.peek(), "unexpected trailing input in expression")
            .with_detail("UnexpectedSyntax"));
    }
    Ok(expr)
}

/// Parse a bare value expression on the dialect floor
/// ([`crate::lexer::ParseOptions::default`]) -- the public wrapper for
/// callers with no preview gates to thread; gated callers (and the
/// interpolation-embed sub-parse) use `parse_expression_str_with_options`.
pub fn parse_expression_str(input: &str) -> ParseResult<Expr> {
    parse_expression_str_with_options(input, crate::lexer::ParseOptions::default())
}

// TODO(CIP-261): an interpolated literal is scanned three times -- once by
// the lexer (`Lexer::read_interpolation`), once here to split text from
// `{...}` embeds, and once more when each embed is re-tokenized by
// `parse_expression_str_with_options`. Having the lexer emit real tokens for
// the body (an
// open/expression/close triple) would collapse this to a single scan; that
// reshape of the token stream is deferred, not done in this change.
/// Split a CIP-261 interpolation body (the [`TokenType::InterpString`]
/// payload) into text and expression parts. `\{`/`\}` are escaped literal
/// braces; a top-level `{` starts an expression segment that runs to its
/// matching `}` (nesting, inner strings and backtick-quoted identifiers
/// respected), sub-parsed with [`parse_expression_str_with_options`] under
/// the caller's [`crate::lexer::ParseOptions`], so an embed inherits the
/// dialect gates of the parse that lexed the enclosing statement.
///
/// The lexer leaves every escape unresolved in the payload, so literal-text
/// escapes are decoded here -- after the two-byte lookahead that tells `\\`
/// (an escaped backslash) apart from `\{` (an escaped literal brace).
fn parse_interp_body(
    body: &str,
    options: crate::lexer::ParseOptions,
) -> Result<Vec<crate::ast::InterpPart>, String> {
    use crate::ast::InterpPart;

    let chars: Vec<char> = body.chars().collect();
    let mut parts: Vec<InterpPart> = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            i += push_interp_escape(&mut text, &chars, i)?;
            continue;
        }
        if c != '{' {
            text.push(c);
            i += 1;
            continue;
        }
        // Expression segment: find the matching close brace.
        let mut depth = 1usize;
        let mut inner_quote: Option<char> = None;
        let mut in_backtick = false;
        let mut j = i + 1;
        while j < chars.len() {
            let d = chars[j];
            if let Some(q) = inner_quote {
                if d == '\\' {
                    j += 2;
                    continue;
                }
                if d == q {
                    inner_quote = None;
                }
            } else if in_backtick {
                // Inside `...` everything -- braces and quotes included -- is
                // an inert identifier name.
                if d == '`' {
                    in_backtick = false;
                }
            } else {
                match d {
                    '\\' => j += 1,
                    '`' => in_backtick = true,
                    '{' | '[' | '(' => depth += 1,
                    '}' | ']' | ')' => {
                        // A closer with nothing open -- e.g. the `)` in
                        // `{)}` -- is malformed. Guard the decrement so
                        // `depth` can never underflow on hostile input.
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                        // Only the outer `}` reaching depth 0 ends it.
                        if d == '}' && depth == 0 {
                            break;
                        }
                    }
                    '\'' | '"' => inner_quote = Some(d),
                    _ => {}
                }
            }
            j += 1;
        }
        if depth != 0 {
            return Err("unterminated { expression } in interpolated string".into());
        }
        let expr_src: String = chars[i + 1..j].iter().collect();
        let expr = parse_expression_str_with_options(&expr_src, options)
            .map_err(|e| format!("invalid expression in interpolated string: {e}"))?;
        if !text.is_empty() {
            parts.push(InterpPart::Text(std::mem::take(&mut text)));
        }
        parts.push(InterpPart::Interp(Box::new(expr)));
        i = j + 1;
    }
    if !text.is_empty() {
        parts.push(InterpPart::Text(text));
    }
    Ok(parts)
}

/// Decode one escape in a CIP-261 literal-text segment, appending the decoded
/// text to `text` and returning how many chars were consumed (`chars[i]` is
/// the `\`). The lexer leaves escapes unresolved so a later scanner cannot
/// confuse a resolved `\\` with the start of `\{`; this is where they resolve
/// again. `\{`/`\}` become the literal brace, known escapes decode, and an
/// unknown escape keeps its two-character form.
fn push_interp_escape(text: &mut String, chars: &[char], i: usize) -> Result<usize, String> {
    let next = chars[i + 1]; // caller guarantees `i + 1 < chars.len()`
    match next {
        '\\' => {
            text.push('\\');
            Ok(2)
        }
        '{' => {
            text.push('{');
            Ok(2)
        }
        '}' => {
            text.push('}');
            Ok(2)
        }
        'n' => {
            text.push('\n');
            Ok(2)
        }
        't' => {
            text.push('\t');
            Ok(2)
        }
        '\'' => {
            text.push('\'');
            Ok(2)
        }
        '"' => {
            text.push('"');
            Ok(2)
        }
        'u' => {
            if i + 6 > chars.len() {
                return Err("invalid unicode escape sequence".into());
            }
            let hex: String = chars[i + 2..i + 6].iter().collect();
            match u32::from_str_radix(&hex, 16) {
                Ok(cp) => {
                    // A UTF-16 surrogate pair escape (`𐀀`) is one
                    // supplementary code point, same as in a plain string
                    // literal (see the lexer's `surrogate_pair_codepoint`).
                    if let Some(pair) = interp_surrogate_pair(cp, chars, i) {
                        text.push(pair.0);
                        return Ok(pair.1);
                    }
                    if let Some(ch) = char::from_u32(cp) {
                        text.push(ch);
                    }
                    Ok(6)
                }
                Err(_) => Err(format!("invalid unicode escape sequence: \\u{hex}")),
            }
        }
        other => {
            text.push('\\');
            text.push(other);
            Ok(2)
        }
    }
}

/// Decode a high-surrogate escape at `chars[i..]` (the `\`) together with an
/// immediately following low-surrogate escape, returning the combined
/// supplementary code point and the 12 chars the pair occupies.
fn interp_surrogate_pair(cp: u32, chars: &[char], i: usize) -> Option<(char, usize)> {
    if !(0xD800..=0xDBFF).contains(&cp) || i + 12 > chars.len() {
        return None;
    }
    if chars[i + 6] != '\\' || chars[i + 7] != 'u' {
        return None;
    }
    let hex: String = chars[i + 8..i + 12].iter().collect();
    let lo = u32::from_str_radix(&hex, 16).ok()?;
    if !(0xDC00..=0xDFFF).contains(&lo) {
        return None;
    }
    char::from_u32(0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00)).map(|ch| (ch, 12))
}

/// Parse a bare `<graph type specification body>` -- the `{ ... }` grammar
/// shared by `CREATE GRAPH TYPE`/`ALTER CURRENT GRAPH TYPE`, without the
/// surrounding statement (`canonical` is wrapped in braces here, so callers
/// pass the CIP-180 canonical form's comma-joined contents, no braces of
/// their own). Used by `relate-executor`'s `Graph` to reconstruct a
/// persisted `current_graph_type` model from the snapshot manifest on load
/// -- see `Graph::restore_graph_type_spec`. The ADR-0257 size cap rides the
/// constructor (`Parser::new`'s first statement) and checks the wrapped
/// string -- the exact text this entry point lexes.
pub fn parse_graph_type_body(canonical: &str) -> ParseResult<GraphTypeBody> {
    let wrapped = format!("{{{canonical}}}");
    let mut parser = Parser::new(
        &wrapped,
        "graph type body",
        crate::lexer::ParseOptions::default(),
    )?;
    let (open, body) = parser.parse_graph_type_spec_body()?;
    parser.reject_bare_constraint_drops(&open, &body)?;
    Ok(body)
}

/// CIP-150: the parsed statement preamble (`Parser::parse_preamble`) --
/// `EXPLAIN`/`PROFILE` flags plus the effective `CYPHER <version>`, if any
/// was given. Distinct option `key=value` pairs are validated for
/// recognized keys, constrained value vocabularies
/// ([`CYPHER_OPTION_VALUES`]), and internal consistency while parsing, but
/// their values aren't retained past that check -- Relate has no
/// runtime concept of `runtime=`/`planner=` etc. to act on, so only the
/// fact that they parsed and didn't conflict matters.
struct Preamble {
    version: Option<u32>,
    explain: bool,
    profile: bool,
}

/// CIP-150: the pre-parser option keys Relate recognizes after a
/// `CYPHER [<version>]` clause (besides the version itself). These mirror
/// Neo4j's own pre-parser option vocabulary; Relate validates that a
/// key is recognized and internally consistent but -- since it implements
/// only one query-execution engine/runtime -- doesn't act on any of their
/// values.
const CYPHER_OPTION_KEYS: &[&str] = &[
    "planner",
    "runtime",
    "updatestrategy",
    "expressionengine",
    "replan",
    "connectcomponentsplanner",
    "debug",
    "operatorengine",
];

/// CIP-150 / Neo4j 2026.10: the pre-parser option keys whose *values*
/// carry a constrained vocabulary, with each key's valid values -- the
/// value-side twin of [`CYPHER_OPTION_KEYS`]' 22N07 key check. The one
/// key with a vocabulary today is `runtime`, mirroring the three runtimes
/// of Neo4j's current line (slotted, pipelined, parallel; the CIP-150
/// corpus's accepted `runtime=slotted` examples pin the Cypher-5 side of
/// that set). Relate implements a single engine, so every listed
/// value is accepted-and-ignored; any other value fails with an authored
/// 22N10 error naming the valid set.
const CYPHER_OPTION_VALUES: &[(&str, &[&str])] =
    &[("runtime", &["slotted", "pipelined", "parallel"])];

/// The recursive-descent parser over a borrowed token stream. `'a` is the
/// input query text's lifetime: the [`Token`] literals and [`Self::source`]
/// borrow it (ADR-0505), and the parser — built and dropped inside the
/// parse entry points — never outlives it. The AST it produces stays
/// `'static` (owned strings).
struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token<'a>>,
    pos: usize,
    /// Byte offset of the first byte of each 1-based source line
    /// (`line_starts[0] == 0` is line 1's start). Precomputed once per parse
    /// so `token_byte_offset` is O(1) instead of rescanning the whole source
    /// per projection item. The lexer counts columns in *bytes* (see
    /// `Lexer::advance`), so a token's byte offset is exactly
    /// `line_starts[line - 1] + col - 1` — this also fixes the old
    /// char-counted scan, which mismatched the lexer's byte-counted columns
    /// (and returned `None`) as soon as any non-ASCII char preceded the
    /// token.
    line_starts: Vec<usize>,
    /// The clause-level pattern side-effect flags (label/quantifier mixing
    /// state); see [`PatternSideEffectFlags`]. One field so a speculative
    /// parse can snapshot/restore it by plain assignment.
    side_effects: PatternSideEffectFlags,
    /// Nesting depth of `parse_when_query` branch/ELSE bodies currently
    /// being parsed -- 0 outside any such body. While positive, a bare
    /// (non-brace) body's own clause loop (`parse_statement`) must stop at
    /// a following `WHEN` too, not just `ELSE`/EOF/etc, so control returns
    /// to `parse_when_query`'s own loop to see the next branch (CIP-213
    /// `next_conditional_interactions.feature` PE3.1: `WHEN a THEN RETURN
    /// x WHEN b THEN RETURN y ELSE RETURN z`, no braces anywhere). Scoped
    /// to *this* mechanism only -- a bare `WHEN` appearing after an
    /// ordinary clause outside any WHEN-body context (e.g. `MATCH (n)
    /// WHEN ...`) must remain a hard parse error (CIP-135
    /// syntax_placement.feature [8]/[9]), so this must stay zero there.
    in_when_branch_body: u32,
    /// Nesting depth of `parse_statement` calls -- 0 outside any statement,
    /// 1 while parsing the outermost statement, 2+ while parsing a nested
    /// `EXISTS {}`/`COUNT {}`/`CALL {}` subquery's own statement body. Used
    /// to scope the label-expression mixing flags to one top-level
    /// statement; see `parse_statement`'s doc comment. Also capped at
    /// [`MAX_NESTING_DEPTH`] (ADR-0257) so deeply nested subquery bodies
    /// cannot overflow the stack via pure statement recursion.
    statement_depth: u32,
    /// Current statement-block nesting depth (ADR-0257): incremented on
    /// entering `parse_statement_block`, decremented on exit. Every
    /// statement-nesting cycle funnels through `parse_statement_block` --
    /// CALL/EXISTS/COUNT/COLLECT subquery bodies, brace-enclosed nested
    /// procedure specifications (`{ { ... } }`), and WHEN/ELSE bodies
    /// (`WHEN ... THEN { WHEN ... }`) -- including the cycles that bypass
    /// `parse_statement`, `parse_clause`, and `parse_expression` and so
    /// never engage any other counter (LABS-69: ~3000 nested braces used
    /// to SIGABRT on an 8 MiB release stack). Unlike `statement_depth`,
    /// this counter is *not* consulted for label-mixing scoping, so a
    /// top-level `{ ... }` body incrementing it cannot flip
    /// `is_outer_clause`. Capped at [`MAX_NESTING_DEPTH`].
    block_depth: u32,
    /// Current expression nesting depth (ADR-0257): incremented on entering
    /// `parse_expression` and on each recursive `parse_not` step, decremented
    /// on exit. Every nested expression -- parenthesized constructs, list
    /// literals, map literals, function-call arguments, CASE operands,
    /// comprehension bodies, subquery WHERE/RETURN expressions -- descends
    /// through `parse_expression`, so this one counter bounds all expression
    /// recursion. Also counts recursion cycles that bypass
    /// `parse_expression` but are expression-shaped: unary `-`/`!` chains,
    /// nested label/subpath parens, and nested `LIST<...>`/`ANY<...>`
    /// value-type brackets (see `parse_value_type`). Capped at
    /// [`MAX_NESTING_DEPTH`] (LABS-69).
    expr_depth: u32,
    /// Current clause nesting depth (ADR-0257): incremented on entering
    /// `parse_clause`, decremented on exit. Both callers of `parse_clause`
    /// (`parse_statement`'s body loop and `parse_foreach`'s body loop) are
    /// loops, so the counter returns to baseline after every clause and
    /// only grows with *true* clause nesting. That nesting exists: nested
    /// FOREACH bodies recurse `parse_clause` -> `parse_clause_inner` ->
    /// `parse_foreach` -> `parse_clause`, bypassing `parse_statement`, so
    /// the `statement_depth` cap never engages on that cycle (LABS-69).
    /// Capped at [`MAX_NESTING_DEPTH`].
    clause_depth: u32,
    /// True while parsing the `WHERE`-part expression of a list/pattern
    /// comprehension. There, a bare `|` is genuinely ambiguous: it could
    /// continue a label-expression disjunction (`x:A|B`) or be the
    /// comprehension's own map-expression separator (`WHERE x:A | expr]`).
    /// `parse_label_or` consults this flag to check, before committing to
    /// the disjunction reading, whether doing so would strand a
    /// well-formed label chain in front of tokens that can't follow one
    /// (e.g. `.` for property access) -- see `label_or_pipe_is_ambiguous`,
    /// which answers the question with a pure token-level scan.
    comprehension_where_ambiguous_pipe: bool,
    /// Whether the comprehension being parsed *requires* a map expression
    /// after its `|` separator (a pattern comprehension always does, a list
    /// comprehension need not). A `]` right after the scanned label chain
    /// then proves that `|` was the separator, not a label disjunction --
    /// see `label_or_pipe_is_ambiguous`.
    comprehension_where_map_required: bool,
    /// Depth of `EXISTS { … }` bodies currently being parsed. An unbounded
    /// quantified path pattern under REPEATABLE ELEMENTS is safe inside an
    /// existence probe (the probe stops at the first row) and is accepted
    /// there, while a top-level match still rejects it (42N53).
    exists_subquery_depth: u32,
    /// Memo of `pattern_comprehension_follows_equals` verdicts, keyed by
    /// the token position of the `=` the probe starts at. The parse is
    /// deterministic and the probe restores the token position and every
    /// side-effect flag before returning, so the verdict depends only on
    /// that position -- and a `true` verdict is always re-parsed for real
    /// by the caller, which re-enforces the nesting-depth caps. Without
    /// the memo, each level of a nested `[v = (n {p: ...}) | ...]` shape
    /// probes *and* re-parses the whole nested term, giving 2^k parse
    /// cost (LABS-183: ~0.7 s release at k=20, doubling per level); with
    /// it each position is probed once and the shape is O(k^2) overall.
    pattern_comprehension_probe_memo: std::collections::HashMap<usize, bool>,
    /// Memo of `scan_label_primary_extent`'s group-case extents, keyed by
    /// the opener's absolute token index and the scan's nesting depth. The
    /// scan is a pure function of `self.tokens` (fixed for the parse) and
    /// that depth -- `dynamic_label_quantifier_at(pos - self.pos)` reads
    /// `tokens[self.pos + (pos - self.pos)]` == `tokens[pos]`, so `self.pos`
    /// cancels out, and scan positions always stay at or past `self.pos` --
    /// so a cached entry always equals a fresh scan. Without it each level
    /// of a nested wide label disjunction re-walks its whole child subtree
    /// (O(width * depth^2), ~31 s on a slow Windows debug runner); with it
    /// every group extent is computed once (O(tokens)).
    label_group_extent_memo: std::collections::HashMap<(usize, u32), Option<usize>>,
    /// The parse-affecting preview opt-ins for this parse (see
    /// [`crate::lexer::ParseOptions`]). Set once by `Parser::new` from the
    /// entry point's options; never mutated mid-parse, so nested and
    /// speculative parses all observe one dialect.
    options: crate::lexer::ParseOptions,
    /// Which label-mixing accumulator the clause being parsed feeds (see
    /// [`LabelMixTarget`]). Saved and restored around each clause so a
    /// nested subquery body's clauses choose their own side.
    label_mix_target: LabelMixTarget,
}

/// The clause-level side-effect flags a pattern parse can set. Kept as one
/// `Copy` field on `Parser` (`Parser::side_effects`) so a speculative
/// `parse_pattern_path` probe snapshots and restores them by plain
/// assignment (see `pattern_comprehension_follows_equals`), and the
/// clause-start reset is a single `Default::default()` (plus the two
/// quantifier flags' per-clause reset for nested clauses).
#[derive(Default, Clone, Copy)]
struct PatternSideEffectFlags {
    /// The label-mixing forms seen in the clause currently being parsed.
    /// `parse_insert` reads this for its own clause-local rejection (a
    /// `:A:B` chain is never allowed in INSERT); it is reset at the start of
    /// every clause, nested or not.
    clause_labels: LabelMixFlags,
    /// The label-mixing forms seen by the read clauses accumulated in the
    /// current scope. Neo4j keeps the read side (MATCH/WITH/RETURN label
    /// predicates) and the write side (CREATE/INSERT; MERGE counts as both)
    /// apart: inside one `CALL { … }` body `MATCH (n:A:B)` followed by
    /// `CREATE (m IS B)` is legal, while two MATCHes (or a MERGE and a
    /// CREATE) mixing the forms are not. See `PatternSideEffectFlags::read_labels`.
    read_labels: LabelMixFlags,
    /// The write side of the split described on [`Self::read_labels`].
    write_labels: LabelMixFlags,
    /// True if the ONE label/type expression currently being parsed saw a
    /// colon-conjunction (`:A:B`), and
    /// [`Self::label_expr_saw_gql_symbol`] the GQL symbol case. Reset at
    /// the entry of `parse_label_expr_body`; `mixed-label-syntax` admits
    /// mixing across separate label expressions of one clause, but Neo4j
    /// keeps it illegal *within* one expression
    /// (GpmSyntaxMixingAllowedAcceptance "Mixing : conjunction with
    /// GPM-only label/type expression operators": `(n:A|B:C)`).
    label_expr_saw_legacy_conjunction: bool,
    /// See [`Self::label_expr_saw_legacy_conjunction`].
    label_expr_saw_gql_symbol: bool,
    /// True if a parenthesized label-expression group (`(A:B)`, `(A&B)`,
    /// ...) was parsed anywhere in the clause currently being parsed.
    /// CIP-114: CREATE/MERGE/MATCH allow this grouping, but INSERT does
    /// not -- `parse_insert` checks this flag after parsing its pattern,
    /// same reset/scoping as [`Self::clause_labels`].
    label_saw_paren_group: bool,
    /// True if a legacy bracket-syntax variable-length quantifier
    /// (`[*min..max]`, including a bare `[*]`) was parsed anywhere in the
    /// clause currently being parsed. Neo4j rejects mixing this pre-GPM
    /// quantifier syntax with a GPM postfix/QPP quantifier (`->+`, `->*`,
    /// `->{m,n}`, `((...)...){m,n}`) *within the same clause* -- but not
    /// across separate clauses/statements (CIP-40 quantifier-mixing.feature
    /// scenario 9 vs scenario 10), checked in `parse_clause`.
    quantifier_saw_legacy_bracket: bool,
    /// True if a GPM postfix or QPP quantifier was parsed anywhere in the
    /// clause currently being parsed. See `quantifier_saw_legacy_bracket`.
    quantifier_saw_gpm_form: bool,
    /// Quantifier forms seen inside pattern comprehensions of the clause
    /// currently being parsed (accumulated across all of its
    /// comprehensions). A comprehension's inner pattern is its own pattern
    /// context: its quantifier forms never combine with the clause's own
    /// (a legacy `[:R*1..2]` inside a comprehension nested in a quantified
    /// path pattern's WHERE is legal, Neo4j
    /// QuantifiedPathPatternAcceptance "Pattern comprehension containing a
    /// variable-length relationship inside a QPP is allowed"), but two
    /// comprehensions in the same clause still combine with each other
    /// (CIP-40 label-expression-mixing.feature scenario 13,
    /// quantifier-mixing.feature scenario 10) -- so these are separate
    /// accumulators, checked separately from the clause-level pair in
    /// `parse_clause_at_current_depth`.
    quantifier_comprehension_saw_legacy_bracket: bool,
    /// See `quantifier_comprehension_saw_legacy_bracket`.
    quantifier_comprehension_saw_gpm_form: bool,
}

/// The three label-expression forms whose mixing Neo4j rejects within one
/// clause (CIP-14/CIP-40).
#[derive(Default, Clone, Copy)]
struct LabelMixFlags {
    /// A legacy multi-label colon-conjunction (`:A:B`).
    legacy_conjunction: bool,
    /// A GQL label-expression symbol (`&`, `!`, `%`, a grouping paren, or
    /// `|` on a node).
    gql_symbol: bool,
    /// A pattern-position `IS` label/type introducer (`(n IS A)`,
    /// `-[IS R]->`), even without any GQL symbol beside it.
    is_introducer: bool,
}

impl LabelMixFlags {
    /// The rejection this set of forms triggers, if any. The GQL-symbol rule
    /// is reported before the `IS` rule, matching the reference engine's
    /// 42I10-before-42I29 precedence.
    fn mixing(&self) -> Option<&'static str> {
        if self.legacy_conjunction && self.gql_symbol {
            return Some(
                "InvalidLabelExpression: mixing label expression symbols (|, &, !, and %) with colon (:) between labels is not allowed",
            );
        }
        if self.legacy_conjunction && self.is_introducer {
            return Some(
                "MixedIsAndColonConjunction: cannot mix the IS introducer with colon-conjunction within the same clause",
            );
        }
        None
    }
}

/// Which accumulator(s) the label forms of the clause being parsed feed.
/// Neo4j keeps read clauses and write clauses apart when combining the forms
/// across the clauses of one subquery scope; MERGE is both, and a label
/// specification written by SET/REMOVE never joins either side.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum LabelMixTarget {
    #[default]
    None,
    Read,
    Write,
    Both,
}

/// A single postfix accessor (`.prop` or CIP-131's `[<expr>]`) parsed after
/// a SET/REMOVE target's base expression by `Parser::parse_property_target_accessors`.
enum PropertyTargetAccessor {
    Static(SmallName),
    Dynamic(Expr),
}

/// Generates the `enter_*_depth` / `exit_*_depth` / `with_*_depth` triple
/// for one depth-guard counter field (ADR-0257, LABS-69). The four
/// families (`expr_depth`, `clause_depth`, `statement_depth`,
/// `block_depth`) share one mechanically identical shape; generating them
/// from a single definition keeps the balanced-enter/exit property true by
/// construction: `enter` leaves the counter unchanged on error, `exit` is
/// a saturating decrement, and `with` captures `f`'s result (including
/// error propagation), decrements, and only then returns, so no `?` can
/// ever sit between enter and exit. The limit check and error message are
/// byte-identical across families.
macro_rules! depth_guard_family {
    (
        field = $field:ident,
        enter = $enter:ident,
        exit = $exit:ident,
        with = $with:ident,
        enter_doc = $enter_doc:expr,
        exit_doc = $exit_doc:expr,
        with_doc = $with_doc:expr $(,)?
    ) => {
        #[doc = $enter_doc]
        fn $enter(&mut self) -> ParseResult<()> {
            if self.$field >= MAX_NESTING_DEPTH {
                return Err(self.error(format!(
                    "maximum nesting depth of {MAX_NESTING_DEPTH} exceeded"
                )));
            }
            self.$field += 1;
            Ok(())
        }

        #[doc = $exit_doc]
        fn $exit(&mut self) {
            self.$field = self.$field.saturating_sub(1);
        }

        #[doc = $with_doc]
        fn $with<T>(&mut self, f: impl FnOnce(&mut Self) -> ParseResult<T>) -> ParseResult<T> {
            self.$enter()?;
            let result = f(self);
            self.$exit();
            result
        }
    };
}

/// The running base of [`Parser::parse_postfix`]'s postfix chain, making
/// the namespace-call accumulation invariant structural instead of
/// comment-held.
///
/// While a dot-chain the memoized `scan_dot_chain_for_call` classified as
/// ending in `(` is being accumulated, the base is [`PostfixBase::NsPath`]
/// — a bare namespace-path string with *no* `Expr` — so no postfix arm can
/// observe or rebuild a stale intermediate expression (materializing one
/// per segment cost Θ(n²) in string clones, a parse-time DoS on maximal
/// inputs). Everywhere else the base is [`PostfixBase::Built`].
enum PostfixBase {
    /// A materialized expression, plus its namespace path while it is a
    /// plain dotted path (a `Variable`, or a property chain over one) —
    /// maintained incrementally so a long `a.b.c...` chain costs amortized
    /// O(1) per segment instead of re-walking and re-`format!`ing the
    /// whole chain per dot. `None` once any non-path postfix form
    /// (subscript, map projection, function call) has been applied.
    Built(Expr, Option<String>),
    /// A call-bound dot-chain accumulated as a bare path string. Entered
    /// only under the scan's ends-in-`(` verdict, so mid-chain the token
    /// after each `.ident` is always `.` or `(`, and the chain's
    /// terminating `(` arm — which builds the `FunctionCall` name from
    /// this path — is the only way out.
    NsPath(String),
}

impl<'a> Parser<'a> {
    /// Builds a parser over `source` under one explicit dialect: the
    /// constructor tokenizes the source itself via
    /// [`tokenize_with_options`], so the token stream and the
    /// [`crate::lexer::ParseOptions`] that lexed it are one fact, never two
    /// independently supplied arguments that could disagree. Every entry
    /// point (`parse_with_options`, `parse_expression_str_with_options`,
    /// `parse_graph_type_body`) passes its options here -- the ungated floor
    /// entry points pass [`crate::lexer::ParseOptions::default`] explicitly,
    /// and `parse_with_options` forwards the caller's preview gates -- so a
    /// `Parser` can never exist half-configured.
    ///
    /// The constructor is the parser's only tokenizing site, so the
    /// ADR-0257 admission cap ([`MAX_QUERY_BYTES`]) is structural here, not
    /// a convention each entry point must remember to enforce: the first
    /// statement checks `source.len()` *before* lexing, so adversarial
    /// input cannot amplify into unbounded token allocations (LABS-81: 8
    /// MiB of commas used to become over 520 MB of tokens). `subject` only
    /// labels the input kind in the over-cap error message.
    fn new(
        source: &'a str,
        subject: &str,
        options: crate::lexer::ParseOptions,
    ) -> Result<Self, ParseError> {
        if source.len() > MAX_QUERY_BYTES {
            return Err(ParseError::new(
                format!(
                    "{subject} of {} bytes exceeds the maximum query size of {MAX_QUERY_BYTES} bytes",
                    source.len()
                ),
                1,
                1,
            ));
        }
        let tokens = tokenize_with_options(source, options);
        Ok(Parser {
            source,
            line_starts: crate::token::line_starts_of(source),
            tokens,
            pos: 0,
            side_effects: PatternSideEffectFlags::default(),
            label_mix_target: LabelMixTarget::None,
            in_when_branch_body: 0,
            statement_depth: 0,
            block_depth: 0,
            expr_depth: 0,
            clause_depth: 0,
            comprehension_where_ambiguous_pipe: false,
            comprehension_where_map_required: false,
            exists_subquery_depth: 0,
            pattern_comprehension_probe_memo: std::collections::HashMap::new(),
            label_group_extent_memo: std::collections::HashMap::new(),
            options,
        })
    }

    // ---- Token access ----

    /// The synthetic past-the-end EOF sentinel — the one spelling of the
    /// token every out-of-range probe returns. `Token<'static>` covariantly
    /// subtypes to any `Token<'a>`, so one shared empty-literal instance
    /// serves every parse.
    fn eof_token() -> &'static Token<'static> {
        static EOF: std::sync::OnceLock<Token<'static>> = std::sync::OnceLock::new();
        EOF.get_or_init(|| Token::new(TokenType::EOF, "", 0, 0))
    }

    fn peek(&self) -> &Token<'a> {
        self.peek_at(0)
    }

    fn peek_type(&self) -> TokenType {
        self.peek().ty
    }

    /// Looks ahead `offset` tokens past the current position (0 == `peek`)
    /// without consuming anything.
    fn peek_at(&self, offset: usize) -> &Token<'a> {
        let idx = self.pos + offset;
        if idx >= self.tokens.len() {
            Self::eof_token()
        } else {
            &self.tokens[idx]
        }
    }

    /// True if the current token is a case-insensitive identifier matching
    /// `word` — the generic probe for the many contextual keywords that are
    /// not reserved words in the lexer: type grammar words (`VALUE`,
    /// `PROPERTY`, `MAP`) and schema/administration words (`DEFAULT`,
    /// `KEY`, ...).
    pub(super) fn peek_is_ident(&self, word: &str) -> bool {
        self.peek_at_is_ident(0, word)
    }

    /// Like `peek_is_ident`, but `offset` tokens ahead (0 == `peek_is_ident`).
    pub(super) fn peek_at_is_ident(&self, offset: usize, word: &str) -> bool {
        Self::token_is_ident(self.peek_at(offset), word)
    }

    /// True if `tok` is a case-insensitive identifier matching `word` — the
    /// token-level core of [`Self::peek_at_is_ident`], for call sites that
    /// already hold the token.
    pub(super) fn token_is_ident(tok: &Token, word: &str) -> bool {
        tok.ty == TokenType::Identifier && tok.literal.eq_ignore_ascii_case(word)
    }

    /// True when the cursor is on the `ALL` set-quantifier spelling
    /// (`count(ALL x)`, `collect(ALL x)`, `RETURN ALL`, `GROUP BY ALL`),
    /// false for the `all(x IN list WHERE predicate)` list-predicate
    /// function: `ALL` immediately followed by `(` is always the function,
    /// the same priority the RETURN/WITH/GROUP-BY modifier readers apply.
    /// The one spelling of that rule, shared by every set-quantifier site so
    /// a nested `all(...)` inside an argument list cannot be swallowed as
    /// the aggregate's quantifier.
    pub(super) fn all_is_set_quantifier(&self) -> bool {
        self.peek_type() == TokenType::ALL && self.peek_at(1).ty != TokenType::LeftParen
    }

    /// Consume and return the current token. The value is cloned out of the
    /// stream because `backup`/`restore_pos` re-read tokens from
    /// `self.tokens` after they have been consumed; with ADR-0505's borrowed
    /// literals that clone is a pointer copy, not a heap allocation (only
    /// the rare owned literal — an escaped string, a formatted error — pays
    /// a String clone here).
    fn next_token(&mut self) -> Token<'a> {
        if self.pos >= self.tokens.len() {
            Self::eof_token().clone()
        } else {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            tok
        }
    }

    fn backup(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
        }
    }

    fn save_pos(&self) -> usize {
        self.pos
    }

    fn restore_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Extract a namespace string from an expression.
    /// Returns Some("name") for Variable("name"), or Some("a.b") for nested property accesses
    /// on variables that look like namespace paths.
    fn extract_namespace(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Variable(name) => Some(name.with_str(str::to_owned)),
            Expr::Property(prop) => {
                Self::extract_namespace(&prop.expr).map(|ns| format!("{}.{}", ns, prop.property))
            }
            _ => None,
        }
    }

    fn expect(&mut self, ty: TokenType) -> ParseResult<Token<'a>> {
        let tok = self.next_token();
        if tok.ty != ty {
            return Err(self.error_at_token(
                &tok,
                format!("expected {:?}, got {:?} ({:?})", ty, tok.literal, tok.ty),
            ));
        }
        Ok(tok)
    }

    /// Like [`Parser::expect`] but tags the raised error with a specific
    /// openCypher-TCK error *detail* name. Used at grammar positions
    /// where a mismatch has a more precise classification than the generic
    /// `UnexpectedSyntax` — e.g. the closing `]` of a relationship pattern
    /// (`InvalidRelationshipPattern`).
    fn expect_detail(&mut self, ty: TokenType, detail: &'static str) -> ParseResult<Token<'a>> {
        let tok = self.next_token();
        if tok.ty != ty {
            return Err(self
                .error_at_token(
                    &tok,
                    format!("expected {:?}, got {:?} ({:?})", ty, tok.literal, tok.ty),
                )
                .with_detail(detail));
        }
        Ok(tok)
    }

    fn error(&self, msg: impl Into<String>) -> ParseError {
        let tok = self.peek();
        self.error_at_token(tok, msg)
    }

    fn error_at_token(&self, tok: &Token, msg: impl Into<String>) -> ParseError {
        let err = ParseError::new(msg, tok.line, tok.col).with_source(self.source);
        // A lexer error token (malformed number/unicode literal) carries the
        // specific openCypher-TCK error *detail* it should surface — propagate
        // it so the raised parse error reports e.g. `InvalidNumberLiteral`
        // instead of the generic `UnexpectedSyntax`. Non-error
        // tokens carry no detail, so this is a no-op for the common case.
        match tok.detail {
            Some(detail) => err.with_detail(detail),
            None => err,
        }
    }

    /// A violated parser-internal invariant (e.g. the `parse_postfix`
    /// namespace-call state machine reaching a state the memoized scan's
    /// verdict rules out). The query engine's contract is typed errors,
    /// never panics, so even a "cannot happen" state surfaces as a typed
    /// parse error whose message marks it as an internal bug rather than a
    /// user-syntax problem.
    fn internal_error(&self, msg: impl Into<String>) -> ParseError {
        self.error(format!("internal parser error: {}", msg.into()))
    }

    // ---- Top level ----

    depth_guard_family! {
        field = expr_depth,
        enter = enter_expr_depth,
        exit = exit_expr_depth,
        with = with_expr_depth,
        enter_doc = "ADR-0257: enter one level of expression nesting. \
            Returns a typed error naming the limit once [`MAX_NESTING_DEPTH`] \
            levels are already active, instead of recursing until the process \
            aborts (LABS-69). On error the counter is left unchanged, so a \
            speculative re-parse at a shallower position is not poisoned.",
        exit_doc = "Pairs with [`Parser::enter_expr_depth`]; must run on \
            every exit path (including error propagation) so \
            backtracking/speculative parses keep the counter balanced.",
        with_doc = "Runs `f` one expression-nesting level deep: the only \
            sanctioned way to guard a recursive expression-shaped parse with \
            `expr_depth`. The enter/exit pair is balanced by construction -- \
            `f`'s result (including error propagation) is captured, the \
            counter is decremented, and only then is the result returned, so \
            no `?` can ever sit between enter and exit.",
    }

    depth_guard_family! {
        field = clause_depth,
        enter = enter_clause_depth,
        exit = exit_clause_depth,
        with = with_clause_depth,
        enter_doc = "ADR-0257: enter one level of clause nesting. Returns a \
            typed error naming the limit once [`MAX_NESTING_DEPTH`] levels \
            are already active, instead of recursing until the process aborts \
            (LABS-69: nested FOREACH bodies recurse through `parse_clause` \
            without passing through `parse_statement`). On error the counter \
            is left unchanged, so a speculative re-parse at a shallower \
            position is not poisoned.",
        exit_doc = "Pairs with [`Parser::enter_clause_depth`]; must run on \
            every exit path (including error propagation) so sequential \
            clauses in one statement/FOREACH body do not accumulate.",
        with_doc = "[`Parser::with_expr_depth`] for `clause_depth`.",
    }

    depth_guard_family! {
        field = statement_depth,
        enter = enter_statement_depth,
        exit = exit_statement_depth,
        with = with_statement_depth,
        enter_doc = "ADR-0257: enter one level of statement nesting. Returns \
            a typed error naming the limit once [`MAX_NESTING_DEPTH`] levels \
            are already active, instead of recursing until the process aborts \
            (LABS-69). On error the counter is left unchanged, so a \
            speculative re-parse at a shallower position is not poisoned.",
        exit_doc = "Pairs with [`Parser::enter_statement_depth`]; must run on \
            every exit path (including error propagation) so \
            backtracking/speculative parses keep the counter balanced.",
        with_doc = "[`Parser::with_expr_depth`] for `statement_depth`.",
    }

    depth_guard_family! {
        field = block_depth,
        enter = enter_block_depth,
        exit = exit_block_depth,
        with = with_block_depth,
        enter_doc = "ADR-0257: enter one level of statement-block nesting. \
            Returns a typed error naming the limit once \
            [`MAX_NESTING_DEPTH`] levels are already active, instead of \
            recursing until the process aborts (LABS-69: braced single \
            queries and WHEN/ELSE bodies recurse through \
            `parse_statement_block` without passing through \
            `parse_statement`, `parse_clause`, or `parse_expression`). On \
            error the counter is left unchanged, so a speculative re-parse \
            at a shallower position is not poisoned.",
        exit_doc = "Pairs with [`Parser::enter_block_depth`]; must run on \
            every exit path (including error propagation) so sequential NEXT \
            stages in one block and backtracking/speculative parses keep the \
            counter balanced.",
        with_doc = "[`Parser::with_expr_depth`] for `block_depth`.",
    }

    /// Parses an `Integer` token as a `u32` bound (relationship hop count,
    /// path quantifier bound, number of paths/groups) via the checked
    /// [`parse_int_literal`] path. Literals that don't fit are a typed parse
    /// error naming the bound -- never a silent clamp (LABS-81:
    /// `[*99999999999..2]` used to parse as `min_hops = 1`).
    fn parse_u32_bound(&self, tok: &Token, what: &str) -> ParseResult<u32> {
        // Both failures render through the one integer-literal error path,
        // naming the bound.
        let site = IntLiteralSite::Bounded {
            what,
            max: u64::from(u32::MAX),
        };
        let value = parse_int_literal(&tok.literal)
            .map_err(|error| self.int_literal_error(tok, error, site))?;
        u32::try_from(value)
            .map_err(|_| self.int_literal_error(tok, IntLiteralError::Overflow, site))
    }

    fn parse(&mut self) -> ParseResult<Statement> {
        let preamble = self.parse_preamble()?;

        let mut stmt = self.parse_statement_block()?;
        stmt.explain = preamble.explain;
        stmt.profile = preamble.profile;
        stmt.cypher_version = preamble.version;

        if self.peek_type() == TokenType::Semicolon {
            self.next_token();
        }
        // Any token left unconsumed here means `parse_regular_query`
        // returned without absorbing the rest of the input -- e.g. a bare
        // CIP-135 `<when query>` cannot be UNIONed (syntax_placement.feature
        // scenarios 16/17), and two brace-enclosed `<single query>`s
        // juxtaposed with no `UNION`/`NEXT` connector between them
        // (scenario 12/13) leave the second `{...}` sitting here. Every
        // other clause-loop path already consumes all the way to EOF on
        // its own (`parse_statement`'s loop keeps calling `parse_clause`
        // until it hits one of EOF/`;`/`UNION`/`}`, and any clause-shaped
        // leftover token already produces its own "unexpected token" error
        // there), so this is specifically the top-level backstop for the
        // brace-form `SingleQuery`/`<when query>` productions, which don't
        // loop.
        if self.peek_type() != TokenType::EOF {
            return Err(self.error(format!("unexpected token {:?}", self.peek().literal)));
        }

        Ok(stmt)
    }

    /// CIP-150: consumes the optional statement preamble at the very start
    /// of the query -- any interleaving of `EXPLAIN`/`PROFILE` (each at
    /// most once, mutually exclusive) with zero or more `CYPHER [<version>]
    /// [<key>=<value>]...` clauses. `CYPHER` is a non-reserved word --
    /// recognized by spelling on a plain `Identifier` token, like `SHOW`/
    /// `USE` elsewhere -- checked only here, once, at the true start of
    /// input, so it never shadows a variable named `cypher` appearing
    /// anywhere else in a query.
    ///
    /// Grammar (per the CIP): `<preamble> ::= <preamble option>...`,
    /// `<preamble option> ::= EXPLAIN | PROFILE | <cypher options>`,
    /// `<cypher options> ::= CYPHER [<cypher version>] <cypher option>...`.
    /// A fresh `CYPHER` keyword is required to open each options clause --
    /// once one ends (at the first token that isn't a recognized
    /// `<key>=<value>` pair), only another `CYPHER`/`EXPLAIN`/`PROFILE`
    /// token can continue the preamble; anything else (including a bare
    /// `key=value` with no preceding fresh `CYPHER`) falls through to
    /// ordinary statement parsing and is a generic syntax error there --
    /// this is what makes `CYPHER 5 EXPLAIN runtime=slotted RETURN 1`
    /// (doc example E1) a hard error: `EXPLAIN` ends the `CYPHER` clause,
    /// and `runtime=slotted` isn't a valid statement start.
    ///
    /// Two different `<cypher version>` values for the same statement (or
    /// two different values for the same non-version option key) are
    /// rejected as a conflicting pre-parser combination; the same value
    /// repeated is fine. An out-of-range or non-integer version number, or
    /// an unrecognized option key, is also rejected. Relate recognizes
    /// exactly the two versions Neo4j 2026.05 ships (`5`, `25`); either is
    /// accepted and recorded on the returned `Statement` for the engine to
    /// act on (see `Statement::cypher_version`) -- Relate otherwise
    /// implements a single dialect (the modern, GQL-aligned grammar this
    /// whole parser targets), so the two versions differ only where the
    /// engine explicitly checks `cypher_version` (the gates in
    /// `relate-front`'s `validate::dialect`, wired into lowering).
    fn parse_preamble(&mut self) -> ParseResult<Preamble> {
        let mut version: Option<u32> = None;
        let mut options: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut explain = false;
        let mut profile = false;

        loop {
            match self.peek_type() {
                TokenType::EXPLAIN if !explain && !profile => {
                    self.next_token();
                    explain = true;
                }
                TokenType::PROFILE if !explain && !profile => {
                    self.next_token();
                    profile = true;
                }
                TokenType::Identifier if self.peek().literal.eq_ignore_ascii_case("cypher") => {
                    self.next_token(); // consume CYPHER

                    // Optional `<cypher version> ::= <digit>...` -- only
                    // attempted when the next token could plausibly be one
                    // (an integer or decimal digit sequence); anything else
                    // (an option key, EXPLAIN, EOF, ...) means no version
                    // was given at all, per the grammar's `[<cypher
                    // version>]`.
                    if matches!(self.peek_type(), TokenType::Integer | TokenType::Float) {
                        let tok = self.next_token();
                        if tok.ty != TokenType::Integer
                            || (tok.literal != "5" && tok.literal != "25")
                        {
                            return Err(self.error_at_token(
                                &tok,
                                format!(
                                    "SyntaxError: 22N10 invalid pre-parser option value: {:?} is not a valid cypher version",
                                    tok.literal
                                ),
                            ));
                        }
                        let v: u32 = tok.literal.parse().unwrap();
                        match version {
                            Some(existing) if existing != v => {
                                return Err(self.error_at_token(
                                    &tok,
                                    "SyntaxError: 22N09 conflicting pre-parser combination: multiple values for 'cypher version'",
                                ));
                            }
                            _ => version = Some(v),
                        }
                    }

                    // Zero or more `<cypher option key> = <cypher option
                    // value>` pairs, each `<key>` a plain identifier
                    // directly followed by `=`.
                    while self.peek_type() == TokenType::Identifier
                        && self.peek_at(1).ty == TokenType::Equals
                    {
                        let key_tok = self.next_token();
                        self.next_token(); // consume =
                        let value_tok = self.next_token();
                        let key_lower = key_tok.literal.to_ascii_lowercase();
                        if !CYPHER_OPTION_KEYS.contains(&key_lower.as_str()) {
                            return Err(self.error_at_token(
                                &key_tok,
                                format!(
                                    "SyntaxError: 22N07 invalid pre-parser option key: {:?}",
                                    key_tok.literal
                                ),
                            ));
                        }
                        // Keys with a constrained value vocabulary
                        // (`CYPHER_OPTION_VALUES`) validate their value
                        // the same way -- an authored 22N10 error naming
                        // the valid set, the value-side twin of the 22N07
                        // key error above.
                        if let Some((_, values)) = CYPHER_OPTION_VALUES
                            .iter()
                            .find(|(key, _)| *key == key_lower)
                        {
                            if !values.contains(&value_tok.literal.to_ascii_lowercase().as_str()) {
                                return Err(self.error_at_token(
                                    &value_tok,
                                    format!(
                                        "SyntaxError: 22N10 invalid pre-parser option value: {:?} is not a valid {} value (valid values: {})",
                                        value_tok.literal,
                                        key_tok.literal,
                                        values.join(", ")
                                    ),
                                ));
                            }
                        }
                        match options.get(&key_lower) {
                            Some(existing) if existing != &value_tok.literal => {
                                return Err(self.error_at_token(
                                    &value_tok,
                                    format!(
                                        "SyntaxError: 22N09 conflicting pre-parser combination: multiple values for {:?}",
                                        key_tok.literal
                                    ),
                                ));
                            }
                            _ => {
                                options.insert(key_lower, value_tok.literal.into_owned());
                            }
                        }
                    }
                }
                _ => break,
            }
        }

        Ok(Preamble {
            version,
            explain,
            profile,
        })
    }

    /// `<RegularQuery> ::= <SingleQuery> (UNION [ALL] <SingleQuery>)*` -- a
    /// single query optionally chained to further queries via UNION/UNION
    /// ALL. This is the top-level query grammar minus EXPLAIN/PROFILE (which
    /// only apply once, at the very start of a top-level query), so it is
    /// also the grammar every subquery body uses: `COUNT { ... }`,
    /// `EXISTS { ... }`, `COLLECT { ... }`, and `CALL { ... }` all embed a
    /// `<RegularQuery>` and must therefore accept a UNION chain between their
    /// braces, not just a single statement.
    ///
    /// CIP-146: each `<SingleQuery>` is now parsed via `parse_single_query`,
    /// which additionally accepts the brace-enclosed
    /// `[ USE <graph-expr> ] '{' <RegularQuery> '}'` form. Per the CIP's
    /// Semantics Step 3 (Unwrap), a brace-enclosed arm is transparent
    /// (Case 1) when it is the *sole* `SingleQuery` here (no `UNION`
    /// follows) -- its clauses/union chain are simply used as this
    /// `RegularQuery` directly, which is exactly what `parse_single_query`
    /// already returns. But once a `UNION` token *does* follow, every arm
    /// (including this first one) is now a `SingleQuery`-only/UNION-argument
    /// context (Case 2): an arm whose own brace body was itself composite
    /// (nested UNION) needs the `CALL (*) { BODY } RETURN *` / `... FINISH`
    /// rewrap (Case 2.2) via `maybe_wrap_composite_union_arm` so the outer
    /// union's dedup boundary lands exactly where the braces put it (see
    /// union-braces.feature scenario 7); a linear arm needs no rewrap since
    /// splicing it in directly is already observably identical.
    fn parse_regular_query(&mut self) -> ParseResult<Statement> {
        // CIP-135: `<regular query> ::= <union query> | <when query> |
        // <single query>` -- a `<when query>` (bare `WHEN ... THEN {...}
        // [WHEN ...]* [ELSE {...}]`) is a distinct top-level alternative,
        // not composable with UNION and not a `<single query>` (so it
        // can't appear as a UNION arm) unless explicitly wrapped in
        // top-level braces (`{ WHEN ... }`, which *is* a `<single query>`
        // per CIP-146 since its content is a full `<regular query>`).
        // Only checked here, at the very start of a `<regular query>` --
        // never inside `parse_clause_inner`, so a `WHEN` appearing after
        // an earlier clause in the same statement (e.g. `MATCH (n) WHEN
        // ...`) still falls through to that dispatch's catch-all
        // "unexpected token" error.
        if self.peek_type() == TokenType::WHEN {
            return self.parse_when_query();
        }
        let mut stmt = self.parse_single_query()?;

        if self.peek_type() != TokenType::UNION {
            return Ok(stmt);
        }
        stmt = Self::maybe_wrap_composite_union_arm(stmt);

        // UNION / UNION ALL / UNION DISTINCT (CIP-154: `DISTINCT` is an
        // optional synonym for plain UNION's set semantics -- mutually
        // exclusive with `ALL`, and represented identically to plain UNION
        // as `all: false`; mixing `UNION` with `UNION ALL` in one chain is
        // accepted, as in Neo4j).
        while self.peek_type() == TokenType::UNION {
            self.next_token();
            let all = if self.peek_type() == TokenType::ALL {
                self.next_token();
                true
            } else {
                if self.peek_type() == TokenType::DISTINCT {
                    self.next_token();
                }
                false
            };
            let next_stmt = self.parse_single_query()?;
            let next_stmt = Self::maybe_wrap_composite_union_arm(next_stmt);
            // Attach to end of union chain
            let mut current = &mut stmt;
            while current.union.is_some() {
                current = &mut current.union.as_mut().unwrap().statement;
            }
            current.union = Some(Box::new(UnionPart {
                all,
                statement: next_stmt,
            }));
        }

        Ok(stmt)
    }

    /// True if the current token is the CIP-213 `NEXT` contextual keyword --
    /// a non-reserved word (per the CIP's own grammar), recognized by
    /// spelling on a plain `Identifier` token, the same technique already
    /// used for `SHOW`/`USE`/`LET`/`FILTER`/`FINISH`. Only ever checked at
    /// clause-list-loop boundaries (`parse_statement`'s stop condition) and
    /// at the top of `parse_statement_block`'s own loop -- never inside
    /// `parse_clause_inner` -- so `next` remains usable as an ordinary
    /// variable/label name everywhere else (e.g. `MATCH (next) RETURN
    /// next`).
    fn at_next_keyword(&self) -> bool {
        self.peek_type() == TokenType::Identifier
            && self.peek().literal.eq_ignore_ascii_case("next")
    }

    /// CIP-213 `<statement block> ::= <statement> [<next statement>...]`,
    /// `<next statement> ::= NEXT <statement>`. Wraps `parse_regular_query`
    /// (a `<statement>`: a possibly-UNION'd `<composite statement>`, or a
    /// bare `<conditional statement>` i.e. a `<when query>`) with a
    /// trailing loop over `NEXT <statement>` stages, threading each stage
    /// onto the previous one's `.next` field. This is the production every
    /// "full regular query" call site now uses in place of a raw
    /// `parse_regular_query` call, so NEXT is parseable everywhere a
    /// `<RegularQuery>` embeds (top level, brace-enclosed nested procedure
    /// specifications, CALL subquery bodies, and EXISTS/COUNT/COLLECT
    /// subquery-expression bodies) -- matching every context the CIP's own
    /// examples chain NEXT through.
    ///
    /// ADR-0257: this is the single choke point through which EVERY
    /// statement-nesting cycle funnels -- CALL/EXISTS/COUNT/COLLECT
    /// subquery bodies, brace-enclosed nested procedure specifications
    /// (`parse_braced_single_query`), and WHEN/ELSE bodies
    /// (`parse_when_query` via `parse_single_query`). The braced/WHEN
    /// cycles recurse `parse_single_query` -> `parse_braced_single_query`
    /// -> here without passing through `parse_statement`, `parse_clause`,
    /// or `parse_expression`, so none of the other depth counters engages
    /// on them (LABS-69: ~3000 nested braces on a ~6 KB input used to
    /// SIGABRT on an 8 MiB release stack). Guarding here with the
    /// dedicated `block_depth` counter -- deliberately *not*
    /// `statement_depth`, which doubles as the `is_outer_clause`
    /// label-mixing signal and must not be inflated by a top-level
    /// `{ ... }` body -- bounds every cycle at the same one-increment-per-
    /// nesting-level rate.
    fn parse_statement_block(&mut self) -> ParseResult<Statement> {
        self.with_block_depth(Self::parse_statement_block_at_current_depth)
    }

    fn parse_statement_block_at_current_depth(&mut self) -> ParseResult<Statement> {
        if self.at_next_keyword() {
            return Err(
                self.error("InvalidNextStatement: NEXT must follow a statement, not begin one")
            );
        }
        // Every "full regular query" embedding context (CALL/EXISTS/COUNT/
        // COLLECT bodies, brace-enclosed nested procedure specifications)
        // starts a *fresh* scope for `in_when_branch_body` -- that flag
        // only exists to let a bare (non-brace) WHEN/THEN body's own
        // clause loop stop at a sibling `WHEN`, and must not leak into an
        // unrelated nested body parsed while it happens to be set (e.g. a
        // `CALL { ... }` subquery embedded inside a bare THEN body, whose
        // own bare `MATCH (n) WHEN ...` must still be a hard error per
        // CIP-135, not silently treated as a statement-block boundary).
        let saved_in_when_branch_body = std::mem::take(&mut self.in_when_branch_body);
        // CIP-231: the optional `<definition block>` heads every
        // `<procedure specification>` -- top level and every braced nested
        // body (this is the one choke point all of them funnel through, so
        // definitions parse in exactly the grammar's positions and nowhere
        // else). Parsed before the statement proper; the resolver in the
        // front end rewrites definitions and their call sites away before
        // lowering.
        let definitions = self.parse_local_definitions()?;
        let result = self.parse_statement_block_inner();
        self.in_when_branch_body = saved_in_when_branch_body;
        let mut result = result?;
        // `extend`, not assign: a block that starts with its own DEFINE
        // statements can still parse into a braced inner body carrying
        // definitions of its own (`DEFINE ... { DEFINE ... ... }`); the
        // outer block's own (possibly empty) definitions come first in
        // declaration order, the inner body's after them.
        result.definitions.splice(0..0, definitions);
        Ok(result)
    }

    fn parse_statement_block_inner(&mut self) -> ParseResult<Statement> {
        let mut stmt = self.parse_regular_query()?;
        // `stmt` may already carry its own `.next` chain -- e.g. a
        // brace-enclosed nested procedure specification
        // (`parse_braced_single_query`) whose own content is itself a
        // multi-stage NEXT chain (CIP-213 PE8.1). Find the *true* current
        // tail before appending anything more, so an outer NEXT here
        // extends that chain instead of clobbering it.
        let mut tail = &mut stmt;
        while tail.next.is_some() {
            tail = tail.next.as_mut().unwrap();
        }
        while self.at_next_keyword() {
            self.next_token(); // consume NEXT
            if self.at_next_keyword() {
                return Err(self.error(
                    "InvalidNextStatement: NEXT cannot be immediately followed by another NEXT",
                ));
            }
            if matches!(
                self.peek_type(),
                TokenType::EOF | TokenType::Semicolon | TokenType::RightBrace | TokenType::UNION
            ) {
                return Err(
                    self.error("InvalidNextStatement: NEXT must be followed by a statement")
                );
            }
            let next_stmt = self.parse_regular_query()?;
            tail.next = Some(Box::new(next_stmt));
            tail = tail.next.as_mut().unwrap();
        }
        Ok(stmt)
    }

    /// CIP-146 Semantics Step 3, Case 2.2: rewrites a composite (nested-union)
    /// UNION argument `arm` -- which can only arise from a brace-enclosed
    /// body whose own content was itself a UNION chain -- into
    /// `CALL (*) { arm } RETURN *` (arm ends in a tabular RETURN) or
    /// `CALL (*) { arm } FINISH` (arm's result is omitted, e.g. every leaf
    /// arm is CREATE-only). A linear `arm` (`.union.is_none()`) is returned
    /// unchanged, since splicing it directly into the outer chain is already
    /// observably identical to wrapping it.
    ///
    /// CIP-213: also fires when `arm.next.is_some()` -- a brace-enclosed
    /// body whose own content is a NEXT chain (PE4.5/PE4.6). Without this,
    /// attaching a sibling UNION arm directly onto `arm` would leave `arm`
    /// carrying *both* `.union` (the sibling arm) and `.next` (arm's own
    /// internal continuation) on the same node, and `Engine::
    /// execute_statement_seeded` -- which always resolves `.union` before
    /// `.next` -- would then wrongly feed the *whole merged* union row set
    /// (both arms' rows) into `arm`'s own internal NEXT stage instead of
    /// just its own branch's rows. Wrapping `arm` (chain and all) inside an
    /// opaque `CALL (*) { ... }` first makes it a single self-contained,
    /// `.next`-free node before any sibling UNION arm is ever attached to
    /// it, so the two concepts never collide.
    fn maybe_wrap_composite_union_arm(arm: Statement) -> Statement {
        if arm.union.is_none() && arm.next.is_none() {
            return arm;
        }
        let mut tail = &arm;
        while let Some(next_stmt) = &tail.next {
            tail = next_stmt;
        }
        let tabular = tail.clauses.iter().any(|c| matches!(c, Clause::Return(_)));
        let trailer = if tabular {
            Clause::Return(ReturnClause {
                group_by: None,
                distinct: false,
                items: vec![ReturnItem {
                    expr: Expr::Star,
                    alias: None,
                    all: true,
                    source_text: None,
                }],
                order_by: Vec::new(),
                skip: None,
                limit: None,
            })
        } else {
            Clause::Finish
        };
        Statement {
            clauses: vec![
                Clause::CallSubquery(CallSubqueryClause {
                    statement: arm,
                    scope: CallScope::Wildcard,
                    optional: false,
                    in_transactions: None,
                }),
                trailer,
            ],
            union: None,
            explain: false,
            profile: false,
            next: None,
            cypher_version: None,
            brace_wrapped: false,
            definitions: Vec::new(),
        }
    }

    /// `<SingleQuery> ::= <Statement> | [ USE <graph-expr> ] '{' <RegularQuery> '}'`
    /// (CIP-146). The plain form is unchanged pre-CIP-146 behavior
    /// (delegated to `parse_statement`); the brace form is new. Used
    /// everywhere a `SingleQuery` is expected: the first arm of a
    /// `RegularQuery`, every subsequent UNION/UNION ALL argument, and (since
    /// `CALL`/`CALL (...)` bodies are themselves a `RegularQuery`) the top
    /// level of a CALL subquery body -- exactly the "known to process one
    /// row" contexts the CIP restricts this syntax to. Deliberately *not*
    /// wired into `parse_clause_inner`, so brace syntax stays rejected mid-
    /// query (e.g. after WITH) and as a LOAD CSV argument body.
    fn parse_single_query(&mut self) -> ParseResult<Statement> {
        if self.peek_type() == TokenType::LeftBrace {
            return self.parse_braced_single_query(None);
        }
        if self.peek_type() == TokenType::Identifier
            && self.peek().literal.eq_ignore_ascii_case("use")
        {
            // Could be the new `USE <graph-expr> { ... }` brace-prefixed
            // SingleQuery, or an ordinary leading `USE` clause of a plain
            // statement (pre-CIP-146, e.g. `USE db RETURN 1`) -- parse the
            // USE clause (it already knows how to parse a graph expression)
            // and check what follows before committing.
            let save = self.save_pos();
            let use_clause = self.parse_use()?;
            if self.peek_type() == TokenType::LeftBrace {
                return self.parse_braced_single_query(Some(use_clause.graph_expr));
            }
            self.restore_pos(save);
        }
        self.parse_statement()
    }

    /// Parses the body of a CIP-146 `[ USE <graph-expr> ] '{' <RegularQuery> '}'`
    /// SingleQuery; `use_prefix` is the already-parsed USE graph expression
    /// (if any) -- the leading `{` is the current token. Empty braces are a
    /// syntax error (grammar requires at least one SingleQuery inside).
    /// Nested braces (Semantics Step 1, Simplify) collapse for free via
    /// plain recursion into `parse_regular_query`, which itself dispatches
    /// back through `parse_single_query` for its own arms. A USE prefix is
    /// pushed (Semantics Step 2, Normalize) as the first clause of every arm
    /// of the resulting body -- linear body: just the one statement;
    /// composite body: every arm of its union chain.
    fn parse_braced_single_query(&mut self, use_prefix: Option<Expr>) -> ParseResult<Statement> {
        let brace_tok = self.next_token(); // consume {
        if self.peek_type() == TokenType::RightBrace {
            return Err(self.error_at_token(
                &brace_tok,
                "InvalidInput: empty braces `{ }` are not a valid query body",
            ));
        }
        let mut inner = self.parse_statement_block()?;
        self.expect(TokenType::RightBrace)?;
        inner.brace_wrapped = true;

        if let Some(graph_expr) = use_prefix {
            let mut cur = &mut inner;
            loop {
                cur.clauses.insert(
                    0,
                    Clause::Use(UseClause {
                        graph_expr: graph_expr.clone(),
                    }),
                );
                if cur.union.is_some() {
                    cur = &mut cur.union.as_mut().unwrap().statement;
                } else {
                    break;
                }
            }
        }

        Ok(inner)
    }

    /// CIP-135: `<when query> ::= <when>, {<when>}, [<else>]`, where
    /// `<when> ::= "WHEN", <predicate>, "THEN", <single-query>` and
    /// `<else> ::= "ELSE", <single-query>`. Called from `parse_regular_query`
    /// once it has confirmed the current token is `WHEN`. Each branch body
    /// (and the optional ELSE body) is parsed via `parse_single_query`, so
    /// it may be a top-level-brace `{ ... }` body (the only form every
    /// scenario in this CIP's eval suite actually uses) or a bare
    /// single-/multi-part query per the grammar. Result is wrapped as the
    /// sole clause of a fresh `Statement` -- a `<when query>` never mixes
    /// with other clauses and (being a `<regular query>`-level alternative,
    /// not a `<single query>`) is deliberately not connected to a following
    /// `UNION` here; callers that need brace+UNION composition already get
    /// it for free via `parse_braced_single_query`'s recursive call into
    /// this same function.
    fn parse_when_query(&mut self) -> ParseResult<Statement> {
        let mut branches = Vec::new();
        loop {
            self.expect(TokenType::WHEN)?;
            let predicate = self.parse_expression()?;
            self.expect(TokenType::THEN)?;
            let body = self.parse_when_branch_body()?;
            branches.push(WhenBranch { predicate, body });
            if self.peek_type() != TokenType::WHEN {
                break;
            }
        }
        let else_body = if self.peek_type() == TokenType::ELSE {
            self.next_token();
            Some(self.parse_when_branch_body()?)
        } else {
            None
        };
        Ok(Statement {
            clauses: vec![Clause::Conditional(ConditionalClause {
                branches,
                else_body,
            })],
            ..Statement::default()
        })
    }

    /// Parses one WHEN or ELSE branch body with `in_when_branch_body`
    /// raised, restoring it before any error propagates (same balanced
    /// shape as the `with_*_depth` helpers).
    fn parse_when_branch_body(&mut self) -> ParseResult<Statement> {
        self.in_when_branch_body += 1;
        let body = self.parse_single_query();
        self.in_when_branch_body -= 1;
        body
    }

    fn parse_statement(&mut self) -> ParseResult<Statement> {
        // ADR-0257: pure statement recursion (`CALL { CALL { ... } }`,
        // `EXISTS { EXISTS { ... } }`) must hit the same hard nesting cap as
        // expression recursion instead of overflowing the stack (LABS-69).
        // Routed through the balanced helper so an error mid-body cannot
        // leave `statement_depth` inflated for a speculative re-parse.
        //
        // This counter does NOT engage on every statement-nesting cycle:
        // `parse_braced_single_query` and `parse_when_query` bodies recurse
        // `parse_single_query` -> `parse_braced_single_query` ->
        // `parse_statement_block` -> ... without passing through
        // `parse_statement` (or `parse_clause`/`parse_expression`). Those
        // cycles are now guarded by `block_depth` at `parse_statement_block`
        // -- the choke point every statement-nesting cycle funnels through.
        // The hard cap here is kept anyway, belt-and-braces: on any stack,
        // every active `parse_statement` frame sits inside a distinct
        // enclosing `parse_statement_block` frame (its only caller,
        // `parse_single_query`, is reached only via `parse_regular_query` /
        // `parse_when_query`, both of which run inside a block), so
        // `block_depth >= statement_depth` always holds and `block_depth`
        // provably trips at the same-or-earlier nesting depth on every
        // cycle this cap catches -- but keeping it costs nothing and
        // protects against a future caller path that bypasses
        // `parse_statement_block`. The counter itself is load-bearing
        // regardless of the cap: it is the `is_outer_clause` label-mixing
        // scoping signal (see `parse_clause`).
        self.with_statement_depth(Self::parse_statement_at_current_depth)
    }

    fn parse_statement_at_current_depth(&mut self) -> ParseResult<Statement> {
        let mut stmt = Statement {
            clauses: Vec::new(),
            ..Statement::default()
        };

        // See `parse_clause`'s doc comment for how label-expression mixing
        // is scoped relative to this statement-nesting depth.
        loop {
            match self.peek_type() {
                TokenType::EOF
                | TokenType::Semicolon
                | TokenType::UNION
                | TokenType::RightBrace
                // CIP-135: a bare (non-brace) WHEN/THEN body -- e.g.
                // `WHEN a = 1 THEN RETURN 2 AS a ELSE RETURN 3 AS a` --
                // must stop before the following `ELSE`, the same way a
                // brace already stops it, rather than trying to parse
                // `ELSE` as the start of a new clause. `WHEN` is
                // deliberately *not* added here (see `parse_regular_query`'s
                // doc comment): a bare `WHEN` appearing after an ordinary
                // clause must remain a hard parse error, not silently
                // become a new statement-block boundary.
                | TokenType::ELSE => {
                    if self.peek_type() == TokenType::Semicolon {
                        self.next_token();
                    }
                    break;
                }
                TokenType::WHEN if self.in_when_branch_body > 0 => break,
                // CIP-213: `NEXT` is a statement-block boundary, not a
                // clause -- stop the clause loop here so the enclosing
                // `parse_statement_block` can consume it. No real clause
                // ever starts with the bare word "next", so this can never
                // misfire mid-clause (see `at_next_keyword`'s doc comment).
                _ if self.at_next_keyword() => break,
                _ => {
                    let clause = self.parse_clause()?;
                    stmt.clauses.push(clause);
                }
            }
        }

        self.check_label_set_mixing(&stmt.clauses)?;
        // CIP-123/CIP-242/CIP-253: composed SHOW/TERMINATE clauses (multiple
        // SHOW/TERMINATE commands chained together, or a single SHOW/
        // TERMINATE composed with other Cypher clauses) are now genuinely
        // supported -- each acts as a nested-loop cross join against the
        // rows produced so far, exactly like `SHOW FUNCTIONS`/`SHOW
        // PROCEDURES` already did before this feature landed (see
        // `crate::clause::show`). The structural rules that govern
        // composition (mandatory non-star YIELD, mandatory trailing RETURN,
        // no re-declaring an in-scope variable) are checked at the semantic
        // (post-parse) validation stage instead of here, in
        // `relate_front::validate::validate_show_composability` --
        // this parser only builds the AST.

        Ok(stmt)
    }

    /// CIP-126 Examples 5/6: within a single SET clause (or a single REMOVE
    /// clause), `IS` and a colon-conjunction (`:A:B`, i.e. more than one
    /// label in one item) cannot both be used across that one clause's
    /// label-set items -- single-label items of either syntax may freely
    /// coexist. Checked independently per clause (CIP-126
    /// `05-invalid-mixing.feature` never combines items across two
    /// different clauses; CIP-40 `dml-label-set-mixing.feature` scenario 14
    /// is `@neo4j-verified` legal for exactly that cross-clause shape: a
    /// SET clause using `IS` and a *separate* REMOVE clause using colon
    /// conjunction, both in the same statement).
    fn check_label_set_mixing(&self, clauses: &[Clause]) -> ParseResult<()> {
        for clause in clauses {
            let items: Vec<(bool, usize)> = match clause {
                Clause::Set(sc) => sc
                    .items
                    .iter()
                    .filter(|i| !i.labels.is_empty())
                    .map(|i| (i.is_keyword, i.labels.len()))
                    .collect(),
                Clause::Remove(rc) => rc
                    .items
                    .iter()
                    .filter(|i| !i.labels.is_empty())
                    .map(|i| (i.is_keyword, i.labels.len()))
                    .collect(),
                _ => continue,
            };
            let mut saw_is = false;
            let mut saw_colon_conjunction = false;
            for (is_keyword, len) in items {
                if is_keyword {
                    saw_is = true;
                } else if len > 1 {
                    saw_colon_conjunction = true;
                }
            }
            if saw_is && saw_colon_conjunction {
                return Err(self.error(
                    "MixedIsAndColonConjunction: cannot mix IS and colon-conjunction across SET/REMOVE label items in the same clause",
                ));
            }
        }
        Ok(())
    }
}

mod case;
mod clauses;
mod definitions;
mod expressions;
mod labels;
mod literals;
mod names;
mod patterns;

pub use patterns::selector_is_selective;
mod schema;
mod search;
mod show;
mod types;

#[cfg(test)]
mod tests;

/// True if any element of a quantified path pattern's operand is itself
/// quantified -- directly (`PatternElement::Quantified`) or via a
/// variable-length/quantified relationship (`RelPattern` with hop bounds,
/// whether legacy `*min..max` or the CIP-28 postfix form) -- which CIP-28
/// Rule 4 forbids (no nested quantification).
fn contains_quantification(elements: &[PatternElement]) -> bool {
    elements.iter().any(|e| match e {
        PatternElement::Quantified(_) => true,
        PatternElement::Relationship(rp) => rp.is_variable_length(),
        PatternElement::Node(_) => false,
    })
}

/// True if `elements` contains a *GPM-syntax* quantification -- a
/// quantified path pattern (`((...))+`) or a CIP-28 Form-5 postfix-quantified
/// relationship (`-->+`, `-[]->{1,5}`) -- but *not* the legacy bracket
/// variable-length form (`-[*1..5]->`), which remains legal inside
/// `shortestPath(...)`/`allShortestPaths(...)` (CIP-40 path-selector-mixing
/// scenario 7). Used to reject `QuantifiedPathPatternInShortestPath`
/// (scenario 6).
fn contains_gpm_quantification(elements: &[PatternElement]) -> bool {
    elements.iter().any(|e| match e {
        PatternElement::Quantified(_) => true,
        PatternElement::Relationship(rp) => rp.quantified,
        PatternElement::Node(_) => false,
    })
}

fn comparison_op(ty: TokenType) -> Option<&'static str> {
    match ty {
        TokenType::Equals => Some(EQUALS),
        TokenType::NotEquals => Some("<>"),
        TokenType::LessThan => Some("<"),
        TokenType::GreaterThan => Some(">"),
        TokenType::LessEqual => Some("<="),
        TokenType::GreaterEqual => Some(">="),
        TokenType::RegexMatch => Some("=~"),
        _ => None,
    }
}

/// The AST's one spelling table for operator text (ADR-0505 §2): the `op`
/// fields of `BinaryExpr`, `UnaryExpr` and `StringMatchExpr` are
/// `&'static str` drawn from these constants, so every producer consults
/// the table and no spelling is re-spelled at a build site. The comparison
/// family's spellings live in the `comparison_op` token-to-spelling table
/// above — its own one table, consumed by every comparison build site —
/// with one exception: the equality spelling is a named constant below,
/// because simple-CASE's plain-value default (`parse_when_operand_element`)
/// builds the same comparison without a token to read the table from.
const EQUALS: &str = "=";
const OR: &str = "OR";
const XOR: &str = "XOR";
const AND: &str = "AND";
const NOT: &str = "NOT";
/// `+`, shared by the unary and binary producers.
const PLUS: &str = "+";
/// `-`, shared by the unary and binary producers.
const MINUS: &str = "-";
const CONCAT: &str = "||";
const MUL: &str = "*";
const DIV: &str = "/";
const MOD: &str = "%";
const POW: &str = "^";
const STARTS_WITH: &str = "STARTS WITH";
const ENDS_WITH: &str = "ENDS WITH";
const CONTAINS: &str = "CONTAINS";

// Test-only probe for the CIP-272 map-comprehension lookahead: counts the
// speculative full parses entered for a `{` in expression position. A brace
// with no top-level `|` must never reach that parse (the exponential
// parse-time regression), so the regression test asserts on this count rather
// than on wall-clock time. Thread-local: the test harness runs every test on
// its own thread.
#[cfg(test)]
std::thread_local! {
    static SPECULATIVE_COMPREHENSION_PARSES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}
