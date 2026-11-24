//! Hex byte strings: the `<RAW>` / `!: HANDSHAKE` payloads, the `0xNN`
//! message tags and `#NN` structure tags a script writes, and the byte
//! dumps a failure prints.

/// Decode hex digits into bytes. ASCII whitespace between digits is
/// ignored, so `60 60 B0 17` and `6060b017` are the same four bytes.
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.is_empty() {
        return Err("expected hex bytes, found none".to_string());
    }
    if let Some(bad) = digits.iter().find(|b| !b.is_ascii_hexdigit()) {
        return Err(format!(
            "`{}` is not a hex digit in `{text}`",
            char::from(*bad)
        ));
    }
    if !digits.len().is_multiple_of(2) {
        return Err(format!("odd number of hex digits in `{text}`"));
    }
    Ok(digits
        .chunks(2)
        .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
        .collect())
}

/// Decode exactly one byte written as two hex digits (a message or
/// structure tag).
pub(crate) fn byte(text: &str) -> Result<u8, String> {
    match decode(text)?.as_slice() {
        [tag] => Ok(*tag),
        _ => Err(format!("`{text}` is not exactly two hex digits")),
    }
}

/// Render bytes as space-separated uppercase hex pairs (`00 00 04 04`).
pub(crate) fn encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn nibble(digit: u8) -> u8 {
    char::from(digit)
        .to_digit(16)
        .expect("decode checked every digit is hex") as u8
}
