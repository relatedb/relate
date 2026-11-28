//! The statement normalizer (ADR-0503): one pass over the lexer's own
//! token stream that derives a plan-cache template key, the rewritten
//! statement text, and the extracted literal values — the enabler for
//! template-keyed plan caching on workloads that emit literals instead of
//! parameters.
//!
//! The three outputs of one walk, so they cannot drift:
//!
//! - **template** — the canonical key. Value-bearing literal tokens
//!   (`Integer`, `Float`, `String`) become typed placeholders
//!   `$__AUTO_INT_0__`/`$__AUTO_FLOAT_1__`/`$__AUTO_STR_2__` (the type is
//!   part of the key, so `42` and `'42'` never share a template); a
//!   bracket group in **list-literal position** whose elements are all
//!   such literals (recursively) collapses to one `$__AUTO_LIST_n__`
//!   placeholder; identifiers render through the
//!   canonical quoting policy ([`quote_identifier`]), parameters through
//!   [`parameter_reference`] (the shaped `$name` render — a
//!   non-identifier-shaped name renders backtick-quoted, so two different
//!   names never share a template and the two spellings of one name
//!   share one), and every other token — keywords included — as written.
//!   Two statements share a template only if they parse identically.
//!   Keyword case is deliberately NOT folded: the dialect keeps keyword
//!   spellings as case-sensitive names (`(o:Order)`, `-[:ORDER]->`,
//!   `n.match`, `{match: 1}`), and the token stream cannot prove a
//!   keyword sits in a clause position rather than a name position — so
//!   `match …` and `MATCH …` keep distinct templates (a miss, never a
//!   wrong plan).
//! - a bracket in **postfix position** — a subscript or slice on the
//!   expression before it — and a bracket after a pattern head or a
//!   subtraction operator (`-[]->`, `1 - [1, 2]`) both keep their
//!   brackets: [`bracket_opens_list_group`] in the bracket-role module
//!   is the one classification, and [`Pass::collapses_as_list_group`]
//!   the one composition of the bracket facts. The postfix bracket's interior
//!   literals normalize individually: the placeholder stands for the
//!   literal's VALUE, not for the brackets a postfix position needs
//!   (`labels(n)[0]` rewrites to `labels(n)[$__AUTO_INT_0__]`, never to a
//!   bare list parameter glued to the expression).
//! - **rewritten** — the source with each normalized literal's byte span
//!   replaced by its placeholder: a real Cypher statement whose literals
//!   are parameters. Parsing it (with the extracted values bound as
//!   `$__AUTO_*__` parameters, through the ordinary front-end entry) is
//!   what a cache entry is built from, so any literal position whose
//!   parameter form is ungrammatical fails that parse and the statement
//!   simply stays uncached — an over-eager normalization costs a miss,
//!   never a wrong plan.
//! - **values** — the placeholder/value pairs in token order.
//!
//! Grammar guards skip normalization where a parameter is not grammatical
//! or would change meaning (each costs only a cache miss): a `String`
//! followed by `:` is a map-literal key; a `String` preceded by the
//! contextual `FIELDTERMINATOR` identifier is a LOAD CSV field terminator;
//! an `Integer`/`Float` preceded by `*` or `..` is a var-length quantifier
//! bound; a `String` that is a function call's first argument feeds the
//! front-end's constant folding (a temporal constructor's literal folds
//! to a constant at lowering — a parameter there stays a per-row call).
//! `CYPHER <version>` integers need no guard — the rewritten parse fails
//! closed.
//!
//! Deliberately NOT done (the 2026-10-31 session's constraints, §7): no
//! variable renaming (variable names are structural), no size threshold,
//! no folding of non-clause keywords (`n.true` and `n.TRUE` are different
//! property keys).

use std::cell::Cell;

use crate::auto_parameter::name_is_auto_parameter;
use crate::bracket_role::bracket_opens_list_group;
use crate::lexer::{tokenize_with_options, ParseOptions};
use crate::parser::{parse_int_literal, MAX_NESTING_DEPTH, MAX_QUERY_BYTES};

use crate::token::{
    is_field_terminator_word, line_starts_of, quote_identifier, token_start_byte, Token, TokenType,
};
use relate_model::identifier::parameter_reference;

/// One extracted literal value: the normalizer's own small vocabulary (the
/// parser crate does not depend on the engine's value type). The facade
/// converts these into parameter values at the one boundary where both
/// exist.
#[derive(Debug, Clone, PartialEq)]
pub enum NormalizedValue {
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<NormalizedValue>),
}

/// What [`normalize_statement`] produced.
#[derive(Debug, Clone, PartialEq)]
pub enum NormalizeOutcome {
    /// The template key, the rewritten statement, and the values, all of
    /// one walk.
    Normalized(Normalized),
    /// The statement must not participate in the plan cache. Oversized
    /// input, a lexer error token, an unparseable literal, a `$__AUTO_*`
    /// name in the stream, or a list scan past its total step budget (an
    /// adversarially nested payload — the parser's own guards are then
    /// the statement's verdict). Never an error: the statement itself
    /// parses and runs exactly as before.
    NotCacheable,
}

/// The normalizer's outputs. See the [module documentation](self).
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    /// The canonical template: two statements share it only if they parse
    /// identically.
    pub template: String,
    /// The statement with normalized literals as `$__AUTO_*__`
    /// parameters.
    pub rewritten: String,
    /// The `(placeholder, value)` pairs, in token order.
    pub values: Vec<(String, NormalizedValue)>,
    /// The original source spelling of each [`Self::values`] entry, in the
    /// same order: the text the result column keeps (`RETURN 1` stays `1`).
    pub spellings: Vec<String>,
}

/// Normalize `src` under the same [`ParseOptions`] the real parse would
/// use (the preview gates change what the lexer produces, so the template
/// must be derived from the identical token stream).
pub fn normalize_statement(src: &str, options: ParseOptions) -> NormalizeOutcome {
    // The same cap the parse enforces (ADR-0257), checked for the same
    // reason: no adversarial amplification into unbounded token
    // allocation. An oversized statement fails the parse anyway.
    if src.len() > MAX_QUERY_BYTES {
        return NormalizeOutcome::NotCacheable;
    }
    let tokens = tokenize_with_options(src, options);
    let line_starts = line_starts_of(src);
    let scan_budget = tokens.len() as u64 * 2 + SCAN_STEP_HEADROOM;
    let mut pass = Pass {
        src,
        tokens: &tokens,
        line_starts: &line_starts,
        template: String::new(),
        spans: Vec::new(),
        values: Vec::new(),
        spellings: Vec::new(),
        next_index: 0,
        call_argument: call_argument_context(&tokens),
        scan_steps: Cell::new(0),
        scan_budget,
    };
    match pass.run() {
        Some(normalized) => NormalizeOutcome::Normalized(normalized),
        None => NormalizeOutcome::NotCacheable,
    }
}

/// The list scan's step-budget headroom over `2 × token count` — slack
/// for the empty-group and single-element shapes whose scans touch a few
/// tokens more than once.
const SCAN_STEP_HEADROOM: u64 = 64;

/// The one-pass state. `Option` returns are the not-cacheable bailouts,
/// never errors: every guard's failure mode is a statement that stays
/// uncached. The two lifetimes are the source's and the tokens' own
/// (ADR-0505 borrowed them from it).
struct Pass<'a, 't> {
    src: &'a str,
    tokens: &'t [Token<'a>],
    line_starts: &'a [usize],
    /// The template under construction: the rendered tokens joined with
    /// single spaces.
    template: String,
    /// The rewritten text's replacements, in source order:
    /// `(byte start, byte end, placeholder)` — the span runs from the
    /// normalized token's (or group's) first byte to the next token's
    /// first byte, so the inter-token whitespace rides out with the
    /// literal.
    spans: Vec<(usize, usize, String)>,
    /// The extracted values in placeholder order.
    values: Vec<(String, NormalizedValue)>,
    /// Original source spellings, parallel to `values`.
    spellings: Vec<String>,
    /// The one counter every placeholder name derives from.
    next_index: usize,
    /// Whether each token sits inside a function call's argument list —
    /// the innermost unmatched opener enclosing it is a `(` whose callee
    /// identifier stands before it (a map or list nested in the argument
    /// list is still argument territory). One linear precompute pass
    /// (before the run loop), so the literal guards are a lookup, never a
    /// per-literal rescan of the token prefix.
    call_argument: Vec<bool>,
    /// The list scan's consumed steps against `scan_budget` (see the
    /// scan's doc) — a `Cell` so the scan stays `&self`, matching the
    /// run loop's borrows.
    scan_steps: Cell<u64>,
    /// The total step budget: a healthy statement's scans visit each
    /// token a bounded number of times, so `2 × tokens + headroom` is
    /// never reached; the pathological re-descending shape trips it
    /// early.
    scan_budget: u64,
}

impl<'a, 't> Pass<'a, 't> {
    /// Whether the list scan's step budget tripped — derived from the
    /// counter and the budget, the same comparison the scan itself
    /// declines on, never a stored copy of it: the whole statement
    /// declines to not-cacheable (the parser's own guards are then its
    /// verdict).
    fn scan_exhausted(&self) -> bool {
        self.scan_steps.get() > self.scan_budget
    }

    fn run(&mut self) -> Option<Normalized> {
        let mut i = 0;
        while i < self.tokens.len() {
            if self.scan_exhausted() {
                return None;
            }
            let tok = &self.tokens[i];
            match tok.ty {
                // A lexer error token fails the real parse; there is
                // nothing to cache.
                TokenType::Error => return None,
                TokenType::Parameter => {
                    if name_is_auto_parameter(&tok.literal) {
                        return None;
                    }
                    if tok.is_dynamic_opener_sentinel() {
                        // The empty literal is the `$(expr)` encoding,
                        // never a name — `parameter_reference` cannot
                        // render it, so the template declines. (The
                        // dynamic-operand uncacheability rule is a
                        // separate fact, enforced where it lives,
                        // `cache_insert_eligible`.) The sentinel's
                        // encoding is the token layer's one fact
                        // (`Token::is_dynamic_opener_sentinel`), never
                        // re-tested as a raw literal here.
                        return None;
                    }
                    self.push_rendered(&parameter_reference(&tok.literal));
                }
                // A CIP-261 interpolated string embeds expressions, and an
                // embedded parameter reference is invisible to the
                // reserved-name check above — the payload is one token.
                // The whole statement bails: an embedded `$__AUTO_*__`
                // could otherwise collide with the rewritten text's own
                // placeholders (found in review), and the payload's
                // embedded literals are as unnormalizable as the
                // interpolation itself.
                TokenType::InterpString => return None,
                TokenType::Identifier => {
                    self.push_rendered(&quote_identifier(&tok.literal));
                }
                TokenType::Integer | TokenType::Float
                    if !self.after_quantifier_bound(i) && !self.call_argument[i] =>
                {
                    let value = literal_value(tok)?;
                    self.replace_token(i, value)?;
                }
                TokenType::String => {
                    if self.is_map_key(i) || self.is_field_terminator(i) || self.call_argument[i] {
                        // A guarded string (map key, LOAD CSV field
                        // terminator, or a function-call argument) stays
                        // in the template, quoted — the quotes keep it
                        // from aliasing an identifier's render, and the
                        // escaping keeps the render injective.
                        self.push_rendered(&format!("'{}'", escape_single_quoted(&tok.literal)));
                    } else {
                        self.replace_token(i, literal_value(tok)?)?;
                    }
                }
                TokenType::LeftBracket if self.collapses_as_list_group(i) => {
                    if let Some((close, elements)) = self.scan_literal_group(i + 1) {
                        // The whole bracket group is one parameter whose
                        // value is the list (lists-as-one-param, the
                        // `$__AUTO_INT_3__` positional-typed-key precedent).
                        self.replace_group(i, close, NormalizedValue::List(elements))?;
                        i = close;
                    } else if self.scan_exhausted() {
                        // The scan's step budget tripped: the statement
                        // declines whole, not bracket by bracket.
                        return None;
                    } else {
                        self.push_rendered(&tok.literal);
                    }
                }
                // Every remaining token renders as written — keywords
                // included. Case folding a keyword token is unsound at
                // token level: the dialect keeps keyword spellings as
                // case-sensitive NAMES (`(o:Order)` and `-[:ORDER]->`
                // parse with the name as written, `n.match` and
                // `{match: 1}` bind by spelling), and the token stream
                // cannot prove a keyword sits in a clause position
                // rather than a name position — so `match …` and
                // `MATCH …` keep distinct templates, the safe direction.
                _ => {
                    self.push_rendered(&tok.literal);
                }
            }
            i += 1;
        }
        Some(Normalized {
            template: std::mem::take(&mut self.template),
            rewritten: self.splice(),
            values: std::mem::take(&mut self.values),
            spellings: std::mem::take(&mut self.spellings),
        })
    }

    /// Append one rendered token to the template, space-separated.
    fn push_rendered(&mut self, text: &str) {
        if !self.template.is_empty() {
            self.template.push(' ');
        }
        self.template.push_str(text);
    }

    /// The byte offset of token `i`'s first byte.
    fn start_byte(&self, i: usize) -> Option<usize> {
        token_start_byte(&self.tokens[i], self.line_starts, self.src.len())
    }

    /// The byte offset the token after `i` starts at — the end of token
    /// `i`'s replacement span (the inter-token whitespace rides out with
    /// the literal); the source end for the final token.
    fn end_byte(&self, i: usize) -> Option<usize> {
        match self.tokens.get(i + 1) {
            Some(next) => token_start_byte(next, self.line_starts, self.src.len()),
            None => Some(self.src.len()),
        }
    }

    /// Whether token `i` sits directly after `*` or `..`: the var-length
    /// quantifier bounds (`[:TYPE*1..3]`), which take literal integers
    /// only. The same shape after an arithmetic `*` (`2 * 3`) is a false
    /// positive that costs a miss, never a wrong key.
    fn after_quantifier_bound(&self, i: usize) -> bool {
        i > 0 && matches!(self.tokens[i - 1].ty, TokenType::Star | TokenType::DotDot)
    }

    /// Whether the `[` at token `i` **collapses as a list group** — the
    /// decision the run loop's collapse arm guards on, composed of two
    /// facts, each by its own home:
    ///
    /// - the bracket is not inside a function-call argument list (the
    ///   precomputed context above — its literals feed constant folding,
    ///   ADR-0503's participation rule; the bracket may still be a list
    ///   literal there, it just never collapses);
    /// - the prefix before the bracket opens a collapsible group
    ///   ([`bracket_opens_list_group`], the one classification): a
    ///   **postfix** bracket — a subscript or slice on the expression
    ///   before it — and a bracket after a pattern head or a subtraction
    ///   operator (`-[]->`, `1 - [1, 2]`) keep their brackets, because
    ///   the collapse's placeholder stands for a literal's VALUE, not for
    ///   the brackets those positions need; both render as written
    ///   instead, their interior literals taking their own arms
    ///   (`labels(n)[$__AUTO_INT_0__]`).
    fn collapses_as_list_group(&self, i: usize) -> bool {
        debug_assert!(self.tokens[i].ty == TokenType::LeftBracket);
        !self.call_argument[i] && bracket_opens_list_group(&self.tokens[..i])
    }

    /// Whether token `i` is a `String` followed by `:` — a map-literal
    /// key (`{'name': 1}`). Parameters are not map keys, and the key is
    /// structural: it stays in the template.
    fn is_map_key(&self, i: usize) -> bool {
        self.tokens
            .get(i + 1)
            .is_some_and(|next| next.ty == TokenType::Colon)
    }

    /// Whether token `i` is the `String` after the contextual
    /// `FIELDTERMINATOR` identifier of LOAD CSV, which requires a literal
    /// there — decided through the crate's one spelling of that word.
    fn is_field_terminator(&self, i: usize) -> bool {
        i > 0
            && self.tokens[i - 1].ty == TokenType::Identifier
            && is_field_terminator_word(&self.tokens[i - 1].literal)
    }

    /// Replace token `i` with a fresh typed placeholder: the whole-group
    /// emitter over a one-token span. `None` only when the token's byte
    /// offset cannot derive — a real token stream from this source always
    /// carries real positions.
    fn replace_token(&mut self, i: usize, value: NormalizedValue) -> Option<()> {
        self.replace_group(i, i, value)
    }

    /// Replace the token span `open..=close` (inclusive) with one
    /// placeholder — the one emitter of the template text, the rewritten
    /// span, and the value, from the one counter, so the three outputs
    /// cannot drift. `None` only when a span endpoint's byte offset
    /// cannot derive.
    fn replace_group(&mut self, open: usize, close: usize, value: NormalizedValue) -> Option<()> {
        let name = self.fresh_name(&value);
        // One placeholder value feeds both outputs, so the template's
        // rendering and the rewritten span are the same text by
        // construction — the one parameter render, shaped names included
        // (fresh names are always plain, but the spelling stays canonical).
        let placeholder = parameter_reference(&name);
        self.push_rendered(&placeholder);
        let start = self.start_byte(open)?;
        // The span ends at the literal's own last byte, NOT the next
        // token's first: the inter-token separator stays outside the
        // span, so the splice's gap copy reproduces it verbatim (ADR-0718
        // — the substituted name keeps the source's spacing) and the
        // spelling below is the span as stored, no trim.
        let raw_end = self.end_byte(close)?;
        let literal_end = start + self.src[start..raw_end].trim_end().len();
        self.spans.push((start, literal_end, placeholder));
        self.values.push((name, value));
        self.spellings
            .push(self.src[start..literal_end].to_string());
        Some(())
    }

    fn fresh_name(&mut self, value: &NormalizedValue) -> String {
        // The type tag derives from the value it names — the two cannot
        // disagree — and the name grammar itself has one writer,
        // `crate::auto_parameter::auto_parameter_name`, the same home the
        // substitution's reader parses back.
        let kind = match value {
            NormalizedValue::Int(_) => "INT",
            NormalizedValue::Float(_) => "FLOAT",
            NormalizedValue::Str(_) => "STR",
            NormalizedValue::List(_) => "LIST",
        };
        let name = crate::auto_parameter::auto_parameter_name(kind, self.next_index);
        self.next_index += 1;
        name
    }

    /// Scan a bracket group's interior starting at `j` (just past the
    /// `[`): `Some((index of the closing `]`, element values))` when every
    /// element is a normalizable literal or such a group, comma-separated
    /// (an empty group scans clean); `None` when anything else appears —
    /// the caller then renders the `[` and lets the element tokens take
    /// their own arms. The recursion is depth-capped at
    /// [`MAX_NESTING_DEPTH`], the parser's own ADR-0257 limit: this scan
    /// runs BEFORE the parse, so an adversarially nested payload must not
    /// recurse past what the parser would reject anyway — a group past the
    /// cap declines (no collapse), and the parse's depth guard is the
    /// statement's verdict.
    fn scan_literal_group(&self, j: usize) -> Option<(usize, Vec<NormalizedValue>)> {
        self.scan_literal_group_at_depth(j, 0)
    }

    /// [`Pass::scan_literal_group`] with the recursion depth counted and
    /// the total step budget charged: every visited token costs one step,
    /// and a budget exhausted mid-scan flags the whole statement
    /// not-cacheable — without it, a declined deep group re-descends from
    /// the very next bracket (each restart at depth 0), turning an
    /// adversarial payload into quadratic token visits.
    fn scan_literal_group_at_depth(
        &self,
        mut j: usize,
        depth: u32,
    ) -> Option<(usize, Vec<NormalizedValue>)> {
        if depth >= MAX_NESTING_DEPTH {
            // Past the parser's depth cap the whole statement declines.
            // A bare `None` would restart a depth-0 scan at the next `[`.
            self.scan_steps.set(self.scan_budget.saturating_add(1));
            return None;
        }
        let mut elements = Vec::new();
        loop {
            let steps = self.scan_steps.get() + 1;
            self.scan_steps.set(steps);
            if self.scan_exhausted() {
                return None;
            }
            let tok = self.tokens.get(j)?;
            let element = match tok.ty {
                TokenType::LeftBracket => {
                    let (close, inner) = self.scan_literal_group_at_depth(j + 1, depth + 1)?;
                    j = close;
                    NormalizedValue::List(inner)
                }
                TokenType::RightBracket if elements.is_empty() => return Some((j, elements)),
                // `literal_value` is the scan's only token vocabulary: a
                // non-literal is `None`, which declines the group.
                _ => literal_value(tok)?,
            };
            elements.push(element);
            j += 1;
            match self.tokens.get(j)?.ty {
                TokenType::Comma => j += 1,
                TokenType::RightBracket => return Some((j, elements)),
                _ => return None,
            }
        }
    }

    /// Whether `ch` continues a parameter name after a replaced literal —
    /// the glue rule's one predicate, the crate's own identifier-part
    /// test (ADR-0182's Unicode 13 tables) plus the two sigils, so the
    /// splice's separator decision cannot drift from the lexer's name
    /// grammar: a non-ASCII identifier-part byte directly after a
    /// placeholder must get the separator as any ASCII one does, or the
    /// rewritten text would lex the two as one longer name (the
    /// parameter then is not carried, and the plan's column keeps the
    /// wrong text).
    fn continues_parameter_name(ch: char) -> bool {
        ch == '$' || ch == '_' || crate::unicode_identifier::is_identifier_part(ch)
    }

    /// Build the rewritten statement: the source with every replacement
    /// span spliced out for its placeholder. The span swallows the
    /// inter-token whitespace with the literal, so a separator is put back
    /// only when the next character would glue onto the placeholder
    /// (`$__AUTO_INT_0__LIMIT` would lex as one name). An operator stays
    /// where the source put it (`1+2`), which is what a projection's
    /// column name round-trips from. The spans are in source order by
    /// construction (the walk appends them in token order and
    /// `replace_group` covers its whole interior).
    fn splice(&self) -> String {
        let mut out = String::with_capacity(self.src.len());
        let mut cursor = 0;
        for (start, end, placeholder) in &self.spans {
            if *start > cursor {
                out.push_str(&self.src[cursor..*start]);
            }
            out.push_str(placeholder);
            // The span ends at the literal's own last byte (the separator
            // whitespace stays outside it), so the gap copies above and
            // below reproduce the source's spacing verbatim — the
            // rewritten text is what the cached plan's column source
            // derives from, and the authored name comes back by
            // substituting the placeholder with the stored spelling
            // (ADR-0718: `1 + 2` stays `1 + 2`, `1  +  2` stays
            // `1  +  2`). Only the adjacency the span end cannot leave
            // outside — a literal directly followed by a name-continuing
            // byte, where `$__AUTO_…` and that byte would lex as ONE
            // parameter name — gets a separator the source never had: the
            // one lexing-forced divergence. `1+2` keeps its `+` with
            // nothing added.
            let glue = self.src[*end..]
                .chars()
                .next()
                .is_some_and(Self::continues_parameter_name);
            if glue {
                out.push(' ');
            }
            cursor = *end;
        }
        out.push_str(&self.src[cursor..]);
        out
    }
}

/// The per-token function-call-argument context, in one linear pass: a
/// token is argument territory when the innermost unmatched **paren**
/// enclosing it is a call's (its callee identifier standing before it).
/// A paren-only stack answers that in O(1) per token — brackets and
/// braces are transparent (a map or list nested in an argument list is
/// still argument territory, its literals feeding the same constant
/// fold; one outside any call sees an empty or non-call paren stack) —
/// and closers of a different kind never pop a paren, so the stack stays
/// exact for parseable input. The precompute replaces a per-literal
/// rescan of the token prefix (quadratic on the pre-parse lookup path,
/// found in review) and itself never rescans the stack.
fn call_argument_context(tokens: &[Token<'_>]) -> Vec<bool> {
    #[derive(Clone, Copy, PartialEq)]
    enum Paren {
        Call,
        Other,
    }
    let mut parens: Vec<Paren> = Vec::new();
    let mut context = Vec::with_capacity(tokens.len());
    for (i, tok) in tokens.iter().enumerate() {
        context.push(parens.last() == Some(&Paren::Call));
        match tok.ty {
            TokenType::LeftParen => {
                let is_call = i > 0 && tokens[i - 1].ty == TokenType::Identifier;
                parens.push(if is_call { Paren::Call } else { Paren::Other });
            }
            TokenType::RightParen => {
                parens.pop();
            }
            // Brackets and braces: transparent — no push, no pop, so a
            // paren opened inside them still matches its own closer and
            // the territory answer passes through unchanged.
            _ => {}
        }
    }
    context
}

/// The literal value one literal token carries — the one token-to-value
/// mapping both the run loop's literal arms and the list scan read, with
/// the same bail policy (`None`, nothing to cache) for anything that does
/// not decode.
fn literal_value(tok: &Token) -> Option<NormalizedValue> {
    match tok.ty {
        TokenType::Integer => Some(NormalizedValue::Int(parse_int_literal(&tok.literal).ok()?)),
        TokenType::Float => Some(NormalizedValue::Float(parse_finite_float(&tok.literal)?)),
        TokenType::String => Some(NormalizedValue::Str(tok.literal.to_string())),
        _ => None,
    }
}

/// Parse a float literal the way the parser's own expression arm does
/// (CIP-52): a numeric literal that overflows to ±infinity is a
/// compile-time error, so the statement never parses — `None`, nothing to
/// cache.
fn parse_finite_float(literal: &str) -> Option<f64> {
    let value: f64 = literal.parse().ok()?;
    value.is_finite().then_some(value)
}

/// Escape a skipped string's value for its single-quoted template render:
/// backslash and quote doubled, everything else as-is. The render is
/// injective on the value (a key string is data, not a spelling), which
/// is all the template needs from it. The parser crate does not depend
/// on `relate-core` (which spells this same two-character policy as
/// its `escape_cypher_string`), so the policy is owned here, at this
/// boundary, rather than imported.
fn escape_single_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '\'' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
