//! The token-fact pins: the vocabularies of [`super`]'s classification,
//! one test per fact, each owning its positive and negative lists.

use super::{token_facts, TokenFact};
use crate::token::TokenType;

/// The operand-ending set: the value tokens, the closers, the atoms,
/// END — and nothing else.
#[test]
fn the_operand_ending_set_is_pinned() {
    for ty in [
        TokenType::Identifier,
        TokenType::String,
        TokenType::InterpString,
        TokenType::Integer,
        TokenType::Float,
        TokenType::ObfuscatedLiteral,
        TokenType::Parameter,
        TokenType::RightParen,
        TokenType::RightBracket,
        TokenType::RightBrace,
        // The keyword literal atoms (CIP-55): the keyword-spelled
        // operands the expression parser's literal arms read.
        TokenType::TRUE,
        TokenType::FALSE,
        TokenType::NULL,
        TokenType::INFINITY,
        TokenType::NAN,
        TokenType::END,
    ] {
        assert!(ty.ends_expression(), "{ty:?} ends an expression");
    }
    for ty in [
        TokenType::RETURN,
        TokenType::THEN,
        TokenType::IN,
        TokenType::Dot,
        TokenType::Comma,
        TokenType::Plus,
    ] {
        assert!(!ty.ends_expression(), "{ty:?} does not end an expression");
    }
}

/// The operand-introducing set: access, openers, separators, the
/// operators — the comparison family through its one table — and the
/// operand-requiring keywords.
#[test]
fn the_operand_introducing_set_is_pinned() {
    // The comparison family, iterated over its one enumerable home
    // (`token::COMPARISON_SPELLINGS`): every variant the table spells
    // is an introducer, so an eighth comparison variant added there
    // is an introducer here with no list to update. (The lookup is a
    // first-match scan, so the pin also holds the table's own
    // uniqueness: one row per variant.)
    let rows = crate::token::COMPARISON_SPELLINGS;
    for (index, (ty, _)) in rows.iter().enumerate() {
        assert_eq!(
            rows.iter().position(|(row, _)| row == ty),
            Some(index),
            "{ty:?} has exactly one table row"
        );
        assert!(ty.introduces_operand(), "{ty:?} introduces an operand");
    }
    for ty in [
        TokenType::Dot,
        TokenType::LeftParen,
        TokenType::LeftBracket,
        TokenType::LeftBrace,
        TokenType::Comma,
        TokenType::Colon,
        TokenType::FatArrow,
        TokenType::Plus,
        TokenType::Dash,
        TokenType::Star,
        TokenType::Slash,
        TokenType::Percent,
        TokenType::Caret,
        TokenType::Pipe,
        TokenType::DoublePipe,
        TokenType::AND,
        TokenType::OR,
        TokenType::XOR,
        TokenType::NOT,
        TokenType::IN,
        TokenType::IS,
        TokenType::STARTS,
        TokenType::ENDS,
        TokenType::CONTAINS,
        TokenType::CASE,
        TokenType::WHEN,
        TokenType::THEN,
        TokenType::ELSE,
        TokenType::RETURN,
        TokenType::WHERE,
        TokenType::WITH,
        TokenType::UNWIND,
        TokenType::AS,
        TokenType::BY,
        TokenType::ON,
        TokenType::SKIP,
        TokenType::LIMIT,
        TokenType::DISTINCT,
        TokenType::YIELD,
    ] {
        assert!(ty.introduces_operand(), "{ty:?} introduces an operand");
    }
    for ty in [
        TokenType::MATCH,
        TokenType::CREATE,
        TokenType::SET,
        TokenType::END,
        TokenType::Identifier,
        TokenType::Integer,
        TokenType::RightParen,
    ] {
        assert!(
            !ty.introduces_operand(),
            "{ty:?} does not introduce an operand"
        );
    }
}

/// This module's side of the pattern-head split: the canonical
/// predicate (`is_relationship_start`) owns the pair; `-` still
/// introduces (binary subtraction), `<-` carries neither fact.
#[test]
fn the_pattern_head_split_is_pinned() {
    assert_eq!(token_facts(TokenType::Dash), TokenFact::IntroducesOperand);
    assert_eq!(token_facts(TokenType::ArrowLeft), TokenFact::Neither);
}
