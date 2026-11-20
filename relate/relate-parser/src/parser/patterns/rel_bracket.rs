//! The relationship pattern's bracket body: the variable, type spec,
//! legacy hop bounds, inline properties and element `WHERE` read between
//! the dashes of `-[ ... ]`.

use super::*;

impl<'a> Parser<'a> {
    /// Parses a relationship pattern's bracket body at the position of
    /// its `[` (the caller has already checked the token is one): the
    /// optional variable, the type spec, the legacy `*min..max` hop
    /// bounds, the property spec and element `WHERE`
    /// ([`Parser::parse_element_property_and_where`]), and the closing
    /// `]`. Fills `rel` in place. The legacy `:A|:B`-separator verdict
    /// waits until the variable, properties and hop bounds are all known,
    /// so it fires here, still inside the bracket; a malformed bracket (a
    /// stray `..`/`-`/token where the closing `]` is expected) is
    /// `SyntaxError: InvalidRelationshipPattern`, not the generic
    /// `UnexpectedSyntax`.
    pub(super) fn parse_rel_bracket_body(&mut self, rel: &mut RelPattern) -> ParseResult<()> {
        self.next_token(); // consume [

        // Variable. As with node patterns, a keyword spelling is a
        // relationship variable too (ADR-0191); a bare `IS` here is the
        // variable-less GPM label introducer (`-[IS R]->`) only when
        // the lookahead does not prove it is the variable.
        let peek = self.peek().clone();
        if self.peek_pattern_name_is_variable(0) {
            rel.variable = Some(SmallName::from_name(&peek.literal));
            self.next_token();
        }

        // Types (relationship `|` disjunction predates CIP-14, so a
        // bare top-level `|` here doesn't itself count as a GQL symbol
        // for the clause-level mixing check).
        // The legacy `:A|:B` separator is only legal for a bare type
        // list; with a variable binding, an inline property map or a
        // variable length Neo4j rejects it outright (HelpfulErrorMessages
        // "should provide sensible error message when using colon in the
        // separation of alternative relationship types in failing cases").
        // The fact comes back from the type-spec parse itself, so a
        // `HasLabel` or node-pattern parse of the same spelling cannot
        // leave it set behind this element.
        let mut legacy_pipe_colon = false;
        if matches!(self.peek_type(), TokenType::Colon | TokenType::IS) {
            // Same element-chain scope as the node arm: the
            // `{` of an inline rel property map is not a projection.
            // The scoped guard keeps the depth balanced across the `?`.
            let spec = self.with_pattern_element_depth(|p| p.parse_label_spec(true))?;
            rel.type_expr = Some(spec.expr);
            legacy_pipe_colon = spec.saw_legacy_pipe_colon;
        }
        // The element's variable, properties and hop bounds are read
        // after the type spec, so the verdict waits until they are all
        // known (below).

        // Variable length *min..max
        if self.peek_type() == TokenType::Star {
            let (min, max) = self.parse_legacy_hop_bounds()?;
            rel.min_hops = Some(min);
            rel.max_hops = max;
            // A negation/wildcard type expression on the legacy
            // bracket-syntax variable-length form is accepted: it is
            // the same AST the GPM postfix form `()-[:!A&!B]->*()`
            // normalizes to (CIP-28 Form 5), so rejecting the bracket
            // spelling alone is a syntax-gap, not a semantic rule
            // (Neo4j GpmSyntaxMixingAcceptance "Var-length relationship
            // with GPM relationship type expression - OK"; the CIP-40
            // quantifier-mixing [12] DEVIATION row captured the older
            // rejection and is retired with it).
        }

        // Property spec, then the element-pattern WHERE (CIP-18); the WHERE is
        // accepted on the legacy bracket-syntax variable-length form
        // (`[r*1..5 WHERE ...]`) too: that is the same AST the GPM postfix
        // form `-[r WHERE ...]->{1,5}` normalizes to, so the old
        // bracket-only rejection was a syntax-gap (Neo4j
        // GpmSyntaxMixingAcceptance "Var-length relationship with inline
        // predicate - OK"; the quantifier-mixing DEVIATION row
        // captured the older rejection and is retired with it).
        let tail = self.parse_element_property_and_where()?;
        rel.has_properties = tail.has_properties;
        rel.properties = tail.properties;
        rel.properties_param = tail.properties_param;
        rel.where_expr = tail.where_expr;

        if legacy_pipe_colon
            && (rel.variable.is_some() || rel.has_properties || rel.is_variable_length())
        {
            return Err(self.error(
                "InvalidRelationshipPattern: the semantics of using colon in the separation of alternative relationship types in conjunction with the use of variable binding, inlined property predicates, or variable length is no longer supported. Please separate the relationship types with a single bar between them instead",
            ));
        }

        // A malformed relationship bracket (a stray `..`/`-`/token where the
        // closing `]` is expected — Match4 [9] `[:LIKES..]`, [10]
        // `[:LIKES*-2]`) is `SyntaxError: InvalidRelationshipPattern`,
        // not the generic `UnexpectedSyntax`.
        self.expect_detail(TokenType::RightBracket, "InvalidRelationshipPattern")?;
        Ok(())
    }
}
