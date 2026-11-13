//! The batch rule of the session dispatch (ADR-0317): which message of a
//! framed batch is its last reachable RESET, so everything queued ahead of
//! it answers IGNORED. Policy of the session, kept out of `session.rs`
//! beside `session_5x` and `session_pages`.

use crate::packstream::{decode_message, struct_tag, DecodeLimit};
use crate::requests::Message;

/// The batch's last reachable RESET (0x0F), by index (ADR-0317): the last
/// RESET inside the prefix before the first fatal decode error (an
/// over-budget message is not fatal, ADR-0577). A message after the first
/// fatal one is never reached — the fatal decode (ADR-0315) closes the session
/// first — so a RESET there queues nothing ahead of it. The prefix bound is
/// found by decoding the prefix one message at a time and dropping each
/// before the next: the single-message peak again, never the batch
/// (SEC-serve-batch). The scan decodes under the limit in force at the
/// batch's start, so a pre-auth batch is never scanned under the open limit.
pub(super) fn last_reset(batch: &[Vec<u8>], limit: DecodeLimit) -> Option<usize> {
    // The one spelling of "this payload is a RESET": two wire bytes, no
    // decode and no allocation, and the tag hoisted out of the per-payload
    // closure.
    let reset_tag = Message::Reset.signature();
    let is_reset = |payload: &Vec<u8>| struct_tag(payload) == Some(reset_tag);
    let prefix_end = batch
        .iter()
        .position(|payload| decode_message(payload, limit).is_err_and(|e| e.is_fatal()))
        .unwrap_or(batch.len());
    batch[..prefix_end].iter().rposition(is_reset)
}
