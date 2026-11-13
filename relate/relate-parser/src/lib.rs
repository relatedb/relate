pub mod ast;
pub mod error;
pub mod lexer;
mod normalize;
pub mod parser;
pub mod token;
pub mod type_normalize;

// The Unicode 13.0.0 identifier predicates and their generated tables
// (ADR-0182) are a lexer implementation detail, not part of the crate's
// public API.
mod unicode_identifier;

// Re-export the main parse function and key types for convenience.
pub use ast::*;
pub use error::{ParseError, ParseResult};
pub use normalize::{
    normalize_statement, params_carry_reserved_prefix, NormalizeOutcome, Normalized,
    NormalizedValue,
};
pub use parser::{parse, parse_graph_type_body, parse_with_options, selector_is_selective};
pub use token::{Token, TokenType};
pub use type_normalize::*;
