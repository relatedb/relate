use super::*;

impl<'a> Parser<'a> {
    // ---- Expression parsing (precedence climbing) ----

    pub(super) fn parse_expression(&mut self) -> ParseResult<Expr> {
        // ADR-0257: every nested expression descends through here, so this
        // single counter bounds all expression recursion (LABS-69).
        self.with_expr_depth(Self::parse_or)
    }

    /// The `<expr> (, <expr>)*` production shared by `SHOW SETTINGS`
    /// `<names>`, `SHOW TRANSACTIONS` `<id-list>` and `TERMINATE
    /// TRANSACTIONS` `<id-list>` (CIP-68/CIP-84). Parses the first
    /// expression itself; the caller checks
    /// `peek_starts_show_filter_expr` first when the list is optional.
    pub(super) fn parse_comma_separated_exprs(&mut self) -> ParseResult<Vec<Expr>> {
        let mut items = vec![self.parse_expression()?];
        while self.peek_type() == TokenType::Comma {
            self.next_token();
            items.push(self.parse_expression()?);
        }
        Ok(items)
    }

    pub(super) fn parse_or(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_xor()?;
        while self.peek_type() == TokenType::OR {
            self.next_token();
            let right = self.parse_xor()?;
            left = Expr::Binary(Box::new(BinaryExpr {
                left,
                op: OR,
                right,
            }));
        }
        Ok(left)
    }

    pub(super) fn parse_xor(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_and()?;
        while self.peek_type() == TokenType::XOR {
            self.next_token();
            let right = self.parse_and()?;
            left = Expr::Binary(Box::new(BinaryExpr {
                left,
                op: XOR,
                right,
            }));
        }
        Ok(left)
    }

    pub(super) fn parse_and(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_not()?;
        while self.peek_type() == TokenType::AND {
            self.next_token();
            let right = self.parse_not()?;
            left = Expr::Binary(Box::new(BinaryExpr {
                left,
                op: AND,
                right,
            }));
        }
        Ok(left)
    }

    pub(super) fn parse_not(&mut self) -> ParseResult<Expr> {
        if self.peek_type() == TokenType::NOT {
            self.next_token();
            // ADR-0257: `NOT` chains recurse here directly, bypassing
            // `parse_expression`, so each step must count separately
            // (LABS-69).
            let expr = self.with_expr_depth(|p| p.parse_not())?;
            return Ok(Expr::Unary(Box::new(UnaryExpr { op: NOT, expr })));
        }
        self.parse_comparison()
    }

    pub(super) fn parse_comparison(&mut self) -> ParseResult<Expr> {
        let left = self.parse_string_null_check()?;

        let op = comparison_op(self.peek_type());
        if op.is_none() {
            return Ok(left);
        }
        let op = op.unwrap();
        self.next_token();
        let mut right = self.parse_string_null_check()?;
        let mut result = Expr::Comparison(Box::new(ComparisonExpr {
            left,
            op,
            right: right.clone(),
        }));

        // Chained comparisons
        loop {
            let next_op = comparison_op(self.peek_type());
            if next_op.is_none() {
                break;
            }
            let next_op = next_op.unwrap();
            self.next_token();
            let next_right = self.parse_string_null_check()?;
            result = Expr::Binary(Box::new(BinaryExpr {
                left: result,
                op: AND,
                right: Expr::Comparison(Box::new(ComparisonExpr {
                    left: right,
                    op: next_op,
                    right: next_right.clone(),
                })),
            }));
            right = next_right;
        }

        Ok(result)
    }

    /// Parses `ComparisonExpression6`: an `AddOrSubtractExpression` followed
    /// by *at most one* of `IN`/`STARTS WITH`/`ENDS WITH`/`CONTAINS`/`IS
    /// [NOT] NULL`/`IS [NOT] [<normal form>] NORMALIZED`/the CIP-87 type
    /// predicate (`IS [NOT] ::`, `IS [NOT] TYPED`, bare `::`). These
    /// alternatives do not chain: per CIP-87's "Precedence of Type
    /// Predicate Expression" section, combining any two of them without
    /// parentheses (e.g. `"ab" STARTS WITH "a" IS :: BOOLEAN`, or two type
    /// predicates back to back) is a compile-time syntax error, so this is
    /// a single optional suffix rather than a loop.
    pub(super) fn parse_string_null_check(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_add_sub()?;
        // Per CIP-87's precedence rules, a string/list predicate and an
        // IS-suffix (null/type/normalized check) do not combine without
        // parens: only try the IS-suffix if the first did not fire.
        match self.parse_in_or_string_match_suffix(left, Self::parse_add_sub)? {
            Suffix::Parsed(predicate) => return Ok(predicate),
            Suffix::Unmatched(operand) => left = operand,
        }

        match self.peek_type() {
            TokenType::IS => {
                self.next_token();
                let negated = if self.peek_type() == TokenType::NOT {
                    self.next_token();
                    true
                } else {
                    false
                };

                left = match self.parse_is_predicate_common(left, negated)? {
                    Suffix::Parsed(predicate) => predicate,
                    Suffix::Unmatched(operand) => {
                        self.parse_is_label_tail(operand, negated, IsLabelPolicy::General)?
                    }
                };
            }
            TokenType::DoubleColon => {
                // x :: <value type>  (CIP-87 bare shorthand for IS ::)
                self.next_token();
                let type_expr = self.parse_value_type()?;
                left = Expr::IsType(Box::new(IsTypeExpr {
                    expr: left,
                    type_expr,
                    negated: false,
                }));
            }
            TokenType::Colon => {
                // x:<label-expr>  (CIP-14, colon spelling of the label
                // predicate, interchangeable with `IS <label-expr>`).
                // CIP-190 moves this from `<postfix operator>` (tighter than
                // `+`) to here, `<advanced comparison predicate part 2>`
                // (looser than `+`): `[1,'abc',3] + n:A` now parses as
                // `([1,'abc',3] + n):A`, not `[1,'abc',3] + (n:A)`
                // (label-expression-precedence.feature [1]/[3]). Unlike the
                // pre-CIP-190 postfix version, `left` here is a whole
                // `<add or subtract expression>`, not necessarily a bare
                // variable -- but that's fine: SET/REMOVE's own `n:Label`
                // grammar is parsed by its own bespoke logic before ever
                // reaching general expression parsing, and
                // `parse_general_property_target` (SET's complex-target
                // fallback) calls `parse_postfix` directly, which no longer
                // handles `:` at all, so there's still no conflict.
                let label_expr = self.parse_label_spec(true)?.expr;
                left = Expr::HasLabel(Box::new(HasLabelExpr {
                    expr: left,
                    label_expr,
                    negated: false,
                    strict: false,
                }));
            }
            _ => {}
        }

        Ok(left)
    }

    /// True if the current token is the contextual `TYPED` keyword (like
    /// `NORMALIZED`, not a reserved word in the lexer).
    pub(super) fn peek_is_typed_keyword(&self) -> bool {
        self.peek_is_ident("TYPED")
    }

    /// The chronological comparison operator starting at the current token
    /// (Cypher 25): `BEFORE`, `AFTER`, or the two-token `SAME INSTANT` /
    /// `SAME REPRESENTATION`. `None` when the current token starts none of
    /// them -- a lone `SAME` (a variable so spelled) is not an operator
    /// without its second word.
    pub(super) fn peek_chronological_op(&self) -> Option<ChronologicalOp> {
        if self.peek_is_ident("BEFORE") {
            Some(ChronologicalOp::Before)
        } else if self.peek_is_ident("AFTER") {
            Some(ChronologicalOp::After)
        } else if self.peek_is_ident("SAME") {
            if self.peek_at_is_ident(1, "INSTANT") {
                Some(ChronologicalOp::SameInstant)
            } else if self.peek_at_is_ident(1, "REPRESENTATION") {
                Some(ChronologicalOp::SameRepresentation)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Consume the one or two contextual identifier words of the
    /// chronological operator `op` ([`Self::peek_chronological_op`]
    /// reported it).
    pub(super) fn consume_chronological_op(&mut self, op: ChronologicalOp) {
        self.next_token();
        if matches!(
            op,
            ChronologicalOp::SameInstant | ChronologicalOp::SameRepresentation
        ) {
            self.next_token();
        }
    }

    /// Returns the canonical uppercase form name ("NFC"/"NFD"/"NFKC"/"NFKD")
    /// if the current token is one of the normal-form keywords, else None.
    pub(super) fn peek_normal_form_keyword(&self) -> Option<String> {
        let tok = self.peek();
        if tok.ty == TokenType::Identifier {
            let upper = tok.literal.to_ascii_uppercase();
            if matches!(upper.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
                return Some(upper);
            }
        }
        None
    }

    /// True if the current token starts a `[<normal form>] NORMALIZED` tail
    /// (used to distinguish `IS NORMALIZED`/`IS <form> NORMALIZED` from `IS NULL`).
    pub(super) fn peek_is_normal_form_or_normalized(&self) -> bool {
        if self.peek_normal_form_keyword().is_some() {
            return true;
        }
        self.peek_is_ident("NORMALIZED")
    }

    /// Consumes and returns a normal-form keyword if present; otherwise leaves
    /// the token stream untouched and returns None.
    pub(super) fn try_consume_normal_form_keyword(&mut self) -> Option<String> {
        let form = self.peek_normal_form_keyword();
        if form.is_some() {
            self.next_token();
        }
        form
    }

    /// Expects the current token to be a case-insensitive identifier keyword
    /// -- one that is not a reserved word in the lexer, e.g. `NORMALIZED`,
    /// or a word of the multi-word DDL/SHOW forms (`CURRENT GRAPH TYPE`,
    /// `DEFAULT DATABASE`, `CURRENT USER`) where only one spelling is valid
    /// at this position. `keyword` is written uppercase, as the error shows
    /// it.
    pub(super) fn expect_keyword_ident(&mut self, keyword: &str) -> ParseResult<()> {
        let tok = self.peek().clone();
        if tok.ty == TokenType::Identifier && tok.literal.eq_ignore_ascii_case(keyword) {
            self.next_token();
            Ok(())
        } else {
            Err(self.error_at_token(&tok, format!("expected {}, got {:?}", keyword, tok.literal)))
        }
    }

    /// The consume-and-validate gate for a command's kind word: the next
    /// token must be the `singular` keyword token itself, or an identifier
    /// spelled `singular_ident` or `plural` (CIP-68's singular/plural KIND
    /// acceptance). Serves the SHOW family's kind word (`FUNCTION(S)`,
    /// `INDEX(ES)`, `CONSTRAINT(S)`, ...) and TERMINATE's `TRANSACTION(S)`.
    /// Pass `TokenType::Identifier` as `singular` when the word has no
    /// reserved token of its own (`FUNCTION(S)`, `DEFINED`) -- the
    /// identifier-spelling check is then the whole gate. Anything else gets
    /// an `expected <expected>, got <literal>` error.
    pub(super) fn expect_kind_word(
        &mut self,
        singular: TokenType,
        singular_ident: &str,
        plural: Option<&str>,
        expected: &str,
    ) -> ParseResult<()> {
        let tok = self.next_token();
        let is_singular_token = singular != TokenType::Identifier && tok.ty == singular;
        let is_ident_spelling = tok.ty == TokenType::Identifier
            && (tok.literal.eq_ignore_ascii_case(singular_ident)
                || plural.is_some_and(|p| tok.literal.eq_ignore_ascii_case(p)));
        if !(is_singular_token || is_ident_spelling) {
            return Err(
                self.error_at_token(&tok, format!("expected {expected}, got {:?}", tok.literal))
            );
        }
        Ok(())
    }

    pub(super) fn parse_primary(&mut self) -> ParseResult<Expr> {
        let tok = self.peek().clone();

        match tok.ty {
            TokenType::Integer => {
                self.next_token();
                let val = parse_int_literal(&tok.literal)
                    .map_err(|error| self.int_literal_error(&tok, error, IntLiteralSite::Expression))?;
                Ok(Expr::Literal(LiteralValue::Integer(val)))
            }
            TokenType::Float => {
                self.next_token();
                let val: f64 = tok.literal.parse().map_err(|_| {
                    self.error_at_token(&tok, format!("invalid float literal: {}", tok.literal))
                })?;
                // A numeric literal (as opposed to the Infinity/NaN keyword
                // literals, which are their own token types) that overflows
                // to +-infinity is a compile-time error, not a valid way to
                // spell Infinity (`RETURN 1e400` is rejected
                // by live Neo4j with "numeric value out of range").
                if val.is_infinite() {
                    return Err(self
                        .error_at_token(
                            &tok,
                            "FloatingPointOverflow: float literal is too large".to_string(),
                        )
                        .with_detail("FloatingPointOverflow"));
                }
                Ok(Expr::Literal(LiteralValue::Float(val)))
            }
            TokenType::ObfuscatedLiteral => {
                // The Neo4j internal obfuscated literal (preview-gated at
                // the lexer): a dummy `false` that exists to pass semantic
                // analysis, never meant to execute.
                self.next_token();
                Ok(Expr::Literal(LiteralValue::Bool(false)))
            }
            TokenType::String => {
                self.next_token();
                Ok(Expr::Literal(LiteralValue::String(tok.literal.into_owned())))
            }
            TokenType::InterpString => {
                self.next_token();
                let parts = parse_interp_body(&tok.literal, self.options)
                    .map_err(|e| self.error_at_token(&tok, e).with_detail("UnexpectedSyntax"))?;
                Ok(Expr::StringInterp(parts))
            }
            TokenType::TRUE => {
                self.next_token();
                Ok(Expr::Literal(LiteralValue::Bool(true)))
            }
            TokenType::FALSE => {
                self.next_token();
                Ok(Expr::Literal(LiteralValue::Bool(false)))
            }
            TokenType::NULL => {
                self.next_token();
                Ok(Expr::Literal(LiteralValue::Null))
            }
            TokenType::INFINITY => {
                self.next_token();
                Ok(Expr::Literal(LiteralValue::Float(f64::INFINITY)))
            }
            TokenType::NAN => {
                self.next_token();
                Ok(Expr::Literal(LiteralValue::Float(f64::NAN)))
            }
            TokenType::Parameter => {
                self.next_token();
                Ok(Expr::Parameter(tok.literal.into_owned()))
            }
            TokenType::LeftBracket => self.parse_list_literal(),
            TokenType::LeftBrace => {
                // CIP-272: `{k : v IN map [WHERE p] | k' : v'}` and
                // `{item IN list [WHERE p] | k' : v'}` are map comprehensions;
                // anything else (including `{k: v IN m}`, whose value is a
                // genuine IN expression) stays a map literal.
                if let Some(mc) = self.try_parse_map_comprehension() {
                    return Ok(mc);
                }
                let pairs = self.parse_map_literal()?;
                Ok(Expr::Map(pairs))
            }
            TokenType::LeftParen => {
                if self.node_pattern_precedes_relationship() {
                    return self.parse_pattern_predicate();
                }
                self.next_token();
                // A parenthesized expression may carry a leading unary `+`:
                // the openCypher `oC_UnaryAddOrSubtractExpression` permits a
                // run of `PLUS`/`MINUS`, and the simple-CASE predicate
                // rendering spells the keyword reading `CONTAINS + 1` as
                // `CONTAINS (+1)` (Neo4j `KeywordPriority`). The bare form is
                // deliberately not accepted — ADR-0191's reference rows pin
                // `RETURN NOT + 1` and `RETURN DISTINCT + 1` as syntax errors.
                let expr = if self.peek_type() == TokenType::Plus {
                    self.next_token();
                    Expr::Unary(Box::new(UnaryExpr {
                        op: PLUS,
                        expr: self.parse_expression()?,
                    }))
                } else {
                    self.parse_expression()?
                };
                self.expect(TokenType::RightParen)?;
                Ok(Expr::Paren(Box::new(expr)))
            }
            TokenType::COUNT => {
                self.next_token();
                if self.peek_type() == TokenType::LeftBrace && !self.peek_brace_is_map_projection()
                {
                    return self.parse_count_subquery();
                }
                if self.peek_type() != TokenType::LeftParen {
                    return Ok(Expr::Variable(SmallName::from_name(&tok.literal)));
                }
                self.next_token(); // consume (
                if self.peek_type() == TokenType::Star {
                    self.next_token();
                    self.expect(TokenType::RightParen)?;
                    return Ok(Expr::CountStar);
                }
                let distinct = if self.peek_type() == TokenType::DISTINCT {
                    self.next_token();
                    true
                } else if self.all_is_set_quantifier() {
                    // ALL is the explicit (default) set-quantifier; semantically
                    // equivalent to omitting the quantifier entirely.
                    self.next_token();
                    false
                } else {
                    false
                };
                let arg = self.parse_expression()?;
                self.expect(TokenType::RightParen)?;
                Ok(Expr::FunctionCall(FunctionCallExpr {
                    name: SmallName::from_name("count"),
                    args: vec![arg],
                    distinct,
                }))
            }
            TokenType::COLLECT => {
                self.next_token();
                if self.peek_type() == TokenType::LeftBrace && !self.peek_brace_is_map_projection()
                {
                    return self.parse_collect_subquery();
                }
                if self.peek_type() != TokenType::LeftParen {
                    return Ok(Expr::Variable(SmallName::from_name(&tok.literal)));
                }
                self.next_token(); // consume (
                let distinct = if self.peek_type() == TokenType::DISTINCT {
                    self.next_token();
                    true
                } else if self.all_is_set_quantifier() {
                    self.next_token();
                    false
                } else {
                    false
                };
                let args = self.parse_arg_list()?;
                Ok(Expr::FunctionCall(FunctionCallExpr {
                    name: SmallName::from_name(&tok.literal),
                    args,
                    distinct,
                }))
            }
            TokenType::CASE => {
                // `case` can be an ordinary bound variable (CIP-190
                // near-ambiguities.feature [1]/[3]). The ambiguity is
                // resolved by the token after CASE, not by swallowing a
                // failure: when that token starts the CASE grammar (`WHEN`,
                // or a valid test-expression start for the simple form) an
                // error inside a recognised CASE is the real one and must
                // propagate (`IntegerOverflow` in `CASE WHEN …`, for
                // instance) instead of being replaced by a misleading
                // `unexpected token "WHEN"` from the identifier reading.
                // Only when the grammar was never entered (the token is a
                // terminator/postfix/clause keyword) is the identifier
                // reading taken, letting `parse_postfix` see any trailing
                // `<map projection>` (`case {name: case.name}`).
                if !self.case_grammar_entered() {
                    self.next_token(); // consume CASE as an identifier
                    return Ok(Expr::Variable(SmallName::from_name(&tok.literal)));
                }
                self.parse_case_expr()
            }
            TokenType::ALL => self.parse_quantifier_or_function(),
            TokenType::Identifier => {
                self.next_token();
                let name_lower = tok.literal.to_ascii_lowercase();

                // Quantifiers
                if (name_lower == "any" || name_lower == "none" || name_lower == "single")
                    && self.peek_type() == TokenType::LeftParen
                {
                    self.backup();
                    return self.parse_quantifier_or_function();
                }

                // allReduce(acc = init, x IN list | expr, predicate)
                if name_lower == "allreduce" && self.peek_type() == TokenType::LeftParen {
                    return self.parse_allreduce_expr();
                }

                // shortestPath / allShortestPaths
                if (name_lower == "shortestpath" || name_lower == "allshortestpaths")
                    && self.peek_type() == TokenType::LeftParen
                {
                    self.next_token(); // consume (
                    let pattern = self.parse_pattern_path()?;
                    self.expect(TokenType::RightParen)?;
                    return Ok(Expr::ShortestPath(Box::new(ShortestPathExpr {
                        pattern,
                        all: name_lower == "allshortestpaths",
                    })));
                }

                // normalize(input [, normalForm]) — the optional second argument
                // is a bare normal-form keyword (NFC/NFD/NFKC/NFKD), not an
                // ordinary expression (CIP-132), so it needs dedicated parsing.
                if name_lower == "normalize" && self.peek_type() == TokenType::LeftParen {
                    return self.parse_normalize_call();
                }

                // trim([[LEADING|TRAILING|BOTH] [trimCharacterString] FROM] input) —
                // the GQL TRIM special form (CIP-149) is a distinct grammar production
                // from an ordinary comma-separated argument list.
                if name_lower == "trim" && self.peek_type() == TokenType::LeftParen {
                    return self.parse_trim_call();
                }

                // PROPERTY_EXISTS(<element variable>, <property name>) (CIP-247) —
                // the second argument is a bare property-name identifier, not an
                // ordinary expression, so it needs dedicated parsing.
                if name_lower == "property_exists" && self.peek_type() == TokenType::LeftParen {
                    return self.parse_property_exists_call();
                }

                // Function call
                if self.peek_type() == TokenType::LeftParen {
                    self.next_token(); // consume (
                    let distinct = if self.peek_type() == TokenType::DISTINCT {
                        self.next_token();
                        true
                    } else if self.all_is_set_quantifier() {
                        // ALL is the explicit (default) set-quantifier; semantically
                        // equivalent to omitting the quantifier entirely.
                        self.next_token();
                        false
                    } else {
                        false
                    };
                    let args = self.parse_arg_list()?;
                    return Ok(Expr::FunctionCall(FunctionCallExpr {
                        name: SmallName::from_name(&tok.literal),
                        args,
                        distinct,
                    }));
                }

                Ok(Expr::Variable(SmallName::from_name(&tok.literal)))
            }
            TokenType::VECTOR => {
                self.next_token();
                if self.peek_type() == TokenType::LeftParen {
                    return self.parse_vector_call();
                }
                Ok(Expr::Variable(SmallName::from_name(&tok.literal)))
            }
            TokenType::NODE
            | TokenType::REL
            | TokenType::POINT
            | TokenType::INDEX
            | TokenType::SPATIAL
            | TokenType::FROM
            // CIP-187: `options` is a real SHOW INDEXES/SHOW CONSTRAINTS
            // YIELD column name (e.g. `SHOW CONSTRAINTS YIELD options
            // RETURN options`), so it must be usable as a bare variable
            // reference like the other non-reserved schema/DDL keywords
            // above, not just in alias/YIELD-name position.
            | TokenType::OPTIONS
            // CIP-123: `constraint` (and similarly-shaped schema keywords)
            // is a non-reserved word usable as an ordinary `YIELD ... AS
            // constraint` alias / variable reference, e.g. when composing
            // `SHOW CONSTRAINTS YIELD name AS constraint WHERE constraint
            // = ...` -- same treatment as `TokenType::INDEX` above.
            | TokenType::CONSTRAINT
            // `RETURN` is a non-reserved word usable
            // as an ordinary variable reference (e.g. `RETURN RETURN AS
            // r`, where the second token names the variable bound by an
            // earlier `WITH ... AS RETURN`) -- reached here only when a
            // primary expression was already expected, so a `RETURN`
            // starting a fresh top-level clause is never affected.
            | TokenType::RETURN => {
                self.next_token();
                if self.peek_type() == TokenType::LeftParen {
                    self.next_token(); // consume (
                    let args = self.parse_arg_list()?;
                    return Ok(Expr::call(&tok.literal, args));
                }
                Ok(Expr::Variable(SmallName::from_name(&tok.literal)))
            }
            TokenType::EXISTS => {
                self.next_token();
                if self.peek_type() == TokenType::LeftBrace && !self.peek_brace_is_map_projection()
                {
                    return self.parse_exists_subquery();
                }
                if self.peek_type() == TokenType::LeftParen {
                    self.next_token();
                    let args = self.parse_arg_list()?;
                    return Ok(Expr::call("exists", args));
                }
                Ok(Expr::Variable(SmallName::from_name(&tok.literal)))
            }
            // ADR-0191: the reduce form only applies when `(` follows; a
            // bare `REDUCE` is a plain variable reference owned by the
            // schema-name fallthrough below. The form itself (and its
            // accumulator/loop variables) is parsed by `parse_reduce_expr`,
            // which consumes REDUCE.
            TokenType::REDUCE if self.peek_at(1).ty == TokenType::LeftParen => {
                self.parse_reduce_expr()
            }
            _ if tok.ty.is_schema_name() => {
                // ADR-0191: the primary-expression variable read -- any
                // keyword token that reached this fallthrough is a variable
                // reference, spelling kept as written. Every arm above has
                // priority: the TRUE/FALSE/NULL/INF/INFINITY/NAN literals,
                // prefix NOT, the CASE/COUNT/EXISTS/COLLECT/REDUCE/VECTOR
                // forms, the ALL quantifier/function, the contextual schema
                // words and the DISTINCT/ALL set quantifiers their callers
                // consume.
                self.next_token();
                Ok(Expr::Variable(SmallName::from_name(&tok.literal)))
            }
            _ => Err(self.error_at_token(
                &tok,
                format!(
                    "unexpected token {:?} ({:?}) at line {}, col {}",
                    tok.literal, tok.ty, tok.line, tok.col
                ),
            )),
        }
    }

    pub(super) fn parse_quantifier_or_function(&mut self) -> ParseResult<Expr> {
        let tok = self.next_token();
        let name_lower = tok.literal.to_ascii_lowercase();

        if self.peek_type() == TokenType::LeftParen {
            self.next_token(); // consume (
            let var_tok = self.peek().clone();
            // The quantifier variable is a symbolic name, so any keyword
            // spelling works unquoted too (ADR-0191).
            if var_tok.ty.is_schema_name() {
                self.next_token(); // consume variable
                if self.peek_type() == TokenType::IN {
                    self.next_token(); // consume IN
                    let list_expr = self.parse_expression()?;
                    // LABS-201: the GQL pipe spelling `any(x IN list | pred)`
                    // -- openCypher binds the predicate with WHERE. The pipe
                    // used to fall through to `expect(RightParen)` below and
                    // surface as a bare `expected RightParen, got |`; name
                    // the fix instead.
                    if self.peek_type() == TokenType::Pipe {
                        return Err(self.error(format!(
                            "a list predicate introduces its predicate with WHERE, not '|': \
                             write {name_lower}(x IN list WHERE predicate)"
                        )));
                    }
                    let predicate = if self.peek_type() == TokenType::WHERE {
                        self.next_token();
                        Some(self.parse_expression()?)
                    } else {
                        None
                    };
                    self.expect(TokenType::RightParen)?;
                    return Ok(Expr::Quantifier(Box::new(QuantifierExpr {
                        quantifier: name_lower,
                        variable: SmallName::from_name(&var_tok.literal),
                        list: list_expr,
                        predicate,
                    })));
                }
                // Not IN - backup variable
                self.backup();
            }
            // Not a quantifier shape (no `var IN <list>` prefix): parse as a
            // regular function call. The cursor sits after `(` at the first
            // argument on this path, whichever backup ran above.
            // LABS-201: `any((a)-[:R]->(b) | pred)` -- a pattern used
            // directly inside a list predicate. The pattern argument parses
            // as a pattern predicate and then the pipe fails
            // `expect(RightParen)` with the bare `expected RightParen, got |`
            // and no guidance. Detect the shape up front (the argument starts
            // with a node pattern and a pipe follows before the closing
            // paren) and point at the EXISTS requirement. Without the pipe
            // (e.g. `any((a)-[:R]->(b))`) the argument parses on and the
            // validator already reports `pattern predicates are only valid
            // in WHERE`, so this lookahead must not hijack it.
            if self.peek_type() == TokenType::LeftParen
                && self.node_pattern_precedes_relationship()
                && self.pipe_before_closing_paren()
            {
                return Err(self.error(
                    "a pattern cannot be used directly inside a list predicate: wrap it in \
                     EXISTS { ... } (e.g. EXISTS { (a)-[:R]->(b) }) or bind a pattern \
                     comprehension first (any(x IN [(a)-[:R]->(b) | b.prop] WHERE predicate))",
                ));
            }
            let distinct = if self.peek_type() == TokenType::DISTINCT {
                self.next_token();
                true
            } else {
                false
            };
            let args = self.parse_arg_list()?;
            return Ok(Expr::FunctionCall(FunctionCallExpr {
                name: SmallName::from_name(&tok.literal),
                args,
                distinct,
            }));
        }
        Ok(Expr::Variable(SmallName::from_name(&tok.literal)))
    }

    pub(super) fn parse_arg_list(&mut self) -> ParseResult<Vec<Expr>> {
        let mut args = Vec::new();
        if self.peek_type() == TokenType::RightParen {
            self.next_token();
            return Ok(args);
        }
        loop {
            args.push(self.parse_expression()?);
            if self.peek_type() != TokenType::Comma {
                break;
            }
            self.next_token();
        }
        self.expect(TokenType::RightParen)?;
        Ok(args)
    }

    pub(super) fn parse_pattern_predicate(&mut self) -> ParseResult<Expr> {
        let path = self.parse_pattern_path()?;
        Ok(Expr::PatternPredicate(Box::new(PatternPredicateExpr {
            pattern: path,
        })))
    }
}

mod collections;
mod postfix;
mod special_calls;
mod subquery;
