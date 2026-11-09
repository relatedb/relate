//! Typed engine errors for Relate.
//!
//! This crate hosts [`EngineError`], the typed error channel that replaces the
//! engine's historical `Result<_, String>`, and — since ADR-0502 §2 — the
//! engine's one classification of a query-engine error, [`error_info`]: the
//! Neo4j status code, the GQLSTATUS, the compile/runtime/internal stage
//! (ADR-0319) and the drivers' transient test, single-sourced here so every
//! surface (Bolt, HTTP, the CLI classifier, Python, the C ABI) is a thin
//! derivation, never a second table. It depends on `relate-parser`
//! (for [`ParseError`]) and `relate-core` (for the query-engine
//! [`Error`](relate_core::Error) that [`error_info`] classifies, and for
//! the shared error-kind vocabulary, which ADR-0502 moved down to core and
//! this crate re-exports).
//! See `docs/adr/0003-tck-named-error-kinds.md` and
//! `docs/adr/0502-typed-c-abi-v2.md`.
//!
//! # The prime invariant
//!
//! `Display` output is preserved verbatim against the pre-refactor stringly
//! errors: a parse-time failure renders exactly as `ParseError`'s
//! `"parse error: …"`, a storage failure exactly as `StorageError`'s message,
//! and a semantic/runtime failure exactly as the historical `"Kind: message"`
//! shape. Changing an error *message* is not a breaking change; changing a
//! *kind* is (per ADR 0003).

use std::fmt;

mod classification;
mod storage;

pub use classification::{
    client_message, error_info, log_internal_original, nested_error_class, ErrorInfo, ErrorStage,
    NestedErrorClass, ARGUMENT_ERROR_CODE, ARITHMETIC_ERROR_CODE,
    CONSTRAINT_VALIDATION_FAILED_CODE, DATABASE_NOT_FOUND_CODE, GQLSTATUS_DATABASE_NOT_FOUND,
    GQLSTATUS_GENERAL, GQLSTATUS_SYNTAX, INTERNAL_ERROR_INFO, INTERNAL_MESSAGE,
    MEMORY_POOL_OUT_OF_MEMORY_CODE, OUTDATED_CODE, PARAMETER_MISSING_CODE,
    PROCEDURE_NOT_FOUND_CODE, SEMANTIC_ERROR_CODE, SYNTAX_ERROR_CODE,
    TRANSACTION_OUT_OF_MEMORY_CODE, TRANSACTION_TIMED_OUT_CODE, TYPE_ERROR_CODE,
    UNKNOWN_ERROR_CODE,
};
pub use relate_core::{
    duplicate_variable_detail, undefined_variable_detail, unknown_function_detail, ErrorKind,
    TckErrorClass,
};
pub use relate_parser::ParseError;
pub use storage::StorageError;

/// The two `COUNT { … }` statement-shape UNION rule message bodies (CIP-66),
/// single-sourced because two layers raise them byte-identically after
/// [`ErrorKind::DifferentColumnsInUnion`]'s name: the front's validate
/// battery (`validate_count_subquery_union_shapes`, rejecting before
/// lowering) and the executor's per-row defense-in-depth scan
/// (`eval_count_query`, for callers that bypass validation). The rendered
/// text is pinned contract (ADR-0003's message immutability rule).
pub const COUNT_UNION_MIX_MESSAGE: &str = "cannot mix explicit and omitted RETURN";
pub const COUNT_UNION_OMITTED_MESSAGE: &str = "omitted RETURN requires UNION ALL";

/// The typed engine error channel. Per ADR 0003 it has four variants:
///
/// - [`EngineError::Syntax`] wraps a parser [`ParseError`] verbatim.
/// - [`EngineError::Semantic`] is a validation-time failure carrying an
///   [`ErrorKind`] and message.
/// - [`EngineError::Runtime`] is a row-evaluation-time failure carrying an
///   [`ErrorKind`] and message.
/// - [`EngineError::Storage`] wraps a [`StorageError`] verbatim.
///
/// `Display` reproduces the pre-refactor strings byte-for-byte (see the crate
/// docs). `kind()` returns the finest TCK-aligned name; `tck_class()` returns
/// the coarser TCK class.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineError {
    /// A parse-time failure. Renders as `ParseError`'s `"parse error: …"`.
    Syntax(ParseError),
    /// A semantic (validation-time) failure.
    Semantic { kind: ErrorKind, message: String },
    /// A runtime (row-evaluation-time) failure.
    Runtime { kind: ErrorKind, message: String },
    /// A storage-layer failure. Renders as `StorageError`'s message.
    Storage(StorageError),
}

impl EngineError {
    /// A reference to a name the scope does not bind.
    pub fn undefined_variable(name: &str) -> Self {
        Self::semantic(
            ErrorKind::UndefinedVariable,
            undefined_variable_detail(name),
        )
    }

    /// A binding that redeclares a name its scope already binds.
    pub fn duplicate_variable(name: &str) -> Self {
        Self::semantic(
            ErrorKind::VariableAlreadyBound,
            duplicate_variable_detail(name),
        )
    }

    /// Construct a semantic error.
    pub fn semantic(kind: ErrorKind, message: impl Into<String>) -> Self {
        EngineError::Semantic {
            kind,
            message: message.into(),
        }
    }

    /// Construct a runtime error.
    pub fn runtime(kind: ErrorKind, message: impl Into<String>) -> Self {
        EngineError::Runtime {
            kind,
            message: message.into(),
        }
    }

    /// The finest TCK-aligned [`ErrorKind`] for this error.
    ///
    /// A parse failure is a `SyntaxError` *class*, but if its [`ParseError`]
    /// carries a [`detail`](ParseError::detail) tag (e.g. `"IntegerOverflow"`),
    /// that finer kind is surfaced here so the TCK harness can match the
    /// TCK's `<Detail>` slot. The tag is never rendered, so `Display` is
    /// unaffected.
    pub fn kind(&self) -> ErrorKind {
        match self {
            // A parse failure with no finer `detail` tag is the openCypher TCK's
            // generic `UnexpectedSyntax` detail: the corpus has no
            // scenario pinning a bare `SyntaxError` *detail*, while it has 17
            // pinning `UnexpectedSyntax` for exactly these generic
            // "expected/unexpected token" parse errors (Literals7/8, Literals2
            // symbol case, …). Both kinds share the `SyntaxError` TCK *class*, so
            // class-only assertions are unaffected. A tagged parse error still
            // surfaces its finer kind (IntegerOverflow, InvalidNumberLiteral, …).
            EngineError::Syntax(e) => e
                .detail
                .and_then(ErrorKind::from_name)
                .unwrap_or(ErrorKind::UnexpectedSyntax),
            EngineError::Semantic { kind, .. } | EngineError::Runtime { kind, .. } => *kind,
            EngineError::Storage(e) => storage_kind(e),
        }
    }

    /// The openCypher TCK error *class* for this error.
    pub fn tck_class(&self) -> TckErrorClass {
        self.kind().tck_class()
    }
}

/// Maps a [`StorageError`] to the finest [`ErrorKind`] for reporting. `Display`
/// still delegates to `StorageError` itself, so this only affects
/// `kind()`/`tck_class()`, never the rendered message.
fn storage_kind(e: &StorageError) -> ErrorKind {
    match e {
        StorageError::NodeNotFound(_) | StorageError::RelationshipNotFound(_) => {
            ErrorKind::EntityNotFound
        }
        // Deleting a still-connected node without `DETACH` carries the finer
        // `DeleteConnectedNode` detail (TCK clauses/delete/Delete1 scenario 7);
        // a genuine constraint violation keeps `ConstraintViolation`. Both are
        // the `ConstraintVerificationFailed` TCK class.
        StorageError::NodeHasRelationships(_) => ErrorKind::DeleteConnectedNode,
        StorageError::ConstraintViolation(_) => ErrorKind::ConstraintViolation,
        StorageError::IndexAlreadyExists(_)
        | StorageError::IndexNotFound(_)
        | StorageError::ConstraintAlreadyExists(_)
        | StorageError::ConstraintNotFound(_) => ErrorKind::SchemaError,
        StorageError::TransactionsUnsupported
        | StorageError::TransactionAlreadyActive
        | StorageError::NoActiveTransaction
        // A `Conflict` today is the flush-during-transaction guard, which is a
        // transaction-state conflict.
        | StorageError::Conflict(_) => ErrorKind::TransactionError,
        // Filesystem I/O, on-disk corruption, a database already locked by
        // another process, and a write rejected against a read-only snapshot
        // (a historical MVCC checkout) are all database-level operational
        // failures.
        StorageError::Io { .. }
        | StorageError::Corruption(_)
        | StorageError::Locked(_)
        | StorageError::ReadOnly(_) => ErrorKind::DatabaseError,
        StorageError::PersistenceOutcomeIndeterminate(_) | StorageError::Other(_) => {
            ErrorKind::Uncategorized
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Syntax(e) => write!(f, "{e}"),
            EngineError::Storage(e) => write!(f, "{e}"),
            EngineError::Semantic { kind, message } | EngineError::Runtime { kind, message } => {
                if *kind == ErrorKind::Uncategorized {
                    // Escape hatch: reproduce the raw legacy string verbatim.
                    f.write_str(message)
                } else {
                    write!(f, "{}: {}", kind.as_str(), message)
                }
            }
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::Syntax(e) => Some(e),
            EngineError::Storage(e) => Some(e),
            EngineError::Semantic { .. } | EngineError::Runtime { .. } => None,
        }
    }
}

impl From<ParseError> for EngineError {
    fn from(e: ParseError) -> Self {
        EngineError::Syntax(e)
    }
}

impl From<StorageError> for EngineError {
    fn from(e: StorageError) -> Self {
        EngineError::Storage(e)
    }
}

#[cfg(test)]
mod tests;
