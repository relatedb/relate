//! Tests for the ADR-0307 classification table.

use super::*;
use crate::nested_class::{
    GQLSTATUS_UNSUPPORTED_ADMINISTRATION_COMMAND, GQLSTATUS_WRITE_ON_READ_ONLY,
};
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

/// The schema/DDL condition tokens carry their documented GQLSTATUS,
/// whether the engine raised the bare token or a `<Token>: <detail>`
/// payload — live-verified 2026-11-12 on Neo4j 2026.10.1.
#[test]
fn schema_condition_tokens_carry_their_documented_gql_status() {
    let rows: Vec<(Error, &str)> = vec![
        (Error::semantic("ConstraintAlreadyExists"), "22N67"),
        (
            Error::semantic("ConstraintAlreadyExists: `c` already exists"),
            "22N67",
        ),
        (
            Error::semantic("EquivalentConstraintAlreadyExists: equivalent constraint exists"),
            "22N65",
        ),
        (Error::semantic("ConstraintConflict"), "22N66"),
        (Error::syntax("ConstraintConflict"), "22N66"),
        // The creation condition is typed; it carries its own row.
        (
            Error::constraint_creation_failed("ConstraintValidationFailed: `c` property type"),
            "50N11",
        ),
        (Error::semantic("ConstraintNotFound"), "50N12"),
        (
            Error::execution(
                "DependentConstraintManagedIndividually: dependent constraints cannot be dropped",
            ),
            "50N12",
        ),
        (Error::semantic("DuplicatedIndexName"), "22N71"),
        (Error::semantic("IndexAlreadyExists"), "22N71"),
        (Error::semantic("EquivalentSchemaRuleAlreadyExists"), "22N70"),
        (
            Error::execution(
                "IncompatibleGraphTypeDependence: dependent constraints are managed by the graph type",
            ),
            "22NB2",
        ),
        (Error::execution("IndexDoesNotExist: idx"), "22N69"),
        (
            Error::execution("SystemDatabaseRequired: SHOW DATABASES cannot be combined with NEXT"),
            "51N28",
        ),
        (
            Error::syntax(
                "SystemDatabaseRequired: an administrative command cannot participate in a NEXT chain",
            ),
            "51N28",
        ),
        (
            Error::execution("ElementIdUnexpectedFormat: Element ID has an unexpected format"),
            "50N00",
        ),
    ];
    for (e, status) in rows {
        let i = error_info(&e);
        assert_eq!(i.gql_status, status, "{e}");
    }
}

/// The creation condition is typed, so it carries its own row: the
/// client code every creation over violating data has always reported,
/// and the documented `50N11`. Every other condition token keeps the
/// variant's own code.
#[test]
fn the_creation_condition_carries_its_own_row() {
    let info = error_info(&Error::constraint_creation_failed(
        "ConstraintValidationFailed: `p_id` uniqueness",
    ));
    assert_eq!(info.neo4j_code, CONSTRAINT_VALIDATION_FAILED_CODE);
    assert_eq!(info.gql_status, GQLSTATUS_CONSTRAINT_CREATION_FAILED);
    assert!(!info.retryable);

    assert_eq!(
        error_info(&Error::semantic("ConstraintAlreadyExists")).neo4j_code,
        SEMANTIC_ERROR_CODE
    );
    assert_eq!(
        error_info(&Error::semantic(
            "ConstraintValidationFailed: `p_id` uniqueness"
        ))
        .neo4j_code,
        SEMANTIC_ERROR_CODE
    );
}
