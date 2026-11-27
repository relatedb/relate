//! The token-fact vocabulary (`token::facts`, REL8-1423): one token
//! type's bracket-relevant fact — whether it can end an expression,
//! requires an operand after it, or carries neither fact — classified in
//! one exhaustive match over `TokenType` so a variant added there fails
//! to compile here, never silently falling through to "nothing" (the
//! permanent cache-miss class the bracket classification exists to
//! close). The vocabulary is generic `TokenType` kinding, not
//! bracket-specific, so it lives under the token module with the other
//! kind predicates; the one consumer of the two methods is the bracket
//! classification (`bracket_role::bracket_opens_list_group`), whose
//! module doc carries the history.

use crate::token::TokenType;

/// One token type's bracket fact — exactly one per variant (the module
/// header carries the compile-failure rationale). The third position,
/// heading a relationship pattern (`-`, `<-`), is
/// [`TokenType::is_relationship_start`]'s, the token layer's canonical
/// predicate consulted directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenFact {
    /// The token ends an expression.
    EndsExpression,
    /// The token requires an operand after it.
    IntroducesOperand,
    /// The token neither ends an expression nor requires an operand.
    Neither,
}

/// The [`TokenFact`] of one token type — the exhaustive classification,
/// every variant in exactly one arm.
fn token_facts(ty: TokenType) -> TokenFact {
    match ty {
        // Ends an expression: value operands, closers, the keyword
        // literal atoms, a `CASE`'s `END`.
        TokenType::Identifier
        | TokenType::String
        | TokenType::InterpString
        | TokenType::Integer
        | TokenType::Float
        | TokenType::ObfuscatedLiteral
        | TokenType::Parameter
        | TokenType::RightParen
        | TokenType::RightBracket
        | TokenType::RightBrace
        | TokenType::TRUE
        | TokenType::FALSE
        | TokenType::NULL
        | TokenType::INFINITY
        | TokenType::NAN
        | TokenType::END => TokenFact::EndsExpression,
        // Introduces an operand: property access, the grouping openers
        // and separators, the arithmetic/string/pipe operators, the
        // comparison family, the operand-requiring keywords. The
        // exception here: `-` is a binary subtraction (its pattern-head
        // half is [`TokenType::is_relationship_start`]'s, consulted
        // directly by the classification).
        TokenType::Dot
        | TokenType::LeftParen
        | TokenType::LeftBracket
        | TokenType::LeftBrace
        | TokenType::Comma
        | TokenType::Colon
        | TokenType::FatArrow
        | TokenType::Dash
        // The comparison family (equality included — assignment's `=` is
        // one of them): the seven variants of the one set-and-spelling
        // table, [`TokenType::comparison_spelling`]; the pin iterates
        // that table, so the set cannot drift from the spellings.
        | TokenType::Equals
        | TokenType::NotEquals
        | TokenType::LessThan
        | TokenType::GreaterThan
        | TokenType::LessEqual
        | TokenType::GreaterEqual
        | TokenType::RegexMatch
        | TokenType::Plus
        | TokenType::Star
        | TokenType::Slash
        | TokenType::Percent
        | TokenType::Caret
        | TokenType::Pipe
        | TokenType::DoublePipe
        | TokenType::AND
        | TokenType::OR
        | TokenType::XOR
        | TokenType::NOT
        | TokenType::IN
        | TokenType::IS
        | TokenType::STARTS
        | TokenType::ENDS
        | TokenType::CONTAINS
        | TokenType::CASE
        | TokenType::WHEN
        | TokenType::THEN
        | TokenType::ELSE
        | TokenType::RETURN
        | TokenType::WHERE
        | TokenType::WITH
        | TokenType::UNWIND
        | TokenType::AS
        | TokenType::BY
        | TokenType::ON
        | TokenType::SKIP
        | TokenType::LIMIT
        | TokenType::DISTINCT
        | TokenType::YIELD => TokenFact::IntroducesOperand,
        // Neither: the pattern heads' `<-` (their canonical predicate is
        // [`TokenType::is_relationship_start`]), the stream terminators,
        // the annotations and pattern punctuation, and the clause
        // keywords that start patterns or clauses. Enumerated, not `_`,
        // so a variant added to `TokenType` lands here by name and a
        // maintainer decides.
        TokenType::EOF
        | TokenType::Error
        | TokenType::At
        | TokenType::DoubleColon
        | TokenType::Semicolon
        | TokenType::ArrowRight
        | TokenType::DotDot
        | TokenType::Bang
        | TokenType::Ampersand
        | TokenType::MATCH
        | TokenType::OPTIONAL
        | TokenType::CREATE
        | TokenType::INSERT
        | TokenType::MERGE
        | TokenType::DELETE
        | TokenType::DETACH
        | TokenType::SET
        | TokenType::REMOVE
        | TokenType::ORDER
        | TokenType::ASC
        | TokenType::DESC
        | TokenType::FOREACH
        | TokenType::CALL
        | TokenType::UNION
        | TokenType::ALL
        | TokenType::INDEX
        | TokenType::CONSTRAINT
        | TokenType::FOR
        | TokenType::REQUIRE
        | TokenType::UNIQUE
        | TokenType::EXISTS
        | TokenType::IF
        | TokenType::DROP
        | TokenType::VECTOR
        | TokenType::POINT
        | TokenType::OPTIONS
        | TokenType::NODE
        | TokenType::REL
        | TokenType::COUNT
        | TokenType::COLLECT
        | TokenType::LOAD
        | TokenType::FROM
        | TokenType::CSV
        | TokenType::REDUCE
        | TokenType::SPATIAL
        // `<-`, the other pattern head, carries no fact (the canonical
        // predicate owns the pair).
        | TokenType::ArrowLeft
        | TokenType::EXPLAIN
        | TokenType::PROFILE => TokenFact::Neither,
    }
}

impl TokenType {
    /// Whether a token of this type can **end an expression** — the
    /// production read of [`token_facts`], the one classification.
    pub(crate) fn ends_expression(self) -> bool {
        token_facts(self) == TokenFact::EndsExpression
    }

    /// Whether a token of this type **requires an operand after it** —
    /// the production read of [`token_facts`], the one classification.
    pub(crate) fn introduces_operand(self) -> bool {
        token_facts(self) == TokenFact::IntroducesOperand
    }
}

#[cfg(test)]
mod tests;
