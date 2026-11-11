//! Bolt FAILURE rendering: the Neo4j status codes the server sends
//! (ADR-0307), the engine-error classification at the trust boundary
//! (ADR-0260) and the wire senders for FAILURE and IGNORED messages.
//!
//! ADR-0502 §2: the classification itself — the code, the stage, the
//! retryable verdict — is single-sourced in the engine
//! (`relate_error::error_info`); this module renders it onto the Bolt
//! wire. The engine-classification code constants live there too; the two
//! codes below are this transport's own (authentication and request
//! shape), never an engine error's.

use std::io::Write;

use relate_error::{client_message, error_info, log_internal_original};

use crate::packstream::{encode_error, try_struct_msg, Pack};

/// The `code` sent when authentication is required or fails (ADR-0126): the
/// Neo4j code drivers branch on.
pub(crate) const UNAUTHORIZED_CODE: &str = "Neo.ClientError.Security.Unauthorized";
pub(crate) const REQUEST_INVALID_CODE: &str = "Neo.ClientError.Request.Invalid";

/// The delay before a failed HELLO's Unauthorized FAILURE is sent
/// (ADR-0139): bounds online guessing of a preset credential to one attempt
/// per connection per second. A successful HELLO is answered immediately.
pub(crate) const FAILED_HELLO_DELAY_MS: u64 = 1000;

/// The FAILURE text for a message over the decode budget: what was
/// measured, the limit, how the cost is counted, and what to do.
/// `at_least_bytes` is a lower bound — the decoder stops at the limit — so
/// the text says so.
pub(crate) fn decode_budget_message(at_least_bytes: usize, wire_bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    let limit_mib = crate::packstream::DecodeError::BUDGET_BYTES / MIB;
    let at_least_mib = at_least_bytes.div_ceil(MIB);
    format!(
        "Bolt message of {wire_bytes} bytes decodes to at least {at_least_mib} MiB of values, \
         over the per-message limit of {limit_mib} MiB (every value costs a fixed node plus its \
         string bytes: a row of 3 integer fields is about 230 bytes). Send fewer rows per \
         request: split the batch into smaller UNWIND chunks, for example 100000 rows of 3 \
         fields is about 22 MiB."
    )
}

pub(crate) fn failure_metadata(code: &str, message: &str) -> Vec<(Pack, Pack)> {
    vec![
        (Pack::String("code".into()), Pack::String(code.into())),
        (Pack::String("message".into()), Pack::String(message.into())),
    ]
}

pub(crate) fn send_failure_with_code<W: Write>(
    stream: &mut W,
    code: &str,
    message: &str,
) -> Result<(), std::io::Error> {
    stream.write_all(
        &try_struct_msg(0x7F, vec![Pack::Map(failure_metadata(code, message))])
            .map_err(encode_error)?,
    )
}

/// The workspace-limit outcome of `e`, when it is one.
///
/// The same split HTTP reads from `metrics.retryable`: retryable when
/// the pool was busy, non-retryable when the statement cannot fit an
/// empty pool. Graph capacity and `writer-draining` are not this limit.
pub(crate) fn workspace_refusal(e: &relate::Error) -> Option<crate::protocol::QueryOutcome> {
    match e {
        relate::Error::ResourceLimit {
            name, retryable, ..
        } if *name == relate::Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT => Some(if *retryable {
            crate::protocol::QueryOutcome::RetryableRefusal
        } else {
            crate::protocol::QueryOutcome::NonRetryableRefusal
        }),
        _ => None,
    }
}

/// Send the FAILURE for an engine error (ADR-0307): the code and the
/// message both come from the engine's one classification (ADR-0502 §2) —
/// [`ErrorInfo::client_message`] carries the caller-facing classes' Display
/// text and genericizes the internal class (ADR-0260,
/// OPLANE_REQ-00340359), so this boundary holds neither a classification
/// nor a message-exposure table of its own; the internal class's original
/// is logged server-side (Bolt is its own process, stderr is the server
/// log). The engine-classification stage that decides which message an
/// engine error fails — the RUN for a compile-class error, the first PULL
/// or DISCARD for a runtime-class one (ADR-0319) — is `error_info`'s
/// stage, read directly by `fails_run` (in `results`).
pub(crate) fn send_engine_failure<W: Write>(
    stream: &mut W,
    e: &relate::Error,
) -> Result<(), std::io::Error> {
    let info = error_info(e);
    log_internal_original(e, "bolt");
    send_failure_with_code(stream, info.neo4j_code, &client_message(e))
}

pub(crate) fn send_ignored<W: Write>(stream: &mut W) -> Result<(), std::io::Error> {
    stream.write_all(&try_struct_msg(0x7E, vec![]).map_err(encode_error)?)
}

#[cfg(test)]
#[path = "failures_tests.rs"]
mod tests;
