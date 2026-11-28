//! Entry-point unit tests (moved out of main.rs to keep both under the
//! repo's 1k-line file budget).

mod default_mode_tests {
    use crate::{default_mode, Mode};

    /// The ADR-0426 decision table: a terminal is a human's (the table,
    /// whatever the agent mode — the flag never rewrites the human view);
    /// a pipe is JSONL for human pipelines (ADR-0107) and `toon` when
    /// agent ergonomics are in effect (ADR-0407's ladder).
    #[test]
    fn terminal_defaults_to_table_pipe_to_jsonl_and_agent_pipe_to_toon() {
        assert_eq!(default_mode(true, false), Mode::Table);
        assert_eq!(default_mode(true, true), Mode::Table);
        assert_eq!(default_mode(false, false), Mode::Jsonl);
        assert_eq!(default_mode(false, true), Mode::Toon);
    }
}

mod classify_session_tests {
    use crate::classify_session;
    use crate::session::Session;

    /// The ADR-0231 bare-form decision table: only a bare invocation on a
    /// non-terminal stdin prints help; a bare TTY invocation opens the
    /// shell, and a statement or the `query` verb never prints help here.
    #[test]
    fn bare_tty_opens_the_shell_and_bare_pipe_prints_help() {
        // A statement is a oneshot on the path-first form either way.
        for explicit in [false, true] {
            for tty in [false, true] {
                assert!(matches!(
                    classify_session(Some("RETURN 1"), explicit, tty),
                    Some(Session::Oneshot("RETURN 1"))
                ));
            }
        }
        // The `query` verb with no STATEMENT: shell on a terminal, stdin
        // script otherwise.
        assert!(matches!(
            classify_session(None, true, true),
            Some(Session::Repl)
        ));
        assert!(matches!(
            classify_session(None, true, false),
            Some(Session::Stdin)
        ));
        // Bare: shell on a terminal, help on piped/redirected stdin.
        assert!(matches!(
            classify_session(None, false, true),
            Some(Session::Repl)
        ));
        assert!(classify_session(None, false, false).is_none());
    }
}
