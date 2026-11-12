//! The condition-token vocabulary: the class names the engine embeds in a
//! still-stringly payload, the one canonical scan that recovers them
//! (ADR-0502 §2), and the documented GQLSTATUS each carries. Split out of
//! `classification` so the vocabulary and its codes stay one module apart
//! from the error-classification table that reads them.

/// The GQL general error code: the code of a caller-facing class without a
/// class-specific GQLSTATUS.
pub const GQLSTATUS_GENERAL: &str = "42000";

/// The GQLSTATUS of a rejected statement (the `SyntaxError` class).
pub const GQLSTATUS_SYNTAX: &str = "42N06";

/// The GQLSTATUS of an absent query parameter (ADR-0308).
pub(crate) const GQLSTATUS_PARAMETER_MISSING: &str = "42N02";

/// The GQLSTATUS of a type failure.
pub(crate) const GQLSTATUS_TYPE: &str = "22G03";

/// The GQLSTATUS of an unknown database (ADR-0116 §9).
pub const GQLSTATUS_DATABASE_NOT_FOUND: &str = "42N00";

/// The GQLSTATUS of an unsupported administration command (ADR-0116 §9).
pub(crate) const GQLSTATUS_UNSUPPORTED_ADMINISTRATION_COMMAND: &str = "51N27";

/// The GQLSTATUS of a write against the read-only `system_ontologies`
/// namespace (ADR-0116 §9).
pub(crate) const GQLSTATUS_WRITE_ON_READ_ONLY: &str = "42N18";

/// The GQLSTATUS of an engine-internal failure.
pub(crate) const GQLSTATUS_INTERNAL: &str = "50N42";

// The documented GQLSTATUS of the schema/DDL condition tokens (Neo4j's
// GQLSTATUS table, read 2026-11-12; live-probed on 2026.10.1). A token
// names one failure kind, so the code follows the condition, never the
// raise site.

/// A constraint created over data that violates it.
pub(crate) const GQLSTATUS_CONSTRAINT_CREATION_FAILED: &str = "50N11";

/// A failed `DROP CONSTRAINT` — an absent name or a dependent constraint
/// that only its graph type may manage.
const GQLSTATUS_CONSTRAINT_DROP_FAILED: &str = "50N12";

/// An equivalent constraint already exists.
const GQLSTATUS_CONSTRAINT_EQUIVALENT: &str = "22N65";

/// A conflicting constraint already exists.
const GQLSTATUS_CONSTRAINT_CONFLICTING: &str = "22N66";

/// A constraint with the same name already exists.
const GQLSTATUS_CONSTRAINT_DUPLICATED_NAME: &str = "22N67";

/// An equivalent index already exists.
const GQLSTATUS_INDEX_EQUIVALENT: &str = "22N70";

/// An index with the same name already exists.
const GQLSTATUS_INDEX_DUPLICATED_NAME: &str = "22N71";

/// The index a query names does not exist.
const GQLSTATUS_INDEX_NOT_FOUND: &str = "22N69";

/// A constraint's graph-type dependence is incompatible with an existing
/// constraint.
const GQLSTATUS_INCOMPATIBLE_GRAPH_TYPE_DEPENDENCE: &str = "22NB2";

/// An administration command that needs the system database.
const GQLSTATUS_NOT_SYSTEM_DATABASE: &str = "51N28";

/// A statement execution failure (live's `Statement.ExecutionFailed`); the
/// element-id format rejections carry it.
const GQLSTATUS_EXECUTION_FAILED: &str = "50N00";

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
    // The schema/DDL condition tokens. They name a failure kind whose
    // documented GQLSTATUS differs from its openCypher class; unlike the
    // ten class tokens above they are not part of the bindings' class
    // vocabulary (see [`Self::binding_class`]).
    /// A constraint with the same name already exists (`22N67`).
    ConstraintAlreadyExists,
    /// An equivalent constraint already exists (`22N65`).
    EquivalentConstraintAlreadyExists,
    /// A conflicting constraint already exists (`22N66`).
    ConstraintConflict,
    /// A `DROP CONSTRAINT` whose name does not exist (`50N12`).
    ConstraintNotFound,
    /// A `DROP CONSTRAINT` of a dependent constraint (`50N12`).
    DependentConstraintManagedIndividually,
    /// An index with the same name already exists (`22N71`).
    DuplicatedIndexName,
    /// An index with the same name already exists (`22N71`).
    IndexAlreadyExists,
    /// An equivalent index already exists (`22N70`).
    EquivalentSchemaRuleAlreadyExists,
    /// A constraint's graph-type dependence is incompatible (`22NB2`).
    IncompatibleGraphTypeDependence,
    /// The index a query names does not exist (`22N69`).
    IndexDoesNotExist,
    /// An administration command that needs the system database (`51N28`).
    SystemDatabaseRequired,
    /// An element id whose format cannot be resolved (`50N00`).
    ElementIdUnexpectedFormat,
}

impl NestedErrorClass {
    /// Every class the scan recognizes, in enum order — the one list the
    /// scan's exact-match half and the round-trip test derive from, so a
    /// new variant lands in both by construction and the spellings are
    /// written exactly once (in [`as_str`](Self::as_str)).
    pub(crate) const ALL: [NestedErrorClass; 22] = [
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
        NestedErrorClass::ConstraintAlreadyExists,
        NestedErrorClass::EquivalentConstraintAlreadyExists,
        NestedErrorClass::ConstraintConflict,
        NestedErrorClass::ConstraintNotFound,
        NestedErrorClass::DependentConstraintManagedIndividually,
        NestedErrorClass::DuplicatedIndexName,
        NestedErrorClass::IndexAlreadyExists,
        NestedErrorClass::EquivalentSchemaRuleAlreadyExists,
        NestedErrorClass::IncompatibleGraphTypeDependence,
        NestedErrorClass::IndexDoesNotExist,
        NestedErrorClass::SystemDatabaseRequired,
        NestedErrorClass::ElementIdUnexpectedFormat,
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
            NestedErrorClass::ConstraintAlreadyExists => "ConstraintAlreadyExists",
            NestedErrorClass::EquivalentConstraintAlreadyExists => {
                "EquivalentConstraintAlreadyExists"
            }
            NestedErrorClass::ConstraintConflict => "ConstraintConflict",
            NestedErrorClass::ConstraintNotFound => "ConstraintNotFound",
            NestedErrorClass::DependentConstraintManagedIndividually => {
                "DependentConstraintManagedIndividually"
            }
            NestedErrorClass::DuplicatedIndexName => "DuplicatedIndexName",
            NestedErrorClass::IndexAlreadyExists => "IndexAlreadyExists",
            NestedErrorClass::EquivalentSchemaRuleAlreadyExists => {
                "EquivalentSchemaRuleAlreadyExists"
            }
            NestedErrorClass::IncompatibleGraphTypeDependence => "IncompatibleGraphTypeDependence",
            NestedErrorClass::IndexDoesNotExist => "IndexDoesNotExist",
            NestedErrorClass::SystemDatabaseRequired => "SystemDatabaseRequired",
            NestedErrorClass::ElementIdUnexpectedFormat => "ElementIdUnexpectedFormat",
        }
    }

    /// The openCypher class the language bindings report for this token, or
    /// `None` for a schema/DDL condition token — those are classified for
    /// their GQLSTATUS while the bindings keep the class they had before
    /// the condition had a token (a `Syntax` payload's `SyntaxError`, a
    /// `Semantic`/`Execution` payload's `SemanticError`).
    pub fn binding_class(self) -> Option<&'static str> {
        match self {
            NestedErrorClass::ConstraintVerificationFailed
            | NestedErrorClass::ArgumentError
            | NestedErrorClass::EntityNotFound
            | NestedErrorClass::ProcedureError
            | NestedErrorClass::SyntaxError
            | NestedErrorClass::SemanticError
            | NestedErrorClass::UnsupportedAdministrationCommand
            | NestedErrorClass::WriteOnReadOnlyAccessDatabase
            | NestedErrorClass::DatabaseNotFound
            | NestedErrorClass::TransactionError => Some(self.as_str()),
            NestedErrorClass::ConstraintAlreadyExists
            | NestedErrorClass::EquivalentConstraintAlreadyExists
            | NestedErrorClass::ConstraintConflict
            | NestedErrorClass::ConstraintNotFound
            | NestedErrorClass::DependentConstraintManagedIndividually
            | NestedErrorClass::DuplicatedIndexName
            | NestedErrorClass::IndexAlreadyExists
            | NestedErrorClass::EquivalentSchemaRuleAlreadyExists
            | NestedErrorClass::IncompatibleGraphTypeDependence
            | NestedErrorClass::IndexDoesNotExist
            | NestedErrorClass::SystemDatabaseRequired
            | NestedErrorClass::ElementIdUnexpectedFormat => None,
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
    // A leading name token: a `<Class>:` prefix, or the whole payload when
    // the engine raised the bare token (`Error::semantic("DuplicatedIndexName")`).
    // The names are distinct and the historical match was exact equality,
    // so this lookup over [`NestedErrorClass::ALL`] is behavior-identical to
    // the exact-match arms it replaces — and a new variant joins the scan by
    // joining ALL.
    let name = payload
        .split_once(':')
        .map_or(payload, |(name, _)| name)
        .trim();
    let leading = NestedErrorClass::ALL
        .into_iter()
        .find(|&class| class.as_str() == name);
    if leading.is_some() {
        return leading;
    }
    // The one documented alias: the legacy constraint class was spelled
    // `ConstraintValidationFailed:` and can appear anywhere in the payload,
    // not only as the leading token, so the leading-name lookup above
    // cannot carry it.
    if payload.contains("ConstraintValidationFailed:")
        || payload.contains("ConstraintVerificationFailed:")
    {
        return Some(NestedErrorClass::ConstraintVerificationFailed);
    }
    None
}

/// The GQLSTATUS of a still-stringly payload: a recognized nested class
/// carries its own code (the engine-documented ones of ADR-0116 §9, the
/// syntax class's own code, the GQL general code for every other nested
/// class), and a payload with no nested class carries `own` — its
/// variant's code. The exact split HTTP's error envelope (ADR-0232) has
/// always made over the same scan. Both projections read the one scan the
/// arm makes.
pub(crate) fn stringly_gql_status(
    nested: Option<NestedErrorClass>,
    own: &'static str,
) -> &'static str {
    match nested {
        None => own,
        Some(NestedErrorClass::DatabaseNotFound) => GQLSTATUS_DATABASE_NOT_FOUND,
        Some(NestedErrorClass::UnsupportedAdministrationCommand) => {
            GQLSTATUS_UNSUPPORTED_ADMINISTRATION_COMMAND
        }
        Some(NestedErrorClass::WriteOnReadOnlyAccessDatabase) => GQLSTATUS_WRITE_ON_READ_ONLY,
        Some(NestedErrorClass::SyntaxError) => GQLSTATUS_SYNTAX,
        // The schema/DDL condition tokens carry their documented code.
        Some(NestedErrorClass::ConstraintAlreadyExists) => GQLSTATUS_CONSTRAINT_DUPLICATED_NAME,
        Some(NestedErrorClass::EquivalentConstraintAlreadyExists) => {
            GQLSTATUS_CONSTRAINT_EQUIVALENT
        }
        Some(NestedErrorClass::ConstraintConflict) => GQLSTATUS_CONSTRAINT_CONFLICTING,
        Some(
            NestedErrorClass::ConstraintNotFound
            | NestedErrorClass::DependentConstraintManagedIndividually,
        ) => GQLSTATUS_CONSTRAINT_DROP_FAILED,
        Some(NestedErrorClass::DuplicatedIndexName | NestedErrorClass::IndexAlreadyExists) => {
            GQLSTATUS_INDEX_DUPLICATED_NAME
        }
        Some(NestedErrorClass::EquivalentSchemaRuleAlreadyExists) => GQLSTATUS_INDEX_EQUIVALENT,
        Some(NestedErrorClass::IndexDoesNotExist) => GQLSTATUS_INDEX_NOT_FOUND,
        Some(NestedErrorClass::IncompatibleGraphTypeDependence) => {
            GQLSTATUS_INCOMPATIBLE_GRAPH_TYPE_DEPENDENCE
        }
        Some(NestedErrorClass::SystemDatabaseRequired) => GQLSTATUS_NOT_SYSTEM_DATABASE,
        Some(NestedErrorClass::ElementIdUnexpectedFormat) => GQLSTATUS_EXECUTION_FAILED,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The binding class vocabulary does not move: a condition token reports
    /// no class of its own, so the calling variant's fallback stays.
    #[test]
    fn condition_tokens_keep_the_binding_class_vocabulary() {
        assert_eq!(
            NestedErrorClass::ArgumentError.binding_class(),
            Some("ArgumentError")
        );
        assert_eq!(
            NestedErrorClass::ConstraintAlreadyExists.binding_class(),
            None
        );
        assert_eq!(NestedErrorClass::IndexDoesNotExist.binding_class(), None);
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
