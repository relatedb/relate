//! The bracket classification (REL8-1423): whether a `[` opens a group
//! the normalizer may collapse as one list parameter. The operand
//! vocabulary it derives from is `crate::token::facts`'s — the inherent
//! `TokenType::ends_expression` / `TokenType::introduces_operand`, with
//! [`TokenType::is_relationship_start`] for the pattern heads. The
//! classification's one consumer is the normalizer's list-literal
//! collapse (`normalize::Pass::collapses_as_list_group`), and the
//! collapse algorithm and its history live there.

use crate::token::Token;

/// Whether the `[` that FOLLOWS `before` opens a group the normalizer may
/// **collapse as one list parameter** — the one fact the prefix can
/// support, and the only one the collapse's consumer
/// (`normalize::Pass::collapses_as_list_group`) asks. `before` is the
/// token stream up to (and excluding) the bracket, so the caller's index
/// arithmetic ends at the call; that the next token is a `[` is the
/// caller's contract, asserted at the call site where the token is in
/// hand.
///
/// The answer is the complement of the three predecessor positions whose
/// bracket must keep its brackets — the prefix cannot always name the
/// exact role (a `-` before the bracket is a relationship head OR binary
/// subtraction), but it never needs to: every excluded position keeps
/// brackets whichever role it is, and the exclusion is the decision:
///
/// - a **relationship-pattern head OR subtraction operator** (`-`, `<-`,
///   [`TokenType::is_relationship_start`]): `-[r:T]->`'s bracket is the
///   pattern's own, and `RETURN 1 - [1, 2]`'s is a subtraction's list —
///   the prefix cannot tell them apart, and both keep their brackets
///   (the subtraction's list renders with its brackets and its elements
///   normalize individually — a coarser template, never a broken one).
/// - a token that can **end an expression** ([`ends_expression`]): the
///   bracket is a subscript (`labels(n)[0]`, `xs[0]`, `[1, 2, 3][0]`,
///   `RETURN true[0]`, `RETURN CASE … END[0]`).
/// - a keyword in **operand position** ([`keyword_operand_position`]) —
///   the dialect's keyword-spelled variables (`AS count … RETURN
///   count[0]`, `RETURN RETURN[0]`, ADR-0169): the bracket is a
///   subscript.
///
/// Everything else — a clause word (`RETURN [1, 2]`, `x IN [1, 2]`,
/// `THEN [1]`), an operator, or nothing (the stream start) — leaves the
/// `[` opening a list literal the group scan may collapse.
pub(crate) fn bracket_opens_list_group(before: &[Token<'_>]) -> bool {
    let Some((prev, rest)) = before.split_last() else {
        // A bracket at the start of the stream opens a list literal:
        // nothing precedes it that could end an expression or a pattern
        // head.
        return true;
    };
    !prev.ty.is_relationship_start()
        && !prev.ty.ends_expression()
        && !(prev.ty.is_keyword() && keyword_operand_position(rest.last()))
}

/// Whether the keyword immediately before the bracket sits in operand
/// position — the keyword-spelled-variable test: the token before the
/// keyword, when there is one, is an operand-INTRODUCING token
/// ([`TokenType::introduces_operand`]), the same names ADR-0169 keeps
/// usable as identifiers. A keyword with nothing before it (the stream
/// start) is in clause position.
fn keyword_operand_position(introducer: Option<&Token<'_>>) -> bool {
    introducer.is_some_and(|token| token.ty.introduces_operand())
}

#[cfg(test)]
mod tests {
    use super::bracket_opens_list_group;
    use crate::lexer::tokenize;
    use crate::token::TokenType;

    /// The classification itself, one representative per position — the
    /// statement-level shapes live in the normalizer's suite; these pin
    /// the token-level decision.
    #[test]
    fn the_positions_classify_one_representative_each() {
        let opens_group = |src: &str| -> Vec<bool> {
            let tokens = tokenize(src);
            tokens
                .iter()
                .enumerate()
                .filter(|(_, tok)| tok.ty == TokenType::LeftBracket)
                .map(|(i, _)| bracket_opens_list_group(&tokens[..i]))
                .collect()
        };
        // Subscripts (keep brackets): identifier, keyword variable, atom,
        // closer.
        assert_eq!(opens_group("RETURN xs[0]"), vec![false]);
        assert_eq!(opens_group("WITH 1 AS count RETURN count[0]"), vec![false]);
        assert_eq!(opens_group("RETURN true[0]"), vec![false]);
        assert_eq!(opens_group("RETURN labels(n)[0]"), vec![false]);
        // Collapsible list groups: after a clause word, an operator word,
        // and the stream start (no predecessor at all).
        assert_eq!(opens_group("RETURN [1, 2]"), vec![true]);
        assert_eq!(
            opens_group("MATCH (n) WHERE n.x IN [1, 2] RETURN n"),
            vec![true]
        );
        assert_eq!(opens_group("[1, 2]"), vec![true]);
        // Pattern heads and subtraction (keep brackets — the prefix cannot
        // tell them apart, and both must): anonymous and named patterns,
        // and a subtraction's list literal.
        assert_eq!(opens_group("MATCH (a)-[]->(b)"), vec![false]);
        assert_eq!(opens_group("MATCH (a)-[r:T]->(b)"), vec![false]);
        assert_eq!(opens_group("RETURN 1 - [1, 2]"), vec![false]);
    }
}
