//! The Bolt request vocabulary (ADR-0315, gated by the negotiated
//! version per ADR-0484): the typed [`Message`] enum, its signature and
//! Neo4j class-name tables, the failed-state classification, the decode
//! step, and the breach text derived from it. The version concept itself
//! lives in the `version` leaf module; `session.rs` runs the
//! gates and handlers over this vocabulary; the request-field decoders
//! live in `fields.rs`.

use crate::packstream::{DecodeLimit, Pack};
use crate::version::BoltVersion;

/// The Bolt request messages (ADR-0315; the 5.x additions per
/// ADR-0484): the one vocabulary the signature table and Neo4j's message
/// class names derive from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Message {
    Hello,
    Goodbye,
    Reset,
    Run,
    Begin,
    Commit,
    Rollback,
    Discard,
    Pull,
    Route,
    Logon,
    Logoff,
    Telemetry,
}

impl Message {
    const ALL: [Message; 13] = [
        Message::Hello,
        Message::Goodbye,
        Message::Reset,
        Message::Run,
        Message::Begin,
        Message::Commit,
        Message::Rollback,
        Message::Discard,
        Message::Pull,
        Message::Route,
        Message::Logon,
        Message::Logoff,
        Message::Telemetry,
    ];

    /// The PackStream structure signature of this request.
    pub(crate) const fn signature(self) -> u8 {
        match self {
            Message::Hello => 0x01,
            Message::Goodbye => 0x02,
            Message::Reset => 0x0F,
            Message::Run => 0x10,
            Message::Begin => 0x11,
            Message::Commit => 0x12,
            Message::Rollback => 0x13,
            Message::Discard => 0x2F,
            Message::Pull => 0x3F,
            Message::Route => 0x66,
            Message::Logon => 0x6A,
            Message::Logoff => 0x6B,
            Message::Telemetry => 0x54,
        }
    }

    /// The request with this signature at `version`; `None` for any other
    /// signature — a 5.x-only signature at 4.4 among them, which stays a
    /// fatal unknown exactly like any unlisted one (ADR-0315).
    pub(crate) fn from_signature(signature: u8, version: BoltVersion) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|m| m.signature() == signature && m.spoken_at(version))
    }

    /// Whether `version` speaks this message: the shared 4.4 vocabulary
    /// at every version, the 5.x additions (ADR-0484) only through the
    /// version's named derivation [`BoltVersion::speaks_5x`] — the one
    /// table a new spoken version extends, never a second equality here.
    fn spoken_at(self, version: BoltVersion) -> bool {
        match self {
            Message::Logon | Message::Logoff | Message::Telemetry => version.speaks_5x(),
            _ => true,
        }
    }

    /// Whether the failed state IGNOREs this message (ADR-0110,
    /// ADR-0314): the query and transaction messages — and the 5.x
    /// session messages (ADR-0484, LOGOFF per this feature's contract) —
    /// wait for RESET to clear the failure; HELLO, RESET and ROUTE are
    /// always answered. The one classification, beside the vocabulary's
    /// other tables; the exhaustive match makes a new message's class a
    /// compile-time decision instead of a silently-answered default.
    pub(crate) fn ignored_while_failed(self) -> bool {
        match self {
            Message::Run
            | Message::Pull
            | Message::Discard
            | Message::Begin
            | Message::Commit
            | Message::Rollback
            | Message::Logon
            | Message::Logoff
            | Message::Telemetry => true,
            Message::Hello | Message::Goodbye | Message::Reset | Message::Route => false,
        }
    }

    /// Whether this message is never answered IGNORED when queued ahead
    /// of a batch's RESET (ADR-0317): HELLO, GOODBYE and RESET itself —
    /// a re-HELLO is answered, GOODBYE closes, and an earlier RESET of
    /// the batch still runs. Every other message queued ahead of the
    /// batch's last RESET answers IGNORED. The one classification both
    /// dispatch spellings of the rule consult, beside the vocabulary's
    /// other tables; the exhaustive match makes a new message's
    /// interruptibility a compile-time decision instead of a
    /// silently-IGNOREd default.
    pub(crate) fn never_interrupted(self) -> bool {
        match self {
            Message::Hello | Message::Goodbye | Message::Reset => true,
            Message::Run
            | Message::Begin
            | Message::Commit
            | Message::Rollback
            | Message::Discard
            | Message::Pull
            | Message::Route
            | Message::Logon
            | Message::Logoff
            | Message::Telemetry => false,
        }
    }

    /// Neo4j's message class name, as its breach messages spell it — the
    /// one table both `breach_text` and the tests' breach pins read.
    pub(crate) fn class_name(self) -> &'static str {
        match self {
            Message::Hello => "HelloMessage",
            Message::Goodbye => "GoodbyeMessage",
            Message::Reset => "ResetMessage",
            Message::Run => "RunMessage",
            Message::Begin => "BeginMessage",
            Message::Commit => "CommitMessage",
            Message::Rollback => "RollbackMessage",
            Message::Discard => "DiscardMessage",
            Message::Pull => "PullMessage",
            Message::Route => "RouteMessage",
            Message::Logon => "LogonMessage",
            Message::Logoff => "LogoffMessage",
            Message::Telemetry => "TelemetryMessage",
        }
    }
}

/// A decoded message that is not a request this connection's version
/// speaks: a fatal decode error (ADR-0315).
pub(crate) enum Undecodable {
    NotAStruct,
    UnknownSignature(u8),
}

impl std::fmt::Display for Undecodable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Undecodable::NotAStruct => f.write_str("a message that is not a structure"),
            Undecodable::UnknownSignature(signature) => {
                write!(f, "unknown message signature {signature:#04x}")
            }
        }
    }
}

/// A well-formed message over the decode budget (ADR-0577, ADR-0579): the
/// decoder stopped at the limit, so `at_least_bytes` is a lower bound on
/// the size it decodes to. `message` is the request the payload's structure tag names
/// at this connection's version, when it names one — read without
/// decoding, so the session's ignore classifications still apply.
pub(crate) struct OverBudget {
    pub(crate) at_least_bytes: usize,
    pub(crate) limit: DecodeLimit,
    pub(crate) bytes: usize,
    pub(crate) message: Option<Message>,
}

impl OverBudget {
    /// The one constructor: the refusal of `payload` at `version`, naming
    /// the request its structure tag reads as without decoding it.
    pub(crate) fn of_payload(
        at_least_bytes: usize,
        limit: DecodeLimit,
        payload: &[u8],
        version: BoltVersion,
    ) -> Self {
        OverBudget {
            at_least_bytes,
            limit,
            bytes: payload.len(),
            message: crate::packstream::struct_tag(payload)
                .and_then(|tag| Message::from_signature(tag, version)),
        }
    }

    /// The FAILURE text naming what was measured, the limit and the remedy.
    pub(crate) fn failure_text(&self) -> String {
        crate::failures::decode_budget_message(self.at_least_bytes, self.limit, self.bytes)
    }

    /// Whether queued ahead of a batch's last RESET this message answers
    /// IGNORED (ADR-0317); a payload naming no request is interruptible.
    pub(crate) fn interruptible(&self) -> bool {
        !self.message.is_some_and(Message::never_interrupted)
    }

    /// Whether the failed state answers this message IGNORED (ADR-0110); a
    /// payload naming no request is ignored like the work it most
    /// likely is.
    pub(crate) fn ignored_while_failed(&self) -> bool {
        self.message.is_none_or(Message::ignored_while_failed)
    }
}

/// Why a message is not dispatched as a request: a fatal decode error
/// (ADR-0315) or the recoverable value budget (ADR-0577). Each session
/// gate's arm owns its answer to either.
pub(crate) enum Refused {
    Undecodable(Undecodable),
    OverBudget(OverBudget),
}

impl Refused {
    /// Classify a decode failure through [`DecodeError::is_fatal`], the
    /// one home of the split: the recoverable budget becomes a refusal
    /// of `payload` at `version`; a fatal error is handed back.
    pub(crate) fn from_decode(
        error: crate::packstream::DecodeError,
        payload: &[u8],
        version: BoltVersion,
    ) -> Result<Self, crate::packstream::DecodeError> {
        match error {
            crate::packstream::DecodeError::BudgetExceeded {
                at_least_bytes,
                limit,
            } => Ok(Refused::OverBudget(OverBudget::of_payload(
                at_least_bytes,
                limit,
                payload,
                version,
            ))),
            // Exhaustive on purpose: a new `DecodeError` variant must
            // choose its class here, beside `is_fatal`.
            fatal @ crate::packstream::DecodeError::Malformed(_) => Err(fatal),
        }
    }
}

/// Split a decoded message into its request and fields (ADR-0315), at the
/// connection's negotiated version: a 5.x-only signature at 4.4 is the
/// same fatal unknown as any other unrecognized signature.
pub(crate) fn request(
    pack: Pack,
    version: BoltVersion,
) -> Result<(Message, Vec<Pack>), Undecodable> {
    let Pack::Struct { tag, fields } = pack else {
        return Err(Undecodable::NotAStruct);
    };
    Message::from_signature(tag, version)
        .map(|message| (message, fields))
        .ok_or(Undecodable::UnknownSignature(tag))
}

/// Neo4j's protocol-breach text (ADR-0313, ADR-0314).
pub(crate) fn breach_text(message: Message, state: &str) -> String {
    format!(
        "Message of type {} cannot be handled by a session in the {state} state.",
        message.class_name()
    )
}

#[cfg(test)]
#[path = "requests_tests.rs"]
mod tests;
