//! The one output-safety classification, shared by every surface that
//! renders untrusted text.

/// The single classification of characters that must never reach a human-facing
/// output surface raw: Unicode general-category **`Cc`** controls (C0, DEL, C1,
/// via [`char::is_control`]) **and** **`Cf`** format characters (bidi overrides
/// such as U+202E RIGHT-TO-LEFT OVERRIDE, zero-width marks such as U+200B/U+200E,
/// and U+FEFF), which `is_control` does not cover but which reorder or hide
/// printable text and can spoof a terminal.
///
/// This is shared by every output surface — the formatter's terminal/markdown
/// escaper and Cypher string literal, the CLI's remote-string sanitizer,
/// and the storage layer's untrusted-name rendering — there is
/// deliberately no per-surface table to drift out of sync.
pub fn is_unsafe_output_char(c: char) -> bool {
    c.is_control() || is_format_char(c)
}

/// The one output-safety escape: characters the classification
/// ([`is_unsafe_output_char`]) marks — controls, bidi and zero-width
/// format marks — render through `char::escape_default` (`\n`,
/// `\u{1b}`, …); every other character passes through unchanged, so the
/// text stays recognizable. The storage layer's untrusted-name rendering
/// and the quoted message terms share this one loop.
pub fn escape_unsafe_output(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for c in input.chars() {
        if is_unsafe_output_char(c) {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// The input as a single-quoted message term: the frame's own two
/// escapes (`'` and `\`) applied first, then [`escape_unsafe_output`]'s
/// one escape over the whole body — the frame's escapes produce only
/// printable characters, so the one escape leaves them intact — wrapped
/// in the frame. Printable text passes through unchanged, so the term
/// still spells what the user wrote.
pub fn quote_message(input: &str) -> String {
    let mut body = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\'' => body.push_str("\\'"),
            '\\' => body.push_str("\\\\"),
            c => body.push(c),
        }
    }
    format!("'{}'", escape_unsafe_output(&body))
}

/// `true` for Unicode general category `Cf` (format) characters. Rust's
/// `char::is_control` covers only `Cc`, so the bidi controls, zero-width marks,
/// and other invisible formatters need their own check.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

#[cfg(test)]
mod tests {
    use super::{escape_unsafe_output, is_unsafe_output_char, quote_message};

    #[test]
    fn the_escape_and_the_quoted_frame_split_their_concerns() {
        // The bare escape: unsafe chars visibly escaped, everything else
        // — the frame's two characters included — passes through.
        assert_eq!(escape_unsafe_output("a\nb"), "a\\nb");
        assert_eq!(escape_unsafe_output("a\u{202e}b"), "a\\u{202e}b");
        assert_eq!(escape_unsafe_output("it's a\\b"), "it's a\\b");
        assert_eq!(escape_unsafe_output("café"), "café");
        // The quoted term: that escape plus the frame's two, wrapped.
        assert_eq!(quote_message("it's"), "'it\\'s'");
        assert_eq!(quote_message("a\\b"), "'a\\\\b'");
        assert_eq!(quote_message("a\nb"), "'a\\nb'");
        // Printable text — non-ASCII included — passes through: a quoted
        // term spells what the user wrote.
        assert_eq!(quote_message("café"), "'café'");
    }

    #[test]
    fn covers_cc_and_cf() {
        // Cc: ESC, DEL, a C1 CSI byte.
        for c in ['\u{1b}', '\u{7f}', '\u{85}'] {
            assert!(is_unsafe_output_char(c), "Cc {c:?} not classified unsafe");
        }
        // Cf: bidi controls, zero-width/BOM marks, soft hyphen, and an astral
        // format character (LANGUAGE TAG) that `is_control` never sees.
        for c in [
            '\u{202e}',
            '\u{200e}',
            '\u{200b}',
            '\u{feff}',
            '\u{00ad}',
            '\u{e0001}',
        ] {
            assert!(is_unsafe_output_char(c), "Cf {c:?} not classified unsafe");
        }
        // Ordinary printable text (including non-ASCII) stays untouched.
        for c in ['A', '0', ' ', 'é', '你'] {
            assert!(!is_unsafe_output_char(c), "{c:?} wrongly unsafe");
        }
    }
}
