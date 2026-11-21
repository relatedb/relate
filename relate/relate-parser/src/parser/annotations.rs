//! CIP-281 clause annotations: `@name(params) <clause>`.
//!
//! Under the `cypher-annotations` preview gate the lexer emits
//! [`TokenType::At`]; this module parses each annotation that precedes a
//! clause, checks it, and discards it -- an annotation has no runtime effect
//! and no AST node. Disabled, the lexer never emits `At`, so none of this
//! runs and `@` is the ordinary unexpected-character error.
//!
//! The checks, all rejections raised as `InvalidAnnotation`:
//! - the parameters are literals only: a boolean, string, integer or float
//!   (an integer or float may carry a unary sign), a list of those, or a map
//!   of those and lists of those -- never a nested map, `null`, an
//!   expression, a variable or a query parameter;
//! - one parameter may be positional or named; two or more must all be named;
//! - names are unique within one annotation;
//! - an annotation directly precedes a clause: one followed by `ORDER BY`,
//!   `SKIP`, `OFFSET`, `LIMIT` or `UNION` is rejected.

use super::*;
use std::collections::HashSet;

impl<'a> Parser<'a> {
    /// Consumes every annotation at the current position (none when the
    /// gate is closed: the lexer never produces the introducer then).
    pub(super) fn skip_clause_annotations(&mut self) -> ParseResult<()> {
        let mut seen = false;
        while self.peek_type() == TokenType::At {
            self.parse_annotation()?;
            seen = true;
        }
        if seen && (self.peek_page_stage_start().is_some() || self.peek_type() == TokenType::UNION)
        {
            // An annotation here would end the WITH/RETURN before it and
            // re-read its ORDER BY / SKIP / LIMIT as a standalone paging
            // clause, silently changing what the query means.
            return Err(self.error(
                "InvalidAnnotation: an annotation must directly precede a clause, not ORDER BY, SKIP, OFFSET, LIMIT or UNION",
            ));
        }
        Ok(())
    }

    fn parse_annotation(&mut self) -> ParseResult<()> {
        self.next_token(); // consume @
        self.parse_schema_name("annotation name")?;
        self.expect(TokenType::LeftParen)?;
        let mut named: HashSet<SmallName> = HashSet::new();
        let mut positional = 0usize;
        if self.peek_type() != TokenType::RightParen {
            loop {
                let is_named =
                    self.peek().ty.is_schema_name() && self.peek_at(1).ty == TokenType::Colon;
                if is_named {
                    let name = self.parse_schema_name("annotation parameter name")?;
                    if !named.insert(name.clone()) {
                        return Err(self.error(format!(
                            "InvalidAnnotation: parameter name `{name}` is used twice in one annotation"
                        )));
                    }
                    self.expect(TokenType::Colon)?;
                } else {
                    positional += 1;
                }
                let value_tok = self.peek().clone();
                let value = self.parse_expression()?;
                if !is_annotation_value(&value) {
                    return Err(self.error_at_token(
                        &value_tok,
                        "InvalidAnnotation: an annotation parameter must be a literal \
                         (boolean, string, integer, float, or a list or map of those; \
                         no nested maps, expressions or variables)",
                    ));
                }
                if self.peek_type() == TokenType::Comma {
                    self.next_token();
                    continue;
                }
                break;
            }
        }
        if positional > 0 && positional + named.len() > 1 {
            return Err(self.error(
                "InvalidAnnotation: an annotation with two or more parameters must name all of them",
            ));
        }
        self.expect(TokenType::RightParen)?;
        Ok(())
    }
}

/// Whether `expr` is a literal an annotation parameter may carry: a scalar,
/// a list of scalars and lists, or a map of those.
fn is_annotation_value(expr: &Expr) -> bool {
    match expr {
        Expr::Map(pairs) => pairs.iter().all(|p| is_annotation_element(&p.value)),
        other => is_annotation_element(other),
    }
}

/// Whether `expr` may sit inside a list or map parameter: the same literals,
/// without a map (no nested maps).
fn is_annotation_element(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(LiteralValue::Null) => false,
        Expr::Literal(_) => true,
        Expr::Unary(u) => {
            (u.op == MINUS || u.op == PLUS)
                && matches!(
                    u.expr,
                    Expr::Literal(LiteralValue::Integer(_) | LiteralValue::Float(_))
                )
        }
        Expr::List(items) => items.iter().all(is_annotation_element),
        _ => false,
    }
}
