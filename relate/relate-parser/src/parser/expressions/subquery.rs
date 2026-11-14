//! `EXISTS`, `COUNT` and `COLLECT` subquery expressions and their column-shape checks.

use super::*;

impl<'a> Parser<'a> {
    /// Builds the one-clause `MATCH` `Statement` wrapping a subquery
    /// expression's bare-pattern form (`EXISTS { pattern [WHERE expr] }` /
    /// `COUNT { pattern [WHERE expr] }`), shared by `parse_exists_subquery`
    /// and `parse_count_subquery`.
    pub(super) fn bare_pattern_match_statement(
        pattern: Vec<PatternPath>,
        where_expr: Option<Expr>,
    ) -> Statement {
        Statement {
            clauses: vec![Clause::Match(MatchClause {
                optional: false,
                match_mode: MatchMode::DifferentRelationships,
                pattern,
                using_scan: Vec::new(),
                using_index: Vec::new(),
                using_join: Vec::new(),
                where_expr,
                search: None,
            })],
            ..Statement::default()
        }
    }

    /// `EXISTS { ... }` subquery expression -- the two grammar forms (bare
    /// pattern, or a full statement of one-or-more clauses), mirrored by
    /// `parse_count_subquery` with row counting instead of the existence
    /// test. Kept out-of-line (not inlined into `parse_primary`) so its
    /// locals don't inflate that function's stack frame, which is on the
    /// hot recursive path for deeply nested expressions (e.g. TCK's 40-deep
    /// nested list literals). The `{` is the current token; the caller has
    /// already ruled out a map projection (`peek_brace_is_map_projection`).
    #[inline(never)]
    pub(in crate::parser) fn parse_exists_subquery(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume {
        self.exists_subquery_depth += 1;
        let result = self.parse_exists_subquery_body();
        self.exists_subquery_depth -= 1;
        result
    }

    /// The body of [`Parser::parse_exists_subquery`] (the `{` consumed and
    /// the existence-probe depth already counted).
    pub(super) fn parse_exists_subquery_body(&mut self) -> ParseResult<Expr> {
        let peek = self.peek().clone();
        if peek.ty == TokenType::LeftParen {
            // Simple pattern: EXISTS { pattern }
            let pattern = self.parse_pattern()?;
            let where_expr = if self.peek_type() == TokenType::WHERE {
                self.next_token();
                Some(self.parse_expression()?)
            } else {
                None
            };
            self.expect(TokenType::RightBrace)?;
            let stmt = Self::bare_pattern_match_statement(pattern, where_expr);
            return Ok(Expr::ExistsSubquery(Box::new(ExistsSubqueryExpr {
                statement: stmt,
            })));
        }
        let stmt = self.parse_statement_block()?;
        self.expect(TokenType::RightBrace)?;
        // CIP-63: unlike COUNT, a distinct UNION inside EXISTS
        // may omit RETURN in every branch (see the shared
        // helper's doc comment).
        self.check_subquery_union_consistency(&stmt, false)?;
        Ok(Expr::ExistsSubquery(Box::new(ExistsSubqueryExpr {
            statement: stmt,
        })))
    }

    /// `COUNT { ... }` subquery expression -- mirrors `EXISTS { ... }`'s two
    /// grammar forms (bare pattern, or a full statement of one-or-more
    /// clauses), just counting rows instead of testing non-emptiness. Kept
    /// out-of-line (not inlined into `parse_primary`) so its locals don't
    /// inflate that function's stack frame, which is on the hot recursive
    /// path for deeply nested expressions (e.g. TCK's 40-deep nested list
    /// literals).
    #[inline(never)]
    pub(in crate::parser) fn parse_count_subquery(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume {
        if self.peek_type() == TokenType::RightBrace {
            // `COUNT { }` is a compile-time
            // syntax error, not a valid (empty) subquery -- unlike EXISTS
            // and COLLECT, which this batch leaves alone (out of scope).
            return Err(
                self.error("InvalidCountExpression: COUNT {} must contain a pattern or a query")
            );
        }
        let peek = self.peek().clone();
        if peek.ty == TokenType::LeftParen {
            let pattern = self.parse_pattern()?;
            let where_expr = if self.peek_type() == TokenType::WHERE {
                self.next_token();
                Some(self.parse_expression()?)
            } else {
                None
            };
            self.expect(TokenType::RightBrace)?;
            let stmt = Self::bare_pattern_match_statement(pattern, where_expr);
            return Ok(Expr::CountSubquery(Box::new(CountSubqueryExpr {
                statement: stmt,
            })));
        }
        let stmt = self.parse_statement_block()?;
        self.expect(TokenType::RightBrace)?;
        // The UNION rules (verified against Neo4j): a *distinct*
        // UNION inside COUNT can never omit RETURN in any branch (its
        // dedup needs comparable, named columns), unlike EXISTS below,
        // which only cares about row existence and has no such need.
        self.check_subquery_union_consistency(&stmt, true)?;
        Ok(Expr::CountSubquery(Box::new(CountSubqueryExpr {
            statement: stmt,
        })))
    }

    /// CIP-63 (EXISTS) / CIP-66 (COUNT): shared UNION-consistency check for
    /// a subquery body's `<RegularQuery>`. Now that `parse_regular_query`
    /// lets UNION parse inside these bodies at all, the semantic rules both
    /// CIPs spell out for RETURN omission across UNION branches need
    /// enforcing here, or a UNION that omission rules forbid would silently
    /// "succeed" instead of raising the compile-time error the doc requires:
    /// (1) if any branch has an explicit RETURN, every branch must have one
    /// -- RETURN omission is all-or-nothing "for consistency"; (2) when
    /// `reject_omitted_distinct` is set (COUNT, CIP-66: a distinct union
    /// needs named columns to compare rows for deduplication), a plain
    /// `UNION` (not `UNION ALL`) can never omit RETURN, even consistently
    /// -- CIP-63 verified against Neo4j that EXISTS has no such
    /// restriction, since existence doesn't care about column identity, so
    /// callers checking an EXISTS body pass `false`; (3) when every branch
    /// does have a RETURN, they must all project the same column names,
    /// compared as a name multiset (order is irrelevant: the executor
    /// reorders each branch's rows onto the head's columns) -- the general
    /// (pre-existing) `DifferentColumnsInUnion` rule, which a UNION that
    /// failed to parse at all inside these subqueries had been trivially
    /// "satisfying" before.
    pub(in crate::parser) fn check_subquery_union_consistency(
        &self,
        stmt: &Statement,
        reject_omitted_distinct: bool,
    ) -> ParseResult<()> {
        use relate_model::name_list::same_name_multiset;

        if stmt.union.is_none() {
            return Ok(());
        }

        // A branch's result shape. CIP-135: a `{ WHEN ... }`/`{ WHEN ...
        // ELSE ... }` arm has no direct `Clause::Return` -- its shape is
        // whatever its branch bodies project (all branches agree, enforced
        // by the conditional's own rules), so recursing on the first one
        // stands in for the whole clause. Mirrors the front's
        // `union_branch_shape`, which owns the same classification for
        // statements the parser does not screen.
        enum BranchShape {
            Cols(Vec<String>),
            /// `RETURN *` -- columns unknowable until execution.
            Wildcard,
            /// No RETURN clause at all (FINISH / write-only arm).
            Omitted,
        }
        fn branch_shape(s: &Statement) -> BranchShape {
            let body = s.projection_stage();
            if let [Clause::Conditional(cc)] = body.clauses.as_slice() {
                let inner = cc
                    .branches
                    .first()
                    .map(|b| &b.body)
                    .or(cc.else_body.as_ref());
                return match inner {
                    Some(inner) => branch_shape(inner),
                    None => BranchShape::Omitted,
                };
            }
            let Some(Clause::Return(rc)) = body.clauses.last() else {
                // An arm whose final clause is a CALL subquery exports that
                // body's columns (plus the in-scope names before it), so the
                // column set is only known at execution: a wildcard. A unit
                // subquery call with no RETURN leaves the arm columnless.
                if let Some(Clause::CallSubquery(csq)) = body.clauses.last() {
                    if csq.statement.projects_columns() {
                        return BranchShape::Wildcard;
                    }
                }
                return BranchShape::Omitted;
            };
            let mut cols = Vec::with_capacity(rc.items.len());
            for item in &rc.items {
                if item.all {
                    return BranchShape::Wildcard;
                }
                if let Some(alias) = &item.alias {
                    cols.push(alias.to_string());
                } else if let Expr::Variable(v) = &item.expr {
                    cols.push(v.to_string());
                } else if let Some(src) = &item.source_text {
                    cols.push(src.clone());
                } else {
                    cols.push(String::new());
                }
            }
            BranchShape::Cols(cols)
        }

        let mut branches: Vec<&Statement> = vec![stmt];
        let mut distinct_link_seen = false;
        let mut current = &stmt.union;
        while let Some(u) = current {
            if !u.all {
                distinct_link_seen = true;
            }
            branches.push(&u.statement);
            current = &u.statement.union;
        }

        let shapes: Vec<BranchShape> = branches.iter().map(|s| branch_shape(s)).collect();
        let any_missing = shapes.iter().any(|s| matches!(s, BranchShape::Omitted));
        let any_present = shapes.iter().any(|s| !matches!(s, BranchShape::Omitted));

        if any_missing && any_present {
            return Err(self.error(
                "DifferentColumnsInUnion: all UNION branches must consistently include or omit RETURN",
            ));
        }
        if any_missing && distinct_link_seen && reject_omitted_distinct {
            return Err(self.error(
                "InvalidUnionOmittedReturn: a distinct UNION requires an explicit RETURN in every branch",
            ));
        }
        if !any_missing {
            let mut reference: Option<Vec<String>> = None;
            for shape in &shapes {
                if let BranchShape::Cols(cols) = shape {
                    match &reference {
                        None => reference = Some(cols.clone()),
                        Some(r) if !same_name_multiset(r, cols) => {
                            return Err(self.error(
                                "DifferentColumnsInUnion: all UNION branches must have the same column names",
                            ));
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// `COLLECT { <RegularQuery> } (CIP-71): mirrors `parse_count_subquery`'s
    /// full-statement path (no bare-pattern shorthand -- COLLECT always
    /// requires a full `<RegularQuery>` ending in `RETURN`). Enforces the
    /// doc's syntactic restrictions that are checkable on the parsed shape
    /// alone: every branch of the (possibly UNION-chained) inner query must
    /// end with a `RETURN` clause projecting exactly one non-`*` item.
    /// Restrictions that require scope-aware analysis (no updating clauses,
    /// no shadowing outer variables, inner variables not leaking outward)
    /// are deferred to the eval/validation batch that adds COLLECT
    /// evaluation -- pure re-use of `EvalContext`/`validate.rs` machinery,
    /// not a parsing concern.
    #[inline(never)]
    pub(in crate::parser) fn parse_collect_subquery(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume {
        if self.peek_type() == TokenType::RightBrace {
            return Err(self.error("A COLLECT subquery must end with a single return column"));
        }
        let stmt = self.parse_statement_block()?;
        self.expect(TokenType::RightBrace)?;
        self.check_collect_return_shape(&stmt)?;
        Ok(Expr::CollectSubquery(Box::new(CollectSubqueryExpr {
            statement: stmt,
        })))
    }

    /// Checks that `stmt` and every UNION-chained branch after it ends in a
    /// `RETURN` clause with exactly one item and that the item isn't `*`.
    pub(in crate::parser) fn check_collect_return_shape(
        &self,
        stmt: &Statement,
    ) -> ParseResult<()> {
        let bad_shape = || self.error("A COLLECT subquery must end with a single return column");
        let mut current = stmt;
        loop {
            // CIP-213: the "must end with a single return column" rule
            // applies to this arm's *true* terminal stage -- see
            // [`Statement::tail_stage`].
            let tail = current.tail_stage();
            match tail.clauses.last() {
                Some(Clause::Return(rc)) if rc.items.len() == 1 && !rc.items[0].all => {}
                _ => return Err(bad_shape()),
            }
            match &current.union {
                Some(next) => current = &next.statement,
                None => return Ok(()),
            }
        }
    }
}
