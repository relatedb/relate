pub mod ast;
// The auto-parameter vocabulary in both directions (ADR-0503/0718) and
// the bracket classification (REL8-1423). Both module paths are private;
// the auto-parameter items are re-exported below as public API (the
// facade's naming path calls them), while the bracket classification
// stays internal. (The token-fact vocabulary lives under the token
// module, `token::facts`.)
mod auto_parameter;
mod bracket_role;
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
pub use auto_parameter::{
    authored_spellings, column_carries_auto_parameter, name_is_auto_parameter,
    params_carry_reserved_prefix, substitute_auto_parameters, AuthoredSpelling, AuthoredSpellings,
};
pub use error::{ParseError, ParseResult};
pub use normalize::{normalize_statement, NormalizeOutcome, Normalized, NormalizedValue};
pub use parser::{parse, parse_graph_type_body, parse_with_options, selector_is_selective};
pub use token::{Token, TokenType};
pub use type_normalize::*;
