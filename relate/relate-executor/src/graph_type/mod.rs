//! CIP-176/177/178/180: the graph-type specification sublanguage's
//! semantic layer -- everything that operates on a parsed
//! `relate_parser::GraphTypeBody` beyond grammar: the CIP-178 "shall"
//! rules (`validate_graph_type_body`, invoked only by `CREATE GRAPH TYPE`
//! -- see `GraphTypeBody`'s doc comment for why `ALTER CURRENT GRAPH TYPE`
//! skips it), the CIP-180 canonical string transformation
//! (`canonicalize_graph_type_body`), the CIP-176/182/183 `ALTER CURRENT
//! GRAPH TYPE` per-verb body algebra (`apply_set` / `apply_add` /
//! `apply_alter` / `apply_drop`, dispatched by `apply_verb`), and the
//! CIP-176/187 compilation of a graph type into the flat constraint set it
//! enforces (`derive_constraints`).
//!
//! The graph-type *model* itself (`GraphTypeBody` and friends) lives in
//! `relate-parser` since it's produced by parsing alone; this module is
//! the engine-side behavior built on top of that model.

use std::collections::{BTreeMap, HashMap, HashSet};

use relate_model::identifier::quote_identifier_always;

use relate_error::{EngineError, ErrorKind};
use relate_model::{Constraint, ConstraintType, EntityKind};
use relate_parser::type_normalize::{render_canon, type_expr_to_canon, CanonKind};
use relate_parser::{
    Expr, GraphTypeBody, GraphTypeConstraintSpec, GraphTypeConstraintVerb, GraphTypeEdgeTypeSpec,
    GraphTypeElementRef, GraphTypeEntityQualifier, GraphTypeNodeTypeSpec, GraphTypePropertyType,
    LiteralValue, PredefinedKind,
};

/// Every CIP-178 validation failure in this module renders as `"SyntaxError:
/// <message>"`, matching the historical stringly errors byte-for-byte (the
/// prime invariant). This one-line constructor carries the
/// [`ErrorKind::SyntaxError`] kind natively while keeping the message the
/// bare detail text.
fn syn(msg: impl Into<String>) -> EngineError {
    EngineError::semantic(ErrorKind::SyntaxError, msg)
}

/// Which corpus contract a graph-type body is validated against. The
/// declaration form (`CREATE GRAPH TYPE`) is the CIP-178/180/181 surface,
/// whose doc-derived corpus pins the legacy `indexProvider` OPTIONS key and
/// the constraint-alias/element-type-alias collision. The alteration
/// form (`ALTER CURRENT GRAPH TYPE`) follows Neo4j 2026.09's Cypher 25 rules:
/// the OPTIONS policy of `CREATE CONSTRAINT` applies, and a constraint's
/// `(alias:Label)` reference is its own schema-variable namespace (Neo4j's
/// verified acceptance corpus pairs `(n:EN1 => ...)` with
/// `CONSTRAINT nk FOR (n:L1)` in one `ADD` body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphTypeScope {
    /// `CREATE GRAPH TYPE`'s declaration body.
    Declaration,
    /// `ALTER CURRENT GRAPH TYPE`'s (merged) body.
    Alteration,
}

mod alter;
mod canonicalize;
mod default_eval;
mod derive;
mod inherit;
mod name;
mod storage_constraint;
mod validate;
mod validate_rules;

pub(crate) use alter::*;
pub(crate) use canonicalize::*;
pub(crate) use default_eval::{eval_lowered_default, lowered_default};
pub(crate) use derive::*;
pub(crate) use inherit::*;
pub use relate_engine_api::graph_type::OptionValueModel;
pub use relate_engine_api::graph_type::{
    canon_allows, canon_intersection, canonical_default_clause, DefaultValue, GraphTypeSpec,
    DEFAULT_ON_COMMIT_SEPARATOR,
};
pub(crate) use storage_constraint::{
    render_storage_constraint, standalone_verb, storage_constraint_rank, StorageConstraintStyle,
};
pub(crate) use validate::*;

/// Maps a [`derive_constraints`] failure over persisted (deserialized)
/// graph-type bytes -- a deserialized `DEFAULT ON COMMIT` constructor
/// whose name the CIP-262 table never admitted (a gated construct stored
/// by an enabled follower, then replayed or checkpoint-verified here) --
/// to the storage-corruption error family the committed redo paths use
/// for a corrupt spec. The one mapping every persisted-bytes derivation
/// site (redo preflight, redo replay, checkpoint verification) consults,
/// so a poisoned persisted default surfaces as a typed `Error::storage`
/// at `Database::open`, never a panic.
pub(crate) fn persisted_derive_error(error: relate_core::Error) -> relate_core::Error {
    relate_core::Error::storage(format!(
        "persisted graph type carries an invalid default value: {error}"
    ))
}

/// The same persisted-spec admission failure, from the validator's
/// `EngineError` vantage (the JSON-snapshot restore sibling): the one
/// message, mapped for the caller whose admission runs the validator.
pub(crate) fn persisted_validation_error(error: relate_error::EngineError) -> relate_core::Error {
    relate_core::Error::storage(format!(
        "persisted graph type carries an invalid default value: {error}"
    ))
}

#[cfg(test)]
mod tests;
