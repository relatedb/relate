//! The extension mount's JSON-RPC 2.0 vocabulary (ADR-0442): the request and
//! response records with their strict decode, the error codes, the MCP
//! revision and notification names, the one shared object-or-refuse read
//! (`object_member`) the params and member extractions of both sides
//! consult, the `initialize` handshake facts both sides share (the server's
//! identity, the negotiated revision, the capabilities, and the mount's own
//! store facts), and the request framing bound derived from the published
//! statement cap.
//!
//! Decode is strict and fail-closed (the code-shape rules): an unknown
//! field, a missing `method`, a non-string `jsonrpc`, a foreign protocol
//! version, or an unknown commit policy fails loudly, naming what it saw —
//! never a skipped message.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{PER_BATCH, WHOLE_IMPORT};

/// The JSON-RPC version every request and response carries — the one
/// spelling both sides send and both decoders require.
pub const JSONRPC_VERSION: &str = "2.0";

/// The `initialize` method name.
pub const METHOD_INITIALIZE: &str = "initialize";
/// The `tools/list` method name.
pub const METHOD_TOOLS_LIST: &str = "tools/list";
/// The `tools/call` method name.
pub const METHOD_TOOLS_CALL: &str = "tools/call";

/// The MCP revision this mount serves (the `initialize` handshake's
/// `protocolVersion`): the published MCP specification revision whose
/// handshake, `tools/list` and `tools/call` shapes this wire speaks. One
/// home for both sides — the host answers with it, the child requires it.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// The client's handshake confirmation: the notification MCP sends after a
/// successful `initialize`, before the first request. It carries no id —
/// it is the one notification the mount's protocol requires.
pub const METHOD_INITIALIZED: &str = "notifications/initialized";

/// Whether `method` names an MCP notification — the one method family a
/// request may omit its id for. A message without an id whose method is not
/// a notification is a protocol violation, not something to discard: the
/// child would wait for a response that never comes.
pub fn is_notification(method: &str) -> bool {
    method.starts_with("notifications/")
}

/// The object a JSON value must be — the one object-or-refuse read the
/// params and member extractions share (a request's `params`, a
/// `tools/call`'s `arguments`, a statement's `parameters`): absent or
/// `null` answers `None`, an object answers its map, anything else is
/// refused naming `label`. The refusal is plain text — the wire kit owns
/// the shape, each caller wraps it in its own refusal vocabulary.
pub fn object_member<'v>(
    label: &str,
    value: Option<&'v Value>,
) -> Result<Option<&'v Map<String, Value>>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map)),
        Some(_) => Err(format!("\"{label}\" must be an object")),
    }
}

/// JSON-RPC 2.0 `Parse error`: the line was not valid JSON.
pub const PARSE_ERROR: i64 = -32700;
/// JSON-RPC 2.0 `Invalid Request`: valid JSON that is not a request, or a
/// request this transport does not accept.
pub const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC 2.0 `Method not found`.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC 2.0 `Invalid params` — also the application-refusal code this
/// mount uses for an unknown tool, a cap exceeded, and every malformed
/// `tools/call` payload.
pub const INVALID_PARAMS: i64 = -32602;

/// One JSON-RPC 2.0 request (or notification, when `id` is absent).
///
/// Decode is strict and fail-closed (the code-shape rules): an unknown
/// field, a missing `method`, or a non-string `jsonrpc` fails the decode
/// loudly, naming what it saw — never a skipped message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The protocol version; must be [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The correlation id; absent on a notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// The method name.
    pub method: String,
    /// The method's parameters; absent when it carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// This request as one wire line (no terminator).
    pub fn to_line(&self) -> String {
        // A request is plain serde data: serialization cannot fail.
        serde_json::to_string(self).expect("a JSON-RPC request serializes")
    }
}

/// Why [`decode_request`] refused a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The line is not a strict JSON-RPC request: serde's own refusal,
    /// verbatim.
    Malformed {
        /// The decoder's refusal.
        detail: String,
    },
    /// The line carried another `jsonrpc` value than [`JSONRPC_VERSION`].
    UnsupportedVersion {
        /// The version the line declared.
        version: String,
    },
}

/// Decode one line into a [`Request`], strict and fail-closed.
pub fn decode_request(line: &[u8]) -> Result<Request, RequestError> {
    let request: Request = serde_json::from_slice(line).map_err(|e| RequestError::Malformed {
        detail: e.to_string(),
    })?;
    if request.jsonrpc != JSONRPC_VERSION {
        return Err(RequestError::UnsupportedVersion {
            version: request.jsonrpc,
        });
    }
    Ok(request)
}

impl RequestError {
    /// The JSON-RPC error code this refusal answers with: a line the strict
    /// decode could not read is a parse error, a foreign `jsonrpc` value an
    /// invalid request. The one mapping both MCP surfaces answer with.
    pub fn code(&self) -> i64 {
        match self {
            Self::Malformed { .. } => PARSE_ERROR,
            Self::UnsupportedVersion { .. } => INVALID_REQUEST,
        }
    }
}

impl fmt::Display for RequestError {
    /// The refusal wording both MCP surfaces send: the pinned text the
    /// extension spec's error table promises, never a second phrasing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { detail } => write!(f, "malformed JSON-RPC message: {detail}"),
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported JSON-RPC version '{version}'")
            }
        }
    }
}

/// One JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    /// The error code.
    pub code: i64,
    /// The error message.
    pub message: String,
}

/// One JSON-RPC 2.0 response: exactly one of `result` and `error` is set.
///
/// Decode is strict; the exactly-one rule is enforced by [`Self::into_result`]
/// (serde cannot express an either-or over two optional fields without
/// silently accepting both).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// The protocol version; [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The correlation id echoed from the request.
    pub id: Value,
    /// The success payload; absent on an error response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error payload; absent on a success response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// A success response answering `id`.
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response answering `id`.
    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }

    /// This response as one wire line (no terminator).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("a JSON-RPC response serializes")
    }

    /// The response's outcome, enforcing the exactly-one rule: `Ok(result)`
    /// for a success, `Err(error)` for an error, and a decode refusal for
    /// both or neither.
    pub fn into_result(self) -> Result<Value, ResponseError> {
        match (self.jsonrpc.as_str(), self.result, self.error) {
            (JSONRPC_VERSION, Some(result), None) => Ok(result),
            (JSONRPC_VERSION, None, Some(error)) => Err(ResponseError::Rpc(error)),
            (JSONRPC_VERSION, Some(_), Some(_)) => Err(ResponseError::Shape(
                "the response carries both result and error".to_string(),
            )),
            (JSONRPC_VERSION, None, None) => Err(ResponseError::Shape(
                "the response carries neither result nor error".to_string(),
            )),
            (version, _, _) => Err(ResponseError::Shape(format!(
                "unsupported JSON-RPC version '{version}'"
            ))),
        }
    }
}

/// Why a [`Response`] could not be turned into an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseError {
    /// The host answered with a JSON-RPC error.
    Rpc(RpcError),
    /// The response is not a well-formed JSON-RPC 2.0 response.
    Shape(String),
}

/// The host facts the `initialize` result carries, as the child decodes
/// them (ADR-0442 §3): MCP's three handshake facts — the negotiated
/// protocol revision, the server's capabilities, the server's identity —
/// plus the mount's own facts (whether the run needs a fresh store, the
/// manifest's commit policy, the caps every call runs under, and the
/// host-owned store's [`Capacity`]). The host builds this type and
/// serializes it; the child decodes it — one schema, one home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServerFacts {
    /// The server's identity.
    pub server_info: ServerInfo,
    /// The MCP revision the server answered with — [`MCP_PROTOCOL_VERSION`]
    /// when it speaks the client's, otherwise the one it serves (MCP's
    /// negotiation rule).
    pub protocol_version: String,
    /// The capabilities the server advertises.
    pub capabilities: ServerCapabilities,
    /// Whether the action requires a fresh store (the host refuses a
    /// non-fresh target before the child runs).
    pub fresh_store_required: bool,
    /// The manifest's commit policy: `whole-import` or `per-batch`.
    pub commit_policy: CommitFacts,
    /// The caps every `tools/call` payload runs under.
    pub caps: CapsFacts,
    /// The host-owned store's capacity as the session opened: the
    /// baseline a child measures its own writes against, since every
    /// writing tool result repeats the remaining side.
    pub capacity: Capacity,
}

/// The `initialize` result's capabilities: the one MCP capability this
/// mount serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// The tools capability.
    pub tools: ToolsCapability,
}

impl ServerCapabilities {
    /// The mount's fixed capability set: tools, with no catalog-change
    /// announcements (the five tools never change mid-session).
    pub fn tools_only() -> Self {
        Self {
            tools: ToolsCapability {
                list_changed: false,
            },
        }
    }
}

/// The tools capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolsCapability {
    /// Whether the server announces catalog changes
    /// (`notifications/tools/list_changed`); this mount never does.
    pub list_changed: bool,
}

/// The server's identity in the `initialize` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerInfo {
    /// The server's name.
    pub name: String,
    /// The server's version.
    pub version: String,
}

/// The host-owned store's capacity (ADR-0442 §2): its live logical bytes
/// against its configured cap, in raw bytes. The `initialize` result
/// publishes the whole triple; a writing tool's result repeats the
/// remaining side, so a child subtracting two of them learns what its own
/// calls cost between them.
///
/// The numbers are the engine's own accounting (ADR-0042, read through
/// `Database::memory_usage`), never a wire-side estimate. An unlimited
/// store — the engine's cap of zero — publishes [`u64::MAX`] as
/// [`Self::store_limit_bytes`], so
/// `store_used_bytes == store_limit_bytes - store_remaining_bytes` holds
/// whether the store is capped or not: one derivation for a child, no
/// second convention for "no cap".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Capacity {
    /// The configured live-graph cap, in bytes; [`u64::MAX`] is unlimited.
    pub store_limit_bytes: u64,
    /// The live graph's logical bytes right now.
    pub store_used_bytes: u64,
    /// The room left: `store_limit_bytes - store_used_bytes`, saturated at
    /// zero.
    pub store_remaining_bytes: u64,
}

impl Capacity {
    /// The capacity `used` bytes run against `limit` — the engine's
    /// `(used, limit)` reading (`Database::memory_usage`, ADR-0042) as the
    /// wire's triple. `None` is the engine's unlimited cap and publishes as
    /// [`u64::MAX`]; the remaining bytes saturate at zero, so a store
    /// already at or over its cap reports no room instead of underflowing.
    pub fn new(used: u64, limit: Option<u64>) -> Self {
        let limit = limit.unwrap_or(u64::MAX);
        Self {
            store_limit_bytes: limit,
            store_used_bytes: used,
            store_remaining_bytes: limit.saturating_sub(used),
        }
    }
}

/// The cap facts of the `initialize` result — the wire kit's existing cap
/// constants under the names the MCP mount publishes, plus the request
/// framing bound derived from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CapsFacts {
    /// The per-statement byte cap.
    pub max_statement_bytes: u64,
    /// The rows-per-statement cap.
    pub max_rows_per_statement: usize,
    /// The statements-per-call cap.
    pub max_statements: usize,
    /// The whole-run stream byte cap — the cap a `whole-import` session's
    /// request stream answers to (ADR-0598). A `per-batch` session does
    /// not enforce it: the commit policy the handshake carries beside
    /// this number is what says whether it applies — one derivation,
    /// [`StreamBudget::under`](super::StreamBudget::under)'s match.
    pub max_stream_bytes: u64,
    /// The per-request framing cap: the most bytes one request line may
    /// carry, derived from [`Self::max_statement_bytes`] by
    /// [`request_bytes_cap`] — the bound the host's capped line read runs
    /// under, published so a child can size a call it knows fits.
    pub max_request_bytes: u64,
}

/// The manifest's commit policy in the `initialize` facts: the kit's
/// two-variant vocabulary, so a child never decodes a policy string it
/// cannot act on. [`Self::spelling`] is the single mapping onto the
/// [`WHOLE_IMPORT`]/[`PER_BATCH`] constants and the serde impls are that
/// spelling — the published names and the wire's own words cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitFacts {
    /// One commit around the whole import (ADR-0157).
    WholeImport,
    /// Each batch commits as it lands (ADR-0392).
    PerBatch,
}

impl CommitFacts {
    /// The policy's wire spelling — the kit's own constants.
    pub fn spelling(self) -> &'static str {
        match self {
            Self::WholeImport => WHOLE_IMPORT,
            Self::PerBatch => PER_BATCH,
        }
    }
}

impl Serialize for CommitFacts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.spelling())
    }
}

impl<'de> Deserialize<'de> for CommitFacts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spelling = String::deserialize(deserializer)?;
        match spelling.as_str() {
            WHOLE_IMPORT => Ok(Self::WholeImport),
            PER_BATCH => Ok(Self::PerBatch),
            other => Err(serde::de::Error::custom(format!(
                "unknown commit policy '{other}': expected '{WHOLE_IMPORT}' or '{PER_BATCH}'"
            ))),
        }
    }
}

/// The most bytes a JSON string's serialization can add per input byte: a
/// control byte escapes to `\u00XX`, six bytes. Every statement the mount
/// admits is re-encoded as a JSON string inside a request line, so the
/// framing bound allows for it — a statement at the statement cap fits one
/// request line whatever its bytes.
const JSON_ESCAPE_WORST_CASE: u64 = 6;

/// The fixed envelope allowance: the JSON-RPC frame around the statement
/// (`method`, `id`, tool name, argument keys) — a few hundred bytes at
/// most for the five tools, rounded up.
pub const REQUEST_ENVELOPE_HEADROOM: u64 = 1024;

/// The per-request framing cap for a mount publishing `max_statement_bytes`:
/// the statement cap, its JSON escaping allowance, and the envelope — the
/// one derivation both the host's capped line read and the published
/// [`CapsFacts::max_request_bytes`] use. The bound admits one statement at
/// the cap; a `batch_cypher` call carries several statements under the same
/// line.
pub fn request_bytes_cap(max_statement_bytes: u64) -> u64 {
    max_statement_bytes
        .saturating_mul(JSON_ESCAPE_WORST_CASE)
        .saturating_add(REQUEST_ENVELOPE_HEADROOM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_member_names_the_member_it_refuses() {
        // Absent and null are the same absence: no map, no refusal.
        assert_eq!(object_member("params", None), Ok(None));
        assert_eq!(object_member("params", Some(&json!(null))), Ok(None));
        let map = json!({"protocolVersion": "2025-06-18"});
        assert_eq!(
            object_member("params", Some(&map)),
            Ok(Some(map.as_object().unwrap()))
        );
        // A non-object value is refused naming its own member.
        assert_eq!(
            object_member("arguments", Some(&json!(4))),
            Err("\"arguments\" must be an object".to_string())
        );
    }

    #[test]
    fn a_request_decodes_strictly_and_correlates_its_id() {
        let request = decode_request(br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).unwrap();
        assert_eq!(request.id, Some(json!(7)));
        assert_eq!(request.method, "ping");
        assert_eq!(request.params, None);
        let refused =
            decode_request(br#"{"jsonrpc":"2.0","id":1,"method":"x","extra":1}"#).unwrap_err();
        assert!(matches!(refused, RequestError::Malformed { .. }));
        let refused = decode_request(br#"{"jsonrpc":"1.0","id":1,"method":"x"}"#).unwrap_err();
        assert_eq!(
            refused,
            RequestError::UnsupportedVersion {
                version: "1.0".to_string()
            }
        );
    }

    /// The refusal text and code are the wire's, one home for both MCP
    /// surfaces: a body omitting `jsonrpc` never decodes, and a foreign
    /// version answers `-32600`.
    #[test]
    fn a_request_without_the_jsonrpc_member_is_refused_with_the_pinned_text() {
        let refused = decode_request(br#"{"id":7,"method":"ping"}"#).unwrap_err();
        assert_eq!(refused.code(), PARSE_ERROR);
        assert_eq!(
            refused.to_string(),
            "malformed JSON-RPC message: missing field `jsonrpc` at line 1 column 24"
        );
        let refused = decode_request(br#"{"jsonrpc":"1.0","id":1,"method":"x"}"#).unwrap_err();
        assert_eq!(refused.code(), INVALID_REQUEST);
        assert_eq!(refused.to_string(), "unsupported JSON-RPC version '1.0'");
    }

    #[test]
    fn a_response_enforces_exactly_one_outcome() {
        let ok = Response::success(json!(1), json!({"counts": {}}));
        assert_eq!(ok.clone().into_result(), Ok(json!({"counts": {}})));
        let err = Response::error(json!(1), INVALID_PARAMS, "nope");
        assert_eq!(
            err.clone().into_result(),
            Err(ResponseError::Rpc(RpcError {
                code: INVALID_PARAMS,
                message: "nope".to_string()
            }))
        );
        let both = Response {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: json!(1),
            result: Some(json!({})),
            error: Some(RpcError {
                code: 1,
                message: "x".to_string(),
            }),
        };
        assert!(matches!(both.into_result(), Err(ResponseError::Shape(_))));
    }

    /// The published commit spelling is the wire's own word, and nothing
    /// else decodes: the two constants are the only accepted values.
    #[test]
    fn the_commit_facts_are_the_two_wire_spellings() {
        assert_eq!(
            serde_json::to_value(CommitFacts::WholeImport).unwrap(),
            json!(WHOLE_IMPORT)
        );
        assert_eq!(
            serde_json::to_value(CommitFacts::PerBatch).unwrap(),
            json!(PER_BATCH)
        );
        assert_eq!(
            serde_json::from_value::<CommitFacts>(json!(WHOLE_IMPORT)).unwrap(),
            CommitFacts::WholeImport
        );
        let err = serde_json::from_value::<CommitFacts>(json!("sometimes")).unwrap_err();
        assert!(err.to_string().contains("sometimes"), "{err}");
    }

    /// The handshake facts are MCP's three, spelled as MCP spells them, with
    /// the mount's own facts alongside — and the capability set is the one
    /// tools capability.
    #[test]
    fn the_server_facts_spell_the_mcp_handshake() {
        let facts = ServerFacts {
            server_info: ServerInfo {
                name: "relate".to_string(),
                version: "0.13.0".to_string(),
            },
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ServerCapabilities::tools_only(),
            fresh_store_required: false,
            commit_policy: CommitFacts::PerBatch,
            caps: CapsFacts {
                max_statement_bytes: 8,
                max_rows_per_statement: 2,
                max_statements: 3,
                max_stream_bytes: 4,
                max_request_bytes: 5,
            },
            capacity: Capacity::new(6, Some(10)),
        };
        let json = serde_json::to_value(&facts).unwrap();
        assert_eq!(json["protocolVersion"], json!(MCP_PROTOCOL_VERSION));
        assert_eq!(
            json["capabilities"],
            json!({"tools": {"listChanged": false}})
        );
        assert_eq!(json["serverInfo"]["name"], json!("relate"));
        assert_eq!(
            json["capacity"],
            json!({
                "storeLimitBytes": 10,
                "storeUsedBytes": 6,
                "storeRemainingBytes": 4,
            })
        );
        assert_eq!(
            serde_json::from_value::<ServerFacts>(json).unwrap(),
            facts,
            "the writer and the reader share one schema"
        );
    }

    /// The capacity triple: a capped store publishes its cap, an unlimited
    /// one publishes `u64::MAX`, and `used == limit - remaining` holds in
    /// both cases — the one derivation a child uses.
    #[test]
    fn the_capacity_triple_bounds_an_unlimited_store_with_u64_max() {
        let capped = Capacity::new(64, Some(100));
        assert_eq!(capped.store_limit_bytes, 100);
        assert_eq!(capped.store_remaining_bytes, 36);
        let unlimited = Capacity::new(64, None);
        assert_eq!(unlimited.store_limit_bytes, u64::MAX);
        assert_eq!(unlimited.store_remaining_bytes, u64::MAX - 64);
        for capacity in [capped, unlimited] {
            assert_eq!(
                capacity.store_used_bytes,
                capacity.store_limit_bytes - capacity.store_remaining_bytes
            );
        }
        // A store at its cap has no room, and one past it reports zero
        // rather than underflowing.
        assert_eq!(Capacity::new(100, Some(100)).store_remaining_bytes, 0);
        assert_eq!(Capacity::new(120, Some(100)).store_remaining_bytes, 0);
    }

    /// Only the `notifications/` family may omit a request's id.
    #[test]
    fn the_notification_family_is_the_one_idless_method_set() {
        assert!(is_notification(METHOD_INITIALIZED));
        assert!(is_notification("notifications/cancelled"));
        assert!(!is_notification(METHOD_INITIALIZE));
        assert!(!is_notification(METHOD_TOOLS_CALL));
    }

    /// The framing bound is the statement cap plus the escape allowance and
    /// the envelope: a statement at the cap always fits one request line.
    #[test]
    fn the_request_cap_admits_one_statement_at_the_statement_cap() {
        let cap = request_bytes_cap(1024);
        assert!(
            cap >= 1024 + REQUEST_ENVELOPE_HEADROOM,
            "the framing cap must exceed the statement cap: {cap}"
        );
        assert_eq!(request_bytes_cap(0), REQUEST_ENVELOPE_HEADROOM);
        assert_eq!(request_bytes_cap(u64::MAX), u64::MAX);
    }
}
