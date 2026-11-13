//! The CASE expression family: the searched and simple forms, the
//! CIP-143/CIP-190 `WHEN` operand lists, and the CIP-190
//! `case`-as-variable lookahead. Split out of `expressions` so that
//! file stops growing with the CASE grammar (AGENTS.md file-size rule).

use super::expressions::BracketSet;
use super::*;
use crate::token::is_clause_boundary;

/// Tokens that end the enclosing projection item or clause at bracket depth
/// zero, so the simple-CASE lookahead can stop scanning there. The clause
/// half is the one shared boundary predicate ([`is_clause_boundary`],
/// covering `AS` and every clause keyword/starter); this adds only the
/// item-local terminals.
fn ends_case_test_scan(tok: &Token) -> bool {
    matches!(
        tok.ty,
        TokenType::EOF
            | TokenType::Semicolon
            | TokenType::Comma
            | TokenType::THEN
            | TokenType::ELSE
            | TokenType::END
            | TokenType::RETURN
            | TokenType::YIELD
    ) || is_clause_boundary(tok.ty)
}

impl<'a> Parser<'a> {
    /// Whether the `CASE` at the cursor opens the CASE grammar instead of
    /// naming an ordinary variable (CIP-190 near-ambiguities): exactly when
    /// a `WHEN` follows the test expression at bracket depth zero. The scan
    /// stops at the first token that ends the enclosing item/clause (the
    /// shared boundary predicate), so a `case` variable followed by any
    /// operator, postfix form or clause keeps the identifier reading; it is
    /// exact rather than length-capped, since an over-long operand is still
    /// a valid simple-CASE test. This is what lets both
    /// `CASE [1,2] WHEN …`/`CASE -1 WHEN …` (simple-form tests) and
    /// `case[0]`/`case - 1`/`case {name: …}` (variable uses) parse.
    pub(super) fn case_grammar_entered(&self) -> bool {
        let mut depth = 0usize;
        for tok in &self.tokens[self.pos + 1..] {
            if BracketSet::AllBrackets.opens(tok.ty) {
                depth += 1;
            } else if BracketSet::AllBrackets.closes(tok.ty) {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            } else if depth == 0 {
                if tok.ty == TokenType::WHEN {
                    return true;
                }
                if ends_case_test_scan(tok) {
                    return false;
                }
            }
        }
        false
    }

    pub(super) fn parse_case_expr(&mut self) -> ParseResult<Expr> {
        self.next_token(); // consume CASE

        // The simple form's test operand is lowered into each `WHEN` here
        // (`parse_simple_when_operand`), so no consumer downstream has to
        // carry a comparison-vs-truthy mode.
        let operand = if self.peek_type() != TokenType::WHEN {
            Some(self.parse_expression()?)
        } else {
            None
        };

        let mut whens = Vec::new();
        while self.peek_type() == TokenType::WHEN {
            self.next_token();
            let condition = match &operand {
                Some(operand) => self.parse_simple_when_operand(operand)?,
                None => self.parse_expression()?,
            };
            self.expect(TokenType::THEN)?;
            let result = self.parse_expression()?;
            whens.push(WhenClause { condition, result });
        }

        let else_expr = if self.peek_type() == TokenType::ELSE {
            self.next_token();
            Some(self.parse_expression()?)
        } else {
            None
        };

        self.expect(TokenType::END)?;
        Ok(Expr::Case(Box::new(CaseExpr { whens, else_expr })))
    }

    /// The right operand of a `WHEN <operand> CONTAINS/STARTS WITH/ENDS WITH
    /// <expr>` predicate: a leading `+` is the unary plus that renders the
    /// keyword reading of the source's `CONTAINS + 1` as `CONTAINS (+1)`
    /// (Neo4j `KeywordPriority`, the "CONTAINS +" example). Unary `+` stays
    /// out of the general expression grammar: `RETURN NOT + 1` and
    /// `RETURN DISTINCT + 1` are pinned syntax errors (ADR-0191's reference
    /// rows), so the completion is scoped to the predicate operand that needs
    /// it rather than widening the grammar.
    fn parse_string_match_rhs(&mut self) -> ParseResult<Expr> {
        if self.peek_type() != TokenType::Plus {
            return self.parse_add_sub();
        }
        self.next_token();
        let expr = self.parse_add_sub()?;
        Ok(Expr::Unary(Box::new(UnaryExpr { op: PLUS, expr })))
    }

    /// Parses a simple-CASE `WHEN`'s `<when operand>` (the `WHEN` already
    /// consumed): a comma-separated `<when operand list>` (CIP-143/CIP-190)
    /// whose elements each lower to a predicate on the CASE operand — a
    /// plain value to `operand = value`, a predicate form to itself. A list
    /// of two-or-more elements folds into one OR-ed predicate. Lowering every
    /// element (not just list members) is what lets the model store the
    /// searched form only.
    fn parse_simple_when_operand(&mut self, operand: &Expr) -> ParseResult<Expr> {
        let mut acc = self.parse_when_operand_element(operand)?;
        while self.peek_type() == TokenType::Comma {
            self.next_token();
            let predicate = self.parse_when_operand_element(operand)?;
            acc = Expr::Binary(Box::new(BinaryExpr {
                left: acc,
                op: OR,
                right: predicate,
            }));
        }
        Ok(acc)
    }

    /// One element of a simple-CASE `WHEN` operand list: a leading
    /// comparison operator (`WHEN < 1`, `WHEN =~ 'C.*t'`) or an
    /// IS/`:`/IN/CONTAINS/… predicate becomes a predicate over the CASE
    /// operand; anything else is a plain value, lowered here to the equality
    /// comparison the executor would otherwise build at evaluation time.
    fn parse_when_operand_element(&mut self, operand: &Expr) -> ParseResult<Expr> {
        // Decide the operator first — the leading comparison form consumes
        // its token, the plain-value form defaults to equality — then build
        // the comparison node once for both.
        let op = match comparison_op(self.peek_type()) {
            Some(op) => {
                self.next_token();
                op
            }
            None => match self.parse_when_operand_predicate(operand)? {
                Some(predicate) => return Ok(predicate),
                // A plain value lowers to the equality comparison — the
                // one comparison spelling with a build site that has no
                // token to read `comparison_op`'s table from, hence the
                // `EQUALS` constant.
                None => EQUALS,
            },
        };
        Ok(Expr::Comparison(Box::new(ComparisonExpr {
            left: operand.clone(),
            op,
            right: self.parse_expression()?,
        })))
    }

    /// Parses a simple-CASE WHEN operand that is a predicate on the CASE
    /// `operand` (the `WHEN` already consumed), or returns `None` without
    /// consuming anything when the WHEN operand is a plain value compared
    /// for equality with it.
    pub(super) fn parse_when_operand_predicate(
        &mut self,
        operand: &Expr,
    ) -> ParseResult<Option<Expr>> {
        let predicate = match self.peek_type() {
            TokenType::IS => {
                // Simple-CASE `WHEN IS ...` (CIP-249 label predicate, plus
                // the `<when operand>` completion covering
                // the other IS-introduced `<advanced comparison predicate
                // part 2>` forms): evaluated directly against the CASE
                // operand, not compared for equality with it.
                self.next_token(); // consume IS
                let negated = if self.peek_type() == TokenType::NOT {
                    self.next_token();
                    true
                } else {
                    false
                };
                let operand = operand.clone();
                if let Some(op) = self.peek_chronological_op() {
                    // WHEN IS [NOT] BEFORE | AFTER | SAME INSTANT | SAME
                    // REPRESENTATION ...  (Cypher 25): the postfix
                    // predicate form, evaluated against the CASE operand.
                    self.consume_chronological_op(op);
                    let right = self.parse_add_sub()?;
                    Expr::ChronologicalCompare(Box::new(ChronologicalCompareExpr {
                        left: operand,
                        op,
                        right,
                        negated,
                    }))
                } else if self.peek_type() == TokenType::DoubleColon {
                    // WHEN IS [NOT] :: <value type>  (CIP-87/CIP-143 type predicate)
                    self.next_token();
                    let type_expr = self.parse_value_type()?;
                    Expr::IsType(Box::new(IsTypeExpr {
                        expr: operand,
                        type_expr,
                        negated,
                    }))
                } else if self.peek_is_typed_keyword() {
                    // WHEN IS [NOT] TYPED <value type>
                    self.next_token();
                    let type_expr = self.parse_value_type()?;
                    Expr::IsType(Box::new(IsTypeExpr {
                        expr: operand,
                        type_expr,
                        negated,
                    }))
                } else if self.peek_is_normal_form_or_normalized() {
                    // WHEN IS [NOT] [<normal form>] NORMALIZED (CIP-133),
                    // matching the general grammar's `IS ... NORMALIZED`.
                    let form = self.try_consume_normal_form_keyword();
                    self.expect_keyword_ident("NORMALIZED")?;
                    Expr::IsNormalized(Box::new(IsNormalizedExpr {
                        expr: operand,
                        form,
                        negated,
                    }))
                } else if self.peek_type() == TokenType::NULL {
                    // WHEN IS [NOT] NULL
                    self.next_token();
                    Expr::IsNull(Box::new(IsNullExpr {
                        expr: operand,
                        negated,
                    }))
                } else {
                    // WHEN IS [NOT] [LABELED] <label-expr>  (CIP-249)
                    if self.peek_is_labeled_keyword() {
                        self.next_token();
                    }
                    let label_expr = self.parse_label_expr_body(true, false)?.expr;
                    Expr::HasLabel(Box::new(HasLabelExpr {
                        expr: operand,
                        label_expr,
                        negated,
                        strict: true,
                    }))
                }
            }
            TokenType::Colon => {
                // Generalized-CASE `WHEN :<label-expr>` (CIP-221): the colon
                // form of the same operand label predicate as `WHEN IS
                // <label-expr>` above.
                let label_expr = self.parse_label_spec(true)?.expr;
                Expr::HasLabel(Box::new(HasLabelExpr {
                    expr: operand.clone(),
                    label_expr,
                    negated: false,
                    strict: false,
                }))
            }
            TokenType::DoubleColon => {
                // Generalized-CASE `WHEN :: <value type>` (CIP-87): the bare
                // type predicate, same as `WHEN IS :: <value type>` above.
                self.next_token();
                let type_expr = self.parse_value_type()?;
                Expr::IsType(Box::new(IsTypeExpr {
                    expr: operand.clone(),
                    type_expr,
                    negated: false,
                }))
            }
            TokenType::IN => {
                // `<when operand>` completion: `WHEN IN
                // <list>` is `<operand> IN <list>` with the CASE operand as
                // the implicit LHS.
                self.next_token();
                let list = self.parse_add_sub()?;
                Expr::In(Box::new(InExpr {
                    expr: operand.clone(),
                    list,
                }))
            }
            TokenType::CONTAINS => {
                self.next_token();
                let right = self.parse_string_match_rhs()?;
                Expr::StringMatch(Box::new(StringMatchExpr {
                    left: operand.clone(),
                    op: CONTAINS,
                    right,
                }))
            }
            TokenType::STARTS => {
                self.next_token();
                self.expect(TokenType::WITH)?;
                let right = self.parse_string_match_rhs()?;
                Expr::StringMatch(Box::new(StringMatchExpr {
                    left: operand.clone(),
                    op: STARTS_WITH,
                    right,
                }))
            }
            TokenType::ENDS => {
                self.next_token();
                self.expect(TokenType::WITH)?;
                let right = self.parse_string_match_rhs()?;
                Expr::StringMatch(Box::new(StringMatchExpr {
                    left: operand.clone(),
                    op: ENDS_WITH,
                    right,
                }))
            }
            _ => return Ok(None),
        };
        Ok(Some(predicate))
    }
}
