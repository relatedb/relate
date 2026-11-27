//! The IS-introduced predicate production shared by the general
//! comparison grammar and the simple-CASE `WHEN IS ...` operand.

use super::operators::{CONTAINS, ENDS_WITH, STARTS_WITH};
use super::*;

/// The outcome of one optional predicate-suffix production: it either
/// parsed (`Parsed`) or the next token starts none of its forms, in which
/// case the operand is handed back untouched for the caller's next try.
pub(super) enum Suffix {
    Parsed(Expr),
    Unmatched(Expr),
}

/// How a bare label after `IS [NOT]` (no `LABELED` keyword) is read.
///
/// The two grammar positions deliberately differ and a test pins both: in
/// the general grammar `x IS A` is the non-strict spelling (`IS NOT A` is
/// strict, the shorthand for `IS NOT LABELED`), while a simple-CASE
/// `WHEN IS A` operand is always strict.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum IsLabelPolicy {
    /// `x IS [NOT] ...` in the comparison grammar; a missing label is the
    /// missing-`NULL` error.
    General,
    /// `WHEN IS [NOT] ...` of a simple CASE.
    WhenOperand,
}

impl<'a> Parser<'a> {
    /// The IS-introduced predicate forms the general grammar and a simple
    /// CASE `WHEN IS ...` operand share (`IS` and any `NOT` already
    /// consumed): chronological comparison, `:: <type>`, `TYPED <type>`,
    /// `[<normal form>] NORMALIZED` and `NULL`. Hands `operand` back as
    /// [`Suffix::Unmatched`] when the next token starts none of them, so the caller can
    /// apply its own label-predicate tail.
    pub(super) fn parse_is_predicate_common(
        &mut self,
        operand: Expr,
        negated: bool,
    ) -> ParseResult<Suffix> {
        if let Some(op) = self.peek_chronological_op() {
            // a IS [NOT] BEFORE | AFTER | SAME INSTANT | SAME
            // REPRESENTATION b  (Cypher 25 chronological comparison; the
            // four words are contextual identifiers, not reserved).
            self.consume_chronological_op(op);
            let right = self.parse_add_sub()?;
            return Ok(Suffix::Parsed(Expr::ChronologicalCompare(Box::new(
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
            return Ok(Suffix::Parsed(Expr::IsType(Box::new(IsTypeExpr {
                expr: operand,
                type_expr,
                negated,
            }))));
        }
        if self.peek_is_normal_form_or_normalized() {
            // IS [NOT] [<normal form>] NORMALIZED
            let form = self.try_consume_normal_form_keyword();
            self.expect_keyword_ident("NORMALIZED")?;
            return Ok(Suffix::Parsed(Expr::IsNormalized(Box::new(
                IsNormalizedExpr {
                    expr: operand,
                    form,
                    negated,
                },
            ))));
        }
        if self.peek_type() == TokenType::NULL {
            // IS [NOT] NULL
            self.next_token();
            return Ok(Suffix::Parsed(Expr::IsNull(Box::new(IsNullExpr {
                expr: operand,
                negated,
            }))));
        }
        Ok(Suffix::Unmatched(operand))
    }

    /// The label tail of an `IS [NOT]` suffix that is none of
    /// [`Self::parse_is_predicate_common`]'s forms: `LABELED <label-expr>`
    /// (always strict) or a bare `<label-expr>` whose strictness `policy`
    /// decides (`IS NOT <label-expr>` is the shorthand for `IS NOT
    /// LABELED`). With no label in sight the general grammar reports the
    /// missing `NULL`; a CASE operand reports the label-expression error.
    pub(super) fn parse_is_label_tail(
        &mut self,
        operand: Expr,
        negated: bool,
        policy: IsLabelPolicy,
    ) -> ParseResult<Expr> {
        let labeled = self.peek_is_labeled_keyword();
        if labeled {
            self.next_token();
        } else if policy == IsLabelPolicy::General && !self.peek_can_start_label_expr() {
            let tok = self.next_token();
            return Err(self.expected_token_error(TokenType::NULL, &tok));
        }
        let strict = labeled
            || match policy {
                IsLabelPolicy::General => negated,
                IsLabelPolicy::WhenOperand => true,
            };
        let label_expr = self.parse_label_expr_body(true, false)?.expr;
        Ok(Expr::HasLabel(Box::new(HasLabelExpr {
            expr: operand,
            label_expr,
            negated,
            strict,
        })))
    }

    /// The `IN` / `STARTS WITH` / `ENDS WITH` / `CONTAINS` suffix of the
    /// comparison grammar and of a simple-CASE `WHEN` operand, with `rhs`
    /// parsing the right operand (the two positions differ only there).
    pub(super) fn parse_in_or_string_match_suffix(
        &mut self,
        left: Expr,
        rhs: fn(&mut Self) -> ParseResult<Expr>,
    ) -> ParseResult<Suffix> {
        let op = match self.peek_type() {
            TokenType::IN => {
                self.next_token();
                let list = rhs(self)?;
                return Ok(Suffix::Parsed(Expr::In(Box::new(InExpr {
                    expr: left,
                    list,
                }))));
            }
            TokenType::STARTS => STARTS_WITH,
            TokenType::ENDS => ENDS_WITH,
            TokenType::CONTAINS => CONTAINS,
            _ => return Ok(Suffix::Unmatched(left)),
        };
        if self.next_token().ty != TokenType::CONTAINS {
            self.expect(TokenType::WITH)?;
        }
        let right = rhs(self)?;
        Ok(Suffix::Parsed(Expr::StringMatch(Box::new(
            StringMatchExpr { left, op, right },
        ))))
    }
}
