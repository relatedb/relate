//! Typed errors for ontology document parsing and validation (ADR-0227,
//! ADR-0364, ADR-0487).

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
    UnsupportedConstruct(String),

    /// The document's `schema` URI is not the pinned Ontology Graph
    /// Spec 1.1.0 URI (ADR-0487 §1, succeeding ADR-0364 §1; the
    /// exact-URI rule is Relate policy, since the spec's `schema`
    /// is a plain string). The message names the highest supported
    /// version, interpolated from [`MAX_SUPPORTED_SPEC_VERSION`] and
    /// [`PINNED_SCHEMA_URI`] above so it cannot drift from the gate.
    #[error(
        "Unsupported ontology spec version: '{uri}'. This version of Relate supports up to \
         Ontology Graph Spec {MAX_SUPPORTED_SPEC_VERSION} ('{PINNED_SCHEMA_URI}')."
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
    /// A hand-built value carried a non-finite float (NaN/±Inf) into a
    /// serializer. The parse path rejects non-finite floats; this error
    /// bounds publicly-constructed values the parser never saw.
    #[error("non-finite floats cannot be serialized; the parse path rejects them")]
    NonFiniteFloat,
}
