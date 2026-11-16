//! The one terminal-prompt answer kit: read one line, classify it once
//! (fail-closed). `skill refresh`'s ask and the feedback grant ask
//! (ADR-0588 §3 as revised) share it, so an unrecognized answer can never
//! grant anything — a second classifier would have to re-decide that.

use std::io::BufRead;

/// Read one answer line. `None` is an unread answer — a read failure, or
/// EOF with nothing typed — and no ask is spent on it. EOF is deliberately
/// not an empty line: under a default-yes prompt an empty line grants, and
/// a consent grant must never come from a closed stdin.
pub(crate) fn read_answer(stdin: &mut dyn BufRead) -> Option<String> {
    let mut line = String::new();
    match stdin.read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line),
        Err(_) => None,
    }
}

/// Pure answer classification: empty, `y` and `yes` — trimmed and
/// case-insensitive — are yes; everything else (`n`, `no`, `nope`, `No.`,
/// garbage, EOF residue) is no.
pub(crate) fn answer_is_yes(answer: &str) -> bool {
    let answer = answer.trim();
    answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

#[cfg(test)]
mod tests {
    use super::{answer_is_yes, read_answer};

    #[test]
    fn empty_y_and_yes_grant_and_everything_else_declines() {
        for yes in ["", "  ", "y", "Y", "yes", "YES", " y ", " Yes ", "yEs\n"] {
            assert!(answer_is_yes(yes), "expected yes for {yes:?}");
        }
        // The fail-closed half: an unrecognized answer must never grant.
        for no in [
            "n",
            "N",
            "no",
            "NO",
            " no ",
            "nope",
            "No.",
            "nO",
            "no thanks",
            "ye",
            "1",
        ] {
            assert!(!answer_is_yes(no), "expected no for {no:?}");
        }
    }

    /// EOF is an unread answer, never an empty line: under a default-yes
    /// prompt an empty line grants, and a consent grant must never come
    /// from a closed stdin.
    #[test]
    fn eof_reads_as_unread_not_as_an_empty_answer() {
        assert!(read_answer(&mut &b""[..]).is_none(), "EOF is unread");
        assert_eq!(
            read_answer(&mut &b"\n"[..]).as_deref(),
            Some("\n"),
            "a typed empty line is an answer"
        );
    }
}
