use super::*;

impl<'a> Parser<'a> {
    /// `USE <graph-expr>` (CIP-141 feature-interaction scenarios). `USE`
    /// is not a reserved keyword -- recognized by spelling at clause
    /// position, same technique as `SHOW`.
    pub(in crate::parser) fn parse_use(&mut self) -> ParseResult<UseClause> {
        self.next_token(); // consume USE
        let graph_expr = self.parse_expression()?;
        Ok(UseClause { graph_expr })
    }

    /// `LET <var> = <expr> ("," <var> = <expr>)*` (CIP-206) -- sugar for
    /// `WITH *, <expr> AS <var>, ...`, desugared directly into a
    /// `WithClause` (tagged `WithOrigin::Let`) so it reuses the engine's
    /// existing WITH execution and scope-analysis machinery; the extra
    /// restrictions LET has that plain `WITH *` doesn't (no shadowing, no
    /// sibling/forward/self reference, no aggregation) are enforced on the
    /// lowered IR by `relate-planner`'s binding pass (the
    /// `VariableAlreadyBound`/`InvalidAggregation` checks there), keyed on
    /// that tag.
    pub(in crate::parser) fn parse_let(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume LET
        let mut items = vec![ReturnItem {
            expr: Expr::Star,
            alias: None,
            all: true,
            source_text: None,
        }];
        loop {
            // LET's grammar has no modifier position after the keyword
            // (unlike `WITH DISTINCT`/`RETURN ... AS`), so accepting any
            // word-shaped token -- including normally-reserved ones like
            // DISTINCT/AS -- as the binding variable is unambiguous.
            // CIP-206 PE6.1/PE6.2 rely on exactly this (`LET DISTINCT = 1`,
            // `LET AS = 1` are both valid).
            let name = self.parse_schema_name("a variable name after LET")?;
            self.expect(TokenType::Equals)?;
            let expr = self.parse_expression()?;
            items.push(ReturnItem {
                expr,
                alias: Some(name),
                all: false,
                source_text: None,
            });
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(Clause::With(WithClause {
            group_by: None,
            distinct: false,
            items,
            order_by: Vec::new(),
            skip: None,
            limit: None,
            where_expr: None,
            origin: WithOrigin::Let,
        }))
    }

    /// `FILTER [WHERE] <search condition>` (CIP-203) -- sugar for `WITH *
    /// WHERE <search condition>`, desugared directly into a `WithClause`
    /// (tagged `WithOrigin::Filter`) for the same reason `LET` is: reuse
    /// WITH's execution/scope machinery, with the extra Boolean-declared-
    /// type check (Rule 3) enforced on the lowered IR by
    /// `relate-planner`'s predicate binding (`check_pred`: a statically
    /// non-Boolean search condition is `InvalidArgumentType`). `FILTER` is a
    /// non-reserved word -- recognized by spelling at clause position,
    /// same technique as `LET`/`FINISH`/`SHOW`/`USE`. A bare `WHERE` with
    /// no `FILTER` keyword (NE1) is already rejected: `WHERE` has no
    /// clause-start dispatch of its own.
    pub(in crate::parser) fn parse_filter(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume FILTER
        if self.peek_type() == TokenType::WHERE {
            self.next_token();
        }
        let where_expr = self.parse_expression()?;
        Ok(Clause::With(WithClause {
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
            where_expr: Some(where_expr),
            origin: WithOrigin::Filter,
        }))
    }

    /// The `ORDER BY`/`SKIP`/`LIMIT` paging sub-grammar: loops consuming
    /// one stage per iteration until none of the three introducer tokens
    /// are next, so `LIMIT 3 OFFSET 1` and `ORDER BY x SKIP 1 LIMIT 2`
    /// both parse fully, in any combination and (per Neo4j's observed
    /// behavior, not the CIP's own fixed grammar order -- see `PageStage`)
    /// any order. Shared by the standalone paging clause (CIP-168,
    /// `parse_standalone_order_by_page`) and the SHOW family's YIELD
    /// paging (CIP-46 con[15]/CIP-74 filtering[8]/[9]) -- the one place
    /// the SKIP/OFFSET policy is decided: OFFSET is accepted everywhere
    /// SKIP is (`peek_is_offset`, CIP-168), including inside SHOW paging.
    pub(in crate::parser) fn parse_page_stages(&mut self) -> ParseResult<Vec<PageStage>> {
        let mut stages = Vec::new();
        loop {
            if self.peek_type() == TokenType::ORDER {
                self.next_token();
                self.expect(TokenType::BY)?;
                stages.push(PageStage::OrderBy(self.parse_order_by()?));
            } else if self.peek_type() == TokenType::SKIP || self.peek_is_offset() {
                self.next_token();
                stages.push(PageStage::Skip(self.parse_expression()?));
            } else if self.peek_type() == TokenType::LIMIT {
                self.next_token();
                stages.push(PageStage::Limit(self.parse_expression()?));
            } else {
                break;
            }
        }
        Ok(stages)
    }

    /// CIP-168: a standalone `<order by and page statement>` at clause
    /// start -- `ORDER BY ...`, `SKIP|OFFSET ...`, or `LIMIT ...`; the
    /// stages themselves are `parse_page_stages`.
    pub(in crate::parser) fn parse_standalone_order_by_page(&mut self) -> ParseResult<Clause> {
        let stages = self.parse_page_stages()?;
        Ok(Clause::StandaloneOrderBy(StandaloneOrderByClause {
            stages,
        }))
    }

    /// `FINISH` (CIP-140) -- a terminal clause. `FINISH` is not a reserved
    /// keyword, recognized by spelling at clause position like `SHOW`.
    pub(in crate::parser) fn parse_finish(&mut self) -> ParseResult<Clause> {
        self.next_token(); // consume FINISH
        Ok(Clause::Finish)
    }

    // ---- Return items / Order by ----

    pub(in crate::parser) fn parse_return_items(&mut self) -> ParseResult<Vec<ReturnItem>> {
        let mut items = Vec::new();
        loop {
            if self.peek_type() == TokenType::Star {
                self.next_token();
                items.push(ReturnItem {
                    expr: Expr::Star,
                    alias: None,
                    all: true,
                    source_text: None,
                });
                // CIP-101/CIP-118 usage: `WITH *, expr AS x` / `RETURN *,
                // expr` -- the star may be followed by further projection
                // items, not just terminate the list.
                if self.peek_type() != TokenType::Comma {
                    return Ok(items);
                }
                self.next_token();
                continue;
            }

            let start_pos = self.save_pos();
            let expr = self.parse_expression()?;
            let end_pos = self.save_pos();

            // Extract source text
            let source_text = self.extract_source_text(start_pos, end_pos);

            let alias = if self.peek_type() == TokenType::AS {
                self.next_token();
                // `RETURN 1 AS as` and `RETURN 1 AS union` included.
                Some(self.parse_alias_after_as()?)
            } else {
                None
            };

            items.push(ReturnItem {
                expr,
                alias,
                all: false,
                source_text,
            });

            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(items)
    }

    /// ADR-0116 §2: the parser recognizes every database administration
    /// command Neo4j CE rejects, so the engine answers with CE's
    /// `UnsupportedAdministrationCommand` (51N27) rather than a syntax
    /// error. Consumes the rest of the statement and fails the parse with a
    /// [`ParseError`] tagged [`UNSUPPORTED_ADMINISTRATION_DETAIL`] whose
    /// message is the verbatim statement text (sliced from `start_pos`, the
    /// command's first token, so any leading `CYPHER n` preamble is
    /// excluded) -- fail-closed: an unsupported administration command
    /// never produces an executable clause. `relate-front` maps this
    /// parse error to CE's semantic error text.
    pub(in crate::parser) fn unsupported_admin_clause(
        &mut self,
        start_pos: usize,
    ) -> ParseResult<Clause> {
        while !matches!(self.peek_type(), TokenType::EOF | TokenType::Semicolon) {
            self.next_token();
        }
        // Internal invariant, not user input: `start_pos` is the command's
        // first token, saved by the caller before any of the command was
        // consumed, so it always indexes a real token and the source slice
        // from it to the current position always exists. A violation is a
        // parser bug -- assert loudly instead of masking it with a fallback.
        let text = self.extract_source_text(start_pos, self.pos).expect(
            "invariant: unsupported_admin_clause start_pos indexes the command's first token, \
             so its source slice always exists",
        );
        let tok = &self.tokens[start_pos];
        Err(ParseError::new(text, tok.line, tok.col)
            .with_source(self.source)
            .with_detail(UNSUPPORTED_ADMINISTRATION_DETAIL))
    }

    pub(in crate::parser) fn extract_source_text(
        &self,
        start_pos: usize,
        end_pos: usize,
    ) -> Option<String> {
        if start_pos >= self.tokens.len() || end_pos == 0 {
            return None;
        }
        let start_tok = &self.tokens[start_pos];
        let start_offset = self.token_byte_offset(start_tok)?;

        let end_offset = if end_pos < self.tokens.len() && self.tokens[end_pos].ty != TokenType::EOF
        {
            self.token_byte_offset(&self.tokens[end_pos])
        } else {
            // Expression runs to end of input: the EOF token's position never
            // matches a source character, and token literals exclude quote
            // characters, so the only reliable end is the source length.
            Some(self.source.len())
        };

        match end_offset {
            Some(end) if end > start_offset => {
                Some(self.source[start_offset..end].trim().to_string())
            }
            _ => None,
        }
    }

    pub(in crate::parser) fn token_byte_offset(&self, tok: &Token) -> Option<usize> {
        // The one position-to-byte helper, with the whole contract
        // (including the inside-the-source half) — see its doc.
        token_start_byte(tok, &self.line_starts, self.source.len())
    }

    pub(in crate::parser) fn parse_order_by(&mut self) -> ParseResult<Vec<OrderByItem>> {
        let mut items = Vec::new();
        loop {
            let expr = self.parse_expression()?;
            let descending = match self.peek_type() {
                TokenType::DESC => {
                    self.next_token();
                    true
                }
                TokenType::ASC => {
                    self.next_token();
                    false
                }
                _ => false,
            };
            items.push(OrderByItem { expr, descending });
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        Ok(items)
    }
}
