//! TTY-only REPL behavior (ADR-0073). Piped e2e lives in `e2e_cli.rs`.

#![cfg(feature = "repl")]

// Every test in this file drives a real PTY, so the shared harness is
// Unix-only here too; an unconditional import would be dead on Windows.
#[cfg(unix)]
use crate::support;

/// Left-arrow + Delete rewrites `RETURN 43` into `RETURN 42` instead of
/// echoing `^[[D`. Requires a real PTY; skip on non-Unix.
#[cfg(unix)]
#[test]
fn test_interactive_left_arrow_edits_line() {
    let binary = support::binary_path_str();
    support::assert_pty_driver_ok("pty_line_edit.py", &[&binary]);
}

/// A line holding several statements keeps going after a failure in the shell,
/// so it must not print the "N not run" progress report of a stopped run
/// (ADR-0199). Requires a real PTY; skip on non-Unix.
#[cfg(unix)]
#[test]
fn test_interactive_failure_prints_no_progress_report_and_keeps_going() {
    let binary = support::binary_path_str();
    support::assert_pty_driver_ok("pty_repl_failure.py", &[&binary]);
}

/// Bare `relate` (no statement, no command word) on a TTY opens the
/// interactive shell — the same session `relate query` runs — and exits
/// 0 on `.quit` (ADR-0231). Requires a real PTY; skip on non-Unix.
#[cfg(unix)]
#[test]
fn test_bare_invocation_opens_the_interactive_shell() {
    let binary = support::binary_path_str();
    support::assert_pty_driver_ok("pty_bare_shell.py", &[&binary]);
}
