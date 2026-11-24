//! Typed errors for ontology document parsing and validation (ADR-0227,
//! ADR-0364, ADR-0487), plus the canonicalize path's user-facing
//! envelopes (ADR-0667).

use crate::format::DocumentFormat;

/// The one spelling of the supported Ontology Graph Spec version. Both
/// gate constants below derive from it, so the URI and the version the
/// error messages name cannot drift apart (ADR-0487 §1).
macro_rules! spec_version {
    () => {
        "1.1.0"
    };
}

/// The pinned Ontology Graph Spec document URI: the `$id` of the
/// published `release-spec/v1.1.0` tag and the only accepted value of a
/// document's root `schema` field (ADR-0487 §1).
pub const PINNED_SCHEMA_URI: &str = concat!(
    "https://neo4j.com/docs/ontology-graph-spec/",
    spec_version!(),
    "/schema.json"
);

/// The highest Ontology Graph Spec version this crate supports.
pub const MAX_SUPPORTED_SPEC_VERSION: &str = spec_version!();

/// The support sentence both spec-version refusals end with — the one
/// composition of [`MAX_SUPPORTED_SPEC_VERSION`] and
/// [`PINNED_SCHEMA_URI`], so the two gates' messages cannot drift.
fn spec_version_support() -> String {
    format!(
        "This version of Relate supports up to Ontology Graph Spec \
         {MAX_SUPPORTED_SPEC_VERSION} ('{PINNED_SCHEMA_URI}')."
    )
}

/// A construct outside the JSON-equivalent value model of ADR-0227 §4,
/// the typed handle behind [`OntologyError::UnsupportedConstruct`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Construct {
    /// A YAML anchor or alias; the value model has no alias concept.
    #[error("anchors and aliases are not supported")]
    AnchorsAndAliases,
    /// A non-finite float (`.inf` / `.nan`): rejected by the parse path and,
    /// for a hand-built value the parser never saw, by the serializers.
    #[error("non-finite floats (.inf/.nan) are not supported")]
    NonFiniteFloat,
    /// A mapping key that is not a string.
    #[error("non-string mapping keys are not supported")]
    NonStringKey,
    /// A custom YAML tag; carries the tag as written.
    #[error("custom tags are not supported (found tag '{0}')")]
    CustomTag(String),
    /// More than one YAML document in the input; carries the count.
    #[error("expected a single YAML document, found {0} (multi-document input is not supported)")]
    MultipleDocuments(usize),
}

/// Errors produced while parsing, validating or serializing an ontology
/// document.
///
/// Every variant names the violated construct: syntax errors name the
/// detected syntax and carry the parser's message, the spec-version gate
/// names the offending URI and the highest supported version, and
/// validation errors carry a `/`-separated path to the violated field plus
/// a message naming it.
#[derive(Debug, thiserror::Error)]
pub enum OntologyError {
    /// The submitted bytes are not valid UTF-8.
    #[error("ontology document is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),

    /// The document is not syntactically valid in its detected syntax
    /// (rendered as `JSON` or `YAML`).
    #[error("invalid ontology document syntax ({syntax}): {message}")]
    Syntax {
        /// The syntax the input was parsed as.
        syntax: DocumentFormat,
        /// The parser's message.
        message: String,
    },

    /// The document parses but uses a construct outside the JSON-equivalent
    /// value model of ADR-0227 §4 (non-string mapping keys, custom tags,
    /// anchors and aliases, non-finite floats, multiple documents, …).
    #[error("unsupported construct in ontology document: {0}")]
    UnsupportedConstruct(Construct),

    /// The document's `schema` URI is not the pinned Ontology Graph
    /// Spec 1.1.0 URI (ADR-0487 §1, succeeding ADR-0364 §1; the
    /// exact-URI rule is Relate policy, since the spec's `schema`
    /// is a plain string). The message names the highest supported
    /// version through [`spec_version_support`], the one sentence both
    /// spec-version refusals share.
    #[error(
        "Unsupported ontology spec version: '{uri}'. {}",
        spec_version_support()
    )]
    UnsupportedSpecVersion {
        /// The URI found in the document's `schema` field.
        uri: String,
    },

    /// The document violates the vendored Ontology Graph Spec schema
    /// (ADR-0487 §2). `path` is a `/`-separated path to the violated field
    /// (e.g. `/nodes/Actor/properties/id/type`); the message names the
    /// violated field and the rule. The spec version is interpolated from
    /// [`MAX_SUPPORTED_SPEC_VERSION`] so it cannot drift from the gate.
    #[error(
        "ontology document violates the Ontology Graph Spec {MAX_SUPPORTED_SPEC_VERSION} schema \
         at {path}: {message}"
    )]
    Validation {
        /// Path to the violated field (`/` is the document root).
        path: String,
        /// What rule was violated.
        message: String,
    },

    /// The document is valid, but a cross-reference the ADR-0226 mappings
    /// rely on cannot be resolved: a relationship target with no `node`, a
    /// node with no identifying label, an empty constraint property list,
    /// a `PROPERTY_TYPE` constraint, and the other loud failures ADR-0364
    /// §5.4 enumerates. The vendored schema cannot express these
    /// cross-references and main-valid constructs stay valid, so the
    /// mappings that consume them report the failure at `apply`.
    #[error("ontology mapping failed: {0}")]
    Mapping(String),

    /// The document's structural nesting exceeds the depth limit (128
    /// levels, matching serde_json's recursion cap). An iterative
    /// pre-screen rejects such documents before the recursive parsers
    /// run: saphyr recurses per nesting level with no cap of its own, so
    /// an unbounded document (e.g. a one-line `- ` run) would overflow
    /// the stack and abort the process instead of failing with a typed
    /// error.
    #[error("ontology document nesting exceeds the {limit}-level depth limit")]
    NestingTooDeep {
        /// The enforced nesting limit.
        limit: usize,
    },

    /// The canonicalize path's parse envelope (ADR-0667): the document
    /// failed to parse both as JSON and as YAML, and each attempt's own
    /// message rides along under its syntax's label.
    #[error(
        "The ontology document could not be parsed as a graph-spec JSON or YAML document.\n\
         JSON: {json}\n\
         YAML: {yaml}"
    )]
    UnparsableDocument {
        /// The JSON attempt's message.
        json: String,
        /// The YAML attempt's message.
        yaml: String,
    },

    /// The canonicalize path's version envelope (ADR-0667): the
    /// document's `schema` field names a version the gate does not
    /// support. Like its sibling [`OntologyError::UnsupportedSpecVersion`]
    /// the variant carries the URI — the support sentence is composed
    /// once, in [`spec_version_support`], which both `Display`s call.
    #[error(
        "The ontology document uses an unsupported graph-spec version. The document names \
         '{uri}'; {}",
        spec_version_support()
    )]
    UnsupportedGraphSpecVersion {
        /// The URI found in the document's `schema` field.
        uri: String,
    },

    /// The canonicalize path's validation envelope (ADR-0667): every
    /// issue the core validation found, pre-joined — `[<code>] <message>
    /// (<path>)` rows, semicolon-separated, capped at
    /// [`MAX_ISSUES_IN_ERROR`] with an ellipsis-and-count suffix — so the
    /// variant's `Display` is the user-facing text verbatim.
    #[error("The ontology document failed validation. {rendered}")]
    ValidationFailed {
        /// The capped, joined issue rendering.
        rendered: String,
    },

    /// The stored-document parse path's validation envelope (ADR-0667):
    /// every issue the validation of already-stored bytes found, pre-joined
    /// — `<code>: <message> (<path>)` rows, newline-separated, uncapped —
    /// so the variant's `Display` is the user-facing text verbatim. The
    /// rows are not the canonicalize envelope's: a stored document is
    /// reported whole, never elided.
    #[error("The stored ontology is invalid:\n{rendered}")]
    StoredInvalid {
        /// The joined, uncapped issue rendering.
        rendered: String,
    },
}

impl OntologyError {
    /// Whether this failure is document-shaped — the stored bytes did
    /// not decode as a document at all (neither parse, or the
    /// spec-version gate) — the one classification the procedure
    /// family's stored-document error path branches on: those failures
    /// render as an undecodable stored document, every other failure as
    /// its own `Display`. Exhaustive over the variants, so a variant
    /// added later joins a side by decision, not by falling through a
    /// wildcard.
    pub fn is_document_shaped(&self) -> bool {
        match self {
            Self::UnparsableDocument { .. } | Self::UnsupportedGraphSpecVersion { .. } => true,
            Self::InvalidUtf8(_)
            | Self::Syntax { .. }
            | Self::UnsupportedConstruct(_)
            | Self::UnsupportedSpecVersion { .. }
            | Self::Validation { .. }
            | Self::Mapping(_)
            | Self::NestingTooDeep { .. }
            | Self::ValidationFailed { .. }
            | Self::StoredInvalid { .. } => false,
        }
    }
}

/// How many validation issues the
/// [`OntologyError::ValidationFailed`] envelope carries before the
/// ellipsis-and-count suffix takes over.
pub(crate) const MAX_ISSUES_IN_ERROR: usize = 5;

/// One validation finding: a stable [`IssueCode`] identifying the rule
/// family, the rule's message, and the `/`-separated path of the member
/// that broke it. The canonicalize path renders every collected issue
/// into the [`OntologyError::ValidationFailed`] envelope; the typed-parse
/// path surfaces only the first as [`OntologyError::Validation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    /// The rule family that produced this finding.
    pub code: IssueCode,
    /// The `/`-separated path to the offending member.
    pub path: String,
    /// What rule was violated, in the sentence the typed-parse path
    /// reuses verbatim.
    pub message: String,
}

impl Issue {
    /// The one typed-error view of this issue — the shape
    /// [`OntologyError::Validation`] carries, so the fail-fast parse path
    /// reports the same sentence the envelope renders.
    pub(crate) fn into_validation_error(self) -> OntologyError {
        OntologyError::Validation {
            path: self.path,
            message: self.message,
        }
    }

    /// The envelope's one row: `[<code>] <message> (<path>)`.
    fn render(&self) -> String {
        self.row(format_args!("[{}]", self.code.as_str()))
    }

    /// One issue row with the given code decoration — the row shape the
    /// two envelopes share, `"<decoration> <message> (<path>)"`,
    /// differing only in how the code is written.
    fn row(&self, code: impl std::fmt::Display) -> String {
        format!("{code} {} ({})", self.message, self.path)
    }
}

impl From<Issue> for Vec<Issue> {
    /// One issue lifted into the aggregating layer's list — the seam
    /// between the fail-fast sub-validators (one [`Issue`]) and the
    /// per-entry collectors, so `?` crosses it without an adapter at
    /// every call site.
    fn from(issue: Issue) -> Self {
        vec![issue]
    }
}

/// The closed vocabulary of validation issue codes — one token per rule
/// family the structural walk checks, in the envelope's `[<code>]` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IssueCode {
    /// A closed object carries a member outside its allowed set.
    UnknownField,
    /// A required member is absent.
    MissingRequiredField,
    /// A known member's value is not of the shape its field requires.
    InvalidType,
    /// A closed token vocabulary does not contain the submitted token.
    InvalidEnumToken,
    /// Two members that cannot be combined are both present.
    ConflictingMembers,
}

impl IssueCode {
    /// The envelope's snake_case token for this rule family.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UnknownField => "unknown_field",
            Self::MissingRequiredField => "missing_required_field",
            Self::InvalidType => "invalid_type",
            Self::InvalidEnumToken => "invalid_enum_token",
            Self::ConflictingMembers => "conflicting_members",
        }
    }
}

/// Render the validation envelope's issue list: up to
/// [`MAX_ISSUES_IN_ERROR`] rows joined by `"; "`, then — when more were
/// found — an ellipsis and the count of the unshown remainder.
pub(crate) fn render_issues(issues: &[Issue]) -> String {
    let mut rendered = issues
        .iter()
        .take(MAX_ISSUES_IN_ERROR)
        .map(Issue::render)
        .collect::<Vec<_>>()
        .join("; ");
    if issues.len() > MAX_ISSUES_IN_ERROR {
        let more = issues.len() - MAX_ISSUES_IN_ERROR;
        rendered.push_str(&format!(" … and {more} more"));
    }
    rendered
}

/// Render the stored-document validation envelope's issue list (ADR-0667):
/// every row — `<code>: <message> (<path>)`, colon-joined — newline-
/// separated, uncapped. A stored document is reported whole.
pub(crate) fn render_stored_issues(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(|issue| issue.row(format_args!("{}:", issue.code.as_str())))
        .collect::<Vec<_>>()
        .join("\n")
}
