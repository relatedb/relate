//! The parenthesized subpath group: `((...) [WHERE ...])` (CIP-14) and its
//! quantified form (CIP-28) — the one element-loop arm that can complete
//! the whole path.

use super::*;

impl<'a> Parser<'a> {
    /// Parses a parenthesized subpath group at the position of its outer
    /// `(` and adopts it into the enclosing `path`, mirroring the
    /// fill-in-place shape of the sibling bracket body
    /// (`Parser::parse_rel_bracket_body`): the group body (recursing
    /// through the pattern-body reader), its optional `WHERE`, the
    /// closing `)`, and — when a quantifier follows — the quantifier with
    /// the minimum-length and nesting checks, pushed onto `path` as one
    /// more element. An unquantified group is only legal as the sole
    /// factor of the whole path, so it is checked against `path` (nothing
    /// may precede it, and no element may follow it) and adopted as the
    /// path's entire body. Returns whether the group completed the whole
    /// path — `true`, the caller returns the path; `false`, the loop
    /// continues.
    ///
    /// `subpath_binding_allowed` is the 42N42 sub-path-binding rule,
    /// resolved once by the enclosing reader (the path is named, or the
    /// search is selective) and threaded unchanged through the group
    /// recursion; see the gate below.
    pub(super) fn parse_parenthesized_subpath(
        &mut self,
        subpath_binding_allowed: bool,
        path: &mut PatternPath,
    ) -> ParseResult<bool> {
        self.next_token(); // consume outer (

        // ADR-0257: nested parenthesized subpaths recurse here directly,
        // bypassing `parse_expression`, so each level must count
        // separately (LABS-69).
        let inner = self.with_expr_depth(|p| p.parse_pattern_path_body(subpath_binding_allowed))?;
        let where_expr = if self.peek_type() == TokenType::WHERE {
            self.next_token();
            Some(self.parse_expression()?)
        } else {
            None
        };
        self.expect(TokenType::RightParen)?;

        if self.peek_type().is_quantifier_start() {
            let quantifier = self.parse_quantifier()?;
            // The operand's minimum path length must be > 0 -- a single
            // node pattern cannot be quantified.
            let has_relationship = inner
                .elements
                .iter()
                .any(|e| matches!(e, PatternElement::Relationship(_)));
            if !has_relationship {
                return Err(self.error(
                    "MinimumPathLengthZero: the operand of a quantified path pattern must have a minimum path length greater than zero",
                ));
            }
            // Quantified path patterns cannot be nested, directly or
            // through concatenation.
            if contains_quantification(&inner.elements) {
                return Err(
                    self.error("NestedQuantification: quantified path patterns cannot be nested")
                );
            }
            path.elements
                .push(PatternElement::Quantified(QuantifiedPathPattern {
                    subpath_variable: inner.carried_name().cloned(),
                    elements: inner.elements,
                    where_expr,
                    quantifier,
                }));
            return Ok(false);
        }
        // Unquantified parenthesized pattern: only legal as the entire
        // pattern path (CIP-14 pre-existing feature) -- nothing may
        // precede or follow it.
        if !path.elements.is_empty() || self.peek_type().starts_pattern_element() {
            return Err(self.error(
                "UnquantifiedParenthesizedPattern: an unquantified parenthesized path pattern cannot be juxtaposed with other pattern elements",
            ));
        }
        // Neo4j 2026 (Cypher 25) supports naming a parenthesized group
        // only under a selective path search (ANY / SHORTEST...). Under
        // ALL, or with no selector (the implicit ALL), `(p = ...)` as the
        // whole path body is 42N42 "sub-path assignment is not
        // supported". The unnamed group stays legal in every selector,
        // and so does an outer path name (`p = ((:A)-->+(:B))`). Relate
        // keeps the CIP-28 sub-path form when the enclosing pattern
        // carries its own path name (`p = (q = …)`), an extension the
        // compatibility corpus does not pin. The name is read once,
        // through the same accessor the adoption below uses, so a name
        // adopted from a nested group trips the gate exactly when the
        // group's own written name does.
        let name = inner.carried_name().cloned();
        if name.is_some() && !subpath_binding_allowed {
            return Err(
                self.error("UnsupportedSubPathBinding: sub-path assignment is not supported")
            );
        }
        path.elements = inner.elements;
        path.where_expr = where_expr;
        // The group's own sub-path name stays in `subpath_variable` (not
        // `variable`, which is the enclosing `p = …` name). Keeping the
        // two apart lets the validator tell a group reading its *own*
        // name inside its `WHERE` (Neo4j-legal, `ANY SHORTEST (q = (…)
        // WHERE length(q) …)`) from an enclosing path variable read there
        // (42I21, `p = ((…) WHERE length(p) …)`): both bind the path
        // value, so `path_names()` is unchanged.
        path.subpath_variable = name;
        Ok(true)
    }
}
