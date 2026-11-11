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
// codes the repo's surfaces emit today: the engine-documented codes of
// ADR-0116 §9 for the fixed-namespace classes, and the GQL general error
// code for every class without a class-specific code — exactly the split
// HTTP's error envelope (ADR-0232) has always made.

/// The GQL general error code: the code of a caller-facing class without a
/// class-specific GQLSTATUS.
pub const GQLSTATUS_GENERAL: &str = "42000";

/// The GQLSTATUS of a rejected statement (the `SyntaxError` class).
pub const GQLSTATUS_SYNTAX: &str = "42N06";

/// The GQLSTATUS of an absent query parameter (ADR-0308).
const GQLSTATUS_PARAMETER_MISSING: &str = "42N02";

/// The GQLSTATUS of a type failure.
const GQLSTATUS_TYPE: &str = "22G03";

/// The GQLSTATUS of an unknown database (ADR-0116 §9).
pub const GQLSTATUS_DATABASE_NOT_FOUND: &str = "42N00";

/// The GQLSTATUS of an unsupported administration command (ADR-0116 §9).
const GQLSTATUS_UNSUPPORTED_ADMINISTRATION_COMMAND: &str = "51N27";

/// The GQLSTATUS of a write against the read-only `system_ontologies`
/// namespace (ADR-0116 §9).
const GQLSTATUS_WRITE_ON_READ_ONLY: &str = "42N18";

/// The GQLSTATUS of an engine-internal failure.
const GQLSTATUS_INTERNAL: &str = "50N42";

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
/// The closed vocabulary of that scan: one variant per class name the
/// engine's CE formatters (`relate_core::ce_messages`,
/// ADR-0116 §9/ADR-0117 §6) and the legacy constraint classes embed. The
/// token parsing stays at the scan's one boundary; every consumer matches
/// the typed variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestedErrorClass {
    /// The legacy constraint family (`ConstraintValidationFailed:` /
    /// `ConstraintVerificationFailed:` tokens, anywhere in the payload).
    ConstraintVerificationFailed,
    /// A DDL argument-value rejection (`ArgumentError:`).
    ArgumentError,
    /// A missing node or relationship (`EntityNotFound:`).
    EntityNotFound,
    /// A registered procedure's own failure (`ProcedureError:`).
    ProcedureError,
    /// A rejected statement (`SyntaxError:`).
    SyntaxError,
    /// A semantic rejection (`SemanticError:`).
    SemanticError,
    /// An unsupported administration command (ADR-0116 §9).
    UnsupportedAdministrationCommand,
    /// A write against the read-only `system_ontologies` namespace
    /// (ADR-0116 §9).
    WriteOnReadOnlyAccessDatabase,
    /// An unknown database (`DatabaseNotFound:`).
    DatabaseNotFound,
    /// A transaction-state failure (`TransactionError:`).
    TransactionError,
}

impl NestedErrorClass {
    /// Every class the scan recognizes, in enum order — the one list the
    /// scan's exact-match half and the round-trip test derive from, so a
    /// new variant lands in both by construction and the ten spellings are
    /// written exactly once (in [`as_str`](Self::as_str)).
    pub(crate) const ALL: [NestedErrorClass; 10] = [
        NestedErrorClass::ConstraintVerificationFailed,
        NestedErrorClass::ArgumentError,
        NestedErrorClass::EntityNotFound,
        NestedErrorClass::ProcedureError,
        NestedErrorClass::SyntaxError,
        NestedErrorClass::SemanticError,
        NestedErrorClass::UnsupportedAdministrationCommand,
        NestedErrorClass::WriteOnReadOnlyAccessDatabase,
        NestedErrorClass::DatabaseNotFound,
        NestedErrorClass::TransactionError,
    ];

    /// The one string spelling of the class — the `<Class>:` token the
    /// engine embeds and the bindings report. Derived from the enum, so
    /// the spelling is single-sourced.
    ///
    /// ```
    /// use relate_error::{nested_error_class, NestedErrorClass};
    ///
    /// assert_eq!(
    ///     NestedErrorClass::ConstraintVerificationFailed.as_str(),
    ///     "ConstraintVerificationFailed"
    /// );
    /// assert_eq!(
    ///     nested_error_class("ArgumentError: bad options"),
    ///     Some(NestedErrorClass::ArgumentError)
    /// );
    /// assert_eq!(NestedErrorClass::ArgumentError.as_str(), "ArgumentError");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            NestedErrorClass::ConstraintVerificationFailed => "ConstraintVerificationFailed",
            NestedErrorClass::ArgumentError => "ArgumentError",
            NestedErrorClass::EntityNotFound => "EntityNotFound",
            NestedErrorClass::ProcedureError => "ProcedureError",
            NestedErrorClass::SyntaxError => "SyntaxError",
            NestedErrorClass::SemanticError => "SemanticError",
            NestedErrorClass::UnsupportedAdministrationCommand => {
                "UnsupportedAdministrationCommand"
            }
            NestedErrorClass::WriteOnReadOnlyAccessDatabase => "WriteOnReadOnlyAccessDatabase",
            NestedErrorClass::DatabaseNotFound => "DatabaseNotFound",
            NestedErrorClass::TransactionError => "TransactionError",
        }
    }
}

/// The one canonical scan (AGENTS.md: one canonical helper per policy) of
/// a still-stringly payload's nested class, consulted by both consumers of
/// the nested-class fact: [`error_info`]'s GQLSTATUS for these payloads,
/// and the language bindings' `binding_error_metadata`. A string that
/// merely resembles a typed class's token does not adopt that class — the
/// recognized names are the ones [`NestedErrorClass`] closes over.
pub fn nested_error_class(payload: &str) -> Option<NestedErrorClass> {
    // The one documented alias: the legacy constraint class was spelled
    // `ConstraintValidationFailed:` and can appear anywhere in the payload,
    // not only as the leading token, so the leading-name lookup below
    // cannot carry it.
    if payload.contains("ConstraintValidationFailed:")
        || payload.contains("ConstraintVerificationFailed:")
    {
        return Some(NestedErrorClass::ConstraintVerificationFailed);
    }
    // A leading `<Class>:` name token. The class names are distinct and the
    // historical match was exact equality, so this lookup over
    // [`NestedErrorClass::ALL`] is behavior-identical to the exact-match
    // arms it replaces — and a new variant joins the scan by joining ALL.
    let name = payload.split_once(':').map(|(name, _)| name.trim())?;
    NestedErrorClass::ALL
        .into_iter()
        .find(|&class| class.as_str() == name)
}

/// The GQLSTATUS of a still-stringly payload: a recognized nested class
/// carries its own code (the engine-documented ones of ADR-0116 §9, the
/// syntax class's own code, the GQL general code for every other nested
/// class), and a payload with no nested class carries `own` — its
/// variant's code. The exact split HTTP's error envelope (ADR-0232) has
/// always made over the same scan.
fn stringly_gql_status(payload: &str, own: &'static str) -> &'static str {
    match nested_error_class(payload) {
        None => own,
        Some(NestedErrorClass::DatabaseNotFound) => GQLSTATUS_DATABASE_NOT_FOUND,
        Some(NestedErrorClass::UnsupportedAdministrationCommand) => {
            GQLSTATUS_UNSUPPORTED_ADMINISTRATION_COMMAND
        }
        Some(NestedErrorClass::WriteOnReadOnlyAccessDatabase) => GQLSTATUS_WRITE_ON_READ_ONLY,
        Some(NestedErrorClass::SyntaxError) => GQLSTATUS_SYNTAX,
        // The recognized nested classes without a class-specific GQLSTATUS
        // carry the general code, never the variant's own — named, not
        // wildcarded, so a future variant forces its own decision here.
        Some(
            NestedErrorClass::ConstraintVerificationFailed
            | NestedErrorClass::ArgumentError
            | NestedErrorClass::EntityNotFound
            | NestedErrorClass::ProcedureError
            | NestedErrorClass::SemanticError
            | NestedErrorClass::TransactionError,
        ) => GQLSTATUS_GENERAL,
    }
}

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
            gql_status: stringly_gql_status(payload, GQLSTATUS_SYNTAX),
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
        Error::Semantic(payload) | Error::Execution(payload) => ErrorInfo {
            neo4j_code: SEMANTIC_ERROR_CODE,
            gql_status: stringly_gql_status(payload, GQLSTATUS_GENERAL),
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
mod tests {
    use super::*;
    use relate_core::ErrorKind;
    use ErrorStage::{Compile, Internal, Runtime};

    /// The published internal row is exactly the row `error_info`
    /// classifies an engine-internal failure under — the fact
    /// [`INTERNAL_ERROR_INFO`] exists to single-source for the boundaries
    /// that fail without an engine error.
    #[test]
    fn internal_error_info_is_error_infos_internal_row() {
        assert_eq!(
            error_info(&Error::Internal(String::new())),
            INTERNAL_ERROR_INFO
        );
    }

    /// The ADR-0307 table as it now lives here, one row per engine
    /// variant: the Neo4j code and the compile/runtime/internal stage,
    /// byte-identical to the table Bolt's failure tests pin over the wire.
    #[test]
    fn every_engine_variant_carries_its_adr_0307_code_and_stage() {
        let rows: Vec<(Error, &str, ErrorStage)> = vec![
            (Error::syntax("parse error"), SYNTAX_ERROR_CODE, Compile),
            (
                Error::procedure_not_found("dbms.noSuchProc"),
                PROCEDURE_NOT_FOUND_CODE,
                Compile,
            ),
            (
                Error::parameters_missing(["p"]),
                PARAMETER_MISSING_CODE,
                Compile,
            ),
            (
                Error::database_not_found("nosuchdb"),
                DATABASE_NOT_FOUND_CODE,
                Compile,
            ),
            (
                Error::argument_error("bad options"),
                ARGUMENT_ERROR_CODE,
                Compile,
            ),
            (
                Error::argument_value_kind(ErrorKind::NumberOutOfRange, "range() step"),
                ARGUMENT_VALUE_ERROR_CODE,
                Runtime,
            ),
            (Error::ty("expected number"), TYPE_ERROR_CODE, Runtime),
            (Error::type_error("x"), TYPE_ERROR_CODE, Runtime),
            (Error::invalid_argument_type("x"), TYPE_ERROR_CODE, Runtime),
            (
                Error::arithmetic(ArithmeticFailure::DivisionByZero),
                ARITHMETIC_ERROR_CODE,
                Runtime,
            ),
            (
                Error::arithmetic(ArithmeticFailure::ModuloByZero),
                ARITHMETIC_ERROR_CODE,
                Runtime,
            ),
            (
                Error::arithmetic(ArithmeticFailure::RoundingNecessary),
                ARITHMETIC_ERROR_CODE,
                Runtime,
            ),
            (
                Error::uniqueness_violation("p_id"),
                CONSTRAINT_VALIDATION_FAILED_CODE,
                Runtime,
            ),
            (Error::cancelled(), TRANSACTION_TIMED_OUT_CODE, Runtime),
            (Error::conflict(), OUTDATED_CODE, Runtime),
            (
                Error::capacity(7, 5),
                TRANSACTION_OUT_OF_MEMORY_CODE,
                Runtime,
            ),
            (
                Error::capacity_wal_payload(67_108_864, Error::WAL_PAYLOAD_CAP_BYTES),
                TRANSACTION_OUT_OF_MEMORY_CODE,
                Runtime,
            ),
            (
                Error::resource_limit(Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT, 16, true),
                MEMORY_POOL_OUT_OF_MEMORY_CODE,
                Runtime,
            ),
            (
                Error::resource_limit(Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT, 16, false),
                TRANSACTION_OUT_OF_MEMORY_CODE,
                Runtime,
            ),
            (
                Error::semantic("variable `n` not defined"),
                SEMANTIC_ERROR_CODE,
                Runtime,
            ),
            (
                Error::execution("node ID exhausted"),
                SEMANTIC_ERROR_CODE,
                Runtime,
            ),
            (Error::internal("boom"), UNKNOWN_ERROR_CODE, Internal),
            (Error::storage("/x"), UNKNOWN_ERROR_CODE, Internal),
            (Error::io("os error 2"), UNKNOWN_ERROR_CODE, Internal),
            (Error::Locked("/x".into()), UNKNOWN_ERROR_CODE, Internal),
        ];
        for (e, code, stage) in rows {
            let i = error_info(&e);
            assert_eq!(i.neo4j_code, code, "{e}");
            assert_eq!(i.stage, stage, "{e}");
        }
    }

    /// The drivers' transient test: a write-write conflict and a retryable
    /// processing cap are the only retryable classes.
    #[test]
    fn only_the_transient_classes_are_retryable() {
        let retryable = [
            Error::conflict(),
            Error::resource_limit(Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT, 16, true),
        ];
        let not_retryable = [
            Error::syntax("x"),
            Error::semantic("x"),
            Error::argument_error("x"),
            Error::parameter_missing("x"),
            Error::database_not_found("x"),
            Error::ty("x"),
            Error::arithmetic(ArithmeticFailure::RoundingNecessary),
            Error::uniqueness_violation("u"),
            Error::cancelled(),
            Error::capacity(7, 5),
            // The non-retryable cap is a client memory refusal: retrying
            // fails again.
            Error::resource_limit(Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT, 16, false),
            Error::execution("x"),
            Error::internal("x"),
            Error::storage("x"),
            Error::io("x"),
            Error::Locked("x".into()),
        ];
        for e in &retryable {
            let i = error_info(e);
            assert!(i.retryable, "{e}");
        }
        for e in &not_retryable {
            let i = error_info(e);
            assert!(!i.retryable, "{e}");
        }
    }

    /// The GQLSTATUS column: the codes the repo's surfaces emit today —
    /// the engine-documented codes for the fixed-namespace classes and the
    /// typed classes HTTP maps, the GQL general code for every other
    /// caller-facing class, the internal code for the engine-internal
    /// failure.
    #[test]
    fn gql_status_is_the_code_the_surfaces_emit_today() {
        let rows: Vec<(Error, &str)> = vec![
            (Error::syntax("bad query"), GQLSTATUS_SYNTAX),
            // A syntax error whose text merely resembles a typed class's
            // token keeps the syntax code (ADR-0307).
            (Error::syntax("ProcedureNotFound: dbms.x"), GQLSTATUS_SYNTAX),
            (Error::procedure_not_found("dbms.x"), GQLSTATUS_SYNTAX),
            (Error::parameter_missing("p"), GQLSTATUS_PARAMETER_MISSING),
            (
                Error::database_not_found("movies"),
                GQLSTATUS_DATABASE_NOT_FOUND,
            ),
            (
                Error::argument_error("InvalidIndexConfig"),
                GQLSTATUS_GENERAL,
            ),
            (
                Error::argument_value_kind(ErrorKind::NumberOutOfRange, "range() step"),
                GQLSTATUS_GENERAL,
            ),
            (Error::ty("x"), GQLSTATUS_TYPE),
            (Error::type_error("x"), GQLSTATUS_TYPE),
            (Error::invalid_argument_type("x"), GQLSTATUS_TYPE),
            (
                Error::arithmetic(ArithmeticFailure::DivisionByZero),
                GQLSTATUS_TYPE,
            ),
            (
                Error::arithmetic(ArithmeticFailure::RoundingNecessary),
                GQLSTATUS_GENERAL,
            ),
            (Error::uniqueness_violation("u"), GQLSTATUS_GENERAL),
            (Error::cancelled(), GQLSTATUS_GENERAL),
            (Error::conflict(), GQLSTATUS_GENERAL),
            (Error::capacity(7, 5), GQLSTATUS_GENERAL),
            (
                Error::resource_limit(Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT, 16, true),
                GQLSTATUS_GENERAL,
            ),
            (
                Error::semantic("variable `n` not defined"),
                GQLSTATUS_GENERAL,
            ),
            (Error::execution("node ID exhausted"), GQLSTATUS_GENERAL),
            (Error::internal("boom"), GQLSTATUS_INTERNAL),
            (Error::storage("/x"), GQLSTATUS_GENERAL),
            (Error::io("os error 2"), GQLSTATUS_GENERAL),
            (Error::Locked("/x".into()), GQLSTATUS_GENERAL),
        ];
        for (e, status) in rows {
            let i = error_info(&e);
            assert_eq!(i.gql_status, status, "{e}");
        }
    }

    /// A nested `<Class>:` token in a still-stringly payload carries its
    /// own documented GQLSTATUS (ADR-0116 §9), exactly the code HTTP's
    /// envelope has always resolved through the same scan.
    #[test]
    fn a_nested_class_token_carries_its_own_gql_status() {
        let rows: Vec<(Error, &str)> = vec![
            (
                Error::semantic("WriteOnReadOnlyAccessDatabase: No write operations are allowed"),
                GQLSTATUS_WRITE_ON_READ_ONLY,
            ),
            (
                Error::semantic("UnsupportedAdministrationCommand: CREATE DATABASE foo"),
                GQLSTATUS_UNSUPPORTED_ADMINISTRATION_COMMAND,
            ),
            (
                Error::semantic("DatabaseNotFound: Graph not found: movies"),
                GQLSTATUS_DATABASE_NOT_FOUND,
            ),
            (
                Error::execution("ConstraintVerificationFailed: duplicate"),
                GQLSTATUS_GENERAL,
            ),
            // A recognized class without a class-specific code carries the
            // general code, never the variant's own.
            (Error::syntax("ArgumentError: x"), GQLSTATUS_GENERAL),
            (Error::semantic("SyntaxError: x"), GQLSTATUS_SYNTAX),
            // Lookalike tokens adopt nothing: the variant's own code.
            (
                Error::execution("while running: InvalidArgumentType: q"),
                GQLSTATUS_GENERAL,
            ),
            (
                Error::syntax("TypeError: not the typed class"),
                GQLSTATUS_SYNTAX,
            ),
        ];
        for (e, status) in rows {
            let i = error_info(&e);
            assert_eq!(i.gql_status, status, "{e}");
        }
    }

    /// The canonical nested-class scan: the names the engine embeds are
    /// recovered, lookalikes are not, and every recovered class renders
    /// the token it was scanned from.
    #[test]
    fn nested_error_class_recovers_the_embedded_names() {
        assert_eq!(
            nested_error_class("ArgumentError: x"),
            Some(NestedErrorClass::ArgumentError)
        );
        assert_eq!(
            nested_error_class("outer context: ConstraintVerificationFailed: duplicate key"),
            Some(NestedErrorClass::ConstraintVerificationFailed)
        );
        assert_eq!(
            nested_error_class("outer: ConstraintValidationFailed: bad"),
            Some(NestedErrorClass::ConstraintVerificationFailed)
        );
        assert_eq!(nested_error_class("no class here"), None);
        assert_eq!(nested_error_class("nested ParameterMissing: p"), None);
        assert_eq!(nested_error_class(""), None);
    }

    /// The closed vocabulary: every class in [`NestedErrorClass::ALL`] is
    /// recovered from its own rendered token, so the enum, the scan and
    /// the wire spelling cannot drift.
    #[test]
    fn every_nested_class_renders_its_embedded_token() {
        for class in NestedErrorClass::ALL {
            let token = class.as_str();
            let scanned = nested_error_class(&format!("{token}: x"))
                .unwrap_or_else(|| panic!("token {token} not recognized by the scan"));
            assert_eq!(scanned.as_str(), token);
        }
    }
}
