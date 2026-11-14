//! `relate agent-context [COMMAND]` (ADR-0242, LABS-236): the compact
//! discovery readout for agent harnesses. Everything prints from the
//! `help::COMMANDS` descriptor table that also feeds `--help`, plus the one
//! [`EXIT_CODES`] table (in `errors`, beside the classes that produce the
//! codes), so the readout cannot drift from the parser.

use crate::errors::EXIT_CODES;
use crate::help::{self, CommandSpec};
use crate::parse_error::ParseError;

/// Parse the argument tail after the `agent-context` verb: nothing (the
/// index), or one command word. The word resolves through
/// [`help::find_command`], the one place the `update` → `upgrade` alias
/// lives for lookups; the returned spec is the detail the runner prints, so
/// a validated word is never re-looked-up.
pub fn parse_agent_context_args(
    rest: &[String],
) -> Result<Option<&'static CommandSpec>, ParseError> {
    match rest {
        [] => Ok(None),
        [word] => match help::find_command(word) {
            Some(spec) => Ok(Some(spec)),
            None => {
                let words: Vec<&str> = help::commands().iter().map(|spec| spec.word).collect();
                Err(format!(
                    "agent-context: unknown command '{word}' (expected one of: {})",
                    words.join(", ")
                )
                .into())
            }
        },
        [_, extra, ..] => Err(ParseError::unexpected("agent-context", extra)),
    }
}

/// Run the parsed command and return the process exit code. The readout is
/// read-only and cannot fail once the arguments parsed, so it is always 0.
pub fn run(command: Option<&'static CommandSpec>) -> i32 {
    match command {
        None => print_index(),
        Some(spec) => print_detail(spec),
    }
    0
}

/// The no-argument index: version, one line per compiled-in command, the
/// query-run flag names, and the exit-code table.
fn print_index() {
    println!("relate v{} — agent context", crate::VERSION);
    println!();
    println!("Commands (detail: relate agent-context <COMMAND>):");
    let listed: Vec<&CommandSpec> = help::commands().iter().collect();
    let width = help::command_column_width(&listed);
    for spec in help::commands() {
        println!("{}", help::command_row(spec, width));
    }
    println!();
    println!("Query-run flags (relate [OPTIONS] [QUERY]):");
    println!("  {}", help::session_flags().join(" "));
    println!();
    print_exit_codes();
}

/// One command's detail: synopsis, one-liner, flags, examples, exit codes.
fn print_detail(spec: &CommandSpec) {
    println!("{} — {}", spec.word, spec.blurb);
    println!("{}", spec.usage);
    // `query`'s spec.flags is the shared QUERY_FLAGS session list, so one
    // loop covers every command.
    if !spec.flags.is_empty() {
        println!("Flags:");
        for flag in spec.flags.iter() {
            println!("  {flag}");
        }
    }
    if !spec.examples.is_empty() {
        println!("Examples:");
        for (comment, command) in spec.examples {
            println!("  # {comment}");
            println!("  {command}");
        }
    }
    print_exit_codes();
}

/// The exit-code block shared by the index and the per-command detail.
fn print_exit_codes() {
    println!("Exit codes:");
    for (exit, code, meaning) in EXIT_CODES {
        println!("  {exit}  {code}: {meaning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn bare_verb_is_the_index() {
        assert!(parse_agent_context_args(&args(&[])).unwrap().is_none());
    }

    #[test]
    fn a_compiled_in_command_word_resolves_to_its_spec() {
        let spec = parse_agent_context_args(&args(&["query"]))
            .unwrap()
            .unwrap();
        assert_eq!(spec.word, "query");
        let spec = parse_agent_context_args(&args(&["clone"]))
            .unwrap()
            .unwrap();
        assert_eq!(spec.word, "clone");
    }

    #[test]
    fn update_resolves_to_upgrade_when_compiled_in() {
        #[cfg(feature = "upgrade")]
        assert_eq!(
            parse_agent_context_args(&args(&["update"]))
                .unwrap()
                .unwrap()
                .word,
            "upgrade"
        );
        #[cfg(not(feature = "upgrade"))]
        assert!(parse_agent_context_args(&args(&["update"])).is_err());
    }

    #[test]
    fn unknown_words_and_extras_are_usage_errors() {
        assert!(parse_agent_context_args(&args(&["bogus"])).is_err());
        assert!(parse_agent_context_args(&args(&["query", "clone"])).is_err());
    }

    #[test]
    fn exit_codes_cover_every_produced_code() {
        let codes: Vec<i32> = EXIT_CODES.iter().map(|(exit, ..)| *exit).collect();
        assert_eq!(codes, [0, 1, 2, 5, 6, 7]);
    }
}
