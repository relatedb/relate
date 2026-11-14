//! The `Error` contract's pin tests: each typed variant's Display
//! against the exact text of the stringly error it replaced (ADR-0307,
//! ADR-0489, ADR-0492), and the shared class/detail helpers.

use super::*;

#[test]
fn capacity_display_is_stable() {
    let e = Error::capacity(100, 64);
    assert_eq!(e.to_string(), "capacity exceeded: used=100 limit=64");
    assert!(matches!(
        e,
        Error::Capacity {
            used: 100,
            limit: 64,
            ..
        }
    ));
}

#[test]
fn resource_limit_display_names_the_flag() {
    let e = Error::resource_limit("max-active-queries", 4, false);
    assert_eq!(
        e.to_string(),
        "resource limit exceeded: max-active-queries limit=4"
    );
    assert!(matches!(
        e,
        Error::ResourceLimit {
            retryable: false,
            ..
        }
    ));
    let retry = Error::resource_limit("max-active-queries", 8, true);
    assert!(matches!(
        retry,
        Error::ResourceLimit {
            retryable: true,
            ..
        }
    ));
}

#[test]
fn internal_display_is_stable() {
    let e = Error::internal("query worker thread panicked");
    assert_eq!(
        e.to_string(),
        "internal error: query worker thread panicked"
    );
    assert!(matches!(e, Error::Internal(_)));
}

#[test]
fn locked_is_distinct_from_storage_and_io() {
    let error = Error::Locked("/db/.lock".into());
    assert_eq!(error.to_string(), "storage locked: /db/.lock");
    assert!(matches!(error, Error::Locked(_)));
}

/// ADR-0494: the WAL payload cap refusal is the capacity family's
/// `WalPayload` kind — the memory-limit class on every surface — and its
/// Display is the communicative capacity line: what hit the limit (a
/// single WAL commit record), the counted lower bound, and the knob (the
/// per-record cap), both figures rendered in MiB through `wal_mib`
/// (the unit ADR-0086 states the cap in) from the `limit` the check consulted (`WAL_PAYLOAD_CAP_BYTES`
/// at both refusal sites).
#[test]
fn wal_payload_cap_renders_the_capacity_memory_limit_line() {
    let error = Error::capacity_wal_payload(
        Error::WAL_PAYLOAD_CAP_BYTES + 2 * 1024 * 1024,
        Error::WAL_PAYLOAD_CAP_BYTES,
    );
    assert_eq!(
        error.to_string(),
        "This statement tried to commit a single WAL record of at least \
         66 MiB. This is a safety limit protecting against memory \
         exhaustion — the per-commit WAL payload cap is 64 MiB (ADR-0086); \
         commit the data in smaller transactions (CALL { … } IN \
         TRANSACTIONS OF n ROWS, or client-side batches)."
    );
    assert!(matches!(
        &error,
        Error::Capacity {
            kind: CapacityKind::WalPayload,
            ..
        }
    ));
}

/// ADR-0567: the shortest-route endpoint listing refusal is the capacity
/// family's `SeekCandidates` kind and names the bound and the remedy.
#[test]
fn seek_candidates_cap_renders_the_capacity_line() {
    let error = Error::capacity_seek_candidates(1_048_577, 1_048_576);
    assert_eq!(
        error.to_string(),
        "A shortest-path endpoint pattern matches at least 1048577 nodes; \
         the search lists at most 1048576 to choose its side. This is a \
         safety limit protecting against memory exhaustion — bind the \
         endpoint or narrow it with an indexed property."
    );
    assert!(matches!(
        &error,
        Error::Capacity {
            kind: CapacityKind::SeekCandidates,
            ..
        }
    ));
}

#[test]
fn uniqueness_violation_display_matches_the_legacy_semantic_rendering() {
    let typed = Error::uniqueness_violation("p_id");
    assert_eq!(
        typed.to_string(),
        Error::semantic("ConstraintValidationFailed: `p_id` uniqueness").to_string()
    );
    assert_eq!(
        typed.to_string(),
        "semantic error: ConstraintValidationFailed: `p_id` uniqueness"
    );
    assert!(matches!(
        &typed,
        Error::UniquenessViolation { name } if name == "p_id"
    ));
    assert_eq!(
        uniqueness_violation_detail("p_id"),
        "ConstraintValidationFailed: `p_id` uniqueness"
    );
}

#[test]
fn typed_detail_variants_display_matches_the_legacy_renderings() {
    let typed = Error::invalid_argument_type("round value must be numeric");
    assert_eq!(
        typed.to_string(),
        Error::ty("InvalidArgumentType: round value must be numeric").to_string()
    );
    assert_eq!(
        typed.to_string(),
        "type error: InvalidArgumentType: round value must be numeric"
    );
    assert!(matches!(&typed, Error::InvalidArgumentType(d) if d == "round value must be numeric"));

    let typed = Error::type_error("schema name must be STRING");
    assert_eq!(
        typed.to_string(),
        Error::ty("TypeError: schema name must be STRING").to_string()
    );
    assert_eq!(
        typed.to_string(),
        "type error: TypeError: schema name must be STRING"
    );
    assert!(matches!(&typed, Error::TypeError(d) if d == "schema name must be STRING"));

    let typed = Error::parameter_missing("parameter $n was not supplied");
    assert_eq!(
        typed.to_string(),
        Error::semantic("ParameterMissing: parameter $n was not supplied").to_string()
    );
    assert_eq!(
        typed.to_string(),
        "semantic error: ParameterMissing: parameter $n was not supplied"
    );
    assert!(matches!(&typed, Error::ParameterMissing(d) if d == "parameter $n was not supplied"));

    // The run-time argument rejection keeps the legacy bare rendering; the
    // kind travels as data, not as a prefix inside the detail string.
    let typed = Error::argument_value_kind(ErrorKind::NumberOutOfRange, "percentile");
    assert_eq!(
        typed.to_string(),
        Error::semantic("NumberOutOfRange: percentile").to_string()
    );
    assert_eq!(
        typed.to_string(),
        "semantic error: NumberOutOfRange: percentile"
    );
    assert!(matches!(
        &typed,
        Error::ArgumentValue {
            kind: ErrorKind::NumberOutOfRange,
            detail,
        } if detail == "percentile"
    ));
}

/// ADR-0511: the boolean-requiring family's one constructor builds the
/// `<ctx> requires a boolean` detail on the typed `InvalidArgumentType`
/// variant, so the planner's and the executor's sites render identically.
#[test]
fn requires_boolean_builds_the_shared_boolean_family_detail() {
    let typed = Error::requires_boolean("quantifier WHERE");
    assert_eq!(
        typed.to_string(),
        Error::invalid_argument_type("quantifier WHERE requires a boolean").to_string()
    );
    assert_eq!(
        typed.to_string(),
        "type error: InvalidArgumentType: quantifier WHERE requires a boolean"
    );
    assert!(
        matches!(&typed, Error::InvalidArgumentType(d) if d == "quantifier WHERE requires a boolean")
    );
}

/// The static sibling keeps the same detail over the Syntax class, and a
/// typed construction error re-classed by [`Error::into_syntax`] does not
/// carry its old class prefix into the new message (ADR-0566).
#[test]
fn static_requires_boolean_and_into_syntax_keep_the_detail_only() {
    assert_eq!(
        Error::static_requires_boolean("AND").to_string(),
        "syntax error: InvalidArgumentType: AND requires a boolean"
    );
    // A bare Type payload keeps its detail alone.
    assert_eq!(
        Error::ty("cannot parse date '2022-1'")
            .into_syntax()
            .to_string(),
        "syntax error: cannot parse date '2022-1'"
    );
    // A token-carrying variant keeps its pinned detail token.
    assert_eq!(
        Error::invalid_argument_type("temporal value")
            .into_syntax()
            .to_string(),
        "syntax error: InvalidArgumentType: temporal value"
    );
}

/// One arity contract per shared builtin, by name: the front-end check, the
/// planner's table and the executor's dispatch read the same count and text.
#[test]
fn builtin_arity_contracts_are_shared() {
    assert_eq!(builtin_arity("char_length"), Some(CHAR_LENGTH_ARITY));
    assert_eq!(builtin_arity("character_length"), Some(CHAR_LENGTH_ARITY));
    assert_eq!(builtin_arity("nullif"), Some(NULLIF_ARITY));
    assert_eq!(builtin_arity("format"), Some(FORMAT_ARITY));
    assert_eq!(builtin_arity("vector"), Some(VECTOR_ARITY));
    assert_eq!(
        NULLIF_ARITY.violation(1),
        Some(NULLIF_REQUIRES_TWO_ARGUMENTS)
    );
    assert_eq!(NULLIF_ARITY.violation(2), None);
    assert_eq!(
        FORMAT_ARITY.violation(0),
        Some(FORMAT_REQUIRES_ONE_OR_TWO_ARGUMENTS)
    );
    assert_eq!(FORMAT_ARITY.violation(1), None);
    assert_eq!(FORMAT_ARITY.violation(2), None);
    assert!(FORMAT_ARITY.violation(3).is_some());
    assert!(VECTOR_ARITY.violation(3).is_none() && VECTOR_ARITY.violation(2).is_some());
    assert_eq!(
        CHAR_LENGTH_ARITY.violation(0),
        Some(FUNCTION_REQUIRES_ONE_ARGUMENT)
    );
    assert_eq!(builtin_arity("lower"), None);
}

/// ADR-0307: the three typed variants render exactly as the stringly
/// errors they replace.
#[test]
fn adr_0307_variants_display_matches_the_legacy_renderings() {
    let cases = [
        (
            Error::arithmetic(ArithmeticFailure::DivisionByZero),
            Error::ty("/ by zero"),
        ),
        (
            Error::arithmetic(ArithmeticFailure::ModuloByZero),
            Error::ty("% by zero"),
        ),
        (
            Error::arithmetic(ArithmeticFailure::RoundingNecessary),
            Error::execution("ArithmeticError: rounding necessary"),
        ),
        (
            Error::database_not_found("nosuchdb"),
            Error::semantic("DatabaseNotFound: Graph not found: nosuchdb"),
        ),
        (Error::cancelled(), Error::execution("query cancelled")),
    ];
    for (typed, legacy) in cases {
        assert_eq!(typed.to_string(), legacy.to_string(), "{typed:?}");
    }
    assert_eq!(Error::cancelled(), Error::Cancelled);
    assert_eq!(Error::cancelled().to_string(), "query cancelled");
    assert_eq!(
        Error::database_not_found("x").to_string(),
        "semantic error: DatabaseNotFound: Graph not found: x"
    );
    assert_eq!(ArithmeticFailure::DivisionByZero.detail(), "/ by zero");
    assert_eq!(ArithmeticFailure::ModuloByZero.detail(), "% by zero");
    assert_eq!(
        ArithmeticFailure::RoundingNecessary.detail(),
        "ArithmeticError: rounding necessary"
    );
    assert_eq!(
        Error::database_not_found("x")
            .typed_detail_text()
            .as_deref(),
        Some("DatabaseNotFound: Graph not found: x")
    );
    assert_eq!(Error::cancelled().typed_detail(), None);
    assert_eq!(
        Error::arithmetic(ArithmeticFailure::DivisionByZero).typed_detail(),
        None
    );
}

/// ADR-0338: the inner type renders its bare payload; the `Error`
/// variant composes the legacy full text from the shared Type prefix.
#[test]
fn arithmetic_failure_display_is_the_bare_payload() {
    assert_eq!(ArithmeticFailure::DivisionByZero.to_string(), "/ by zero");
    assert_eq!(ArithmeticFailure::ModuloByZero.to_string(), "% by zero");
    assert_eq!(
        ArithmeticFailure::RoundingNecessary.to_string(),
        "ArithmeticError: rounding necessary"
    );
    assert_eq!(
        Error::arithmetic(ArithmeticFailure::DivisionByZero).to_string(),
        "type error: / by zero"
    );
    assert_eq!(
        Error::ty("anything").to_string(),
        format!("{TYPE_ERROR_PREFIX}: anything")
    );
}

/// ADR-0489: the argument-class variant keeps the semantic detail text
/// and travels as typed data, never as a message prefix.
#[test]
fn argument_error_keeps_the_semantic_detail_text() {
    let error = Error::argument_error("InvalidIndexConfig");
    assert_eq!(
        error.to_string(),
        Error::semantic("InvalidIndexConfig").to_string()
    );
    assert_eq!(error.to_string(), "semantic error: InvalidIndexConfig");
    assert!(matches!(&error, Error::ArgumentError(detail) if detail == "InvalidIndexConfig"));
}

/// ADR-0335: the one spelling of the unbound-variable error.
#[test]
fn undefined_variable_renders_like_the_front_end_error() {
    let error = Error::undefined_variable("x");
    assert_eq!(
        error.to_string(),
        "syntax error: UndefinedVariable: variable 'x' not defined"
    );
    assert!(matches!(error, Error::Syntax(_)));
}

/// ADR-0308: the one spelling of the missing-parameter message.
#[test]
fn parameters_missing_names_every_parameter_in_order() {
    let one = Error::parameters_missing(["p"]);
    assert_eq!(one, Error::parameter_missing("Expected parameter(s): p"));
    assert_eq!(
        one.to_string(),
        "semantic error: ParameterMissing: Expected parameter(s): p"
    );
    assert_eq!(
        Error::parameters_missing(["b", "a"]).to_string(),
        "semantic error: ParameterMissing: Expected parameter(s): b, a"
    );
}

#[test]
fn procedure_not_found_display_matches_the_legacy_syntax_rendering() {
    let typed = Error::procedure_not_found("dbms.noSuchProc");
    assert_eq!(
        typed.to_string(),
        Error::syntax("ProcedureNotFound: dbms.noSuchProc").to_string()
    );
    assert_eq!(
        typed.to_string(),
        "syntax error: ProcedureNotFound: dbms.noSuchProc"
    );
    assert!(matches!(
        &typed,
        Error::ProcedureNotFound { name } if name == "dbms.noSuchProc"
    ));
}

#[test]
fn typed_detail_single_sources_the_class_tokens() {
    fn detail_text(error: &Error) -> Option<(String, String)> {
        error
            .typed_detail()
            .map(|(token, detail)| (token.to_string(), detail.into_owned()))
    }
    let typed = Error::invalid_argument_type("round value must be numeric");
    assert_eq!(
        detail_text(&typed),
        Some((
            "InvalidArgumentType".to_string(),
            "round value must be numeric".to_string()
        ))
    );
    assert_eq!(
        typed.typed_detail_text().as_deref(),
        Some("InvalidArgumentType: round value must be numeric")
    );
    let typed = Error::type_error("schema name must be STRING");
    assert_eq!(
        typed.typed_detail_text().as_deref(),
        Some("TypeError: schema name must be STRING")
    );
    let typed = Error::parameter_missing("parameter $n was not supplied");
    assert_eq!(
        typed.typed_detail_text().as_deref(),
        Some("ParameterMissing: parameter $n was not supplied")
    );
    // The kind answers the accessor as the token, so no consumer parses the
    // rendered `semantic error: <Kind>: <detail>` text.
    let typed = Error::argument_value_kind(ErrorKind::NumberOutOfRange, "percentile");
    assert_eq!(
        detail_text(&typed),
        Some(("NumberOutOfRange".to_string(), "percentile".to_string()))
    );
    assert_eq!(
        typed.typed_detail_text().as_deref(),
        Some("NumberOutOfRange: percentile")
    );
    // The two structured-field variants fit the same (token, detail)
    // shape; the uniqueness detail is the owned `` `<name>` uniqueness ``
    // composition the Display attribute spells inline.
    let typed = Error::procedure_not_found("dbms.noSuchProc");
    assert_eq!(
        detail_text(&typed),
        Some((
            "ProcedureNotFound".to_string(),
            "dbms.noSuchProc".to_string()
        ))
    );
    assert_eq!(
        typed.typed_detail_text().as_deref(),
        Some("ProcedureNotFound: dbms.noSuchProc")
    );
    let typed = Error::uniqueness_violation("p_id");
    assert_eq!(
        detail_text(&typed),
        Some((
            "ConstraintValidationFailed".to_string(),
            "`p_id` uniqueness".to_string()
        ))
    );
    assert_eq!(
        typed.typed_detail_text().as_deref(),
        Some(uniqueness_violation_detail("p_id").as_str())
    );
    assert_eq!(Error::ty("anything").typed_detail(), None);
    assert_eq!(Error::syntax("anything").typed_detail_text(), None);
}

#[test]
fn invalid_argument_value_composes_the_pinned_token_once() {
    let e = Error::invalid_argument_value("size() on paths");
    assert!(matches!(&e, Error::Type(d) if d == "InvalidArgumentValue: size() on paths"));
    assert_eq!(
        e.to_string(),
        Error::ty("InvalidArgumentValue: size() on paths").to_string()
    );
}

#[test]
fn invalid_argument_type_expected_spells_the_shared_sentence() {
    let e = Error::invalid_argument_type_expected("LIST<ANY>", "INTEGER NOT NULL");
    assert_eq!(
        e,
        Error::invalid_argument_type(expected_type_detail("LIST<ANY>", "INTEGER NOT NULL"))
    );
}
