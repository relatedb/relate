//! The IS-introduced predicate production shared by the general
//! comparison grammar and the simple-CASE `WHEN IS ...` operand.

use super::*;

impl<'a> Parser<'a> {
    /// The IS-introduced predicate forms the general grammar and a simple
    /// CASE `WHEN IS ...` operand share (`IS` and any `NOT` already
    /// consumed): chronological comparison, `:: <type>`, `TYPED <type>`,
    /// `[<normal form>] NORMALIZED` and `NULL`. Hands `operand` back as
    /// `Err` when the next token starts none of them, so the caller can
    /// apply its own label-predicate tail.
    pub(super) fn parse_is_predicate_common(
        &mut self,
        operand: Expr,
        negated: bool,
    ) -> ParseResult<Result<Expr, Expr>> {
        if let Some(op) = self.peek_chronological_op() {
            // a IS [NOT] BEFORE | AFTER | SAME INSTANT | SAME
            // REPRESENTATION b  (Cypher 25 chronological comparison; the
            // four words are contextual identifiers, not reserved).
            self.consume_chronological_op(op);
            let right = self.parse_add_sub()?;
            return Ok(Ok(Expr::ChronologicalCompare(Box::new(
                ChronologicalCompareExpr {
                    left: operand,
                    op,
                    right,
                    negated,
                },
            ))));
        }
        if self.peek_type() == TokenType::DoubleColon || self.peek_is_typed_keyword() {
            // IS [NOT] :: <value type> | IS [NOT] TYPED <value type>
            // (type predicate)
            self.next_token();
            let type_expr = self.parse_value_type()?;
            return Ok(Ok(Expr::IsType(Box::new(IsTypeExpr {
                expr: operand,
                type_expr,
                negated,
            }))));
        }
        if self.peek_is_normal_form_or_normalized() {
            // IS [NOT] [<normal form>] NORMALIZED
            let form = self.try_consume_normal_form_keyword();
            self.expect_keyword_ident("NORMALIZED")?;
            return Ok(Ok(Expr::IsNormalized(Box::new(IsNormalizedExpr {
                expr: operand,
                form,
                negated,
            }))));
        }
        if self.peek_type() == TokenType::NULL {
            // IS [NOT] NULL
            self.next_token();
            return Ok(Ok(Expr::IsNull(Box::new(IsNullExpr {
                expr: operand,
                negated,
            }))));
        }
        Ok(Err(operand))
    }

    /// The general grammar's tail for an `IS [NOT]` suffix that is none of
    /// [`Self::parse_is_predicate_common`]'s forms: `LABELED <label-expr>`
    /// (strict), a bare `<label-expr>` (`IS NOT <label-expr>` is the
    /// shorthand for `IS NOT LABELED`), else the missing-`NULL`
    /// error.
    pub(super) fn parse_is_label_tail(
        &mut self,
        operand: Expr,
        negated: bool,
    ) -> ParseResult<Expr> {
        if self.peek_is_labeled_keyword() {
            self.next_token();
            let label_expr = self.parse_label_expr_body(true, false)?.expr;
            return Ok(Expr::HasLabel(Box::new(HasLabelExpr {
                expr: operand,
                label_expr,
                negated,
                strict: true,
            })));
        }
        if self.peek_can_start_label_expr() {
            let label_expr = self.parse_label_expr_body(true, false)?.expr;
            return Ok(Expr::HasLabel(Box::new(HasLabelExpr {
                expr: operand,
                label_expr,
                negated,
                strict: negated,
            })));
        }
        self.expect(TokenType::NULL)?;
        Ok(Expr::IsNull(Box::new(IsNullExpr {
            expr: operand,
            negated,
        })))
    }
}
