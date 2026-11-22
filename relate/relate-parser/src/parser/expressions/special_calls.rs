//! The GQL special-form calls: `normalize`, `trim`, `property_exists` and the vector spellings.

use super::*;

/// The `trim(<spec> ...)` direction word of the keyword spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::parser) enum TrimSpec {
    Both,
    Leading,
    Trailing,
}

impl TrimSpec {
    /// The direction a `trim(...)` word spells, ASCII case-insensitively.
    fn from_ident(word: &str) -> Option<TrimSpec> {
        [
            ("BOTH", TrimSpec::Both),
            ("LEADING", TrimSpec::Leading),
            ("TRAILING", TrimSpec::Trailing),
        ]
        .into_iter()
        .find_map(|(spelling, spec)| word.eq_ignore_ascii_case(spelling).then_some(spec))
    }
}

impl<'a> Parser<'a> {
    /// The comma spelling's lowered form: the specification is an ordinary
    /// expression, so the direction cannot be baked into the dispatch name
    /// at parse time — `trim_spec` carries it as the first argument and
    /// selects the direction (and the strict single-character rule of the
    /// direction-named entries) during evaluation.
    pub(in crate::parser) fn build_trim_spec_call(args: Vec<Expr>) -> Expr {
        Expr::call("trim_spec", args)
    }

    pub(in crate::parser) fn build_trim_call(spec: TrimSpec, args: Vec<Expr>) -> Expr {
        let name = match spec {
            TrimSpec::Leading => "trim_leading",
            TrimSpec::Trailing => "trim_trailing",
            TrimSpec::Both => "trim",
        };
        Expr::call(name, args)
    }

    /// Parses `normalize(input)` or `normalize(input, normalForm)`. The
    /// second argument (if present) must be a bare normal-form keyword
    /// (NFC/NFD/NFKC/NFKD) — a quoted string, parameter, or any other
    /// expression there is a compile-time error (CIP-132), so this bypasses
    /// the generic argument-list parser.
    pub(in crate::parser) fn parse_normalize_call(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume (
        if self.peek_type() == TokenType::RightParen {
            return Err(self.error("normalize() requires at least 1 argument"));
        }
        let input = self.parse_expression()?;
        let mut args = vec![input];
        if self.peek_type() == TokenType::Comma {
            self.next_token();
            let form = self.try_consume_normal_form_keyword().ok_or_else(|| {
                let tok = self.peek().clone();
                self.error_at_token(
                    &tok,
                    format!(
                        "invalid normal form: expected NFC, NFD, NFKC, or NFKD, got {:?}",
                        tok.literal
                    ),
                )
            })?;
            args.push(Expr::Literal(LiteralValue::String(form)));
            if self.peek_type() == TokenType::Comma {
                return Err(self.error("normalize() accepts at most 2 arguments"));
            }
        }
        self.expect(TokenType::RightParen)?;
        Ok(Expr::call("normalize", args))
    }

    /// Parses the GQL TRIM special form (CIP-149):
    /// `trim([[LEADING|TRAILING|BOTH] [trimCharacterString] FROM] input)`,
    /// plus its comma spelling
    /// `trim(trimSpecification [, trimCharacterString], input)`, whose
    /// specification is an ordinary expression and so selects the direction
    /// at run time (the internal `trim_spec` call).
    /// `LEADING`/`TRAILING`/`BOTH` are not reserved keywords elsewhere, so
    /// they lex as plain identifiers and are recognized here by their
    /// literal spelling. The trim specification and character string
    /// selects which of the three underlying dispatch names ("trim" for
    /// BOTH/default, "trim_leading", "trim_trailing") the call compiles to;
    /// an omitted trim character string is distinguished from an explicit
    /// one by arg count (1 vs 2), letting the evaluator apply the correct
    /// default-whitespace vs. explicit-character-string semantics.
    ///
    /// Complexity: a cheap token-level gate (`trim_args_have_top_level_from`)
    /// decides up front whether a `FROM`-bearing spec form is even possible;
    /// without it the plain `trim(input)` form parses in a single pass. When
    /// the gate passes, the candidate trim-character-string expression is
    /// parsed exactly once and *committed to* — the old code restored the
    /// position and re-parsed the same tokens whenever no `FROM` followed,
    /// which made nested `trim(trim(...))` cost T(k) = 2·T(k-1) (k=28 took
    /// ~80 s). The only remaining backtrack is the error-only path where a
    /// leading BOTH/LEADING/TRAILING identifier was tentatively consumed as
    /// `spec` but turns out to be an ordinary variable (e.g. `trim(both)`
    /// inside a larger expression containing a top-level `FROM`).
    pub(in crate::parser) fn parse_trim_call(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume (

        // The `[spec] [chars] FROM input` forms all require a `FROM` keyword
        // at this trim call's own paren depth. If none exists this is
        // necessarily the plain `trim(input)` form — parse it directly with
        // no speculation (this is what keeps deeply nested trim() linear).
        if !self.trim_args_have_top_level_from() {
            let first = self.parse_expression()?;
            if self.peek_type() != TokenType::Comma {
                self.expect(TokenType::RightParen)?;
                return Ok(Self::build_trim_call(TrimSpec::Both, vec![first]));
            }
            self.next_token(); // consume ,
            let second = self.parse_expression()?;
            let mut args = vec![first, second];
            if self.peek_type() == TokenType::Comma {
                self.next_token();
                args.push(self.parse_expression()?);
            }
            self.expect(TokenType::RightParen)?;
            return Ok(Self::build_trim_spec_call(args));
        }

        let start = self.save_pos();

        let spec_word = {
            let peek = self.peek();
            if peek.ty == TokenType::Identifier {
                TrimSpec::from_ident(&peek.literal)
            } else {
                None
            }
        };
        if spec_word.is_some() {
            self.next_token();
        }
        let spec = spec_word.unwrap_or(TrimSpec::Both);

        // `[spec] FROM input` — no trim character string given.
        if self.peek_type() == TokenType::FROM {
            self.next_token(); // consume FROM
            let input = self.parse_expression()?;
            self.expect(TokenType::RightParen)?;
            return Ok(Self::build_trim_call(spec, vec![input]));
        }

        // `[spec] trimCharacterString FROM input` — or the plain form.
        match self.parse_expression() {
            Ok(chars_expr) if self.peek_type() == TokenType::FROM => {
                self.next_token(); // consume FROM
                let input = self.parse_expression()?;
                self.expect(TokenType::RightParen)?;
                Ok(Self::build_trim_call(spec, vec![input, chars_expr]))
            }
            Ok(chars_expr) if spec_word.is_none() => {
                // No spec word was consumed, so the already-parsed expression
                // *is* the plain trim(input) argument: a re-parse from
                // `start` would see the identical token stream, so reuse the
                // result instead of backtracking.
                self.expect(TokenType::RightParen)?;
                Ok(Self::build_trim_call(TrimSpec::Both, vec![chars_expr]))
            }
            Err(e) if spec_word.is_none() => {
                // Same reasoning: with nothing consumed since `start`, a
                // re-parse would fail identically — propagate directly.
                Err(e)
            }
            Ok(_) | Err(_) => {
                // A spec word (BOTH/LEADING/TRAILING) was tentatively
                // consumed but the spec forms didn't pan out: the word is an
                // ordinary variable (e.g. `trim(both)`), so backtrack and
                // parse the plain single-argument form including it. Only
                // reachable on invalid input — valid plain trims have no
                // top-level FROM and never enter the gated block — so this
                // re-parse is not on any well-formed hot path.
                self.restore_pos(start);
                let input = self.parse_expression()?;
                self.expect(TokenType::RightParen)?;
                Ok(Self::build_trim_call(TrimSpec::Both, vec![input]))
            }
        }
    }

    /// Lookahead from `self.pos` without consuming tokens: true when
    /// `target` appears at depth 0 before the closer that ends the
    /// construct opened just before `self.pos` (the caller has already
    /// consumed the opener). Only the bracket kinds in `brackets` count
    /// toward the depth, so a paren-only scan (inside a call's argument
    /// list) and an all-bracket scan (inside a comprehension brace, whose
    /// source expression can nest brackets and parens) are explicit
    /// per-site choices. The one scan behind
    /// [`Parser::trim_args_have_top_level_from`],
    /// [`Parser::pipe_before_closing_paren`], and
    /// [`Parser::has_pipe_before_matching_brace`].
    pub(super) fn top_level_token_before_matching_close(
        &self,
        target: TokenType,
        brackets: BracketSet,
    ) -> bool {
        let mut depth = 0usize;
        let mut pos = self.pos;
        while pos < self.tokens.len() {
            let ty = self.tokens[pos].ty;
            if brackets.opens(ty) {
                depth += 1;
            } else if brackets.closes(ty) {
                if depth == 0 {
                    return false; // the construct's own closer
                }
                depth -= 1;
            } else if ty == TokenType::EOF {
                return false;
            } else if ty == target && depth == 0 {
                return true;
            }
            pos += 1;
        }
        false
    }

    /// Returns true if a `FROM` keyword token appears at this trim call's own
    /// paren depth before its closing `)` — i.e. whether the GQL
    /// `[spec] [chars] FROM input` forms are possible at all. Pure lookahead
    /// over the token stream (O(remaining tokens)); consumes nothing. A
    /// `FROM` nested inside a parenthesized sub-expression does not count;
    /// one inside brackets/braces at paren depth 0 may produce a harmless
    /// false positive (the caller then falls back to the speculative path,
    /// which behaves exactly as before) — never a false negative, since every
    /// real spec-form `FROM` sits at paren depth 0.
    pub(in crate::parser) fn trim_args_have_top_level_from(&self) -> bool {
        self.top_level_token_before_matching_close(TokenType::FROM, BracketSet::Parens)
    }

    /// Parses `PROPERTY_EXISTS(<element variable reference>, <property name>)`
    /// (CIP-247). The GQL grammar restricts this to exactly two arguments: the
    /// first must be a variable (checked here, not a literal or compound
    /// expression), and the second is a bare property-name identifier — a
    /// distinct grammar slot, not an ordinary comma-separated expression, so a
    /// variable appearing there names the *literal* property (matching
    /// observed Neo4j behaviour) rather than being evaluated, and compound
    /// expressions (`a + b`, `toString(x)`) are rejected at parse time.
    /// Compiles down to a plain `property_exists(var, "propName")` call;
    /// evaluation lives in the engine (a generic property-access rewrite
    /// would wrongly accept maps, which this function must reject).
    pub(in crate::parser) fn parse_property_exists_call(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume (
        let first = self.parse_expression()?;
        let var_name = match &first {
            Expr::Variable(v) => v.clone(),
            _ => {
                return Err(self.error(
                    "PROPERTY_EXISTS() first argument must be a variable referencing a node or relationship",
                ))
            }
        };
        self.expect(TokenType::Comma)?;
        let prop_tok = self.peek().clone();
        if !prop_tok.ty.is_ident_like() {
            return Err(self.error_at_token(
                &prop_tok,
                "PROPERTY_EXISTS() second argument must be a property name identifier",
            ));
        }
        self.next_token();
        if self.peek_type() != TokenType::RightParen {
            return Err(self.error(
                "PROPERTY_EXISTS() second argument must be a bare property name, not an expression",
            ));
        }
        self.next_token(); // consume )
        Ok(Expr::call(
            "property_exists",
            vec![
                Expr::Variable(var_name),
                Expr::Literal(LiteralValue::String(prop_tok.literal.into_owned())),
            ],
        ))
    }

    /// Parses `vector(list)` or `vector(list, dimension, coordType)`
    /// (CIP-214), already having consumed the `VECTOR` keyword and
    /// confirmed the next token is `(`. The optional third argument is a
    /// bare coordinate-type keyword (`INT32`, `FLOAT`, ...), not an
    /// ordinary expression -- like `normalize()`'s normal form and
    /// `PROPERTY_EXISTS()`'s property name, it needs dedicated parsing so
    /// the static variable-scope validator doesn't reject it as an
    /// undefined variable reference. Only this construction shape is
    /// supported (as CIP-216 eval-suite test-data setup); full CIP-214
    /// `vector()` semantics are otherwise out of scope.
    pub(in crate::parser) fn parse_vector_call(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume (
        if self.peek_type() == TokenType::RightParen {
            return Err(self.error("vector() requires at least 1 argument"));
        }
        let mut args = vec![self.parse_expression()?];
        if self.peek_type() == TokenType::Comma {
            self.next_token();
            args.push(self.parse_expression()?);
            self.expect(TokenType::Comma)?;
            let coord_tok = self.peek().clone();
            if !coord_tok.ty.is_ident_like() {
                return Err(self.error_at_token(
                    &coord_tok,
                    "vector() third argument must be a coordinate type keyword",
                ));
            }
            self.next_token();
            // "SIGNED INTEGER" (CIP-200's two-word alias for INTEGER) is the
            // only multi-word coordinate type keyword.
            let mut coord_name = coord_tok.literal.to_ascii_uppercase();
            if coord_name == "SIGNED" && self.peek_is_ident("INTEGER") {
                self.next_token();
                coord_name.push_str(" INTEGER");
            }
            args.push(Expr::Literal(LiteralValue::String(coord_name)));
        }
        self.expect(TokenType::RightParen)?;
        Ok(Expr::call("vector", args))
    }
}

/// The bracket kinds that count toward nesting in
/// [`Parser::top_level_token_before_matching_close`] — an explicit,
/// per-site choice, so a paren-only scan (a call's argument list) and an
/// all-bracket scan (a comprehension brace) cannot be silently confused.
#[derive(Clone, Copy)]
pub(in crate::parser) enum BracketSet {
    /// Only `(` / `)` nest: the scan runs inside a call's argument list.
    Parens,
    /// `{`, `[`, and `(` all nest: the scan runs inside a comprehension
    /// brace, whose source expression can contain brackets and parens.
    AllBrackets,
}

impl BracketSet {
    /// True when `ty` is an opening bracket of one of this set's kinds.
    pub(in crate::parser) fn opens(self, ty: TokenType) -> bool {
        match self {
            BracketSet::Parens => ty == TokenType::LeftParen,
            BracketSet::AllBrackets => matches!(
                ty,
                TokenType::LeftBrace | TokenType::LeftBracket | TokenType::LeftParen
            ),
        }
    }

    /// True when `ty` is a closing bracket of one of this set's kinds.
    pub(in crate::parser) fn closes(self, ty: TokenType) -> bool {
        match self {
            BracketSet::Parens => ty == TokenType::RightParen,
            BracketSet::AllBrackets => matches!(
                ty,
                TokenType::RightBrace | TokenType::RightBracket | TokenType::RightParen
            ),
        }
    }
}
