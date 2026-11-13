//! The vocabulary's own round-trip and class tables (moved with the
//! vocabulary from `relate-error` by ADR-0502 §2). The `EngineError`
//! rendering tests that consume the vocabulary stayed in `relate-error`.

use super::*;

/// Every non-`Uncategorized` kind, straight from the `error_kinds!` list.
const ALL_KINDS: &[ErrorKind] = ErrorKind::ALL;

#[test]
fn kind_name_round_trips_through_from_name() {
    for kind in ALL_KINDS {
        let name = kind.as_str();
        assert_eq!(
            ErrorKind::from_name(name),
            Some(*kind),
            "from_name({name:?}) did not round-trip"
        );
    }
}

#[test]
fn tck_error_class_from_name_round_trips() {
    use TckErrorClass as C;
    for class in [
        C::SyntaxError,
        C::TypeError,
        C::ArgumentError,
        C::EntityNotFound,
        C::SemanticError,
        C::ProcedureError,
        C::ParameterMissing,
        C::ConstraintVerificationFailed,
    ] {
        assert_eq!(TckErrorClass::from_name(class.as_str()), Some(class));
    }
    // Classes outside the eight (used by cip-eval error steps) do not parse,
    // which is what steers the harness to lenient fallback for them.
    assert_eq!(TckErrorClass::from_name("GraphTypeError"), None);
    assert_eq!(TckErrorClass::from_name("SchemaError"), None);
    assert_eq!(TckErrorClass::from_name("ConstraintValidationFailed"), None);
    assert_eq!(TckErrorClass::from_name(""), None);
}

#[test]
fn uncategorized_is_not_parseable_and_has_a_name() {
    assert_eq!(ErrorKind::from_name("Uncategorized"), None);
    assert_eq!(ErrorKind::Uncategorized.as_str(), "Uncategorized");
}

#[test]
fn from_name_rejects_unknown_prefixes() {
    assert_eq!(ErrorKind::from_name("storage is unavailable"), None);
    assert_eq!(ErrorKind::from_name("constraint violation"), None);
    assert_eq!(ErrorKind::from_name(""), None);
}

#[test]
fn tck_class_is_total_and_stable() {
    // Total mapping: every kind (including Uncategorized) resolves.
    for kind in ALL_KINDS {
        let _ = kind.tck_class();
    }
    assert_eq!(
        ErrorKind::Uncategorized.tck_class(),
        TckErrorClass::SemanticError
    );
    // Spot-check a few representative mappings.
    assert_eq!(
        ErrorKind::SyntaxError.tck_class(),
        TckErrorClass::SyntaxError
    );
    assert_eq!(ErrorKind::TypeError.tck_class(), TckErrorClass::TypeError);
    assert_eq!(
        ErrorKind::InvalidArgumentValue.tck_class(),
        TckErrorClass::TypeError
    );
    assert_eq!(
        ErrorKind::ParameterMissing.tck_class(),
        TckErrorClass::ParameterMissing
    );
    assert_eq!(
        ErrorKind::EntityNotFound.tck_class(),
        TckErrorClass::EntityNotFound
    );
    assert_eq!(
        ErrorKind::ConstraintViolation.tck_class(),
        TckErrorClass::ConstraintVerificationFailed
    );
    assert_eq!(
        ErrorKind::ProcedureError.tck_class(),
        TckErrorClass::ProcedureError
    );
}

/// The Phase-6 detail gaps: every new ErrorKind variant maps to the TCK
/// error *class* that the corresponding scenarios expect (verified against
/// the `<Class>` slot of the `should be raised` steps in the openCypher
/// TCK — the spec harness checkout's `features/tck/` corpus).
#[test]
fn phase6_detail_kinds_have_expected_tck_class() {
    use ErrorKind as K;
    use TckErrorClass as C;
    let cases: &[(ErrorKind, TckErrorClass)] = &[
        (K::IntegerOverflow, C::SyntaxError),
        (K::FloatingPointOverflow, C::SyntaxError),
        (K::InvalidNumberLiteral, C::SyntaxError),
        (K::InvalidUnicodeLiteral, C::SyntaxError),
        (K::InvalidUnicodeCharacter, C::SyntaxError),
        (K::InvalidRelationshipPattern, C::SyntaxError),
        (K::InvalidArgumentPassingMode, C::SyntaxError),
        (K::InvalidDelete, C::SyntaxError),
        (K::MapElementAccessByNonString, C::TypeError),
        (K::NumberOutOfRange, C::ArgumentError),
        (K::DeleteConnectedNode, C::ConstraintVerificationFailed),
        (K::ProcedureNotFound, C::ProcedureError),
    ];
    for (kind, class) in cases {
        assert_eq!(kind.tck_class(), *class, "{} class drifted", kind.as_str());
        // as_str/from_name round-trip is total for the new variants too.
        assert_eq!(ErrorKind::from_name(kind.as_str()), Some(*kind));
    }
}

/// The `ParameterMissing`/`MissingParameter` alias: the variant keeps its
/// `as_str`/`from_name` identity (byte-stable), while the matcher-facing
/// `tck_detail`/`from_tck_detail` pair speaks the TCK's `MissingParameter`
/// spelling.
#[test]
fn missing_parameter_alias_round_trips_without_touching_display() {
    // Canonical identity is unchanged.
    assert_eq!(ErrorKind::ParameterMissing.as_str(), "ParameterMissing");
    assert_eq!(
        ErrorKind::from_name("ParameterMissing"),
        Some(ErrorKind::ParameterMissing)
    );
    // `MissingParameter` is NOT a canonical name.
    assert_eq!(ErrorKind::from_name("MissingParameter"), None);
    // But it IS the TCK detail, reachable through the matcher-facing API.
    assert_eq!(ErrorKind::ParameterMissing.tck_detail(), "MissingParameter");
    assert_eq!(
        ErrorKind::from_tck_detail("MissingParameter"),
        Some(ErrorKind::ParameterMissing)
    );
}

/// `tck_detail` equals `as_str` for every non-aliased kind, and
/// `from_tck_detail` is its inverse (delegating to `from_name`).
#[test]
fn tck_detail_matches_as_str_except_aliases() {
    for kind in ALL_KINDS {
        if *kind == ErrorKind::ParameterMissing {
            assert_eq!(kind.tck_detail(), "MissingParameter");
            continue;
        }
        assert_eq!(kind.tck_detail(), kind.as_str());
        assert_eq!(ErrorKind::from_tck_detail(kind.tck_detail()), Some(*kind));
    }
    assert_eq!(ErrorKind::from_tck_detail("NoSuchDetail"), None);
}

/// The two detail helpers every error channel renders through, pinned by
/// their exact text so neither channel can drift.
#[test]
fn detail_helpers_spell_the_one_text() {
    assert_eq!(undefined_variable_detail("n"), "variable 'n' not defined");
    assert_eq!(
        duplicate_variable_detail("n"),
        "variable 'n' already declared"
    );
}

#[test]
fn tck_class_name_round_trips_through_from_name() {
    assert_eq!(TckErrorClass::ALL.len(), 8);
    for class in TckErrorClass::ALL {
        assert_eq!(TckErrorClass::from_name(class.as_str()), Some(*class));
    }
    assert_eq!(TckErrorClass::from_name("Uncategorized"), None);
}
