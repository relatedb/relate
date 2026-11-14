//! List, map, comprehension and `reduce` expressions.

use super::*;

/// `parse_reduce_head`'s result: the `( acc = init, var IN list | expr`
/// head shared by `reduce` and `allReduce`.
struct ReduceHead {
    accumulator: SmallName,
    init_expr: Expr,
    variable: SmallName,
    list_expr: Expr,
    expression: Expr,
}

impl<'a> Parser<'a> {
    pub(in crate::parser) fn parse_list_literal(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume [

        if self.peek_type() == TokenType::RightBracket {
            self.next_token();
            return Ok(Expr::List(Vec::new()));
        }

        let first = self.peek().clone();
        // A list/pattern comprehension's own loop/path variable is a
        // symbolic name (ADR-0191), so any keyword spelling works unquoted
        // too. This resolves `[true IN [1]]` in favour of the comprehension
        // (it returns `[1]`, matching Neo4j), not a one-element list
        // literal holding a boolean `IN` expression.
        if first.ty.is_schema_name() {
            let first_pos = self.save_pos();
            self.next_token(); // consume identifier
            if self.peek_type() == TokenType::IN {
                // List comprehension
                let variable = SmallName::from_name(&first.literal);
                self.next_token(); // consume IN
                let list_expr = self.parse_expression()?;

                // A comma can never appear inside a comprehension, so
                // `[true IN [true], false]` is a *list literal* whose first
                // element is a boolean `IN` expression -- Neo4j prefers the
                // literal reading there. Restore and parse it as a literal
                // below instead of failing on the `,`.
                if self.peek_type() == TokenType::Comma {
                    self.restore_pos(first_pos);
                } else {
                    let where_expr = if self.peek_type() == TokenType::WHERE {
                        self.next_token();
                        // A list comprehension may omit its map expression, so
                        // a trailing `]` does not disambiguate a `|`.
                        Some(self.parse_comprehension_where_expr(false)?)
                    } else {
                        None
                    };

                    let map_expr = if self.peek_type() == TokenType::Pipe {
                        self.next_token();
                        Some(self.parse_expression()?)
                    } else {
                        None
                    };

                    self.expect(TokenType::RightBracket)?;
                    return Ok(Expr::ListComprehension(Box::new(ListComprehensionExpr {
                        variable,
                        list_expr,
                        where_expr,
                        map_expr,
                    })));
                }
            } else {
                // Pattern comprehension with path variable: [p = (n)-->() | expr].
                // ADR-0191: a keyword spelling is a path variable too (`[MATCH =
                // (n)-->() | n]`). The pattern-comprehension reading only applies
                // when a pattern path really follows the `=` -- decided by
                // speculatively running the real `parse_pattern_path` reader and
                // keeping the reading only when a `|` or `WHERE` follows the
                // pattern (`pattern_comprehension_follows_equals`); otherwise the
                // list-literal reading wins, so `[true = x]`, `[null = x]`,
                // `[true = (1 = 1)]` and `[x = 1]` stay list literals (matching
                // Neo4j, where a pattern never follows a literal).
                if self.peek_type() == TokenType::Equals
                    && self.pattern_comprehension_follows_equals()
                {
                    let path_var = SmallName::from_name(&first.literal);
                    self.next_token(); // consume =
                    return self.parse_pattern_comprehension_body(Some(path_var));
                }

                // Not a list comprehension, backup and parse as regular list
                self.backup();
            }
        }

        // Pattern comprehension without path variable: [(n)-->() | expr]
        if first.ty == TokenType::LeftParen {
            // Use lookahead to check if this looks like a pattern comprehension.
            // A pattern comprehension starts with a node pattern (a) followed by a relationship.
            if self.node_pattern_precedes_relationship() {
                return self.parse_pattern_comprehension_body(None);
            }
        }

        // Regular list literal
        let mut elements = Vec::new();
        loop {
            elements.push(self.parse_expression()?);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        self.expect(TokenType::RightBracket)?;
        Ok(Expr::List(elements))
    }

    /// Speculative gate for the `=` of a would-be pattern comprehension
    /// (`[pathVar = <pattern> | expr]`, ADR-0191): true when the tokens
    /// after the `=` (the current token must be that `=`) parse as a
    /// complete pattern path through the real `parse_pattern_path` reader
    /// AND the token after the pattern is `|` or `WHERE` (the
    /// comprehension's own grammar). Because the gate runs the real reader,
    /// everything `parse_pattern_path` accepts is recognized here -- label
    /// expressions and GPM forms (`(n:A|B)-->()`, `(IS A&B)-->()`), bare
    /// node or juxtaposed bodies (`(n)`, `(a)(b)`), parenthesized subpaths
    /// (`((q = (a)-->(b)))`) and the `shortestPath(...)` wrapper alike.
    ///
    /// The probe is invisible when it does not commit: the token position
    /// and every clause-level side-effect flag the parse can set (the
    /// `label_saw_*`/`quantifier_saw_*` mixing flags) are restored before
    /// returning, so a negative verdict leaves the list-literal reading to
    /// run exactly as if the probe never happened. A positive verdict is
    /// re-parsed for real by the caller (`parse_pattern_comprehension_body`),
    /// which re-applies those side effects.
    ///
    /// The verdict is memoized per `=` token position
    /// (`Parser::pattern_comprehension_probe_memo`): it depends only on
    /// that position, and without the memo a nested
    /// `[v = (n {p: ...}) | ...]` shape re-probes the whole nested term at
    /// every level -- 2^k parse cost on a sub-KB query (LABS-183).
    pub(super) fn pattern_comprehension_follows_equals(&mut self) -> bool {
        debug_assert_eq!(self.peek_type(), TokenType::Equals);
        let key = self.pos;
        if let Some(&verdict) = self.pattern_comprehension_probe_memo.get(&key) {
            return verdict;
        }
        let saved_pos = self.save_pos();
        let saved_flags = self.side_effects;
        self.next_token(); // consume =
        let probe = self.parse_pattern_path();
        let is_comprehension =
            probe.is_ok() && matches!(self.peek_type(), TokenType::Pipe | TokenType::WHERE);
        self.restore_pos(saved_pos);
        self.side_effects = saved_flags;
        self.pattern_comprehension_probe_memo
            .insert(key, is_comprehension);
        is_comprehension
    }

    /// Lookahead from a `(` without consuming tokens: true when the tokens at
    /// the current position form a node pattern `( var? (:label)* {…}? )`
    /// followed by `-` or `<-`. That relationship start is what tells a
    /// pattern (a pattern comprehension after `[`, a pattern predicate such
    /// as `WHERE (n)-->()`) apart from a bracketed or parenthesized
    /// expression.
    pub(in crate::parser) fn node_pattern_precedes_relationship(&self) -> bool {
        self.node_pattern_precedes_relationship_at(0)
    }

    /// [`node_pattern_precedes_relationship`] starting `offset` tokens ahead
    /// of the current position (0 == the current token).
    pub(super) fn node_pattern_precedes_relationship_at(&self, offset: usize) -> bool {
        let mut pos = self.pos + offset;

        // Must start with (
        if pos >= self.tokens.len() || self.tokens[pos].ty != TokenType::LeftParen {
            return false;
        }
        pos += 1;

        // Skip optional variable. ADR-0191: a keyword spelling is a node
        // variable too, so this mirrors the reader (`parse_node_pattern`),
        // including the `WHERE`/`IS` one-token lookahead that keeps their
        // inline-predicate / label-introducer meanings.
        // (`peek_pattern_name_is_variable` folds in the symbolic-name check.)
        if self.peek_pattern_name_is_variable(pos - self.pos) {
            pos += 1;
        }

        // Skip labels (a keyword is a label name too, ADR-0169)
        while pos < self.tokens.len() && self.tokens[pos].ty == TokenType::Colon {
            pos += 1;
            if pos < self.tokens.len() && self.tokens[pos].ty.is_schema_name() {
                pos += 1;
            } else {
                return false;
            }
        }

        // Skip optional properties { ... }
        if pos < self.tokens.len() && self.tokens[pos].ty == TokenType::LeftBrace {
            let mut depth = 1;
            pos += 1;
            while depth > 0 && pos < self.tokens.len() {
                match self.tokens[pos].ty {
                    TokenType::LeftBrace => depth += 1,
                    TokenType::RightBrace => depth -= 1,
                    TokenType::EOF => return false,
                    _ => {}
                }
                pos += 1;
            }
        }

        // Skip an optional inline WHERE predicate (CIP-17/CIP-18), e.g.
        // `(a:A WHERE a.prop > x)`. The predicate runs to the node
        // pattern's own closing `)`, so scan there while balancing every
        // nested bracket kind; a `)` at depth zero ends the node pattern.
        if pos < self.tokens.len() && self.tokens[pos].ty == TokenType::WHERE {
            pos += 1;
            let mut depth = 0usize;
            loop {
                match self.tokens.get(pos).map(|t| t.ty) {
                    Some(TokenType::LeftParen | TokenType::LeftBracket | TokenType::LeftBrace) => {
                        depth += 1
                    }
                    Some(TokenType::RightParen) if depth == 0 => break,
                    Some(
                        TokenType::RightParen | TokenType::RightBracket | TokenType::RightBrace,
                    ) => {
                        if depth == 0 {
                            return false;
                        }
                        depth -= 1;
                    }
                    Some(TokenType::EOF) | None => return false,
                    Some(_) => {}
                }
                pos += 1;
            }
        }

        // Must see )
        if pos >= self.tokens.len() || self.tokens[pos].ty != TokenType::RightParen {
            return false;
        }
        pos += 1;

        // After ), must see - or <- (indicating a relationship/pattern)
        if pos < self.tokens.len() {
            matches!(self.tokens[pos].ty, TokenType::Dash | TokenType::ArrowLeft)
        } else {
            false
        }
    }

    /// Lookahead from the argument-start position without consuming tokens:
    /// true when a `|` appears at the argument's own paren depth before the
    /// paren that closes the current argument list. That pipe is the tail of
    /// the agent form `any((a)-[:R]->(b) | pred)` (LABS-201) -- a
    /// comprehension's map-expression separator misplaced outside any
    /// `[...]`, where no valid parse ever has one.
    pub(super) fn pipe_before_closing_paren(&self) -> bool {
        self.top_level_token_before_matching_close(TokenType::Pipe, BracketSet::Parens)
    }

    pub(in crate::parser) fn parse_pattern_comprehension_body(
        &mut self,
        path_var: Option<SmallName>,
    ) -> ParseResult<Expr> {
        // A pattern comprehension's inner pattern is its own pattern
        // context for the quantifier-mixing check: its quantifier forms
        // accumulate into the clause's comprehension-scoped flags instead
        // of combining with the clause's own forms (a legacy `[:R*1..2]`
        // inside a comprehension nested in a quantified path pattern's
        // WHERE is legal -- Neo4j QuantifiedPathPatternAcceptance -- while
        // two comprehensions' forms still combine with each other through
        // the shared accumulators). See
        // `comprehension_quantifiers`.
        let saved = std::mem::take(&mut self.side_effects.quantifiers);
        let pattern = self.parse_pattern_path();
        let inner = std::mem::replace(&mut self.side_effects.quantifiers, saved);
        self.side_effects.comprehension_quantifiers.absorb(inner);
        let pattern = pattern?;

        let where_expr = if self.peek_type() == TokenType::WHERE {
            self.next_token();
            // A pattern comprehension always needs `| <map>`, so a `]` right
            // after the scanned label chain proves `|` was the separator.
            Some(self.parse_comprehension_where_expr(true)?)
        } else {
            None
        };

        self.expect(TokenType::Pipe)?;
        let map_expr = self.parse_expression()?;
        self.expect(TokenType::RightBracket)?;

        Ok(Expr::PatternComprehension(Box::new(
            PatternComprehensionExpr {
                path_variable: path_var,
                pattern,
                where_expr,
                map_expr,
            },
        )))
    }

    /// Parses the `WHERE`-part expression of a list/pattern comprehension
    /// with `comprehension_where_ambiguous_pipe` set, so a trailing `|` that
    /// can't validly continue a label-expression disjunction is left for the
    /// comprehension's own map-separator grammar instead of being consumed
    /// as part of the expression. See `label_or_pipe_is_ambiguous`.
    pub(in crate::parser) fn parse_comprehension_where_expr(
        &mut self,
        map_required: bool,
    ) -> ParseResult<Expr> {
        let saved_flag = self.comprehension_where_ambiguous_pipe;
        let saved_map = self.comprehension_where_map_required;
        self.comprehension_where_ambiguous_pipe = true;
        self.comprehension_where_map_required = map_required;
        let result = self.parse_expression();
        self.comprehension_where_ambiguous_pipe = saved_flag;
        self.comprehension_where_map_required = saved_map;
        result
    }

    /// CIP-272 map comprehension, attempted from the `{` of a primary
    /// expression. Returns `Ok(None)` (position restored) when the tokens do
    /// not form one, so the caller falls back to the map-literal parse.
    /// Shapes: `{ident IN <src> [WHERE p] | <key> : <value>}` (list source)
    /// and `{ident : ident IN <src> [WHERE p] | <key> : <value>}` (map
    /// source, distinguished by the second ident before `IN`).
    pub(in crate::parser) fn try_parse_map_comprehension(&mut self) -> ParseResult<Option<Expr>> {
        let start = self.save_pos();
        self.next_token(); // consume '{'

        let first = self.peek().clone();
        // The binding variable(s) are symbolic names, so any keyword
        // spelling works unquoted too (ADR-0191).
        if !first.ty.is_schema_name() {
            self.restore_pos(start);
            return Ok(None);
        }
        self.next_token(); // consume the binding identifier

        // List source: {item IN ...}
        if self.peek_type() == TokenType::IN {
            self.next_token();
            return self.finish_map_comprehension(
                start,
                None,
                SmallName::from_name(&first.literal),
            );
        }

        // Map source: {k : v IN ...}
        if self.peek_type() == TokenType::Colon {
            self.next_token();
            let second = self.peek().clone();
            if second.ty.is_schema_name() {
                self.next_token();
                if self.peek_type() == TokenType::IN {
                    self.next_token();
                    return self.finish_map_comprehension(
                        start,
                        Some(SmallName::from_name(&first.literal)),
                        SmallName::from_name(&second.literal),
                    );
                }
            }
        }

        self.restore_pos(start);
        Ok(None)
    }

    /// True when a `|` at the current brace nesting depth appears before the
    /// `}` that closes the comprehension `{`. Called with the current token
    /// immediately after the `IN` of a map comprehension (both CIP-272
    /// shapes), so `false` means no token run inside this brace can be the
    /// projection separator and the speculation is pointless. All bracket
    /// kinds nest (unlike the paren-only scans): the source expression
    /// inside the brace can itself contain brackets and parens.
    pub(in crate::parser) fn has_pipe_before_matching_brace(&self) -> bool {
        self.top_level_token_before_matching_close(TokenType::Pipe, BracketSet::AllBrackets)
    }

    /// Shared tail of both CIP-272 shapes: the binding variable(s) and `IN`
    /// are consumed; parse source, optional WHERE, the `| key : value`
    /// projection, and the closing brace. Any structural mismatch restores
    /// the start position and yields `None` (the map-literal parse then
    /// reports its own error for genuinely malformed input).
    pub(in crate::parser) fn finish_map_comprehension(
        &mut self,
        start: usize,
        key_var: Option<SmallName>,
        value_var: SmallName,
    ) -> ParseResult<Option<Expr>> {
        // Fail fast before the speculative parse below. The whole shape hinges on
        // a top-level `|` separating the source from the projection; when no such
        // token exists the speculative parse can only rewind and let the
        // map-literal fallback re-parse the same tokens, doubling the work per
        // nesting level for input such as `{a: b IN {a: b IN { ... }}}` -- i.e.
        // exponential parse time on untrusted input. One brace-depth-aware scan
        // rejects that shape up front, in O(tokens); a genuine comprehension
        // scans once and then parses once, exactly as before. A brace that does
        // carry a top-level `|` still takes the speculative path: the scan cannot
        // tell a well-formed projection from one with a malformed tail.
        if !self.has_pipe_before_matching_brace() {
            self.restore_pos(start);
            return Ok(None);
        }
        #[cfg(test)]
        SPECULATIVE_COMPREHENSION_PARSES.with(|c| c.set(c.get() + 1));

        // The source expression must stop at the comprehension's `|`: the
        // flag-guarded parse (same as a comprehension WHERE) refuses an
        // ambiguous trailing pipe, so `{k : v IN m | k : v}` does not try
        // to read `m | k` as a label disjunction.
        let source_expr = match self.parse_comprehension_where_expr(false) {
            Ok(e) => e,
            Err(_) => {
                self.restore_pos(start);
                return Ok(None);
            }
        };

        let where_expr = if self.peek_type() == TokenType::WHERE {
            self.next_token();
            match self.parse_comprehension_where_expr(false) {
                Ok(e) => Some(e),
                Err(_) => {
                    self.restore_pos(start);
                    return Ok(None);
                }
            }
        } else {
            None
        };

        if self.peek_type() != TokenType::Pipe {
            self.restore_pos(start);
            return Ok(None);
        }
        self.next_token();

        // The key expression is parsed at the arithmetic level
        // (`<arithmetic value expression>` in the CIP-272 grammar): a full
        // expression would swallow the `:` as the CIP-14 label predicate
        // (`'k': i` = HasLabel), which is exactly the ambiguity the CIP
        // resolves by restricting the key grammar.
        let key_expr = match self.parse_add_sub() {
            Ok(e) => e,
            Err(_) => {
                self.restore_pos(start);
                return Ok(None);
            }
        };
        if self.peek_type() != TokenType::Colon {
            self.restore_pos(start);
            return Ok(None);
        }
        self.next_token();
        let value_expr = match self.parse_expression() {
            Ok(e) => e,
            Err(_) => {
                self.restore_pos(start);
                return Ok(None);
            }
        };
        if self.peek_type() != TokenType::RightBrace {
            self.restore_pos(start);
            return Ok(None);
        }
        self.next_token();

        Ok(Some(Expr::MapComprehension(Box::new(
            MapComprehensionExpr {
                key_var,
                value_var,
                source_expr,
                where_expr,
                key_expr,
                value_expr,
            },
        ))))
    }

    pub(in crate::parser) fn parse_map_literal(&mut self) -> ParseResult<Vec<MapPair>> {
        self.expect(TokenType::LeftBrace)?;
        let mut pairs = Vec::new();
        if self.peek_type() == TokenType::RightBrace {
            self.next_token();
            return Ok(pairs);
        }
        loop {
            let key_tok = self.next_token();
            // ADR-0191 point 5: an unquoted keyword is a map key too
            // (`{match: 1}`), so this reads through `is_schema_name()` --
            // the same predicate ADR-0169 uses for label/type names.
            if !key_tok.ty.is_schema_name() && key_tok.ty != TokenType::String {
                // A digit-leading key (`{1B2c3e67: 1}`) lexes as an
                // `InvalidNumberLiteral` error token, but in *map-key* position
                // the openCypher TCK classifies every invalid key as the generic
                // `UnexpectedSyntax` detail (Literals8 [19]–[27], SyntaxError
                // class), not `InvalidNumberLiteral`. Override the propagated
                // token detail with `UnexpectedSyntax` here; the same malformed
                // token in *expression* position (`RETURN 1B2c3e67`) keeps its
                // `InvalidNumberLiteral` detail because it never reaches this
                // key-parsing path.
                return Err(self
                    .error_at_token(
                        &key_tok,
                        format!(
                            "expected map key (identifier or string), got {:?}",
                            key_tok.literal
                        ),
                    )
                    .with_detail("UnexpectedSyntax"));
            }
            self.expect(TokenType::Colon)?;
            let val = self.parse_expression()?;
            pairs.push(MapPair {
                key: SmallName::from_name(&key_tok.literal),
                value: val,
            });
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        self.expect(TokenType::RightBrace)?;
        Ok(pairs)
    }

    /// Parses a `<map projection>` filler (the current token must be `{`):
    /// `{ .*, .prop, variable, key: expr, ... }`.
    pub(in crate::parser) fn parse_map_projection_items(
        &mut self,
    ) -> ParseResult<Vec<MapProjectionItem>> {
        self.expect(TokenType::LeftBrace)?;
        let mut items = Vec::new();
        if self.peek_type() == TokenType::RightBrace {
            self.next_token();
            return Ok(items);
        }
        loop {
            if self.peek_type() == TokenType::Dot {
                self.next_token(); // consume .
                if self.peek_type() == TokenType::Star {
                    self.next_token();
                    items.push(MapProjectionItem::AllProperties);
                } else {
                    // ADR-0191 point 5: keywords are map-projection
                    // selectors too (`n{.match}`, `n{match}`).
                    let prop =
                        self.parse_schema_name("a property name after '.' in map projection")?;
                    items.push(MapProjectionItem::Property(prop));
                }
            } else {
                let name = self.parse_schema_name("a map projection item")?;
                if self.peek_type() == TokenType::Colon {
                    self.next_token();
                    let value = self.parse_expression()?;
                    items.push(MapProjectionItem::KeyValue(name, value));
                } else {
                    items.push(MapProjectionItem::Variable(name));
                }
            }
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        self.expect(TokenType::RightBrace)?;
        Ok(items)
    }

    /// Called with `{` as the current token, immediately after a
    /// subquery-introducing keyword (`EXISTS`/`COUNT`/`COLLECT`). Returns
    /// true if the filler looks like a `<map projection>` (the keyword
    /// being used as an ordinary bound variable) rather than subquery
    /// content: a subquery's `{...}` must start with `(` (a bare pattern)
    /// or a clause keyword (MATCH/WITH/RETURN/...), neither of which a map
    /// projection filler (`.prop`, `.*`, a bare selector variable, or
    /// `key: expr`) can also start with, so one token of extra lookahead
    /// past that disambiguates with no backtracking (CIP-190
    /// near-ambiguities.feature [2]).
    ///
    /// ADR-0191 point 5: the item read through `is_schema_name()`, so a
    /// keyword spelling is a map-projection item too (`n {match: 1}`,
    /// `n {match}`, `n {.match}`). A subquery body starting with a clause
    /// keyword (`EXISTS { MATCH ... }`, `COUNT { DELETE n }`) still fails
    /// the two-token lookahead (`(`/an operand after the keyword, not
    /// `:`/`,`/`}`), so subquery classification is unchanged.
    pub(in crate::parser) fn peek_brace_is_map_projection(&self) -> bool {
        match self.peek_at(1).ty {
            TokenType::Dot => true,
            ty if ty.is_schema_name() => matches!(
                self.peek_at(2).ty,
                TokenType::Colon | TokenType::Comma | TokenType::RightBrace
            ),
            _ => false,
        }
    }

    /// Shared head of `reduce` and `allReduce`: parses
    /// `( acc = init, var IN list | expr` with the function-name token
    /// already consumed by the caller, leaving the cursor just after the
    /// per-element expression. `function_name` only shapes the error text;
    /// the two forms differ solely in their tail after `| expr`.
    fn parse_reduce_head(&mut self, function_name: &str) -> ParseResult<ReduceHead> {
        self.expect(TokenType::LeftParen)?;

        // The accumulator and loop variables are symbolic names, so any
        // keyword spelling works unquoted too (ADR-0191).
        let accumulator =
            self.parse_schema_name(&format!("accumulator variable in {function_name}()"))?;
        self.expect(TokenType::Equals)?;
        let init_expr = self.parse_expression()?;
        self.expect(TokenType::Comma)?;

        let variable = self.parse_schema_name(&format!("variable in {function_name}()"))?;
        self.expect(TokenType::IN)?;
        let list_expr = self.parse_expression()?;
        self.expect(TokenType::Pipe)?;
        let expression = self.parse_expression()?;

        Ok(ReduceHead {
            accumulator,
            init_expr,
            variable,
            list_expr,
            expression,
        })
    }

    pub(in crate::parser) fn parse_reduce_expr(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume REDUCE
        let ReduceHead {
            accumulator,
            init_expr,
            variable,
            list_expr,
            expression,
        } = self.parse_reduce_head("reduce")?;
        self.expect(TokenType::RightParen)?;

        Ok(Expr::Reduce(Box::new(ReduceExpr {
            accumulator,
            init_expr,
            variable,
            list_expr,
            expression,
        })))
    }

    pub(in crate::parser) fn parse_allreduce_expr(&mut self) -> ParseResult<Expr> {
        // The 'allReduce' identifier token has already been consumed by the caller.
        let ReduceHead {
            accumulator,
            init_expr,
            variable,
            list_expr,
            expression,
        } = self.parse_reduce_head("allReduce")?;
        self.expect(TokenType::Comma)?;
        let predicate = self.parse_expression()?;
        self.expect(TokenType::RightParen)?;

        Ok(Expr::AllReduce(Box::new(AllReduceExpr {
            accumulator,
            init_expr,
            variable,
            list_expr,
            expression,
            predicate,
        })))
    }
}
