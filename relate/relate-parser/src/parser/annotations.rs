//! CIP-281 clause annotations: `@name(params) <clause>`.
//!
//! Under the `cypher-annotations` preview gate the lexer emits
//! [`TokenType::At`]; this module parses each annotation that precedes a
//! clause, checks it, and discards it -- an annotation has no runtime effect
//! and no AST node. Disabled, the lexer never emits `At`, so none of this
//! runs and `@` is the ordinary unexpected-character error. The one
//! exception is ADR-0610's reserved `@txMetadata` (the in-statement
//! metadata channel): its payload is *captured* onto the statement
//! ([`ast::TxMetadataAnnotation`]) instead of discarded -- still no AST
//! node -- and what the capture then seeds is the front's gate decision
//! (`value-provenance`), never this module's.
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

/// ADR-0610's reserved annotation name: the in-statement metadata
/// channel, and the one name whose payload is captured rather than
/// discarded (see the module doc). Spelled exactly -- annotation names
/// are schema names, case-sensitive.
pub(crate) const TX_METADATA_ANNOTATION: &str = "txMetadata";

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
        let name = self.parse_schema_name("annotation name")?;
        // ADR-0610: the reserved channel name is captured rather than
        // discarded -- its payload survives for the front to gate and the
        // connection to funnel; every other name is checked and dropped
        // exactly as CIP-281 specifies.
        let reserved = name.with_str(|n| n == TX_METADATA_ANNOTATION);
        self.expect(TokenType::LeftParen)?;
        // One parameter list (a positional entry carries `None`) plus the
        // name set for the duplicate-name rule: the grammar check below
        // and the reserved name's capture read the same accumulation,
        // never parallel tallies recombined at each use.
        let mut named: HashSet<SmallName> = HashSet::new();
        let mut params: Vec<(Option<SmallName>, Expr)> = Vec::new();
        if self.peek_type() != TokenType::RightParen {
            loop {
                let is_named =
                    self.peek().ty.is_schema_name() && self.peek_at(1).ty == TokenType::Colon;
                let name = if is_named {
                    let name = self.parse_schema_name("annotation parameter name")?;
                    if !named.insert(name.clone()) {
                        return Err(self.error(format!(
                            "InvalidAnnotation: parameter name `{name}` is used twice in one annotation"
                        )));
                    }
                    self.expect(TokenType::Colon)?;
                    Some(name)
                } else {
                    None
                };
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
                params.push((name, value));
                if self.peek_type() == TokenType::Comma {
                    self.next_token();
                    continue;
                }
                break;
            }
        }
        if params.len() > 1 && params.iter().any(|(name, _)| name.is_none()) {
            return Err(self.error(
                "InvalidAnnotation: an annotation with two or more parameters must name all of them",
            ));
        }
        self.expect(TokenType::RightParen)?;
        if reserved {
            // The capture is the payload as parsed -- the two spellings of
            // one map ([`ast::TxMetadataAnnotation`]), split from the one
            // list: the grammar check above leaves a positional entry only
            // alone, so the list folds into exactly one spelling and no
            // overwrite is observable. Interpreting it (spelling the map,
            // consuming the type key) is the channel's job, at the
            // engine's connection, never the parser's.
            let mut named_pairs: Vec<(SmallName, Expr)> = Vec::new();
            let mut positional: Option<Expr> = None;
            for (name, value) in params {
                match name {
                    Some(name) => named_pairs.push((name, value)),
                    None => positional = Some(value),
                }
            }
            let payload = match positional {
                Some(expr) => TxMetadataAnnotation::Positional(expr),
                None => TxMetadataAnnotation::Named(named_pairs),
            };
            self.tx_metadata.push(payload);
        }
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
