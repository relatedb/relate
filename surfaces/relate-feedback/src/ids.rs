//! The crate's one random-id generator (ADR-0079 §3): the persistent
//! install ID, the envelope's `$insert_id`, and a report's request id.
//!
//! It lives beside the report model rather than in [`crate::state`]: an id
//! has nothing to do with the config-directory files, and every id consumer
//! — the policy, the submit pipeline, the CLI's telemetry spool — should not
//! depend on the state store to mint one.

use std::fmt::Write as _;

use rand::RngCore;

/// A random UUID v4.
pub fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 1 (RFC 4122)
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        write!(s, "{byte:02x}").expect("writing to a String cannot fail");
    }
    s
}
