//! Client-side Bolt exchange helpers for tests (moved out of
//! relate-bolt's `#[cfg(test)]` testutil so integration-test crates —
//! e.g. the CLI's `e2e_serve` — share the one implementation; the bolt
//! crate's own unit tests keep their paths via testutil re-exports).
//! Client sockets should arm a read timeout so a wedged server fails the
//! test instead of hanging the suite.
//!
//! [`client`] is the one real Bolt session client (handshake, auth, RUN/PULL
//! pipelining) every harness that needs a live session shares.

pub mod client;

use std::io::{Read, Write};

use relate_bolt::packstream::Pack;

/// Write one struct message to the client; panics on failure.
pub fn send_struct<S: Write>(client: &mut S, tag: u8, fields: Vec<Pack>) {
    client
        .write_all(&relate_bolt::packstream::try_struct_msg(tag, fields).unwrap())
        .unwrap();
    client.flush().unwrap();
}

/// Extract the metadata map of a FAILURE message; panics otherwise.
pub fn failure_map(msg: Pack) -> Pack {
    let Pack::Struct { tag: 0x7F, fields } = msg else {
        panic!("expected FAILURE, got {msg:?}");
    };
    let Some(m @ Pack::Map(_)) = fields.into_iter().next() else {
        panic!("expected FAILURE map");
    };
    m
}

/// Extract a string field from a Pack map; panics when absent or not a
/// string. The lookup itself is the canonical [`Pack::get`].
pub fn field<'a>(m: &'a Pack, key: &str) -> &'a str {
    m.get(key)
        .and_then(Pack::as_str)
        .unwrap_or_else(|| panic!("missing field {key}"))
}

/// Read one chunked Bolt message and decode it.
///
/// Delegates the chunk loop to [`relate_bolt::protocol::read_message`]
/// so tests inherit its 16 MiB message cap.
pub fn try_read_msg<S: Read>(client: &mut S) -> std::io::Result<Pack> {
    let payload = relate_bolt::protocol::read_message(client)?;
    relate_bolt::packstream::decode(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Read one chunked Bolt message and decode it; panics on failure.
pub fn read_msg<S: Read>(client: &mut S) -> Pack {
    try_read_msg(client).unwrap()
}

/// Extract the metadata map of a SUCCESS message; panics otherwise.
pub fn success_map(msg: Pack) -> Vec<(Pack, Pack)> {
    let Pack::Struct { tag: 0x70, fields } = msg else {
        panic!("expected SUCCESS, got {msg:?}");
    };
    let Some(Pack::Map(m)) = fields.into_iter().next() else {
        panic!("expected SUCCESS map");
    };
    m
}

/// Negotiate a spoken Bolt `version` with three empty fallback version
/// slots: the magic plus the version's exact `00 00 mm MM` form as the
/// one proposal, and the server must reply in kind.
///
/// Fallible core of the client-side version negotiation (ADR-0484's two
/// spoken versions ride it) — the one body the panicking and 4.4
/// spellings below delegate to: returns `Err` on I/O failure or when the
/// server selects a version other than `version`, so callers that expect
/// rejection (e.g. mTLS refusal) can observe the failure instead of
/// panicking.
pub(crate) fn try_handshake_at<S: Read + Write>(
    stream: &mut S,
    version: [u8; 4],
) -> std::io::Result<()> {
    let mut offer = [0u8; 20];
    offer[..4].copy_from_slice(&[0x60, 0x60, 0xB0, 0x17]);
    offer[4..8].copy_from_slice(&version);
    stream.write_all(&offer)?;
    stream.flush()?;

    let mut selected = [0u8; 4];
    stream.read_exact(&mut selected)?;
    if selected != version {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("server selected unsupported Bolt version {selected:?}"),
        ));
    }
    Ok(())
}

/// Negotiate a spoken Bolt `version` with three empty fallback version
/// slots; the server must reply in kind.
///
/// Panics on I/O failure or version mismatch; use [`try_handshake`] (the
/// 4.4 form) where rejection is the expected outcome — the any-version
/// fallible core stays crate-local until a caller outside this crate
/// needs it.
pub fn handshake_at<S: Read + Write>(stream: &mut S, version: [u8; 4]) {
    try_handshake_at(stream, version).unwrap();
}

/// Negotiate Bolt 4.4 with three empty fallback version slots.
///
/// Fallible variant of [`handshake`]: returns `Err` on I/O failure or when
/// the server selects a version other than 4.4, so callers that expect
/// rejection (e.g. mTLS refusal) can observe the failure instead of
/// panicking.
pub fn try_handshake<S: Read + Write>(stream: &mut S) -> std::io::Result<()> {
    try_handshake_at(stream, [0, 0, 4, 4])
}

/// Negotiate Bolt 4.4 with three empty fallback version slots.
///
/// Panics on I/O failure or version mismatch; use [`try_handshake`] where
/// rejection is the expected outcome.
pub fn handshake<S: Read + Write>(stream: &mut S) {
    try_handshake(stream).unwrap();
}
