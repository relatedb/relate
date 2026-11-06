//! Persisted graph-type model shared by query engines and snapshot codecs.
//! Semantic validation and derivation stay with the in-memory engine.

mod canon;
mod default_value;
mod model;

use relate_error::{EngineError, ErrorKind};

pub(crate) fn syn(msg: impl Into<String>) -> EngineError {
    EngineError::semantic(ErrorKind::SyntaxError, msg)
}

pub use canon::{canon_allows, canon_intersection};
pub use default_value::{canonical_default_clause, DefaultValue, DEFAULT_ON_COMMIT_SEPARATOR};
pub use model::*;
