//! The lexer's ASCII fast path: batch classification of identifier-part
//! and whitespace runs (ADR-0505).
//!
//! Eight input bytes are loaded into one `u64` word (little-endian lanes,
//! zero-padded past the end of the input) and classified through a
//! 256-byte class table combined into a per-lane bitmask — the gigatoken
//! `pretokenize/fast/mask.rs` technique implemented inline, safe `u64`
//! arithmetic only (MSRV 1.99, no `std::arch`, no new dependency). Each
//! classification step therefore covers eight bytes and never decodes a
//! character per byte.
//!
//! The ADR-0182 Unicode predicates ([`crate::unicode_identifier`]) stay
//! authoritative: the class tables below never accept a byte >= 0x80, so
//! any non-ASCII byte ends the fast run and the decoded slow path decides
//! it. The ASCII agreement is pinned by a test that sweeps all 128 ASCII
//! bytes against the Unicode predicates (this module's tests).

/// True when `b` is an ASCII identifier-start byte (`[A-Za-z_]`).
///
/// This is the ASCII branch of `unicode_identifier::is_identifier_start`,
/// spelled as a `const fn` so the byte-level decision below is one
/// classification, never a second copy of the rule; the sweep test pins
/// the agreement, so the two can never diverge.
pub(super) const fn ascii_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

/// True when `b` is an ASCII identifier-part byte (`[A-Za-z0-9_]`) — the
/// ASCII branch of `unicode_identifier::is_identifier_part`.
/// `pub(super)` because the lexer module's `at_ident_part` (the number
/// scanner's glue policy) reads the class through its wrapper, exactly
/// as `ascii_ident_start` is read.
pub(super) const fn ascii_ident_part(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

/// True when `b` is ASCII whitespace between tokens (CIP-88: `\t` `\n`
/// `\r` space, plus vertical tab and form feed) — the exact set
/// `Lexer::skip_whitespace` accepted byte-at-a-time before this module.
const fn ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The identifier-part class over all 256 byte values: `false` for every
/// byte >= 0x80, so a non-ASCII byte always ends the fast run.
const IDENT_PART: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = ascii_ident_part(i as u8);
        i += 1;
    }
    t
};

/// The whitespace class over all 256 byte values: `false` for every
/// byte >= 0x80, so non-ASCII whitespace is left to the decoded path.
const WS: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = ascii_ws(i as u8);
        i += 1;
    }
    t
};

/// Byte `lane` of a loaded word (lane 0 = the byte at the cursor).
fn lane_byte(w: u64, lane: u32) -> u8 {
    (w >> (lane * 8)) as u8
}

/// Bit `i` set iff byte `i` of `w` is in `class` — the one combine loop
/// behind both prefix functions: eight table lookups folded into one
/// boundary mask, no decoding, no branches.
fn class_bits(w: u64, class: &[bool; 256]) -> u64 {
    let mut bits = 0;
    for lane in 0..8u32 {
        bits |= (class[lane_byte(w, lane) as usize] as u64) << lane;
    }
    bits
}

/// The number of leading set lanes of `bits`, capped at 8 (an all-set
/// word means the run continues in the next one).
fn leading_lanes(bits: u64) -> usize {
    ((!bits).trailing_zeros() as usize).min(8)
}

/// The number of leading bytes of `w` that are in `class` (`0..=8`) — the
/// one fast-path policy, held once: probe lane 0, then lane 1 (the
/// dominant one- and two-byte runs — a short identifier, a single space —
/// settle in one or two lookups before the full mask is worth building),
/// else the set-lane count of the whole word's class bits. The word
/// loader zero-pads past the end of the input and `0x00` is in neither
/// class, so a padded word never claims a byte the input does not have.
/// The two class tables above are the policy's only parameters.
fn class_prefix(w: u64, class: &[bool; 256]) -> usize {
    if !class[lane_byte(w, 0) as usize] {
        return 0;
    }
    if !class[lane_byte(w, 1) as usize] {
        return 1;
    }
    leading_lanes(class_bits(w, class))
}

/// The number of leading bytes of `w` that are ASCII identifier parts
/// (`0..=8`); zero padding never counts as an identifier part.
pub(super) fn ident_part_prefix(w: u64) -> usize {
    class_prefix(w, &IDENT_PART)
}

/// The number of leading bytes of `w` that are ASCII whitespace
/// (`0..=8`); zero padding never counts as whitespace.
pub(super) fn ws_prefix(w: u64) -> usize {
    class_prefix(w, &WS)
}

/// The cursor summary of an `n`-byte ASCII whitespace run of word `w`:
/// how many `b'\n'` bytes the run contains, and how many of the run's
/// bytes follow the last one — the two numbers the lexer's line/column
/// bookkeeping applies, with the per-lane bit layout decoded here so it
/// never crosses the module boundary. Lanes past the run are masked off:
/// a newline byte beyond a non-whitespace byte inside the word belongs to
/// a later step, not this run. `after_last_newline` is `0` when the run
/// crossed no newline.
pub(super) fn ws_run_summary(w: u64, n: usize) -> (usize, usize) {
    let newlines = newline_lanes(w) & lane_mask(n);
    let count = newlines.count_ones() as usize;
    let after_last_newline = if count == 0 {
        0
    } else {
        // Each flagged lane carries one bit at `8 * i + 7`; the bytes
        // after the last newline (lane `i`) are the run's bytes past it.
        let last = (63 - newlines.leading_zeros()) as usize / 8;
        n - last - 1
    };
    (count, after_last_newline)
}

/// `0x80` in every lane whose byte is `b'\n'` — the newline lanes of a
/// loaded word, for the whitespace batch's line bookkeeping.
fn newline_lanes(w: u64) -> u64 {
    zero_lanes(w ^ 0x0a0a_0a0a_0a0a_0a0a)
}

/// A mask covering all eight bits of each of the first `n` lanes
/// (`n` in `1..=8`), so per-lane flags can be restricted to a run that
/// stopped before the word's end.
fn lane_mask(n: usize) -> u64 {
    if n == 8 {
        u64::MAX
    } else {
        (1u64 << (8 * n)) - 1
    }
}

/// `0x80` in every lane whose byte is zero — the borrow-free per-lane
/// zero test (Bit Twiddling Hacks): `(x & 0x7f..) + 0x7f..` stays inside
/// each lane (max `0xfe`, no carry into the neighbor), so unlike the
/// classic `(x - 0x0101..) & !x & 0x8080..` form this is exact per lane —
/// that form lets a real zero lane's borrow flag the lane to its left.
fn zero_lanes(w: u64) -> u64 {
    const LANES_7F: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    const LANES_80: u64 = 0x8080_8080_8080_8080;
    !(((w & !LANES_80).wrapping_add(LANES_7F)) | w | !LANES_80) & LANES_80
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode_identifier;

    /// A word loaded the way the lexer loads it: `s`'s bytes as lanes,
    /// zero-padded (`s` must not exceed eight bytes).
    fn word(s: &str) -> u64 {
        let mut buf = [0u8; 8];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        u64::from_le_bytes(buf)
    }

    #[test]
    fn test_ascii_classes_agree_with_unicode_predicates() {
        // The fast path must provably agree with the ADR-0182 Unicode
        // predicates on the ASCII range — this sweep is the proof, so the
        // two classifications can never diverge.
        for b in 0u16..128 {
            let c = char::from(b as u8);
            assert_eq!(
                ascii_ident_start(b as u8),
                unicode_identifier::is_identifier_start(c),
                "identifier-start disagreement at ASCII byte {b}"
            );
            assert_eq!(
                IDENT_PART[b as usize],
                unicode_identifier::is_identifier_part(c),
                "identifier-part disagreement at ASCII byte {b}"
            );
        }
        // No byte >= 0x80 is ever classified by the fast path: non-ASCII
        // is the decoded path's decision alone.
        for b in 128..256usize {
            assert!(!ascii_ident_start(b as u8), "byte {b} must not start a run");
            assert!(!IDENT_PART[b], "byte {b} must not continue a run");
            assert!(!WS[b], "byte {b} must not be batch whitespace");
        }
        // The whitespace table keeps the exact CIP-88 ASCII set the
        // byte-at-a-time loop accepted.
        for (b, &ws) in WS.iter().enumerate() {
            assert_eq!(
                ws,
                matches!(b as u8, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
                "whitespace disagreement at byte {b}"
            );
        }
    }

    #[test]
    fn test_ident_part_prefix_counts_leading_identifier_bytes() {
        assert_eq!(ident_part_prefix(word("abcdefgh")), 8);
        assert_eq!(ident_part_prefix(word("ab_d1ef")), 7);
        assert_eq!(ident_part_prefix(word("ab.d")), 2);
        assert_eq!(ident_part_prefix(word("n")), 1);
        assert_eq!(ident_part_prefix(word("n)")), 1);
        // A multi-byte lead byte ends the ASCII run at the character edge.
        assert_eq!(ident_part_prefix(word("abcé")), 3);
        assert_eq!(ident_part_prefix(word("éabc")), 0);
        // Zero padding (past the end of input) is not an identifier part.
        assert_eq!(ident_part_prefix(0), 0);
    }

    #[test]
    fn test_ws_prefix_counts_leading_whitespace_bytes() {
        assert_eq!(ws_prefix(word("  \t\n\r x")), 6);
        assert_eq!(ws_prefix(word(" \x0b\x0c")), 3);
        assert_eq!(ws_prefix(word(" x")), 1);
        assert_eq!(ws_prefix(word("x ")), 0);
        // A non-ASCII byte is never ASCII whitespace — the decoded path
        // decides it.
        assert_eq!(ws_prefix(word(" \u{a0}")), 1);
        assert_eq!(ws_prefix(0), 0);
    }

    #[test]
    fn test_newline_lanes_flags_each_newline_byte_exactly() {
        assert_eq!(
            newline_lanes(word("a\nb\nc")),
            (0x80u64 << 8) | (0x80u64 << 24)
        );
        assert_eq!(newline_lanes(word("\n\n\n\n")), 0x0000_0000_8080_8080);
        assert_eq!(newline_lanes(word("abc")), 0);
        // `\r` is whitespace but not a newline lane.
        assert_eq!(newline_lanes(word("a\rb")), 0);
        // The borrow trap of the classic zero-byte test: a 0x01 lane to
        // the left of a real zero lane must not flag (the classic
        // `(x - 0x0101..) & !x & 0x8080..` form would report two).
        assert_eq!(newline_lanes(word("\n\x0b")), 0x80);
        assert_eq!(zero_lanes(0xFFFF_FFFF_FFFF_0100), 0x80);
    }

    #[test]
    fn test_ws_run_summary_reports_newline_count_and_tail() {
        // (`newlines`, `bytes after the last newline`) of the run's first
        // `n` lanes — the settled facts `advance_ws_run` applies.
        assert_eq!(ws_run_summary(word("  \t\n\r x"), 6), (1, 2));
        assert_eq!(ws_run_summary(word(" \x0b\x0c"), 3), (0, 0));
        assert_eq!(ws_run_summary(word("\n\n\n\n"), 4), (4, 0));
        // A newline lane beyond the run's end is masked off: it belongs
        // to a later step, not this run.
        assert_eq!(ws_run_summary(word("\n\tx\nq"), 2), (1, 1));
    }

    #[test]
    fn test_lane_mask_covers_exactly_the_first_n_lanes() {
        assert_eq!(lane_mask(1), 0xff);
        assert_eq!(lane_mask(2), 0xffff);
        assert_eq!(lane_mask(7), 0x00ff_ffff_ffff_ffff);
        assert_eq!(lane_mask(8), u64::MAX);
    }
}
