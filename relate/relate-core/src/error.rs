use std::borrow::Cow;

use crate::error_kinds::{
    duplicate_variable_detail, undefined_variable_detail, unknown_function_detail, ErrorKind,
};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Which capacity contract a [`Error::Capacity`] refusal belongs to
/// (ADR-0042/ADR-0307 vs ADR-0463 vs ADR-0494). Public so a consumer can
/// render the class without parsing the message; the Display already
/// differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityKind {
    /// The live graph + index cap (`--logical-graph-size-limit`, ADR-0042).
    Graph,
    /// One expression-built allocation over the per-value cap (ADR-0463).
    Expression,
    /// The per-record WAL payload cap (ADR-0086, ADR-0494): one commit
    /// record whose serialized payload crosses
    /// [`Error::WAL_PAYLOAD_CAP_BYTES`], refused mid-serialization before
    /// any frame is written. `used` is the counted byte total at which
    /// the bounded serializer stopped — a lower bound of the record the
    /// statement tried to commit, never its final size.
    WalPayload,
}

/// The one rendering of the three capacity contracts: the graph line is
/// the historical byte contract (ADR-0042/ADR-0307, byte-identical); the
/// expression and WAL payload lines are the plain-language user messages
/// naming what hit the limit, the bound, and the one action.
fn capacity_detail(kind: CapacityKind, used: u64, limit: u64) -> String {
    match kind {
        CapacityKind::Graph => format!("capacity exceeded: used={used} limit={limit}"),
        CapacityKind::Expression => format!(
            "This statement tried to build a single value of {}. This is a \
             safety limit protecting against memory exhaustion (not a \
             database limit) — build the value in smaller pieces, or raise \
             it with --expression-memory-limit.",
            human_bytes(used)
        ),
        CapacityKind::WalPayload => format!(
            "This statement tried to commit a single WAL record of at \
             least {}. This is a safety limit protecting against memory \
             exhaustion — the per-record WAL payload cap is {}; commit the \
             data in smaller transactions.",
            human_bytes(used),
            human_bytes(limit)
        ),
    }
}

/// A user-facing byte size: MB/GB at whole units, never the raw byte count.
fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    if bytes >= GB {
        format!("{:.0} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("syntax error: {0}")]
    Syntax(String),
    #[error("semantic error: {0}")]
    Semantic(String),
    /// An argument-value rejection carrying Neo4j's `ArgumentError` class
    /// (ADR-0489): a schema DDL command's `OPTIONS`/`indexConfig` value that
    /// the argument validator refuses. The class travels as data so no
    /// surface has to scan the rendered text for a prefix. Display keeps the
    /// historical `semantic error: <detail>` rendering, so only the class
    /// moves, never the detail text.
    #[error("semantic error: {0}")]
    ArgumentError(String),
    /// A uniqueness-constraint violation raised by constraint validation,
    /// carrying the violated constraint's name typed, so consumers match the
    /// class and read the name without parsing the rendered message. The
    /// Display text is byte-identical to the historical
    /// `Semantic("ConstraintValidationFailed: `<name>` uniqueness")`
    /// rendering — stored text, bindings classification and pinned messages
    /// do not move.
    #[error("semantic error: {}", uniqueness_violation_detail(name))]
    UniquenessViolation { name: String },
    /// A call to a procedure the engine does not register (ADR-0138),
    /// carrying the missing procedure's name typed, so consumers match the
    /// class without a prefix check on the rendered message. The Display
    /// text is byte-identical to the historical
    /// `Syntax("ProcedureNotFound: <name>")` rendering — pinned messages do
    /// not move.
    #[error("syntax error: ProcedureNotFound: {name}")]
    ProcedureNotFound { name: String },
    /// An argument of the wrong static or runtime type, carrying the detail
    /// text typed, so consumers match the class without scanning the
    /// rendered message. The Display text is byte-identical to the
    /// historical `Type("InvalidArgumentType: <detail>")` rendering.
    #[error("{}", invalid_argument_type_display(.0))]
    InvalidArgumentType(String),
    /// A runtime type failure raised under the legacy `TypeError` detail
    /// name, carrying the detail text typed. The Display text is
    /// byte-identical to the historical `Type("TypeError: <detail>")`
    /// rendering.
    #[error("{}", type_error_variant_display(.0))]
    TypeError(String),
    /// A reference to a query parameter that was not supplied, carrying the
    /// detail text typed. The Display text is byte-identical to the
    /// historical `Semantic("ParameterMissing: <detail>")` rendering.
    #[error("semantic error: ParameterMissing: {0}")]
    ParameterMissing(String),
    /// A selected database or `USE` target that is not one of the fixed
    /// namespaces (ADR-0146, ADR-0307), carrying the name typed. The
    /// Display text is byte-identical to the historical
    /// `Semantic("DatabaseNotFound: Graph not found: <name>")` rendering.
    #[error("semantic error: {}", database_not_found_detail(name))]
    DatabaseNotFound { name: String },
    #[error("{}", type_error_display(.0))]
    Type(String),
    /// An arithmetic failure raised while evaluating an expression
    /// (ADR-0307). The Display text is the historical rendering of each
    /// [`ArithmeticFailure`], composed here from the shared Type-class
    /// prefix (ADR-0338).
    #[error("{}", arithmetic_detail(.0))]
    Arithmetic(ArithmeticFailure),
    /// Failure raised while evaluating an already parsed and planned query.
    /// The wrapped text preserves the executor's typed detail.
    #[error("{0}")]
    Execution(String),
    /// A statement stopped through its [`crate::CancelToken`] (ADR-0111,
    /// ADR-0307). The Display text is byte-identical to the historical
    /// `Execution("query cancelled")` rendering.
    #[error("query cancelled")]
    Cancelled,
    #[error("write-write conflict")]
    Conflict,
    /// A capacity refusal. `kind` selects the user-facing detail: the
    /// graph-cap contract line (`capacity exceeded: used=… limit=…`,
    /// ADR-0042/ADR-0307), the ADR-0463 expression-allocation line, or
    /// the ADR-0494 per-record WAL payload cap line, the last two in
    /// plain language. All three keep this variant so every surface's
    /// status mapping is one memory/capacity class.
    #[error("{}", capacity_detail(*kind, *used, *limit))]
    Capacity {
        used: u64,
        limit: u64,
        kind: CapacityKind,
    },
    /// A processing control refused work. `name` is
    /// [`Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT`].
    /// `retryable` is false when the same call will fail again. `receipt`
    /// is the logical bytes counted when admission produced the error.
    #[error("resource limit exceeded: {name} limit={limit}")]
    ResourceLimit {
        name: &'static str,
        limit: u64,
        retryable: bool,
        receipt: Option<crate::tokens::ExecutionReceipt>,
    },
    #[error("storage error: {0}")]
    Storage(String),
    /// A persistent database is already owned by another independent opener.
    #[error("storage locked: {0}")]
    Locked(String),
    #[error("io error: {0}")]
    Io(String),
    /// Engine-internal failure that is not the query's fault: worker-thread
    /// spawn failure or a panic observed across the worker join (ADR-0259).
    /// Always recoverable by the caller; never an abort.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// `ResourceLimit::name` for the concurrent logical-workspace admission
    /// control. `limit` is the pool capacity in logical bytes.
    pub const CONCURRENT_LOGICAL_WORKSPACE_LIMIT: &'static str =
        "concurrent-logical-workspace-limit";

    /// `ResourceLimit::name` for a writer mid-`demote` (ADR-0440): new
    /// writes are refused while commits already past admission finish and
    /// the handle checkpoints before releasing the writer lease. Always
    /// `retryable: true` — the caller retries once the follower that is
    /// about to be promoted takes over.
    pub const WRITER_DRAINING: &'static str = "writer-draining";

    /// `ResourceLimit::name` for a follower that yielded across a checkpoint
    /// gap (ADR-0441). Always `retryable: true`. The host drops this engine
    /// and opens a new one; this process does not load a second graph.
    pub const FOLLOW_GAP: &'static str = "follow-gap";

    /// The per-record WAL payload cap (ADR-0086): one commit record whose
    /// serialized JSON crosses this many bytes is refused with
    /// [`Self::Capacity`] in the [`CapacityKind::WalPayload`] kind
    /// (ADR-0494) before any frame is written. The one source of the
    /// number — the refusal carries it as `limit`, and the storage
    /// crate's WAL encoder and scanner both consume it, so the rendered
    /// message, the write refusal and the recovery bound cannot drift
    /// apart (ADR-0492).
    pub const WAL_PAYLOAD_CAP_BYTES: u64 = 64 * 1024 * 1024;

    pub fn syntax(msg: impl Into<String>) -> Self {
        Self::Syntax(msg.into())
    }

    pub fn semantic(msg: impl Into<String>) -> Self {
        Self::Semantic(msg.into())
    }

    /// An argument-value rejection in Neo4j's `ArgumentError` class
    /// (ADR-0489): the class a DDL command's `OPTIONS`/`indexConfig`
    /// validation carries. The detail text is the historical one, so only
    /// the class changes.
    pub fn argument_error(detail: impl Into<String>) -> Self {
        Self::ArgumentError(detail.into())
    }

    /// An argument rejection in the `ArgumentError` class whose detail
    /// carries `kind`'s token — live Neo4j reports `point()`'s in-map
    /// rejections this way, with the token naming the *kind* of argument
    /// fault (`InvalidArgumentValue` for a value domain, 22N20/22N21/22N23;
    /// `InvalidArgumentType` for a wrong type, 22N27 → 22G03; verified
    /// 2026-11-06). The token composes from [`ErrorKind`], so the class and
    /// the kind spelling cannot drift apart. One entry point whose name
    /// states the class and whose token is data — the class can no longer
    /// be picked by word order, which is the hazard
    /// [`Self::invalid_argument_type`] (TypeError class) shares a spelling
    /// with.
    pub fn argument_error_kind(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self::argument_error(kind_prefixed(kind, detail))
    }

    /// A uniqueness violation naming the violated constraint — the typed
    /// counterpart of the pinned `ConstraintValidationFailed` message text.
    pub fn uniqueness_violation(name: impl Into<String>) -> Self {
        Self::UniquenessViolation { name: name.into() }
    }

    /// A call to an unregistered procedure, naming it — the typed
    /// counterpart of the pinned `ProcedureNotFound: <name>` message text
    /// (ADR-0138).
    pub fn procedure_not_found(name: impl Into<String>) -> Self {
        Self::ProcedureNotFound { name: name.into() }
    }

    /// A call to a function the engine does not register, naming it — the
    /// one spelling of the pinned `UnknownFunction: unknown function
    /// <name>()` text (42N48, a SyntaxError-class error). The planner's
    /// binding-time check and the executor's scalar-eval backstop both call
    /// this so the two layers cannot drift apart.
    pub fn unknown_function(name: &str) -> Self {
        Self::syntax(kind_prefixed(
            ErrorKind::UnknownFunction,
            unknown_function_detail(name),
        ))
    }

    /// An argument of the wrong type, with the detail text after the pinned
    /// `InvalidArgumentType:` token — the typed counterpart of the legacy
    /// `Type("InvalidArgumentType: <detail>")` string.
    pub fn invalid_argument_type(detail: impl Into<String>) -> Self {
        Self::InvalidArgumentType(detail.into())
    }

    /// A wrong-typed argument whose detail is the shared 22N01 sentence
    /// ([`expected_type_detail`]) — the one constructor of that family, so
    /// callers stop hand-spelling it.
    pub fn invalid_argument_type_expected(expected: &str, actual: &str) -> Self {
        Self::invalid_argument_type(expected_type_detail(expected, actual))
    }

    /// A value outside an argument's domain, with the detail text after the
    /// pinned `InvalidArgumentValue:` token — the typed counterpart of the
    /// legacy `Type("InvalidArgumentValue: <detail>")` string, composing
    /// the token once.
    pub fn invalid_argument_value(detail: impl Into<String>) -> Self {
        Self::ty(kind_prefixed(ErrorKind::InvalidArgumentValue, detail))
    }

    /// A boolean-requiring context that received a non-boolean, with the
    /// detail text `<ctx> requires a boolean` — the one constructor of the
    /// ADR-0511 boolean family, so the wording cannot drift between the
    /// planner's and the executor's requiring sites. `ctx` is the site's
    /// own naming of the position (`"predicate"`, an operator's spelling,
    /// or a dynamic context like `"quantifier WHERE"`).
    pub fn requires_boolean(ctx: &str) -> Self {
        Self::invalid_argument_type(format!("{ctx} requires a boolean"))
    }

    /// A runtime type failure under the legacy `TypeError` detail name, with
    /// the detail text after the pinned `TypeError:` token.
    pub fn type_error(detail: impl Into<String>) -> Self {
        Self::TypeError(detail.into())
    }

    /// A query parameter that was not supplied, with the detail text after
    /// the pinned `ParameterMissing:` token — the typed counterpart of the
    /// legacy `Semantic("ParameterMissing: <detail>")` string.
    pub fn parameter_missing(detail: impl Into<String>) -> Self {
        Self::ParameterMissing(detail.into())
    }

    /// A statement that references parameters the caller did not supply
    /// (ADR-0308): the `Expected parameter(s): <names>` detail, the names
    /// joined by `, ` in the order given. The one spelling of the message.
    pub fn parameters_missing<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let names: Vec<&str> = names.into_iter().collect();
        Self::ParameterMissing(format!("Expected parameter(s): {}", names.join(", ")))
    }

    /// A database or `USE` target that is not a fixed namespace, naming it
    /// (ADR-0307) — the typed counterpart of the pinned
    /// `DatabaseNotFound: Graph not found: <name>` text.
    pub fn database_not_found(name: impl Into<String>) -> Self {
        Self::DatabaseNotFound { name: name.into() }
    }

    /// A reference to a variable the evaluated row does not bind (ADR-0335)
    /// — the executor-side counterpart of the front-end's
    /// `UndefinedVariable` semantic error, for expressions the plan-time
    /// binding walk cannot see (catalog predicates lowered at run time).
    /// Renders byte-identically to the front-end's error:
    /// `syntax error: UndefinedVariable: variable '<name>' not defined`.
    pub fn undefined_variable(name: &str) -> Self {
        Self::syntax(kind_prefixed(
            ErrorKind::UndefinedVariable,
            undefined_variable_detail(name),
        ))
    }

    /// A 42I24 `InvalidAggregation`: an aggregate call outside an
    /// aggregation context. Neo4j reports it as a compile-time
    /// `SyntaxError` carrying the `InvalidAggregation` kind token, the class
    /// the front end's typed channel uses; the executor's scalar-eval
    /// backstop renders the same token through this one constructor.
    pub fn invalid_aggregation(detail: impl Into<String>) -> Self {
        Self::syntax(kind_prefixed(ErrorKind::InvalidAggregation, detail))
    }

    /// A binding that redeclares a name its scope already binds — the
    /// planner-side counterpart of the front end's `VariableAlreadyBound`.
    /// Renders `syntax error: VariableAlreadyBound: variable '<name>'
    /// already declared` from the same detail helper the typed channel
    /// uses, so the two channels cannot drift.
    pub fn duplicate_variable(name: &str) -> Self {
        Self::syntax(kind_prefixed(
            ErrorKind::VariableAlreadyBound,
            duplicate_variable_detail(name),
        ))
    }

    pub fn ty(msg: impl Into<String>) -> Self {
        Self::Type(msg.into())
    }

    pub fn arithmetic(failure: ArithmeticFailure) -> Self {
        Self::Arithmetic(failure)
    }

    pub fn execution(error: impl Into<String>) -> Self {
        Self::Execution(error.into())
    }

    /// The one spelling of statement cancellation (see `crate::cancel`):
    /// [`Error::Cancelled`], whose Display is the documented
    /// `"query cancelled"` contract string.
    pub fn cancelled() -> Self {
        Self::Cancelled
    }

    pub fn conflict() -> Self {
        Self::Conflict
    }

    pub fn capacity(used: u64, limit: u64) -> Self {
        Self::Capacity {
            used,
            limit,
            kind: CapacityKind::Graph,
        }
    }

    /// The ADR-0463 expression refusal: one allocation of `bytes` exceeds
    /// `limit`. The Display is the plain-language line the user sees — what
    /// was built, in MB/GB, and the one action — never the internal
    /// bytes/slots vocabulary.
    pub fn capacity_expression(bytes: u64, limit: u64) -> Self {
        Self::Capacity {
            used: bytes,
            limit,
            kind: CapacityKind::Expression,
        }
    }

    /// The ADR-0494 per-record WAL payload cap refusal: the bounded
    /// serializer stopped at `counted` bytes — a lower bound of the record
    /// the statement tried to commit — against the `limit` the check
    /// consulted (`WAL_PAYLOAD_CAP_BYTES` at both refusal sites).
    pub fn capacity_wal_payload(counted: u64, limit: u64) -> Self {
        Self::Capacity {
            used: counted,
            limit,
            kind: CapacityKind::WalPayload,
        }
    }

    /// A processing control refused work. The display text names the
    /// control and the configured limit.
    pub fn resource_limit(name: &'static str, limit: u64, retryable: bool) -> Self {
        Self::ResourceLimit {
            name,
            limit,
            retryable,
            receipt: None,
        }
    }

    /// [`Self::resource_limit`] plus the token receipt counted so far.
    pub fn resource_limit_with_receipt(
        name: &'static str,
        limit: u64,
        retryable: bool,
        receipt: crate::tokens::ExecutionReceipt,
    ) -> Self {
        Self::ResourceLimit {
            name,
            limit,
            retryable,
            receipt: Some(receipt),
        }
    }

    /// The token receipt attached to a resource-limit error, when the
    /// refusal came from token admission.
    pub fn receipt(&self) -> Option<&crate::tokens::ExecutionReceipt> {
        match self {
            Self::ResourceLimit { receipt, .. } => receipt.as_ref(),
            _ => None,
        }
    }

    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(msg.into())
    }

    pub fn io(msg: impl Into<String>) -> Self {
        Self::Io(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    /// The pinned class token and detail payload of the six typed detail
    /// variants ([`Error::UniquenessViolation`],
    /// [`Error::ProcedureNotFound`], [`Error::InvalidArgumentType`],
    /// [`Error::TypeError`], [`Error::ParameterMissing`],
    /// [`Error::DatabaseNotFound`]), single-sourced
    /// beside the Display attributes that own the same tokens so consumers
    /// classify and re-render the legacy bare text without re-spelling
    /// them. The detail borrows where the variant stores it verbatim and is
    /// owned where Display composes it (the uniqueness violation's
    /// `` `<name>` uniqueness ``, the CE `Graph not found: <name>`).
    /// `None` for every other class.
    pub fn typed_detail(&self) -> Option<(&'static str, Cow<'_, str>)> {
        match self {
            Error::UniquenessViolation { name } => Some((
                CONSTRAINT_VALIDATION_FAILED,
                Cow::Owned(uniqueness_detail(name)),
            )),
            Error::DatabaseNotFound { name } => Some((
                ErrorKind::DatabaseNotFound.as_str(),
                Cow::Owned(graph_not_found_text(name)),
            )),
            Error::ProcedureNotFound { name } => {
                Some(("ProcedureNotFound", Cow::Borrowed(name.as_str())))
            }
            Error::InvalidArgumentType(detail) => {
                Some(("InvalidArgumentType", Cow::Borrowed(detail.as_str())))
            }
            Error::TypeError(detail) => Some(("TypeError", Cow::Borrowed(detail.as_str()))),
            Error::ParameterMissing(detail) => {
                Some(("ParameterMissing", Cow::Borrowed(detail.as_str())))
            }
            _ => None,
        }
    }

    /// The legacy bare detail text — `<Token>: <detail>` — of the typed
    /// detail variants, exactly as the stringly `Type`/`Semantic` payloads
    /// spelled it. Built on [`Error::typed_detail`]; `None` for every
    /// other class.
    pub fn typed_detail_text(&self) -> Option<String> {
        let (token, detail) = self.typed_detail()?;
        Some(format!("{token}: {detail}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// The one spelling of the poisoned-lock policy (AGENTS.md: one canonical
/// helper per policy): a panicked lock holder fences the operation with an
/// internal error instead of silently proceeding on possibly torn state.
/// Every `.lock()?` / `.read()?` on a store mutex or lock resolves through
/// this impl.
impl<T> From<std::sync::PoisonError<T>> for Error {
    fn from(err: std::sync::PoisonError<T>) -> Self {
        Self::internal(err.to_string())
    }
}

/// The pinned class token of a uniqueness-constraint violation, shared by
/// [`Error::UniquenessViolation`]'s Display composition and
/// [`Error::typed_detail`] so both spell it from one place.
const CONSTRAINT_VALIDATION_FAILED: &str = "ConstraintValidationFailed";

/// The Type-class Display prefix, shared by [`Error::Type`]'s attribute and
/// the other Type-class details composed beside it (ADR-0338), so none of
/// them can spell the class differently.
const TYPE_ERROR_PREFIX: &str = "type error";

/// The one spelling of the one-argument arity rejection shared by the
/// planner's `FIXED_ARITY_FUNCTIONS` row and the executor's list-builtin
/// backstop — the two layers both depend on core, so the plan-time
/// rejection and the backstop cannot drift apart.
pub const FUNCTION_REQUIRES_ONE_ARGUMENT: &str =
    "InvalidNumberOfArguments: function requires one argument";

/// The one spelling of the wrong-type sentence the 22N01 argument errors
/// and the SEARCH validators share. The expected set is the caller's own
/// signature; the actual type name is the caller's vocabulary
/// ([`crate::value_type_name`], or a SEARCH source-kind display name).
pub fn expected_type_detail(expected: &str, actual: &str) -> String {
    format!("Expected the value to be of type {expected}, but was of type {actual}")
}

/// [`Error::Type`]'s Display: the shared Type-class prefix and the detail.
fn type_error_display(detail: &str) -> String {
    format!("{TYPE_ERROR_PREFIX}: {detail}")
}

/// [`Error::InvalidArgumentType`]'s Display: [`type_error_display`] with the
/// variant's pinned detail token, so the class prefix comes from the one
/// [`TYPE_ERROR_PREFIX`].
fn invalid_argument_type_display(detail: &str) -> String {
    format!(
        "{}: {detail}",
        type_error_display(ErrorKind::InvalidArgumentType.as_str())
    )
}

/// [`Error::TypeError`]'s Display: [`type_error_display`] with the
/// variant's pinned detail token, on the same one [`TYPE_ERROR_PREFIX`].
fn type_error_variant_display(detail: &str) -> String {
    format!(
        "{}: {detail}",
        type_error_display(ErrorKind::TypeError.as_str())
    )
}

/// [`Error::Arithmetic`]'s Display: the historical rendering of each
/// [`ArithmeticFailure`] — [`type_error_display`] for the two division
/// failures, the bare `ArithmeticError: …` payload for rounding (ADR-0307,
/// ADR-0338).
fn arithmetic_detail(failure: &ArithmeticFailure) -> String {
    match failure {
        ArithmeticFailure::DivisionByZero | ArithmeticFailure::ModuloByZero => {
            type_error_display(failure.detail())
        }
        ArithmeticFailure::RoundingNecessary => failure.detail().to_string(),
    }
}

/// The pinned `` `<name>` uniqueness `` detail of a uniqueness violation,
/// shared by the Display composition and [`Error::typed_detail`].
fn uniqueness_detail(name: &str) -> String {
    format!("`{name}` uniqueness")
}

/// The bare `ConstraintValidationFailed` detail of a uniqueness violation —
/// [`Error::UniquenessViolation`]'s Display without the `semantic error: `
/// prefix, for error composition that embeds the violation's message.
pub fn uniqueness_violation_detail(name: &str) -> String {
    format!(
        "{CONSTRAINT_VALIDATION_FAILED}: {}",
        uniqueness_detail(name)
    )
}

/// The CE text of an unknown graph, `Graph not found: <name>` — the one
/// place it lives; [`Error::DatabaseNotFound`]'s Display and typed detail
/// compose it (moved here from `ce_messages` so the dependency stays
/// one-way `ce_messages -> error.rs`).
fn graph_not_found_text(name: &str) -> String {
    format!("Graph not found: {name}")
}

/// [`Error::DatabaseNotFound`]'s Display without the `semantic error: `
/// prefix — its [`Error::typed_detail_text`], composed from the typed
/// kind token and the CE text (ADR-0149).
fn database_not_found_detail(name: &str) -> String {
    kind_prefixed(ErrorKind::DatabaseNotFound, graph_not_found_text(name))
}

/// The one composition of a kind token and its detail —
/// `"<Kind>: <detail>"` — used by every kind-carrying constructor in this
/// module.
fn kind_prefixed(kind: ErrorKind, detail: impl Into<String>) -> String {
    format!("{}: {}", kind.as_str(), detail.into())
}

/// The arithmetic failures the executor raises (ADR-0307), typed so a
/// surface maps them without reading the message. Each renders exactly as
/// the stringly error it replaced did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticFailure {
    /// Integer or duration division by zero: `type error: / by zero`.
    DivisionByZero,
    /// Integer modulo by zero: `type error: % by zero`.
    ModuloByZero,
    /// `round(x, precision, 'UNNECESSARY')` on a value that needs rounding:
    /// `ArithmeticError: rounding necessary`.
    RoundingNecessary,
}

impl ArithmeticFailure {
    /// The payload the replaced stringly error carried — `/ by zero` and
    /// `% by zero` (historically `Type`), `ArithmeticError: rounding
    /// necessary` (historically `Execution`) — for consumers that embed or
    /// classify that legacy text.
    pub fn detail(self) -> &'static str {
        match self {
            ArithmeticFailure::DivisionByZero => "/ by zero",
            ArithmeticFailure::ModuloByZero => "% by zero",
            ArithmeticFailure::RoundingNecessary => "ArithmeticError: rounding necessary",
        }
    }
}

impl std::fmt::Display for ArithmeticFailure {
    /// The bare legacy payload ([`Self::detail`]); the enclosing
    /// [`Error::Arithmetic`] composes the Type-class prefix (ADR-0338).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}

#[cfg(test)]
mod tests;
