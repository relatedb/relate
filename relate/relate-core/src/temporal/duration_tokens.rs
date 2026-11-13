//! The one CIP-223 duration-pattern tokenizer, shared by the formatter
//! (`format/duration.rs`) and the parser (`duration_pattern.rs`).

use super::field::{DurationField, PatternSymbol};

#[derive(Debug, Clone)]
pub(super) enum Token {
    Literal(String),
    /// One run of the *same letter*: its symbol and the run's width. The
    /// parser keeps `yY` as two year tokens (a duplicate field); the
    /// formatter merges adjacent same-symbol runs ([`merge_symbol_runs`]).
    Symbol(PatternSymbol, usize),
}

/// A quote opened an escape the pattern never closed: Neo4j's
/// pattern-parsing failure. Each caller spells it in its own error family.
#[derive(Debug)]
pub(super) struct UnclosedEscape;

impl Token {
    /// The duration field this token names, if any — the token stream is the
    /// one record of which fields the pattern mentions.
    pub(super) fn field(&self) -> Option<DurationField> {
        match self {
            Token::Literal(_) => None,
            Token::Symbol(symbol, _) => Some(symbol.field()),
        }
    }
}

/// The pattern's token stream. Escaping follows Neo4j's
/// `DurationValue.format` state machine: a quote opens an escape, a quote
/// inside one closes it (emitting nothing), and a doubled quote inside one
/// emits a literal quote and stays escaped. A letter outside the duration
/// alphabet is literal text (Neo4j's `NOT_A_COMPONENT_TOKEN`).
pub(super) fn tokenize(pattern: &str) -> Result<Vec<Token>, UnclosedEscape> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut i = 0;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    literal.push('\'');
                    i += 2;
                } else {
                    escaped = false;
                    i += 1;
                }
            } else {
                literal.push(c);
                i += 1;
            }
            continue;
        }
        if c == '\'' {
            escaped = true;
            i += 1;
            continue;
        }
        match DurationField::pattern_symbol(c) {
            Some(symbol) => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                let mut count = 1;
                while chars.get(i + count) == Some(&c) {
                    count += 1;
                }
                tokens.push(Token::Symbol(symbol, count));
                i += count;
            }
            None => {
                literal.push(c);
                i += 1;
            }
        }
    }
    if escaped {
        return Err(UnclosedEscape);
    }
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    Ok(tokens)
}

/// The formatter's grouping: a run is a contiguous stretch of the same
/// token type, not the same letter (`ML` is one two-wide month run, `Yuy`
/// one three-wide year run — Neo4j's `DurationToken` counters).
pub(super) fn merge_symbol_runs(tokens: Vec<Token>) -> Vec<Token> {
    let mut merged: Vec<Token> = Vec::with_capacity(tokens.len());
    for token in tokens {
        match (merged.last_mut(), &token) {
            (Some(Token::Symbol(prev, width)), Token::Symbol(next, more)) if prev == next => {
                *width += more;
            }
            _ => merged.push(token),
        }
    }
    merged
}
