//! The seam for hidden REPL dot commands (ADR-0543), and the pieces eggs
//! share.
//!
//! Client-side easter eggs are neither engine nor extensions: each is a
//! dot command the `relate` CLI's REPL runs against the open session,
//! listed in no `.help` and no `--help`. One table ([`EGGS`]) maps each
//! dot-command name to its handler; each egg is its own module. The CLI's
//! `eggs` feature links this crate and calls [`try_dispatch`]; the CLI's
//! own first-party entries (Archie) ride the same dispatch as its `local`
//! table.
//!
//! The gate is closed — the name is then an ordinary unknown command —
//! unless the session is the interactive human REPL (never agent mode,
//! which the CLI decides and passes as `human_repl`) and stdout is a
//! terminal ([`gate_open`]). With the gate open, an egg degrades through
//! [`Caps`]: plain output without colour, or a typed [`Refusal`] where it
//! cannot draw at all.
//!
//! This crate depends on the engine only; it never depends on the CLI.

pub mod canvas;
mod cowsay;
mod explain_art;
mod fortune;
#[cfg(feature = "games")]
mod games;
mod graph;
mod layout;
mod lolwut;
mod matrix;
mod rng;
pub mod sprite;
mod text_arg;

use std::io::{IsTerminal, Write};

/// What the terminal can show, read once from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The locale is UTF-8, so block and shade glyphs are safe to print.
    pub utf8: bool,
    /// 24-bit colour is on offer: `COLORTERM` says so, `NO_COLOR` is unset
    /// (or empty) and `TERM` is not `dumb`.
    pub truecolor: bool,
}

impl Caps {
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Caps {
        let set = |k: &str| env(k).filter(|v| !v.is_empty());
        let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|k| set(k))
            .is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("utf-8") || v.contains("utf8")
            });
        let colour_allowed = set("NO_COLOR").is_none() && set("TERM").as_deref() != Some("dumb");
        let truecolor = colour_allowed
            && set("COLORTERM").is_some_and(|v| matches!(v.as_str(), "truecolor" | "24bit"));
        Caps { utf8, truecolor }
    }

    /// `NO_COLOR` is the user's word on colour: kept separate from
    /// [`Caps::truecolor`], which is also false merely for lack of support.
    pub fn no_color(env: &dyn Fn(&str) -> Option<String>) -> bool {
        env("NO_COLOR").is_some_and(|v| !v.is_empty())
    }
}

/// Why an egg declined to run (the CLI prints it to stderr, exit 1).
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The locale cannot show the glyphs the egg draws with.
    NotUtf8,
    /// Wrong arguments; carries the usage line.
    Usage(&'static str),
    /// stdin or stdout is not a terminal, so a full-screen egg cannot run.
    NotATerminal,
    /// The terminal is smaller than the egg's fixed layout (columns, rows).
    TooSmall { need: (u16, u16), have: (u16, u16) },
    /// Writing to the terminal failed.
    Io(String),
    /// The egg declined the request, or the engine failed it; carries the
    /// sentence to show.
    Declined(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotUtf8 => write!(f, "this terminal locale is not UTF-8"),
            Refusal::Usage(u) => write!(f, "{u}"),
            Refusal::NotATerminal => write!(f, "this needs an interactive terminal"),
            Refusal::TooSmall { need, have } => write!(
                f,
                "terminal too small: need {}x{}, have {}x{}",
                need.0, need.1, have.0, have.1
            ),
            Refusal::Io(e) | Refusal::Declined(e) => write!(f, "{e}"),
        }
    }
}

/// What a handler runs against: the open session, its output, the
/// terminal's capabilities.
pub struct Ctx<'a> {
    pub db: &'a relate::Database,
    pub conn: &'a mut relate::Connection,
    pub out: &'a mut dyn Write,
    pub caps: Caps,
    /// Everything after the command name, byte for byte (surrounding
    /// whitespace included): for an egg whose argument is a statement, where
    /// the whitespace inside a string literal is data. Handlers that take
    /// words read the split `args` instead.
    pub raw_args: &'a str,
    /// The terminal's column count as the CLI's one width probe reported it,
    /// unfloored; `None` when it cannot be asked (a pipe, no `COLUMNS`).
    pub columns: Option<usize>,
}

/// A handler: the arguments after the command name.
pub type Handler = fn(&mut Ctx<'_>, &[&str]) -> Result<(), Refusal>;

/// One registry row: dot-command name to handler.
pub type Entry = (&'static str, Handler);

/// The registry of this crate's eggs. A new egg is one row here and one
/// module beside this file.
pub const EGGS: &[Entry] = &[
    (".cowsay", cowsay::run),
    #[cfg(feature = "games")]
    (".dash", games::dash::run),
    (".explain-art", explain_art::run),
    (".fortune", fortune::run),
    (".graph", graph::run),
    #[cfg(feature = "games")]
    (".life", games::life::run),
    (".lolwut", lolwut::run),
    (".matrix", matrix::run),
    #[cfg(feature = "games")]
    (".snake", games::snake::run),
    #[cfg(feature = "games")]
    (".tetris", games::tetris::run),
];

/// The interactive, non-agent REPL the session reports, on a terminal
/// stdout: the one gate every egg surface (commands, banner, prompt) shares.
pub fn gate_open(human_repl: bool) -> bool {
    human_repl && std::io::stdout().is_terminal()
}

/// The session hook: run `stmt` as an egg when its first word is registered
/// (in `local`, the CLI's own first-party rows, or in [`EGGS`]) and the gate
/// is open. `None` means "not an egg" — the caller's unknown command error
/// stands, so a hidden command is indistinguishable from a missing one
/// wherever it is unavailable. `Some(Err(_))` is a refusal for the caller to
/// print. `columns` is the CLI's terminal-width probe, handed through as
/// [`Ctx::columns`].
pub fn try_dispatch(
    local: &[Entry],
    stmt: &str,
    db: &relate::Database,
    conn: &mut relate::Connection,
    human_repl: bool,
    columns: Option<usize>,
) -> Option<Result<(), Refusal>> {
    let stmt = stmt.trim_start();
    let name = stmt.split_whitespace().next()?;
    let (_, handler) = local.iter().chain(EGGS).find(|(n, _)| *n == name)?;
    if !gate_open(human_repl) {
        return None;
    }
    let raw_args = &stmt[name.len()..];
    let args: Vec<&str> = raw_args.split_whitespace().collect();
    let mut stdout = std::io::stdout();
    let mut ctx = Ctx {
        db,
        conn,
        out: &mut stdout,
        caps: Caps::from_env(&|k| std::env::var(k).ok()),
        raw_args,
        columns,
    };
    Some(handler(&mut ctx, &args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn caps_need_a_utf8_locale() {
        assert!(Caps::from_env(&env_of(&[("LANG", "en_US.UTF-8")])).utf8);
        assert!(Caps::from_env(&env_of(&[("LC_ALL", "C.utf8"), ("LANG", "C")])).utf8);
        // LC_ALL wins over LANG, as in the C library.
        assert!(!Caps::from_env(&env_of(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")])).utf8);
        assert!(!Caps::from_env(&env_of(&[])).utf8);
    }

    #[test]
    fn truecolor_needs_colorterm_and_no_opt_out() {
        let tc = ("COLORTERM", "truecolor");
        assert!(Caps::from_env(&env_of(&[tc])).truecolor);
        assert!(Caps::from_env(&env_of(&[("COLORTERM", "24bit")])).truecolor);
        assert!(!Caps::from_env(&env_of(&[])).truecolor);
        assert!(!Caps::from_env(&env_of(&[tc, ("NO_COLOR", "1")])).truecolor);
        assert!(!Caps::from_env(&env_of(&[tc, ("TERM", "dumb")])).truecolor);
        // An empty NO_COLOR is unset (no-color.org).
        assert!(Caps::from_env(&env_of(&[tc, ("NO_COLOR", "")])).truecolor);
    }

    #[test]
    fn no_color_reads_a_non_empty_value() {
        assert!(Caps::no_color(&env_of(&[("NO_COLOR", "1")])));
        assert!(!Caps::no_color(&env_of(&[("NO_COLOR", "")])));
        assert!(!Caps::no_color(&env_of(&[])));
    }

    #[test]
    fn registry_names_are_unique_dot_commands() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in EGGS {
            assert!(name.starts_with('.'), "{name}");
            assert!(seen.insert(*name), "duplicate egg {name}");
        }
    }

    #[test]
    fn refusals_read_as_plain_sentences() {
        assert_eq!(Refusal::Usage("Usage: .x").to_string(), "Usage: .x");
        assert!(Refusal::NotUtf8.to_string().contains("UTF-8"));
    }
}
