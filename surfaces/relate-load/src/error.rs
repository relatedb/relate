use serde_json::Value as Json;

use crate::typed_value::DecodeError;

/// Errors from credential parse, shape check, Query API v2, or materialize.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    Env(String),
    #[error("{0}")]
    Shape(String),
    #[error("{0}")]
    Io(String),
    #[error("HTTP {status}: {reply}")]
    Http { status: u16, reply: String },
    #[error("{0}")]
    Body(String),
    /// A typed Query API v2 value the engine cannot represent, with the
    /// owning element and property attached (ADR-0181 item 7).
    #[error("typed wire decode failed: {0}")]
    TypedWire(#[from] TypedWireError),
    #[error("{0}")]
    Engine(String),
    /// A push-side refusal (ADR-0401): a value the Query API cannot carry as
    /// a typed parameter, an unresolvable scope, a batch the remote rejected.
    #[error("push failed: {0}")]
    Push(String),
    #[error("destination database is not empty; pull writes into an empty database only")]
    NonEmpty,
}

/// A typed Query API v2 value that cannot become an engine value, carrying
/// the owning element id and property key (ADR-0181 item 7).
///
/// This wraps the decoder's `DecodeError` — the decoder itself never sees
/// an element id or a property key — so the position and the reason stay one
/// value. [`TypedWireError::with_context`] attaches the position; the
/// `Display` string is the ADR-0181 error contract, literally.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("property {property:?} on element {element}: {error}")]
pub struct TypedWireError {
    /// The owning element's id, or the cell/response sentinel in cell
    /// position (ADR-0181 item 7).
    pub element: String,
    /// The owning property key, or the `"<cell>"` sentinel in cell
    /// position (ADR-0181 item 7).
    pub property: String,
    /// Why the value could not be decoded (ADR-0181 item 7).
    pub error: DecodeError,
}

impl TypedWireError {
    /// Attach the owning element id and property key to a bare decoder
    /// error (ADR-0181 item 7: the decoder never sees either).
    pub fn with_context(
        element: impl Into<String>,
        property: impl Into<String>,
        error: DecodeError,
    ) -> Self {
        Self {
            element: element.into(),
            property: property.into(),
            error,
        }
    }
}

/// The first error object of a Query API v2 error body
/// (`{"errors":[{"code":"…","message":"…"}]}`), parsed into its parts.
/// The wire contract of the Query API v2 error envelope lives here, in the
/// shared client — callers classify on the typed `code`, never on
/// formatted-message substrings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryApiError {
    /// The Neo4j status code (e.g. `Neo.ClientError.Statement.SyntaxError`).
    pub code: String,
    /// The human-readable message (empty when the body omits it).
    pub message: String,
}

impl LoadError {
    /// The first Query API v2 error object of an HTTP error body, when the
    /// reply parses as `{"errors":[{"code","message"}]}`. Anything else —
    /// a non-HTTP error, a truncated or non-JSON body, a missing code —
    /// yields None: the caller falls back to the untyped message.
    pub fn query_api_error(&self) -> Option<QueryApiError> {
        let LoadError::Http { reply, .. } = self else {
            return None;
        };
        let json: Json = serde_json::from_str(reply).ok()?;
        let first = json.get("errors")?.as_array()?.first()?;
        let code = first.get("code")?.as_str()?.to_string();
        let message = first
            .get("message")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        Some(QueryApiError { code, message })
    }

    /// Attach a context prefix to the payload, not the rendering: a
    /// `Push` message gains the prefix exactly once (the Display's
    /// "push failed: " still appears once at the end); other variants
    /// pass through unchanged.
    pub(crate) fn with_context(self, prefix: &str) -> Self {
        match self {
            LoadError::Push(msg) => LoadError::Push(format!("{prefix}{msg}")),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Query API v2 error envelope parses into its typed code and
    /// message, so callers classify on the code, never on message text.
    #[test]
    fn http_error_body_parses_into_typed_code_and_message() {
        let err = LoadError::Http {
            status: 400,
            reply: r#"{"errors":[{"code":"Neo.ClientError.Statement.SyntaxError","message":"Invalid input 'WIP' (line 1, column 8)"}]}"#
                .to_string(),
        };
        let api = err.query_api_error().expect("typed error: {err:?}");
        assert_eq!(api.code, "Neo.ClientError.Statement.SyntaxError");
        assert_eq!(api.message, "Invalid input 'WIP' (line 1, column 8)");
        // A missing message degrades to empty, not to None.
        let err = LoadError::Http {
            status: 400,
            reply: r#"{"errors":[{"code":"Neo.ClientError.General.UnknownError"}]}"#.to_string(),
        };
        let api = err.query_api_error().expect("typed error: {err:?}");
        assert_eq!(api.code, "Neo.ClientError.General.UnknownError");
        assert_eq!(api.message, "");
    }

    /// A body that is not a Query API v2 error object (truncated, non-JSON,
    /// no code) yields None — the transport/Other fallback case.
    #[test]
    fn malformed_bodies_degrade_to_no_typed_error() {
        for reply in [
            "bad gateway",
            r#"{"errors":[{"message":"no code"}]}"#,
            r#"{"errors":[]}"#,
            r#"{"errors":[{"code":"Neo.Truncated"#,
        ] {
            let err = LoadError::Http {
                status: 502,
                reply: reply.to_string(),
            };
            assert_eq!(err.query_api_error(), None, "reply {reply:?}");
        }
        // Non-HTTP errors never carry a typed Query API error.
        let err = LoadError::Io("connection refused".to_string());
        assert_eq!(err.query_api_error(), None);
    }

    /// The ADR-0181 error contract, literally: a `Base64` property named
    /// `blob` on element `4:x:0`.
    #[test]
    fn typed_wire_error_display_matches_the_adr_contract() {
        let err = LoadError::from(TypedWireError::with_context(
            "4:x:0",
            "blob",
            DecodeError::Unsupported {
                tag: "Base64".into(),
            },
        ));
        assert_eq!(
            err.to_string(),
            "typed wire decode failed: property \"blob\" on element 4:x:0: unsupported typed value \"Base64\""
        );
    }

    /// The `Malformed` and `Untagged` decode errors follow their ADR
    /// templates and convert into `LoadError::TypedWire` through `?`.
    #[test]
    fn typed_wire_error_display_covers_every_decode_error() {
        let malformed = TypedWireError::with_context(
            "4:x:0",
            "since",
            DecodeError::Malformed {
                tag: "Integer".into(),
                detail: "\"abc\" is not a decimal i64 string".into(),
            },
        );
        assert_eq!(
            malformed.to_string(),
            "property \"since\" on element 4:x:0: malformed Integer value: \"abc\" is not a decimal i64 string"
        );
        assert_eq!(
            LoadError::from(malformed).to_string(),
            "typed wire decode failed: property \"since\" on element 4:x:0: malformed Integer value: \"abc\" is not a decimal i64 string"
        );

        let untagged = TypedWireError::with_context("4:x:0", "name", DecodeError::Untagged);
        assert_eq!(
            untagged.to_string(),
            "property \"name\" on element 4:x:0: untagged value where a $type-tagged value was expected"
        );
        assert!(matches!(LoadError::from(untagged), LoadError::TypedWire(_)));
    }
}
