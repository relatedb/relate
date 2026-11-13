//! The one classification of a query-engine error (ADR-0502 §2): the
//! Neo4j status code, the GQLSTATUS, the compile/runtime/internal stage
//! (ADR-0319's split) and the drivers' transient test, single-sourced in
//! [`error_info`] so Bolt's FAILURE rendering, HTTP's error envelope, the
//! CLI's exit-code classifier, Python and the C ABI are thin derivations —
//! never second tables that drift.
//!
//! The exhaustive match and the Neo4j code constants moved here from Bolt's
//! `failures::error_class` (ADR-0307's table, the most complete of the four
//! historical sites); the move is a move, not a redesign — every consumer
//! keeps its exact observable output. The GQLSTATUS column is new wire data
//! for the typed C ABI v2's one error shape; its values are the codes the
//! repo's surfaces already emit (HTTP's error envelope, ADR-0232 — the
//! engine-documented fixed-namespace codes of ADR-0116 §9 and the GQL
//! general code for every other class), so deriving a surface from
//! [`ErrorInfo`] cannot change what that surface sends today.

use crate::nested_class::{
    nested_error_class, stringly_gql_status, GQLSTATUS_CONSTRAINT_CREATION_FAILED,
    GQLSTATUS_DATABASE_NOT_FOUND, GQLSTATUS_GENERAL, GQLSTATUS_INTERNAL,
    GQLSTATUS_PARAMETER_MISSING, GQLSTATUS_SYNTAX, GQLSTATUS_TYPE,
};
use relate_core::{ArithmeticFailure, Error};

/// The `code` of a statement the parser rejects (engine `Syntax`).
pub const SYNTAX_ERROR_CODE: &str = "Neo.ClientError.Statement.SyntaxError";

/// The `code` sent when a query calls a procedure the engine does not
/// register (ADR-0138): neo4j-browser branches on the exact Neo4j code for
/// its `dbms.clientConfig` → `dbms.listConfig` fallback.
pub const PROCEDURE_NOT_FOUND_CODE: &str = "Neo.ClientError.Procedure.ProcedureNotFound";

/// The `code` of a statement that references an absent parameter
/// (ADR-0308).
pub const PARAMETER_MISSING_CODE: &str = "Neo.ClientError.Statement.ParameterMissing";

/// The `code` of an unknown database, from a `db` extra or a `USE` target.
pub const DATABASE_NOT_FOUND_CODE: &str = "Neo.ClientError.Database.DatabaseNotFound";

/// The `code` of every other caller-facing `Semantic` and `Execution`
/// engine error.
pub const SEMANTIC_ERROR_CODE: &str = "Neo.ClientError.Statement.SemanticError";

/// The `code` of a type failure (`Type`, `TypeError`, `InvalidArgumentType`).
pub const TYPE_ERROR_CODE: &str = "Neo.ClientError.Statement.TypeError";

/// The `code` of a DDL argument-value rejection (`ArgumentError`): the code
/// Neo4j 2026.09 sends for an invalid `OPTIONS`/`indexConfig` value
/// (ADR-0489).
pub const ARGUMENT_ERROR_CODE: &str = "Neo.ClientError.General.InvalidArguments";

/// The `code` of a run-time argument rejection (`ArgumentValue`): what Neo4j
/// 2026.10.1 sends for `range()`'s zero step and for the
/// `substring()`/`left()`/`right()` int32-range rejections — the same
/// `ArgumentError` class as [`ARGUMENT_ERROR_CODE`], a different stage.
pub const ARGUMENT_VALUE_ERROR_CODE: &str = "Neo.ClientError.Statement.ArgumentError";

/// The `code` of an arithmetic failure (`Arithmetic`: division or modulo by
/// zero, rounding necessary).
pub const ARITHMETIC_ERROR_CODE: &str = "Neo.ClientError.Statement.ArithmeticError";

/// The `code` of a uniqueness-constraint violation.
pub const CONSTRAINT_VALIDATION_FAILED_CODE: &str =
    "Neo.ClientError.Schema.ConstraintValidationFailed";

/// The `code` of a query stopped by `--query-timeout-secs` (the engine's
/// `Cancelled`).
pub const TRANSACTION_TIMED_OUT_CODE: &str = "Neo.ClientError.Transaction.TransactionTimedOut";

/// The `code` of a write-write conflict: a `TransientError`, so drivers'
/// managed transactions retry it (the engine never does, ADR-0017).
pub const OUTDATED_CODE: &str = "Neo.TransientError.Transaction.Outdated";

/// The `code` of both memory conditions — a write over
/// `--logical-graph-size-limit` (`Capacity`) and a result over
/// `--max-result-bytes` — the one Neo4j memory code in the non-retryable
/// `ClientError` class (ADR-0307 item 4).
pub const TRANSACTION_OUT_OF_MEMORY_CODE: &str =
    "Neo.ClientError.General.TransactionOutOfMemoryError";

/// The `code` of a retryable processing cap
/// (`concurrent-logical-workspace-limit`). Transient, so a driver's managed
/// transaction retries the statement.
pub const MEMORY_POOL_OUT_OF_MEMORY_CODE: &str =
    "Neo.TransientError.General.MemoryPoolOutOfMemoryError";

/// The `code` sent when an internal-class engine error is genericized at a
/// trust boundary (ADR-0260, ADR-0307).
pub const UNKNOWN_ERROR_CODE: &str = "Neo.DatabaseError.General.UnknownError";

// The GQLSTATUS column (ADR-0502 §2's error shape). The values are the
// documented Neo4j GQLSTATUS codes: the engine-documented codes of
// ADR-0116 §9 for the fixed-namespace classes, the documented schema/DDL
// condition codes below (live-verified 2026-11-12 on Neo4j 2026.10.1), and
// the GQL general error code for every class without a class-specific code
// — exactly the split HTTP's error envelope (ADR-0232) has always made.

/// Which stage of statement handling an error belongs to (ADR-0319): a
/// compile-class error fails the statement before any work is done (Bolt's
/// RUN), a runtime-class error is discovered while producing rows (Bolt's
/// first PULL/DISCARD), and an internal-class error is not the statement's
/// fault and is genericized at every trust boundary (ADR-0260).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorStage {
    /// The statement itself is rejected: the RUN fails under the code.
    Compile,
    /// Discovered while executing: reported at the first row pull.
    Runtime,
    /// Engine-internal: genericized for the client (ADR-0260).
    Internal,
}

impl ErrorStage {
    /// The one string spelling of the stage (ADR-0502 §2: the error shape's
    /// `stage` field) — `"compile"`, `"runtime"` or `"internal"`. The C ABI
    /// and the Python binding both render it from here, so the spelling is
    /// single-sourced.
    ///
    /// ```
    /// use relate_error::ErrorStage;
    ///
    /// assert_eq!(ErrorStage::Compile.as_str(), "compile");
    /// assert_eq!(ErrorStage::Runtime.as_str(), "runtime");
    /// assert_eq!(ErrorStage::Internal.as_str(), "internal");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorStage::Compile => "compile",
            ErrorStage::Runtime => "runtime",
            ErrorStage::Internal => "internal",
        }
    }
}

/// The classification of one query-engine error (ADR-0502 §2): everything
/// the ABI v2 error shape carries beside the message, and everything the
/// surfaces need to render a failure. Built only by [`error_info`] — the
/// one exhaustive classification, so a new engine variant does not compile
/// until it is classified here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorInfo {
    /// The Neo4j status code the caller-facing classes carry; the
    /// internal class carries the genericized `UnknownError` code
    /// (ADR-0260).
    pub neo4j_code: &'static str,
    /// The GQLSTATUS (the codes the repo's surfaces emit today, see the
    /// module docs).
    pub gql_status: &'static str,
    /// The compile/runtime/internal stage (ADR-0319).
    pub stage: ErrorStage,
    /// The drivers' transient test (ADR-0502): whether a driver's managed
    /// transaction retries the statement. True for a write-write conflict
    /// and a retryable processing cap, false for everything else.
    pub retryable: bool,
}

/// The message a trust boundary may show the client for `e` — the
/// message-exposure half of ADR-0260 (OPLANE_REQ-00340359), derived from
/// the one classification beside it: the caller-facing classes carry the
/// engine's Display text; the internal class carries the fixed
/// [`INTERNAL_MESSAGE`], because engine-internal, IO, storage and locked
/// messages can hold file paths and engine internals. A boundary that
/// wants the original for its own log reads it itself (it has `e`) —
/// this helper never leaks it.
pub fn client_message(e: &Error) -> std::borrow::Cow<'_, str> {
    match error_info(e).stage {
        ErrorStage::Compile | ErrorStage::Runtime => std::borrow::Cow::Owned(e.to_string()),
        ErrorStage::Internal => std::borrow::Cow::Borrowed(INTERNAL_MESSAGE),
    }
}

/// Log the internal class's original message to stderr — the
/// boundary-log half of the same ADR-0260 policy
/// (OPLANE_REQ-00340359): the class whose message [`client_message`]
/// genericizes is the one whose original an operator needs server-side,
/// so the stage decision and the log format live here once, beside the
/// redaction they complement. `boundary` names the calling surface in
/// the line (`"bolt"`, `"http"`, `"ffi"`); a boundary that gates its
/// channel (the C ABI's host opt-in) applies its gate at its own call
/// site.
pub fn log_internal_original(e: &Error, boundary: &str) {
    if let ErrorStage::Internal = error_info(e).stage {
        eprintln!("{boundary} internal error: {e}");
    }
}

/// The openCypher error class nested in a still-stringly `Syntax`/
/// `Semantic`/`Execution` payload: the legacy constraint-class tokens
/// first, then a leading `<Class>:` name token. [`nested_error_class`]
/// returns `None` when the payload carries no recognized class — the
/// caller's variant-level class is then the answer.
///
/// The fixed client message every trust boundary substitutes for an
/// internal-class failure (ADR-0260, OPLANE_REQ-00340359): engine-internal,
/// IO, storage and locked errors — and caught panic payloads — can carry
/// file paths and engine internals, so their Display text stays
/// server-side (logged by the boundary) and the client sees this. The
/// caller-facing classes pass their Display text through verbatim.
pub const INTERNAL_MESSAGE: &str = "internal error";

/// The internal class's own row of the ADR-0307 table: the genericized
/// `UnknownError` code, the internal GQLSTATUS, the internal stage, never
/// retryable (ADR-0260 — an internal failure is not the statement's
/// fault). [`error_info`] returns exactly this row for an engine-internal
/// failure, and publishes it for the boundaries that fail before the
/// engine sees anything — a null or poisoned handle, an argument decode
/// refusal, a caught panic — so they render the internal classification
/// without fabricating an engine error to classify.
pub const INTERNAL_ERROR_INFO: ErrorInfo = ErrorInfo {
    neo4j_code: UNKNOWN_ERROR_CODE,
    gql_status: GQLSTATUS_INTERNAL,
    stage: ErrorStage::Internal,
    retryable: false,
};

/// Classify a query-engine error (ADR-0307, single-sourced by ADR-0502 §2).
/// Exhaustive on purpose: a new engine variant does not compile until it
/// has a code, a GQLSTATUS, a stage and a retryable verdict — the match
/// that Bolt's `failures::error_class` used to own alone.
///
/// ```
/// use relate_error::{error_info, ErrorStage, PARAMETER_MISSING_CODE};
///
/// let e = relate_core::Error::parameters_missing(["p"]);
/// let info = error_info(&e);
/// assert_eq!(info.neo4j_code, PARAMETER_MISSING_CODE);
/// assert_eq!(info.gql_status, "42N02");
/// assert_eq!(info.stage, ErrorStage::Compile);
/// assert!(!info.retryable);
/// ```
pub fn error_info(e: &Error) -> ErrorInfo {
    use ErrorStage::{Compile, Runtime};
    match e {
        // --- Compile class: the statement is rejected before any work. ---
        Error::Syntax(payload) => ErrorInfo {
            neo4j_code: SYNTAX_ERROR_CODE,
            gql_status: stringly_gql_status(nested_error_class(payload), GQLSTATUS_SYNTAX),
            stage: Compile,
            retryable: false,
        },
        Error::ProcedureNotFound { .. } => ErrorInfo {
            neo4j_code: PROCEDURE_NOT_FOUND_CODE,
            // Live Neo4j sends 42001 for an unregistered procedure, so its
            // GQLSTATUS is the syntax code even though the binding class is
            // `ProcedureError` (ADR-0565).
            gql_status: GQLSTATUS_SYNTAX,
            stage: Compile,
            retryable: false,
        },
        Error::ParameterMissing(_) => ErrorInfo {
            neo4j_code: PARAMETER_MISSING_CODE,
            gql_status: GQLSTATUS_PARAMETER_MISSING,
            stage: Compile,
            retryable: false,
        },
        Error::DatabaseNotFound { .. } => ErrorInfo {
            neo4j_code: DATABASE_NOT_FOUND_CODE,
            gql_status: GQLSTATUS_DATABASE_NOT_FOUND,
            stage: Compile,
            retryable: false,
        },
        // ADR-0489: a DDL option/index-config argument rejection is
        // discovered while the statement is validated, before any write, so
        // it fails the RUN (Neo4j sends it under the same code at RUN).
        Error::ArgumentError(_) => ErrorInfo {
            neo4j_code: ARGUMENT_ERROR_CODE,
            gql_status: GQLSTATUS_GENERAL,
            stage: Compile,
            retryable: false,
        },
        // --- Runtime class: discovered while producing rows. ---
        // The function-argument validator's rejections share the DDL
        // argument class but are discovered while rows are produced, so live
        // fails the PULL — its own code and stage, not the DDL one's.
        Error::ArgumentValue { .. } => ErrorInfo {
            neo4j_code: ARGUMENT_VALUE_ERROR_CODE,
            gql_status: GQLSTATUS_GENERAL,
            stage: Runtime,
            retryable: false,
        },
        Error::Type(_) | Error::TypeError(_) | Error::InvalidArgumentType(_) => ErrorInfo {
            neo4j_code: TYPE_ERROR_CODE,
            gql_status: GQLSTATUS_TYPE,
            stage: Runtime,
            retryable: false,
        },
        // Division and modulo by zero were historically `Type` (the
        // bindings' TypeError class); rounding necessary was a nameless
        // `Execution` token — the GQLSTATUS column keeps that split.
        Error::Arithmetic(ArithmeticFailure::DivisionByZero | ArithmeticFailure::ModuloByZero) => {
            ErrorInfo {
                neo4j_code: ARITHMETIC_ERROR_CODE,
                gql_status: GQLSTATUS_TYPE,
                stage: Runtime,
                retryable: false,
            }
        }
        Error::Arithmetic(ArithmeticFailure::RoundingNecessary) => ErrorInfo {
            neo4j_code: ARITHMETIC_ERROR_CODE,
            gql_status: GQLSTATUS_GENERAL,
            stage: Runtime,
            retryable: false,
        },
        Error::UniquenessViolation { .. } => ErrorInfo {
            neo4j_code: CONSTRAINT_VALIDATION_FAILED_CODE,
            gql_status: GQLSTATUS_GENERAL,
            stage: Runtime,
            retryable: false,
        },
        Error::Cancelled => ErrorInfo {
            neo4j_code: TRANSACTION_TIMED_OUT_CODE,
            gql_status: GQLSTATUS_GENERAL,
            stage: Runtime,
            retryable: false,
        },
        // A `TransientError` code: drivers' managed transactions retry it.
        Error::Conflict => ErrorInfo {
            neo4j_code: OUTDATED_CODE,
            gql_status: GQLSTATUS_GENERAL,
            stage: Runtime,
            retryable: true,
        },
        // The capacity family — the graph cap, the expression cap and the
        // per-record WAL payload cap (ADR-0494) — is the one memory code.
        Error::Capacity { .. } => ErrorInfo {
            neo4j_code: TRANSACTION_OUT_OF_MEMORY_CODE,
            gql_status: GQLSTATUS_GENERAL,
            stage: Runtime,
            retryable: false,
        },
        // A retryable cap (`concurrent-logical-workspace-limit`) is a
        // transient memory refusal: drivers retry it, and the message stays
        // the engine's. A non-retryable cap is the same client memory code
        // as `Capacity`.
        Error::ResourceLimit { retryable, .. } => ErrorInfo {
            neo4j_code: if *retryable {
                MEMORY_POOL_OUT_OF_MEMORY_CODE
            } else {
                TRANSACTION_OUT_OF_MEMORY_CODE
            },
            gql_status: GQLSTATUS_GENERAL,
            stage: Runtime,
            retryable: *retryable,
        },
        Error::Semantic(payload) | Error::Execution(payload) => {
            let nested = nested_error_class(payload);
            ErrorInfo {
                neo4j_code: SEMANTIC_ERROR_CODE,
                gql_status: stringly_gql_status(nested, GQLSTATUS_GENERAL),
                stage: Runtime,
                retryable: false,
            }
        }
        // The creation condition is typed, so it carries its own row rather
        // than a token the classifier recovers from the rendered message.
        // Its client code is the constraint code every creation over
        // violating data has always reported (the legacy
        // `ConstraintVerificationFailed` class the bindings keep), and its
        // documented GQLSTATUS is `50N11`.
        Error::ConstraintCreationFailed { .. } => ErrorInfo {
            neo4j_code: CONSTRAINT_VALIDATION_FAILED_CODE,
            gql_status: GQLSTATUS_CONSTRAINT_CREATION_FAILED,
            stage: Runtime,
            retryable: false,
        },
        // --- Internal class (ADR-0260): genericized at every trust
        // boundary. The engine-internal failure carries the internal
        // GQLSTATUS; the storage/IO/locked family has always ridden the GQL
        // general code on the HTTP wire, so it does here. ---
        Error::Internal(_) => INTERNAL_ERROR_INFO,
        Error::Storage(_) | Error::Io(_) | Error::Locked(_) => ErrorInfo {
            gql_status: GQLSTATUS_GENERAL,
            ..INTERNAL_ERROR_INFO
        },
    }
}

#[cfg(test)]
mod tests;
