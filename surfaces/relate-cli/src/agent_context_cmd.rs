//! `relate agent-context [COMMAND]` (ADR-0242, LABS-236): the compact
//! discovery readout for agent harnesses. Everything prints from the
//! `help::COMMANDS` descriptor table that also feeds `--help`, plus the one
//! [`EXIT_CODES`] table (in `errors`, beside the classes that produce the
//! codes), so the readout cannot drift from the parser.

use crate::agent_mode;
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

/// Run the parsed command and return the process exit code: 0 for the index
/// and a run's own detail page, and the absent-surface refusal's 2 when the
/// word names the other mode's feedback surface (the same refusal the
/// command dispatch gives). `surface` is the run's feedback surface: the
/// index lists the commands this run's mode owns, the same rule the
/// top-level help reads.
pub fn run(command: Option<&'static CommandSpec>, surface: agent_mode::FeedbackSurface) -> i32 {
    match command {
        None => {
            print_index(surface);
            0
        }
        // The run's own commands print their detail; the other mode's
        // feedback surface is the same absent-surface refusal the command
        // dispatch gives (the index never listed it either).
        Some(spec) if spec.visible_to(surface) => {
            print_detail(spec);
            0
        }
        Some(spec) => crate::lifecycle::refuse_absent_surface(spec.word, surface),
    }
}

/// The no-argument index: version, one line per compiled-in command, the
/// query-run flag names, the exit-code table, and the feedback block.
fn print_index(surface: agent_mode::FeedbackSurface) {
    println!("relate v{} — agent context", crate::VERSION);
    println!();
    println!("Commands (detail: relate agent-context <COMMAND>):");
    // The one visibility rule, shared with the top-level help: this run's
    // mode lists only the commands it owns (ADR-0588 §3).
    let listed = help::commands_for(surface);
    let width = help::command_column_width(&listed);
    for spec in listed {
        println!("{}", help::command_row(spec, width));
    }
    println!();
    println!("Query-run flags (relate [OPTIONS] [QUERY]):");
    println!("  {}", help::session_flags().join(" "));
    println!();
    print_exit_codes();
    // The Feedback block advertises the agent channel, so it rides the same
    // mode gate the command rows above and the top-level help apply: a
    // person's run neither lists nor can call `agent-feedback`.
    if surface == agent_mode::FeedbackSurface::Agent {
        println!();
        print_feedback();
    }
}

/// The feedback block (ADR-0588 §3, revised): the agent channel's synopsis
/// and its one instruction line — report first, the person's approval
/// before any send. Additive after the readout's earlier sections, so a
/// harness that parses by section heading is unaffected.
fn print_feedback() {
    println!("Feedback:");
    // The canonical synopsis, straight off the descriptor table — the same
    // `usage` the help page and usage errors print, so a new flag cannot
    // reach one and miss this readout.
    let usage = &help::find_command("agent-feedback")
        .expect("agent-feedback is compiled into every bundle")
        .usage;
    println!("  {usage}");
    println!(
        "  {} — without the person's grant it answers not_enabled (the person can \
         run '{}').",
        crate::agent_feedback_cmd::REPORT_RULE,
        crate::feedback_cmd::enable_invocation()
    );
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
