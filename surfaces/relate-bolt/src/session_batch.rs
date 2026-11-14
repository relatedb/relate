//! The batch rule of the session dispatch (ADR-0317): which message of a
//! framed batch is its last reachable RESET, so everything queued ahead of
//! it answers IGNORED. Policy of the session, kept out of `session.rs`
//! beside `session_5x` and `session_pages`.

use crate::packstream::{decode_message, struct_tag, DecodeLimit};
use crate::requests::{request, Message};
use crate::version::BoltVersion;

/// The batch's last reachable RESET (0x0F), by index (ADR-0317): the last
/// RESET inside the prefix before the first fatal message: a fatal decode
/// error (an over-budget message is not fatal, ADR-0577) or a payload that
/// decodes but names no request `version` speaks (ADR-0315) — both end the
/// session at dispatch. A message after the first
/// fatal one is never reached — the fatal decode (ADR-0315) closes the session
/// first — so a RESET there queues nothing ahead of it. The prefix bound is
/// found by decoding the prefix one message at a time and dropping each
/// before the next: the single-message peak again, never the batch
/// (SEC-serve-batch). The scan decodes under the limit in force at the
/// batch's start, so a pre-auth batch is never scanned under the open limit.
pub(super) fn last_reset(
    batch: &[Vec<u8>],
    limit: DecodeLimit,
    version: BoltVersion,
) -> Option<usize> {
    // The one spelling of "this payload is a RESET": two wire bytes, no
    // decode and no allocation, and the tag hoisted out of the per-payload
    // closure.
    let reset_tag = Message::Reset.signature();
    let is_reset = |payload: &Vec<u8>| struct_tag(payload) == Some(reset_tag);
    let prefix_end = batch
        .iter()
        .position(|payload| match decode_message(payload, limit) {
            Ok(pack) => request(pack, version).is_err(),
            Err(error) => error.is_fatal(),
        })
        .unwrap_or(batch.len());
    batch[..prefix_end].iter().rposition(is_reset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packstream::{encode, Pack};

    fn payload(message: Message) -> Vec<u8> {
        encode(&Pack::Struct {
            tag: message.signature(),
            fields: vec![],
        })
        .unwrap()
    }

    fn unknown() -> Vec<u8> {
        encode(&Pack::Struct {
            tag: 0x0E,
            fields: vec![],
        })
        .unwrap()
    }

    #[test]
    fn a_reset_after_a_payload_naming_no_request_is_not_reached() {
        let limit = DecodeLimit::Open;
        let v = BoltVersion::V4_4;
        // The unknown signature ends the session at dispatch, so the RESET
        // behind it queues nothing ahead of itself.
        let batch = [payload(Message::Run), unknown(), payload(Message::Reset)];
        assert_eq!(last_reset(&batch, limit, v), None);
        // A RESET ahead of it is reached.
        let batch = [payload(Message::Reset), unknown(), payload(Message::Reset)];
        assert_eq!(last_reset(&batch, limit, v), Some(0));
    }

    #[test]
    fn a_5x_only_request_is_unspoken_at_4_4_and_reached_at_5_4() {
        let limit = DecodeLimit::Open;
        let batch = [payload(Message::Logon), payload(Message::Reset)];
        assert_eq!(last_reset(&batch, limit, BoltVersion::V4_4), None);
        assert_eq!(last_reset(&batch, limit, BoltVersion::V5_4), Some(1));
    }
}
