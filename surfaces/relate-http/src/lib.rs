//! HTTP Query API v2 statement endpoint for Relate (ADR-0232 §3).
//!
//! The library exports the request handler ([`serve_handler`]) and the
//! listener loop ([`serve`]) so another process — today the tests, later the
//! `relate serve --http PORT` CLI node — can run the endpoint against an
//! embedded [`relate::Database`]. The HTTP/1.1 layer is deliberately
//! minimal — a request line, headers, and a `Content-Length` body, with
//! head parsing delegated to `httparse` (ADR-0234); one request per
//! connection, answered with `Connection: close`.
//!
//! The surface implements exactly the charter's subset: the statement
//! route `POST /db/{databaseName}/query/v2` and the managed-transaction
//! routes (`…/tx`, `…/tx/{id}`, `…/tx/{id}/commit`, ADR-0237), with request
//! fields `statement`, `parameters`, and `includeCounters`; the response
//! envelope is `data.fields` / `data.values` / `bookmarks` / `counters` on
//! success and `errors[{code,message}]` on failure. No routing headers, no
//! batching. Row values follow the Neo4j Query API v2 encoding (plain JSON,
//! or the negotiated typed `{"$type","_value"}` form) — not ADR-0223's
//! kind-tagged codec, which stays the snapshot/CLI/MCP wire.
//!
//! Authentication is the shared generated-token basic auth of
//! `relate-auth` (ADR-0232 §1): the credential arrives as an RFC 7617
//! `Authorization: Basic` header validated by
//! [`relate_auth::AuthConfig::check_authorization`].

mod admission;
mod content_encoding;
pub mod framing;
pub mod http;
mod origin;
mod query_api;
mod reply;
mod server;
mod stream;
mod streamed;
mod tx;
mod tx_limits;
mod tx_registry;
pub mod typed_params;
mod v2_envelope;
mod v2_json;
mod v2_plan;

pub use relate_auth::{AuthConfig, AuthError, Authenticator};
pub use reply::Reply;
pub use server::{serve, serve_connection, serve_handler, HttpConfig, HttpExtension};
pub use tx_limits::{
    resolve_tx_limits, unguarded_tx_registry_warning, TxLimits, DEFAULT_MAX_OPEN_TRANSACTIONS,
    DEFAULT_TX_IDLE_TIMEOUT_SECS,
};
pub use tx_registry::TxRegistry;
pub use v2_plan::query_plan_field;
