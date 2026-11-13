//! The Bolt versions this client speaks and the handshake that proposes them
//! (ADR-0484 decisions 1 and 2).

/// The handshake's magic preamble: the four bytes every connection opens
/// with, before the proposal slots.
pub(super) const MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17];

/// A Bolt version this client speaks (ADR-0484 decision 1): 5.4, the
/// server's newest, and 4.4, the dialect every earlier server speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoltVersion {
    /// Bolt 4.4 (ADR-0057).
    V4_4,
    /// Bolt 5.4 (ADR-0484).
    V5_4,
}

impl BoltVersion {
    /// The versions this client proposes, in preference order (ADR-0484
    /// decision 2): the server answers the first proposal covering a
    /// version it speaks.
    const SPOKEN: [BoltVersion; 2] = [BoltVersion::V5_4, BoltVersion::V4_4];

    /// This version's `00 00 mm MM` wire form: the proposal the client
    /// sends, and the exact handshake reply it accepts.
    const fn wire(self) -> [u8; 4] {
        match self {
            BoltVersion::V4_4 => [0x00, 0x00, 0x04, 0x04],
            BoltVersion::V5_4 => [0x00, 0x00, 0x04, 0x05],
        }
    }

    /// This version's spoken name, for diagnostics.
    fn name(self) -> &'static str {
        match self {
            BoltVersion::V4_4 => "Bolt 4.4",
            BoltVersion::V5_4 => "Bolt 5.4",
        }
    }

    /// Whether this version speaks the 5.x session protocol (ADR-0484): the
    /// HELLO that does not authenticate, then LOGON.
    pub(super) const fn speaks_5x(self) -> bool {
        match self {
            BoltVersion::V4_4 => false,
            BoltVersion::V5_4 => true,
        }
    }

    /// The version a server's handshake reply selected; anything else is a
    /// server that answered a version this client never proposed.
    pub(super) fn from_reply(reply: [u8; 4]) -> Result<Self, String> {
        Self::SPOKEN
            .into_iter()
            .find(|version| version.wire() == reply)
            .ok_or_else(|| {
                let spoken = Self::SPOKEN.map(Self::name).join(" or ");
                format!("server selected {reply:?}, want {spoken}")
            })
    }
}

/// The handshake's four proposal slots (ADR-0303, ADR-0484 decision 2): the
/// spoken versions in preference order, then empty slots — a zero proposal
/// covers nothing.
pub(super) const PROPOSALS: [[u8; 4]; 4] = {
    let mut proposals = [[0u8; 4]; 4];
    let mut index = 0;
    while index < BoltVersion::SPOKEN.len() {
        proposals[index] = BoltVersion::SPOKEN[index].wire();
        index += 1;
    }
    proposals
};

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply naming a version the client never proposed is an error that
    /// says which versions it did, never a guessed fallback.
    #[test]
    fn an_unproposed_reply_is_rejected() {
        let err = BoltVersion::from_reply([0, 0, 0, 0]).unwrap_err();
        assert_eq!(
            err,
            "server selected [0, 0, 0, 0], want Bolt 5.4 or Bolt 4.4"
        );
    }
}
