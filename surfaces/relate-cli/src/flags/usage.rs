//! The usage-error presentation (ADR-0069's exit-2 convention, ADR-0192
//! flag suggestions, ADR-0518 command suggestions): the one suggestion
//! slot, the one render body every scope prints through, and the
//! assemblies the parse loop and the command grammars share. Extracted
//! from `flags.rs` (the parser) so the presentation rules have one home
//! and both files stay under the repo's 1k-line budget.

use std::borrow::Cow;

use crate::help::{CommandSpec, HelpScope, HelpTopic};
use crate::parse_error::ParseError;
use crate::suggest;

/// The one suggestion a usage error can carry: flag text (ADR-0192) or a
/// mistyped command's spec (the ADR-0230 line, carried on usage exits by
/// ADR-0518). Both arms are data — the renderer formats them — so each
/// arm's origin is the type's guarantee: `Flag` is candidate-list text
/// (borrowed from the static flag table for the static scopes, owned
/// when the caller's candidate list is the `run` mount's manifest data —
/// hence the `Cow`, not `&'static str`), `Command` is a `&'static
/// CommandSpec` from the `COMMANDS` table `--help` renders.
/// `render_usage_error`'s `print_error` is the one sanitizer for either
/// arm. One slot, not two parameters: when the pending statement's first
/// word is a mistyped command, the command suggestion is the only one
/// that can be right — the flag candidate list belongs to the exit's
/// usage scope, the wrong one for the command the user meant (`--bolt`
/// is a `serve` flag, not an unknown-spelling query flag).
pub(crate) enum UsageSuggestion {
    Flag(Cow<'static, str>),
    Command(&'static CommandSpec),
}

impl UsageSuggestion {
    /// The flag-suggestion assembly every command grammar shares
    /// (ADR-0192): the offending token of `err` matched against `known`
    /// — the caller scope's own candidate list, static or manifest —
    /// with no alias spellings (a subcommand grammar accepts none of the
    /// top-level aliases; the query run's `FLAG_ALIASES` apply only at
    /// [`unknown_flag_suggestion`]'s top-level exit). The variant carries
    /// the candidate text itself, owned when the list is manifest data.
    pub(crate) fn from_parse_error(err: &ParseError, known: &[&str]) -> Option<Self> {
        err.suggestion(known, &[])
            .map(|flag| Self::Flag(flag.to_owned().into()))
    }
}

/// The pending statement's ADR-0230 command suggestion for a usage-error
/// exit (ADR-0518): `None` when no statement is pending or its first
/// word is not a plausible command typo — the same matcher and the same
/// rendering as the engine-error fallback, which the usage error's
/// exit(2) would otherwise cut off (the statement never reaches the
/// engine).
pub(super) fn statement_usage_suggestion(query: Option<&str>) -> Option<UsageSuggestion> {
    suggest::suggest_command(query?).map(UsageSuggestion::Command)
}

/// The unknown-flag exit's suggestion (ADR-0518 §3): the pending
/// statement's command suggestion first, then the flag spelling
/// (ADR-0192). The command wins for the reason [`UsageSuggestion`]'s doc
/// states — the flag candidate list is this scope's, the wrong one for
/// the command the user meant.
pub(super) fn unknown_flag_suggestion(
    scope: HelpScope,
    arg: &str,
    query: Option<&str>,
) -> Option<UsageSuggestion> {
    statement_usage_suggestion(query).or_else(|| {
        suggest::suggest_flag(arg, scope.known_flags(), crate::help::FLAG_ALIASES)
            .map(|flag| UsageSuggestion::Flag(flag.into()))
    })
}

/// Print `Error: <msg>`, the suggestion line, the usage synopsis, and
/// the help hint to stderr, then exit(2) — the hand-rolled parser's
/// usage-error convention, printed once for every scope, static or
/// manifest. `suggestion` adds a did-you-mean line after the error: a
/// flag spelling (ADR-0192) or, for a pending statement that looks like
/// a mistyped command, the ADR-0230 command line — blank line first,
/// exactly as the engine fallback renders it, so the two copies cannot
/// drift. Every interpolated line goes through the sanitized `write_error`
/// sink — the message and suggestion by this body, the usage/hint pair by
/// contract (static synopses for the `HelpScope` entry, already-sanitized
/// manifest text for the `run` mount's entry) — so no caller can bypass
/// the sink by construction. The body deliberately does not call
/// `print_error`: that sink would spend the agent-feedback hint's
/// once-per-run latch per line, so the one hint emission happens at the
/// tail with this path's own `usage_error` row.
///
/// `pub(crate)` as the manifest-scope entry: the `run` mount passes its
/// action's own usage and hint (ADR-0430 §5) instead of a `HelpScope`
/// lookup.
pub(crate) fn render_usage_error(
    msg: &str,
    usage: &str,
    hint: &str,
    suggestion: Option<UsageSuggestion>,
) -> ! {
    // Every line of the one usage diagnostic routes through the sanitized
    // writer — one stderr binding, like every sibling report path — and the
    // latch is spent exactly once, at the hint emission below (a
    // `print_error` here would spend it with no code and the prefill could
    // never carry the usage_error row).
    let mut diagnostics = std::io::stderr();
    crate::write_error(&mut diagnostics, format!("Error: {msg}"));
    match suggestion {
        Some(UsageSuggestion::Flag(flag)) => {
            crate::write_error(&mut diagnostics, format!("Did you mean '{flag}'?"));
        }
        Some(UsageSuggestion::Command(spec)) => {
            // The blank separator is a CLI-generated report line, not a
            // diagnostic: the sibling report path routes exactly this
            // separator through the report writer.
            crate::write_report(&mut diagnostics, "");
            crate::write_error(&mut diagnostics, suggest::command_line(spec));
        }
        None => {}
    }
    // The synopsis and hint go through the sink one line at a time, not
    // as a block: a whole-block `print_error` flattens a multi-line
    // synopsis (`extension`'s per-action usage block) — the sanitizer
    // rewrites each newline to a space — while per-line printing keeps
    // the block's shape and still routes every line, static or manifest,
    // through the sanitizer.
    for line in usage.lines().chain(hint.lines()) {
        crate::write_error(&mut diagnostics, line);
    }
    // The usage rejection's own classified code rides the one hint
    // emission — the exit table's usage_error row — so the granted hint
    // prefills the row this path exists for (the generic `print_error`
    // sink holds no code).
    crate::agent_feedback_cmd::write_hint(&mut diagnostics, Some(crate::errors::USAGE_ERROR_CODE));
    std::process::exit(2);
}

/// [`render_usage_error`] for a static help scope: the top-level
/// query-run form or a compiled-in command's page — the shared
/// static-scope entry (LABS-552), this module's own consumers (the
/// parse loop and the lifecycle grammars).
pub(super) fn usage_error(scope: HelpScope, msg: &str, suggestion: Option<UsageSuggestion>) -> ! {
    render_usage_error(msg, scope.usage(), &scope.hint(), suggestion)
}

/// The command-scope `ParseError` → usage-error assembly every command
/// grammar shares: the command's own flag candidates — read through the
/// one scope accessor, like every other suggestion assembly — and no
/// top-level alias spellings (`--output` → `--format` is the query
/// run's; subcommand grammars accept none of them).
pub(crate) fn command_usage_error(topic: HelpTopic, err: &ParseError) -> ! {
    let scope = HelpScope::Command(topic);
    usage_error(
        scope,
        &err.to_string(),
        UsageSuggestion::from_parse_error(err, scope.known_flags()),
    )
}
