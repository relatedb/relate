use super::*;

impl<'a> Parser<'a> {
    /// Parses a `CALL {}` / `CALL (...) {}` subquery's statement block.
    /// The whole body is ONE label-mixing
    /// scope (Neo4j GpmSyntaxMixingAcceptance "Conflicting syntax in
    /// separate statements within a CALL subquery": two clauses of one body
    /// combine), separate from the enclosing clause's own usage — the flags
    /// are saved on entry, cleared for the body, checked at its end, and
    /// restored for the caller's remaining clauses -- on both the success
    /// and error paths.
    pub(super) fn parse_call_subquery_statement(&mut self) -> ParseResult<Statement> {
        let outer = self.side_effects;
        self.side_effects = PatternSideEffectFlags::default();
        let result = self.parse_statement_block();
        let body = self.side_effects;
        self.side_effects = outer;
        let stmt = result?;
        self.check_label_mixing(&body)?;
        Ok(stmt)
    }

    pub(in crate::parser) fn parse_call(&mut self, optional: bool) -> ParseResult<Clause> {
        self.next_token(); // consume CALL
                           // The `CALL (...) { ... }` variable scope clause (CIP-141).
        if self.peek_type() == TokenType::LeftParen {
            let scope = self.parse_call_scope()?;
            self.expect(TokenType::LeftBrace)?;
            let stmt = self.parse_call_subquery_statement()?;
            self.expect(TokenType::RightBrace)?;
            let in_transactions = self.parse_in_transactions()?;
            return Ok(Clause::CallSubquery(CallSubqueryClause {
                statement: stmt,
                scope,
                optional,
                in_transactions,
            }));
        }

        // CALL { ... } subquery (naked, no scope clause -- deprecated)
        if self.peek_type() == TokenType::LeftBrace {
            self.next_token(); // consume {
            let stmt = self.parse_call_subquery_statement()?;
            self.expect(TokenType::RightBrace)?;
            // CIP-213: NEXT cannot be used inside a CALL subquery
            // that uses the (deprecated) importing WITH syntax -- a naked
            // `CALL { ... }` (no scope-clause parens) whose body's very
            // first clause is a `WITH` (the legacy "import these outer
            // variables" idiom) cannot also chain a NEXT stage.
            if stmt.next.is_some() && matches!(stmt.clauses.first(), Some(Clause::With(_))) {
                return Err(self.error(
                    "InvalidClauseComposition: 42N3C invalid use of 'CALL { ... }'. Not possible to enclose 'NEXT' in 'CALL { ... }'. Use 'CALL () { ... }' instead.",
                ));
            }
            let in_transactions = self.parse_in_transactions()?;
            return Ok(Clause::CallSubquery(CallSubqueryClause {
                statement: stmt,
                scope: CallScope::None,
                optional,
                in_transactions,
            }));
        }

        // CALL db.procedure(...) YIELD ...
        let mut name_parts = Vec::new();
        let tok = self.next_token();
        if tok.ty != TokenType::Identifier {
            return Err(self.error_at_token(
                &tok,
                format!("expected procedure name after CALL, got {:?}", tok.literal),
            ));
        }
        name_parts.push(tok.literal);
        while self.peek_type() == TokenType::Dot {
            self.next_token(); // consume .
            let next = self.next_token();
            // `is_schema_name`, not `is_ident_like`: after `CALL name.` the
            // grammar requires a name segment, so no keyword there can
            // start a clause — and procedure names may carry reserved words
            // as segments (`db.ontology.set`, `db.ontology.drop`;
            // ADR-0121/ADR-0227), as Neo4j's symbolic-name rule allows.
            if !next.ty.is_schema_name() {
                return Err(self.error_at_token(
                    &next,
                    format!(
                        "expected identifier in procedure name, got {:?}",
                        next.literal
                    ),
                ));
            }
            name_parts.push(next.literal);
        }
        // The joined namespace-qualified name is classified once, here at AST
        // construction (ADR-0509 §1: a derived spelling is classified where
        // the AST node is built, never re-classified at lookup).
        let proc_name = SmallName::from_name(&name_parts.join("."));

        let explicit_args = self.peek_type() == TokenType::LeftParen;
        let args = if explicit_args {
            self.next_token(); // consume (
            self.parse_arg_list()?
        } else {
            Vec::new()
        };

        let yields = self.parse_show_yield_list()?;
        let saw_yield = yields.present();

        // CIP-159: `WHERE` inside a `YIELD` clause is part of the
        // `<procedure call>` production -- filtering the procedure's own
        // result rows (correlated to the outer row) before they're merged
        // in, distinct from a separate `WHERE` clause that would follow.
        let where_expr = if saw_yield && self.peek_type() == TokenType::WHERE {
            self.next_token();
            Some(self.parse_expression()?)
        } else {
            None
        };

        Ok(Clause::CallProcedure(CallProcedureClause {
            procedure: proc_name,
            args,
            explicit_args,
            yields,
            optional,
            where_expr,
        }))
    }

    /// `<scope clause> ::= "(" ")" | "(" "*" ")" | "(" <var> ("," <var>)* ")"`
    /// -- the `CALL (...)` variable scope clause (CIP-141). The leading `(`
    /// has not been consumed yet.
    pub(in crate::parser) fn parse_call_scope(&mut self) -> ParseResult<CallScope> {
        self.next_token(); // consume (

        if self.peek_type() == TokenType::RightParen {
            self.next_token();
            return Ok(CallScope::Vars(Vec::new()));
        }

        if self.peek_type() == TokenType::Star {
            self.next_token();
            if self.peek_type() == TokenType::Comma {
                return Err(self.error(
                    "InvalidSyntax: a wildcard scope clause cannot be followed by further variable references",
                ));
            }
            self.expect(TokenType::RightParen)?;
            return Ok(CallScope::Wildcard);
        }

        let mut vars = Vec::new();
        loop {
            if self.peek_type() == TokenType::Star {
                return Err(self.error(
                    "InvalidSyntax: a wildcard cannot follow variable references in a scope clause",
                ));
            }
            // The import list names variables already in scope; the name
            // is a symbolic name, so any keyword spelling works unquoted
            // too (ADR-0191).
            let var = self.parse_schema_name("a variable reference in the CALL scope clause")?;
            if self.peek_type() == TokenType::AS {
                return Err(self.error(
                    "VariableDeclarationForbidden: a CALL scope clause may not contain a variable declaration",
                ));
            }
            vars.push(var);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        self.expect(TokenType::RightParen)?;
        Ok(CallScope::Vars(vars))
    }

    /// CIP-32/CIP-34/CIP-198/CIP-30: `IN [<concExpr>] CONCURRENT?
    /// TRANSACTIONS [OF <n> ROW|ROWS] [ON ERROR ...] [REPORT STATUS AS
    /// <var>]`, trailing a `CALL { ... }` / `CALL (...) { ... }`
    /// subquery's closing brace. `TRANSACTIONS`/`CONCURRENT`/`OF`/`ROW`/
    /// `ROWS`/`ERROR`/`BREAK`/`CONTINUE`/`FAIL`/`RETRY`/`REPORT`/`STATUS`/
    /// `SECOND(S)`/`SEC` are all contextual identifiers matched
    /// case-insensitively, not reserved keywords -- `IN`/`ON`/`FOR`/
    /// `THEN`/`AS` are the only real tokens reused here (reserving new
    /// keywords for the rest would regress the TCK). Returns `None` when
    /// the clause has no `IN TRANSACTIONS` trailer at all (an ordinary
    /// CALL subquery). Only CIP-32's base semantics (sequential
    /// batching, nesting/preceding-write restrictions) are enforced by the
    /// engine today; concurrency, retry, and status/error output are
    /// parsed into the AST here so later CIPs only need to add
    /// validation/execution on top of it.
    pub(in crate::parser) fn parse_in_transactions(
        &mut self,
    ) -> ParseResult<Option<InTransactionsClause>> {
        if self.peek_type() != TokenType::IN {
            return Ok(None);
        }
        self.next_token(); // consume IN

        let concurrency = if self.peek_is_ident("transactions") {
            Concurrency::Sequential
        } else if self.peek_is_ident("concurrent") {
            self.next_token(); // consume CONCURRENT
            Concurrency::DefaultUnspecified
        } else {
            let expr = self.parse_expression()?;
            if !self.peek_is_ident("concurrent") {
                return Err(self.error(format!(
                    "expected CONCURRENT after the concurrency expression in 'IN ... TRANSACTIONS', got {:?}",
                    self.peek().literal
                )));
            }
            self.next_token(); // consume CONCURRENT
            Concurrency::Value(expr)
        };

        if !self.peek_is_ident("transactions") {
            return Err(self.error(format!(
                "expected TRANSACTIONS, got {:?}",
                self.peek().literal
            )));
        }
        self.next_token(); // consume TRANSACTIONS

        // CIP-30/CIP-34 verified examples (syntax [1]-[4]/[11]/[12],
        // concurrent-error-handling[7]): `OF <n> ROW|ROWS`, `ON ERROR ...`,
        // and `REPORT STATUS AS s` may appear in any of their 6
        // permutations, so all three are parsed in one loop rather than
        // `OF` being a fixed leading production.
        let mut batch_size: Option<Expr> = None;
        let mut on_error: Option<OnErrorMode> = None;
        let mut report_status: Option<SmallName> = None;
        let mut disjoint: Option<DisjointBy> = None;
        loop {
            if batch_size.is_none() && self.peek_is_ident("of") {
                self.next_token(); // consume OF
                let n = self.parse_expression()?;
                if self.peek_is_ident("row") || self.peek_is_ident("rows") {
                    self.next_token();
                } else {
                    return Err(self.error(format!(
                        "expected ROW or ROWS after 'OF <n>', got {:?}",
                        self.peek().literal
                    )));
                }
                batch_size = Some(n);
                continue;
            }
            if on_error.is_none() && self.peek_type() == TokenType::ON {
                self.next_token(); // consume ON
                if !self.peek_is_ident("error") {
                    return Err(self.error(format!(
                        "expected ERROR after ON, got {:?}",
                        self.peek().literal
                    )));
                }
                self.next_token(); // consume ERROR
                on_error = Some(if self.peek_is_ident("continue") {
                    self.next_token();
                    OnErrorMode::Continue
                } else if self.peek_is_ident("break") {
                    self.next_token();
                    OnErrorMode::Break
                } else if self.peek_is_ident("fail") {
                    self.next_token();
                    OnErrorMode::Fail
                } else if self.peek_is_ident("retry") {
                    self.next_token(); // consume RETRY
                    self.parse_on_error_retry()?
                } else {
                    return Err(self.error(format!(
                        "expected CONTINUE, BREAK, FAIL, or RETRY after ON ERROR, got {:?}",
                        self.peek().literal
                    )));
                });
                continue;
            }
            // CIP-260: `DISJOINT BY (<expr>, ...) | AUTO | NONE`, legal
            // only on the CONCURRENT form (checked after the trailer).
            if disjoint.is_none() && self.peek_is_ident("disjoint") {
                self.next_token(); // consume DISJOINT
                                   // `BY` is a reserved token (GROUP BY), not a contextual
                                   // identifier like the rest of this trailer's words.
                if self.peek_type() != TokenType::BY {
                    return Err(self.error(format!(
                        "expected BY after DISJOINT, got {:?}",
                        self.peek().literal
                    )));
                }
                self.next_token(); // consume BY
                disjoint = Some(if self.peek_is_ident("auto") {
                    self.next_token();
                    DisjointBy::Auto
                } else if self.peek_is_ident("none") {
                    self.next_token();
                    DisjointBy::None
                } else {
                    self.expect(TokenType::LeftParen)?;
                    let exprs = self.parse_arg_list()?;
                    if exprs.is_empty() {
                        return Err(self.error("DISJOINT BY requires at least one expression"));
                    }
                    DisjointBy::Expressions(exprs)
                });
                continue;
            }
            if report_status.is_none() && self.peek_is_ident("report") {
                self.next_token(); // consume REPORT
                if !self.peek_is_ident("status") {
                    return Err(self.error(format!(
                        "expected STATUS after REPORT, got {:?}",
                        self.peek().literal
                    )));
                }
                self.next_token(); // consume STATUS
                self.expect(TokenType::AS)?;
                // ADR-0191 (Decision point 7's follow-up probe): Neo4j
                // 2026.09 accepts every keyword spelling as the REPORT
                // STATUS AS variable, so this reads through the shared
                // symbolic-name read like every other alias slot.
                let var = self.parse_schema_name("a variable name after REPORT STATUS AS")?;
                report_status = Some(var);
                continue;
            }
            break;
        }
        let on_error = on_error.unwrap_or(OnErrorMode::Fail);
        if disjoint.is_some() && concurrency == Concurrency::Sequential {
            return Err(self.error(
                "DisjointByRequiresConcurrent: 'DISJOINT BY' can only be used in \
                 'CALL { ... } IN CONCURRENT TRANSACTIONS'",
            ));
        }

        Ok(Some(InTransactionsClause {
            concurrency,
            batch_size,
            on_error,
            report_status,
            disjoint,
        }))
    }

    /// CIP-198 `RETRY [FOR]? [<dur> SEC|SECOND|SECONDS]? [THEN
    /// CONTINUE|BREAK|FAIL]?` -- the `RETRY` keyword itself has already
    /// been consumed. Whether a duration follows at all is genuinely
    /// ambiguous without `FOR` (`RETRY` alone, immediately followed by
    /// `THEN`/`REPORT`/the next clause, is valid), so the duration is
    /// parsed speculatively when `FOR` is absent: try an expression
    /// followed by a `SEC`/`SECOND`/`SECONDS` unit, and roll back to try
    /// `THEN`/end-of-clause instead if that doesn't pan out.
    pub(in crate::parser) fn parse_on_error_retry(&mut self) -> ParseResult<OnErrorMode> {
        // `FOR` is a real reserved token (reused from e.g. `FOREACH`'s
        // desugaring elsewhere), not a contextual identifier.
        let has_for = self.peek_type() == TokenType::FOR;
        if has_for {
            self.next_token(); // consume FOR
        }
        let checkpoint = self.save_pos();
        let mut duration = None;
        let parsed_duration = self.parse_expression().ok().filter(|_| {
            self.peek_is_ident("sec")
                || self.peek_is_ident("second")
                || self.peek_is_ident("seconds")
        });
        if let Some(dur_expr) = parsed_duration {
            self.next_token(); // consume SEC/SECOND/SECONDS
            duration = Some(dur_expr);
        } else {
            self.restore_pos(checkpoint);
            if has_for {
                return Err(self.error(format!(
                    "expected a duration followed by SEC/SECOND/SECONDS after RETRY FOR, got {:?}",
                    self.peek().literal
                )));
            }
        }

        let fallback = if self.peek_type() == TokenType::THEN {
            self.next_token(); // consume THEN
            if self.peek_is_ident("continue") {
                self.next_token();
                RetryFallback::Continue
            } else if self.peek_is_ident("break") {
                self.next_token();
                RetryFallback::Break
            } else if self.peek_is_ident("fail") {
                self.next_token();
                RetryFallback::Fail
            } else {
                return Err(self.error(format!(
                    "expected CONTINUE, BREAK, or FAIL after THEN, got {:?}",
                    self.peek().literal
                )));
            }
        } else {
            // CIP-198 deviation: a bare RETRY (or RETRY FOR <dur>) with no
            // THEN clause falls back to FAIL, not CONTINUE.
            RetryFallback::Fail
        };

        Ok(OnErrorMode::Retry { duration, fallback })
    }

    /// CIP-168: `OFFSET` is a synonym for `SKIP`, usable everywhere SKIP
    /// is (standalone paging clause, WITH, RETURN, YIELD). Not a reserved
    /// keyword -- recognized by spelling, same technique as `SHOW`/`USE`,
    /// so existing uses like the property access `d.offset` (TCK
    /// Temporal5) keep working.
    pub(in crate::parser) fn peek_is_offset(&self) -> bool {
        self.peek_is_ident("offset")
    }
}
