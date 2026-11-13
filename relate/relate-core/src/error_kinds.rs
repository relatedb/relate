//! The shared error-kind vocabulary: [`ErrorKind`] (the finest TCK-aligned
//! error name) and [`TckErrorClass`] (the eight openCypher TCK error
//! classes), plus the one spelling of the two detail helpers the typed
//! channel and the stringly query-error channel share.
//!
//! This module moved here from `relate-error` (ADR-0502 §2): the
//! engine's one error classification — `relate_error::error_info` over
//! [`crate::Error`] — lives in `relate-error`, which must therefore sit
//! *above* this crate; the vocabulary both crates share sits here, at the
//! lowest layer, and `relate-error` re-exports it so every existing
//! `relate_error::ErrorKind` path keeps resolving.

use std::fmt;

/// Generates [`TckErrorClass`] with its `as_str` / `from_name` tables and
/// the [`TckErrorClass::ALL`] roster from one variant list, so the enum,
/// the spelling and the inverse cannot drift apart.
macro_rules! tck_error_classes {
    ($($variant:ident),+ $(,)?) => {
        /// One of the eight openCypher TCK error *classes*. The spec harness
        /// (ADR-0482) asserts an expected class, while [`ErrorKind`] carries
        /// the finer-grained detail. Every [`ErrorKind`] maps to exactly one
        /// class via [`ErrorKind::tck_class`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum TckErrorClass {
            $($variant,)+
        }

        impl TckErrorClass {
            /// Every class, in declaration order.
            pub const ALL: &'static [TckErrorClass] = &[$(TckErrorClass::$variant,)+];

            /// The canonical openCypher class name.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(TckErrorClass::$variant => stringify!($variant),)+
                }
            }

            /// Parses one of the eight canonical openCypher class names back
            /// into a [`TckErrorClass`]; the inverse of
            /// [`TckErrorClass::as_str`]. Returns `None` for any other
            /// string.
            ///
            /// The spec harness's `should be raised` matcher makes the same
            /// call over the same eight names (its `clspec/errors.py`,
            /// ADR-0482): a step whose class is not one of them (e.g. the
            /// harness `cip-eval` suite's `GraphTypeError`, `SchemaError`,
            /// `ConstraintValidationFailed`) falls back to lenient any-error
            /// matching rather than a strict class assertion.
            pub fn from_name(name: &str) -> Option<TckErrorClass> {
                match name {
                    $(stringify!($variant) => Some(TckErrorClass::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

tck_error_classes! {
    SyntaxError,
    TypeError,
    ArgumentError,
    EntityNotFound,
    SemanticError,
    ProcedureError,
    ParameterMissing,
    ConstraintVerificationFailed,
}

impl fmt::Display for TckErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Generates [`ErrorKind`] plus its `as_str`/`from_name`/`tck_class` tables
/// from a single `Variant => TckClass` list, so the three stay in lockstep.
/// The catch-all `Uncategorized` variant is appended automatically; it is the
/// documented escape hatch for legacy strings that carry no recognized kind
/// prefix, and it renders with no `"Kind: "` prefix at all (see
/// `relate_error::EngineError`'s `Display`).
macro_rules! error_kinds {
    ($($variant:ident => $class:ident),+ $(,)?) => {
        /// The specific, TCK-aligned name of an engine error. `kind()` returns
        /// the most specific name available; nested detail (e.g. the
        /// `UnsupportedIndexOrConstraint` in `"SyntaxError:
        /// UnsupportedIndexOrConstraint: …"`) stays in the message.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorKind {
            $($variant,)+
            /// Escape hatch for legacy/opaque errors with no recognized kind
            /// prefix. Renders without a prefix so the raw message shows
            /// through byte-for-byte.
            Uncategorized,
        }

        impl ErrorKind {
            /// Every non-`Uncategorized` kind, in declaration order —
            /// generated from the same list, so coverage tests iterate the
            /// vocabulary itself rather than a hand copy.
            pub const ALL: &'static [ErrorKind] = &[$(ErrorKind::$variant,)+];

            /// The canonical string form of this kind — the inverse of
            /// [`ErrorKind::from_name`] for every non-`Uncategorized` variant.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(ErrorKind::$variant => stringify!($variant),)+
                    ErrorKind::Uncategorized => "Uncategorized",
                }
            }

            /// Parses a kind name back into an [`ErrorKind`]. Returns `None`
            /// for unrecognized names (including `"Uncategorized"`, which is
            /// never produced by parsing).
            pub fn from_name(name: &str) -> Option<ErrorKind> {
                match name {
                    $(stringify!($variant) => Some(ErrorKind::$variant),)+
                    _ => None,
                }
            }

            /// The openCypher TCK error *class* this kind belongs to. Total
            /// over every variant.
            pub fn tck_class(&self) -> TckErrorClass {
                match self {
                    $(ErrorKind::$variant => TckErrorClass::$class,)+
                    ErrorKind::Uncategorized => TckErrorClass::SemanticError,
                }
            }
        }
    };
}

// The ~60 error-kind prefixes already emitted by the engine (derived by
// grepping the `"Word:"` error-literal pattern), plus a few kinds used to
// classify `StorageError` in `EngineError::kind`. The `=> Class` mapping to
// the eight TCK classes is a best-effort taxonomy refined
// against the real TCK error scenarios; it never affects `Display` bytes.
error_kinds! {
    // --- Syntax / structural / scope / aggregation validation ---
    SyntaxError => SyntaxError,
    UnexpectedSyntax => SyntaxError,
    IncompleteQuery => SyntaxError,
    InvalidPatternCharacter => SyntaxError,
    InvalidLabelExpression => SyntaxError,
    InvalidNodePatternPredicate => SyntaxError,
    InvalidClauseComposition => SyntaxError,
    InvalidAggregation => SyntaxError,
    AmbiguousAggregationExpression => SyntaxError,
    NestedAggregation => SyntaxError,
    NegativeIntegerArgument => SyntaxError,
    NoSingleRelationshipType => SyntaxError,
    DifferentColumnsInUnion => SyntaxError,
    VariableAlreadyBound => SyntaxError,
    VariableAlreadyDefined => SyntaxError,
    VariableTypeConflict => SyntaxError,
    VariableShadowing => SyntaxError,
    UndefinedVariable => SyntaxError,
    UnknownFunction => SyntaxError,
    NoExpressionAlias => SyntaxError,
    NoVariablesInScope => SyntaxError,
    RelationshipUniquenessViolation => SyntaxError,
    CreatingVarLength => SyntaxError,
    RequiresDirectedRelationship => SyntaxError,
    InvalidParameterUse => SyntaxError,
    ColumnNameConflict => SyntaxError,
    QuantifiedPathPatternNotSupported => SyntaxError,
    InvalidSelectivePathPatternDependency => SyntaxError,
    GroupVariableReuse => SyntaxError,
    SingletonReusedAsGroup => SyntaxError,
    NonLocalGroupVariableReference => SyntaxError,
    GroupVariableInNonAggregatingOperation => SyntaxError,
    InvalidSearchVariableReference => SyntaxError,
    SelfReferencingSearch => SyntaxError,
    InvalidUseOfCommand => SyntaxError,
    InvalidPlacementOfUseClause => SyntaxError,
    // ADR-0116 §2 (2026-11-04): a `USE` clause targeting a non-system
    // namespace on a statement headed by a database-administration command —
    // CE's 42N79 auto-routing rejection, which Neo4j surfaces as a
    // SyntaxException (Neo.ClientError.Statement.SyntaxError).
    UseClauseWithAdministrationCommand => SyntaxError,
    InvalidUpdatingClauseInCountSubquery => SyntaxError,
    InvalidUpdatingClauseInCollectSubquery => SyntaxError,
    NonConstantExpression => SyntaxError,
    NonDeterministicDisjointBy => SyntaxError,
    InvalidShortestPathMixing => SyntaxError,
    InvalidShowColumn => SyntaxError,
    InvalidNumberOfArguments => SyntaxError,
    // ADR-0466 (LABS-610): an explicit traversal hop bound above the
    // admission limit (`--max-traversal-depth`, default 500). Raised at the
    // lowering boundary as a syntax error: the statement is admissibly
    // invalid before the walker ever runs (Bolt
    // `Neo.ClientError.Statement.SyntaxError`, HTTP 400/42N06).
    TraversalDepthExceeded => SyntaxError,
    // LABS-680 (ADR-0477): a statement shape `--safe` refuses. Like
    // `TraversalDepthExceeded` it is raised at the lowering boundary and
    // surfaces as a syntax error (Bolt `Neo.ClientError.Statement.SyntaxError`,
    // HTTP 400/42N06, CLI exit 6). The message states that it is a policy,
    // not a database limit.
    SafeModeViolation => SyntaxError,
    // Literal-lexing / literal-range failures raised by the parser and surfaced
    // through `EngineError::Syntax`'s `ParseError::detail` tag. All are
    // `SyntaxError`-class compile-time failures in the TCK error grammar
    // (Literals2/3/5/6, Mathematical3, Match4).
    IntegerOverflow => SyntaxError,
    FloatingPointOverflow => SyntaxError,
    InvalidNumberLiteral => SyntaxError,
    InvalidUnicodeLiteral => SyntaxError,
    InvalidUnicodeCharacter => SyntaxError,
    InvalidRelationshipPattern => SyntaxError,
    InvalidArgumentPassingMode => SyntaxError,
    // `DELETE`ing a non-entity that is specifically a label predicate
    // (`DELETE n:Person`, `DELETE r:T`) is a `SyntaxError: InvalidDelete`
    // (Delete1/Delete2); a non-label non-entity expression (`DELETE 1 + 1`)
    // stays `InvalidArgumentType` (Delete5).
    InvalidDelete => SyntaxError,
    // --- Type errors ---
    TypeError => TypeError,
    InvalidArgumentType => TypeError,
    // A `||` operand that is statically known to be neither a string nor a
    // list (CIP-144). Unlike the other static type checks — which openCypher
    // surfaces as `SyntaxError: InvalidArgumentType` — Neo4j reports this
    // one as a `TypeError`, matching its runtime twin, so the class travels
    // apart from `InvalidArgumentType`.
    InvalidConcatenationOperand => TypeError,
    ParameterWrongTypeException => TypeError,
    InvalidPropertyType => TypeError,
    InvalidPropertyValueType => TypeError,
    // Indexing a map with a non-string key (`{name: 'Apa'}[0]`) is a runtime
    // `TypeError: MapElementAccessByNonString` (Map2).
    MapElementAccessByNonString => TypeError,
    // --- Argument value / arithmetic errors ---
    ArgumentError => ArgumentError,
    // The `InvalidArgumentValue` detail is classified as TypeError by both the
    // openCypher TCK (expressions/graph/Graph3-4, typeConversion/TypeConversion1-4)
    // and the in-repo cip-eval suite (63 TypeError vs 0 ArgumentError occurrences).
    InvalidArgumentValue => TypeError,
    ArithmeticError => ArgumentError,
    // A numeric argument outside its permitted range: `range()` step 0,
    // `percentileCont`/`percentileDisc` percentile outside [0,1] — a runtime
    // `ArgumentError: NumberOutOfRange` (List11, Aggregation6).
    NumberOutOfRange => ArgumentError,
    // ADR-0395: a statement's nested evaluation (CALL bodies,
    // EXISTS/COUNT/COLLECT subqueries, FOREACH clause lists, NEXT
    // stages) exceeded the fixed per-statement pass budget — the runtime
    // twin of the compile-side cloned-node budget (ADR-0382). Raised as
    // an `Error::Execution` payload whose `Kind:` prefix names this row.
    NestedEvaluationBudgetExceeded => ArgumentError,
    // ADR-0478 Decision 6: a frontier-NFA walk's incident-edge visits
    // exceeded the fixed per-run work budget — the traversal twin of
    // ADR-0395's pass budget, so a frontier that would blow up (huge
    // diameter x automaton states) fails loud in milliseconds instead of
    // by watchdog cancel after RSS growth. Raised as an
    // `Error::Execution` payload whose `Kind:` prefix names this row.
    WalkBudgetExceeded => ArgumentError,
    // ADR-0462 (LABS-606): a dynamically constructed `Value` would nest
    // past `MAX_VALUE_DEPTH` (a `reduce`/list-comprehension accumulator
    // growing one level per iteration is the canonical shape) — the value
    // twin of ADR-0257's static parse-nesting limit. Raised as an
    // `Error::Execution` payload whose `Kind:` prefix names this row.
    ValueDepthExceeded => ArgumentError,
    // --- Parameters ---
    ParameterMissing => ParameterMissing,
    // --- Entity access ---
    EntityNotFound => EntityNotFound,
    DeletedEntityAccess => EntityNotFound,
    // --- Constraints ---
    ConstraintViolation => ConstraintVerificationFailed,
    // Deleting a still-connected node without `DETACH` is a runtime
    // `ConstraintVerificationFailed: DeleteConnectedNode` (Delete1). Distinct
    // from a general `ConstraintViolation` so the finer detail survives.
    DeleteConnectedNode => ConstraintVerificationFailed,
    // --- Procedures ---
    ProcedureError => ProcedureError,
    // Calling a procedure that is not registered is a
    // `ProcedureError: ProcedureNotFound` (Call1).
    ProcedureNotFound => ProcedureError,
    // --- Schema / database / graph-type / data / transaction (semantic) ---
    SchemaError => SemanticError,
    MergeReadOwnWrites => SemanticError,
    DataException => SemanticError,
    DataError => SemanticError,
    DatabaseNotFound => SemanticError,
    DatabaseExists => SemanticError,
    DatabaseError => SemanticError,
    SystemDatabaseRequired => SemanticError,
    GraphTypeNotFound => SemanticError,
    InternalError => SemanticError,
    TransactionError => SemanticError,
    // ADR-0116 §9: a database administration command outside the CE-supported
    // set (Neo4j's `Neo.ClientError.Statement.UnsupportedAdministrationCommand`,
    // GQLSTATUS 51N27).
    UnsupportedAdministrationCommand => SemanticError,
    // ADR-0116 §9: a write against the read-only `system_ontologies`
    // namespace (GQLSTATUS 42N18).
    WriteOnReadOnlyAccessDatabase => SemanticError,
}

impl ErrorKind {
    /// The openCypher-TCK error *detail* name for this kind — what the harness
    /// compares against the `<Detail>` slot of `a <Class> should be raised at
    /// <phase>: <Detail>`. Equal to [`ErrorKind::as_str`] for every kind except
    /// the aliased ones below. Never used for `Display`.
    ///
    /// Currently one alias exists:
    ///
    /// | `ErrorKind` variant | Display / `as_str` | TCK detail (`tck_detail`) |
    /// |---------------------|--------------------|---------------------------|
    /// | [`ErrorKind::ParameterMissing`] | `ParameterMissing` | `MissingParameter` |
    pub fn tck_detail(&self) -> &'static str {
        match self {
            // The TCK spells our `ParameterMissing` as `MissingParameter`
            // (clauses/call/Call1 scenario 11). `as_str` stays `ParameterMissing`
            // for Display/`from_name` round-trip compatibility.
            ErrorKind::ParameterMissing => "MissingParameter",
            other => other.as_str(),
        }
    }

    /// Parses a TCK error-*detail* name into an [`ErrorKind`] — the inverse of
    /// [`ErrorKind::tck_detail`]. Handles the aliases first, then delegates to
    /// [`ErrorKind::from_name`]. Returns `None` for unrecognized names.
    pub fn from_tck_detail(name: &str) -> Option<ErrorKind> {
        match name {
            "MissingParameter" => Some(ErrorKind::ParameterMissing),
            other => ErrorKind::from_name(other),
        }
    }
}

/// The one spelling of an undefined-variable rejection's detail (after the
/// `UndefinedVariable:` kind token): `variable '<name>' not defined`. Both
/// error channels — the typed `relate_error::EngineError`
/// and the executor's string-carrying [`crate::Error`] — render the same
/// text.
pub fn undefined_variable_detail(name: &str) -> String {
    format!("variable '{name}' not defined")
}

/// The one spelling of a duplicate-binding rejection's detail (after the
/// `VariableAlreadyBound:` kind token): `variable '<name>' already declared`.
pub fn duplicate_variable_detail(name: &str) -> String {
    format!("variable '{name}' already declared")
}

/// The one spelling of an unknown-function rejection's detail (after the
/// `UnknownFunction:` kind token): `unknown function <name>()`. The planner's
/// binder, the executor's scalar-eval backstop and the front's preview-name
/// admission gate all render this text, so a gated name is rejected exactly
/// like a name the registry has never known.
pub fn unknown_function_detail(name: &str) -> String {
    format!("unknown function {name}()")
}

#[cfg(test)]
mod tests;
