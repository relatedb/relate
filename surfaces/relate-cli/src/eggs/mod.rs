//! The CLI's side of the hidden REPL dot commands (ADR-0543): the
//! `eggs`-feature adapter `session.rs` calls, and the first-party Archie
//! surfaces (the `.archie` command, the first-run banner, the opt-in prompt
//! glyph). The registry seam, the shared terminal [`Caps`] and the sprite
//! kit live in the `relate-eggs` crate; this module adds only the rows that
//! need CLI facilities (the config directory, the prompt type).
//!
//! Without the `eggs` feature every entry point is an inert stub: each egg
//! name is an ordinary unknown command and nothing is drawn.

#[cfg(feature = "eggs")]
mod archie;

use crate::repl::PromptStyle;

/// The CLI's own rows of the one registry (`relate_eggs::try_dispatch`
/// searches these, then `relate_eggs::EGGS`).
#[cfg(feature = "eggs")]
const LOCAL: &[relate_eggs::Entry] = &[(".archie", archie::run)];

#[cfg(feature = "eggs")]
fn real_env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// The session hook: run `stmt` as an egg when its first word is registered
/// and the gate is open; `None` leaves the caller's unknown-command error.
#[cfg(feature = "eggs")]
pub(crate) fn try_dispatch(
    stmt: &str,
    db: &relate::Database,
    conn: &mut relate::Connection,
    human_repl: bool,
) -> Option<i32> {
    let (columns, rows) = crate::width::egg_size();
    Some(
        match relate_eggs::try_dispatch(LOCAL, stmt, db, conn, human_repl, columns, rows)? {
            Ok(()) => 0,
            Err(r) => {
                crate::print_error(format_args!("Error: {r}"));
                1
            }
        },
    )
}

#[cfg(not(feature = "eggs"))]
pub(crate) fn try_dispatch(
    _stmt: &str,
    _db: &relate::Database,
    _conn: &mut relate::Connection,
    _human_repl: bool,
) -> Option<i32> {
    None
}

/// Print the REPL banner, with Archie beside it on the first interactive
/// start. Returns `false` when nothing was printed, so the caller prints the
/// plain banner line itself.
#[cfg(feature = "eggs")]
pub(crate) fn print_banner(human_repl: bool, line: &str) -> bool {
    relate_eggs::gate_open(human_repl)
        && archie::print_first_run_banner(&real_env, line, &mut std::io::stdout())
}

#[cfg(not(feature = "eggs"))]
pub(crate) fn print_banner(_human_repl: bool, _line: &str) -> bool {
    false
}

/// The prompt style for this REPL: the opt-in Archie glyph when requested
/// and the terminal can show it, else the classic prompts.
#[cfg(feature = "eggs")]
pub(crate) fn prompt_style(human_repl: bool) -> PromptStyle {
    if relate_eggs::gate_open(human_repl) {
        archie::prompt_style(&real_env)
    } else {
        PromptStyle::Classic
    }
}

#[cfg(not(feature = "eggs"))]
pub(crate) fn prompt_style(_human_repl: bool) -> PromptStyle {
    PromptStyle::Classic
}

#[cfg(all(test, feature = "eggs"))]
mod tests {
    use super::*;

    #[test]
    fn local_names_are_unique_dot_commands_and_unlisted_in_help() {
        let session = include_str!("../session.rs");
        let help_block = session
            .split("\".help\" =>")
            .nth(1)
            .and_then(|r| r.split("\".timer\" =>").next())
            .expect("the .help arm");
        let mut seen = std::collections::HashSet::new();
        for (name, _) in LOCAL.iter().chain(relate_eggs::EGGS) {
            assert!(name.starts_with('.'), "{name}");
            assert!(seen.insert(*name), "duplicate egg {name}");
            // Hidden means absent from `.help`.
            assert!(!help_block.contains(name), "{name} leaked into .help");
        }
    }
}
