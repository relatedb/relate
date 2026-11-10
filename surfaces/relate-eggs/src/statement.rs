//! The one policy for an egg whose argument is a statement (`.explain-art`,
//! `.graph`): read it from the raw argument tail, never the re-joined words.

/// The statement in the raw argument tail: trimmed of surrounding blanks
/// and of any trailing `;` run (a leading `;` is kept),
/// never re-joined, so whitespace inside a string literal survives.
pub(crate) fn statement(raw: &str) -> &str {
    raw.trim().trim_end_matches(';').trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_blanks_and_semicolons_but_keeps_literal_whitespace() {
        assert_eq!(
            statement("  RETURN 'a  b\tc'  AS s ;  "),
            "RETURN 'a  b\tc'  AS s"
        );
        assert_eq!(statement(" ; "), "");
        // Only the trailing run goes: a leading `;` is the caller's to see.
        assert_eq!(statement("; MATCH (n) RETURN n;;"), "; MATCH (n) RETURN n");
    }
}
