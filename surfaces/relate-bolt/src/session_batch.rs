//! The batch rule of the session dispatch (ADR-0317, ADR-0642): which message
//! of a framed batch is its last reachable RESET, so everything queued ahead
//! of it answers IGNORED, and the batch's serving loop, which also arms the
//! engine's pipelined write batch for it. Policy of the session, kept out of
//! `session.rs` beside `session_5x` and `session_pages`.

use std::io::Write;

use crate::packstream::{decode_message, struct_tag, DecodeLimit};
use crate::protocol::ConnConfig;
use crate::requests::{request, Message, Refused};
use crate::version::BoltVersion;

use super::{Flow, Session};

impl Session {
    /// Serve one batch of framed messages (ADR-0317): every message queued
    /// ahead of the batch's last reachable RESET that has not started
    /// answers IGNORED; the RESETs and everything after the last one run
    /// normally ([`BatchScan::last_reset`] locates that RESET).
    ///
    /// ADR-0642: a genuine pipeline — two or more RUNs in the framed batch —
    /// is also the pipelined write batch's lifetime. Its eligible
    /// CREATE-only auto-commit statements share one WAL commit, landed here
    /// after the batch's last message whatever the dispatch outcome, so a
    /// statement the client was told landed stays durable even when a later
    /// wire write breaks. A lone RUN keeps its own commit and the ending
    /// summary's bookmark, which a batched statement has no own commit to
    /// carry. A commit failure closes the connection: an unsolicited FAILURE
    /// has no request to answer.
    ///
    /// Messages are decoded one at a time, never as a batch: the RESET
    /// pre-scan decodes the decodable prefix one message at a time, and the
    /// dispatch decodes one message at a time, dropping it before the next.
    /// A batch's peak heap is one message's values, not the sum over its
    /// messages (SEC-serve-batch).
    pub(crate) fn serve_batch<W: Write>(
        &mut self,
        stream: &mut W,
        batch: Vec<Vec<u8>>,
        config: &ConnConfig,
    ) -> Result<Flow, Box<dyn std::error::Error + Send + Sync>> {
        let scan = BatchScan::new(&batch, self.decode_limit(), self.dialect.version);
        // A batch that pipelines writes runs its statements materialized: its
        // commit needs the connection back at the batch's end, which a
        // streamed result's worker would still hold (ADR-0682).
        self.pipelined = scan.reachable_runs() > 1;
        if self.pipelined {
            self.conn().begin_write_batch();
        }
        let dispatched = self.dispatch_messages(stream, &batch, scan.last_reset(), config);
        let committed = if self.pipelined {
            self.conn().commit_write_batch()
        } else {
            Ok(None)
        };
        let flow = dispatched?;
        committed?;
        Ok(flow)
    }

    /// One framed batch's dispatch loop (ADR-0317): the pre-scan's last
    /// reachable RESET, then each message decoded and dispatched one at a
    /// time.
    fn dispatch_messages<W: Write>(
        &mut self,
        stream: &mut W,
        batch: &[Vec<u8>],
        last_reset: Option<usize>,
        config: &ConnConfig,
    ) -> Result<Flow, Box<dyn std::error::Error + Send + Sync>> {
        for (index, payload) in batch.iter().enumerate() {
            let queued_ahead_of_reset = last_reset.is_some_and(|reset| index < reset);
            let decoded = match decode_message(payload, self.decode_limit()) {
                Ok(pack) => request(pack, self.dialect.version).map_err(Refused::Undecodable),
                Err(error) => Err(Refused::from_decode(error, payload, self.dialect.version)?),
            };
            if let Flow::Close = self.dispatch(stream, decoded, queued_ahead_of_reset, config)? {
                return Ok(Flow::Close);
            }
        }
        Ok(Flow::Continue)
    }
}

/// One framed batch's pre-scan (ADR-0317): the prefix before the first fatal
/// message and the last RESET inside it. Both the IGNORED prefix and the
/// reachable RUN count read from this one scan, so the batch is decoded once.
///
/// The prefix bound is found by decoding one message at a time and dropping
/// each before the next: the single-message peak, never the batch
/// (SEC-serve-batch). The scan decodes under the limit in force at the
/// batch's start, so a pre-auth batch is never scanned under the open limit.
pub(super) struct BatchScan {
    last_reset: Option<usize>,
    reachable_runs: usize,
}

impl BatchScan {
    pub(super) fn new(batch: &[Vec<u8>], limit: DecodeLimit, version: BoltVersion) -> Self {
        // One forward pass carries the scan's three facts — where the
        // reachable prefix ends, the last RESET inside it, and how many
        // RUNs follow that RESET — so the batch is walked once, never a
        // decode pass plus two tag rescans.
        //
        // The prefix ends at the first message that closes the session at
        // dispatch: a fatal decode error (an over-budget message is not
        // fatal, ADR-0577), a payload that decodes but names no request
        // `version` speaks (ADR-0315), or GOODBYE — which closes from
        // every gate, so nothing queued behind it runs or arms a batch.
        let mut last_reset = None;
        let mut reachable_runs = 0;
        for (index, payload) in batch.iter().enumerate() {
            match decode_message(payload, limit) {
                Ok(pack) => {
                    if request(pack, version).is_err() {
                        break;
                    }
                }
                Err(error) if error.is_fatal() => break,
                Err(_) => {}
            }
            if carries(payload, Message::Goodbye, version) {
                break;
            }
            if carries(payload, Message::Reset, version) {
                // A RESET supersedes everything queued before it: the RUNs
                // ahead of the last reachable RESET answer IGNORED, so the
                // count restarts there.
                last_reset = Some(index);
                reachable_runs = 0;
            } else if carries(payload, Message::Run, version) {
                reachable_runs += 1;
            }
        }
        Self {
            last_reset,
            reachable_runs,
        }
    }

    /// The batch's last reachable RESET (0x0F), by index (ADR-0317):
    /// everything before it answers IGNORED. A message after the first fatal
    /// one is never reached, so a RESET there queues nothing ahead of it.
    pub(super) fn last_reset(&self) -> Option<usize> {
        self.last_reset
    }

    /// How many RUNs the batch actually dispatches — the ADR-0642 pipeline
    /// test: the RUNs at or after the last reachable RESET and before the
    /// prefix end, counted once in [`Self::new`]. A RUN queued ahead of that
    /// RESET answers IGNORED, and one behind a fatal message is never
    /// reached, so neither arms a batch.
    pub(super) fn reachable_runs(&self) -> usize {
        self.reachable_runs
    }
}

/// Whether `payload`'s struct tag names `message` at `version`: two wire
/// bytes, no decode and no allocation — the one payload predicate the
/// pre-scan's RESET, RUN and GOODBYE checks share, classified through
/// [`Message::from_signature`] so the version gate is the crate's one table.
fn carries(payload: &[u8], message: Message, version: BoltVersion) -> bool {
    struct_tag(payload).and_then(|tag| Message::from_signature(tag, version)) == Some(message)
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
        assert_eq!(BatchScan::new(&batch, limit, v).last_reset(), None);
        // A RESET ahead of it is reached.
        let batch = [payload(Message::Reset), unknown(), payload(Message::Reset)];
        assert_eq!(BatchScan::new(&batch, limit, v).last_reset(), Some(0));
    }

    #[test]
    fn a_5x_only_request_is_unspoken_at_4_4_and_reached_at_5_4() {
        let limit = DecodeLimit::Open;
        let batch = [payload(Message::Logon), payload(Message::Reset)];
        assert_eq!(
            BatchScan::new(&batch, limit, BoltVersion::V4_4).last_reset(),
            None
        );
        assert_eq!(
            BatchScan::new(&batch, limit, BoltVersion::V5_4).last_reset(),
            Some(1)
        );
    }

    /// ADR-0642: only RUNs the batch actually dispatches may arm the batch.
    /// A RUN queued ahead of the last reachable RESET answers IGNORED, and one
    /// behind a fatal message is never reached.
    #[test]
    fn only_dispatched_runs_count_toward_the_batch() {
        let limit = DecodeLimit::Open;
        let v = BoltVersion::V4_4;
        let two = [payload(Message::Run), payload(Message::Run)];
        assert_eq!(BatchScan::new(&two, limit, v).reachable_runs(), 2);
        // The first RUN is IGNORED behind the RESET: only one is dispatched.
        let ignored = [
            payload(Message::Run),
            payload(Message::Reset),
            payload(Message::Run),
        ];
        assert_eq!(BatchScan::new(&ignored, limit, v).reachable_runs(), 1);
        // A RUN behind the fatal message is never dispatched.
        let unreached = [payload(Message::Run), unknown(), payload(Message::Run)];
        assert_eq!(BatchScan::new(&unreached, limit, v).reachable_runs(), 1);
    }
}
