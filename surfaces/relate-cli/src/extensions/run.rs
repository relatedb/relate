//! The `run` verb mount (ADR-0429 §1, LABS-552): the reserved word, the
//! dispatch, the `run --help` page, the pinned discovery error, and the
//! telemetry wiring — the production consumer the extension tree's
//! slices (the wire, the registry, the host) landed ahead of.
//!
//! Every `run` invocation resolves the machine's installed extensions
//! FIRST (ADR-0429 §6 — the XDG walk, never the CWD), and a registry load
//! refusal — a malformed manifest, a name collision, an escaping
//! executable — fails the verb closed (exit 2, the refusal, the run
//! synopsis and hint, ADR-0430 §5's manifest class): the grammar `run`
//! accepts is a property of the user's machine, so a broken install
//! breaks the verb — the name-shaped help pages included — never a partial
//! grammar or a partial listing. (A help token with no name to resolve —
//! a bare `run --help`, a flag-shaped tail — prints the verb's own page
//! without loading the registry: there is no extension to resolve.)
//!
//! Only an installed extension mounts under `run` (ADR-0429 §1): the
//! built-in `lab` actions are gone with the `lab` surface (ADR-0431
//! §11–§12 — the importers are installed packages now), so every
//! unresolvable name, whatever it used to be, fails closed with the
//! pinned discovery text (ADR-0429 §7), which names the first-party
//! install path and never fetches.

use relate_formatter::sanitize_error_text;

use crate::extensions::host::{
    action_help, action_usage, flag_candidates, parse_action_args, ExtensionRun,
};
use crate::extensions::registry::{ActionRegistry, ExtensionEntry};
use crate::help::HelpTopic;
use crate::parse_error::ParseError;
use crate::telemetry::Telemetry;

/// A parsed `run <name> [flags]` invocation (the verb's command
/// payload): the resolved installed action and its host-parsed argv.
/// The entry is carried owned — the registry it resolved through is a
/// parse-time value — so the dispatch runs the action it resolved,
/// never a re-lookup through fresh machine state.
#[derive(Debug, PartialEq)]
pub(crate) struct RunCmd {
    /// The resolved installed action.
    pub(crate) entry: ExtensionEntry,
    /// The action's parsed invocation — the host's own parse of the
    /// manifest's flag table (ADR-0429 §5).
    pub(crate) invocation: ExtensionRun,
}

/// What a `run` invocation's parse decided (LABS-552): a run to
/// execute, or one of the two help pages a pre-scanned `-h`/`--help`
/// prints.
#[derive(Debug, PartialEq)]
pub(crate) enum ParsedRun {
    /// `run <name> [flags]`: the resolved action and its parsed argv.
    /// Boxed: the command's structs (the registry entry, the manifest's
    /// parsed argv) carry `serde_json::Value`s, and under
    /// `preserve_order` (ADR-0634) the map's index-table layout grows
    /// them past the `large_enum_variant` threshold against the two
    /// help arms.
    Run(Box<RunCmd>),
    /// `run <name> --help` where the name resolved: the action's own
    /// page, rendered at parse from the same registry load that
    /// resolved the name (a page of manifest data, which no static
    /// `HelpTopic` carries).
    ActionHelp(String),
    /// A help token with no resolved name — a bare `run --help`, a
    /// flag-shaped tail, or a name that does not resolve: help wins and
    /// nothing after the word is validated (the `help_or` rule), so the
    /// verb's own page prints through the ordinary `Help` topic.
    VerbHelp,
}

/// A refused `run` parse (ADR-0430 §5's exit-2 classes at the verb): the
/// refusal plus the scope its synopsis and did-you-mean candidates
/// render in — the typed discrimination the shared static renderer
/// cannot carry, because the action's scope is manifest data.
#[derive(Debug)]
pub(crate) enum RunUsageError {
    /// A refusal at the verb's scope: the static `run` synopsis, hint,
    /// and spec-flag did-you-mean render — the shared usage-error path.
    Verb(ParseError),
    /// A refusal inside the resolved action's grammar: the action's
    /// OWN synopsis and did-you-mean candidates — manifest data,
    /// sanitized where placed — render beside the refusal. Boxed: a
    /// cold path (the renderer exits), kept off the hot `Result`.
    Action(Box<ActionRefusal>),
}

/// The resolved action's own refusal data (the
/// [`RunUsageError::Action`] arm's payload): the parse refusal plus
/// the action-scope rendering data, all manifest data, sanitized where
/// composed.
#[derive(Debug)]
pub(crate) struct ActionRefusal {
    /// The action's parse refusal (its scope is `run <name>`).
    pub(crate) error: ParseError,
    /// The action's usage line (manifest data, sanitized at
    /// composition by `action_usage`).
    pub(crate) usage: String,
    /// The action-scope help hint (the manifest's mount name,
    /// sanitized).
    pub(crate) hint: String,
    /// The did-you-mean candidates — the same flag list the action's
    /// grammar accepts (`flag_candidates`).
    pub(crate) candidates: Vec<String>,
}

/// Parse the argument tail after a first-positional `run` verb
/// (ADR-0429 §1): resolve the machine's installed extensions, then
/// dispatch the name through the two-sourced registry — only an
/// installed extension mounts under `run`. The `-h`/`--help` pre-scan
/// runs FIRST, like every compiled-in command's tail (`help_or`'s
/// rule): a help token wins before the action's grammar is even seen —
/// `parse_action_args` rejects help tokens like any unknown flag, its
/// doc says so.
///
/// The real-env entry point (the `parse_lab_args` pattern): the
/// registry resolves through the shared XDG rule, and a load refusal
/// fails the verb closed through the usage-error path — never a
/// partial grammar.
pub(crate) fn parse_run_args(rest: &[String]) -> Result<ParsedRun, RunUsageError> {
    if crate::flags::wants_help(rest) {
        // A name candidate is a word-shaped first token; anything else
        // — a bare tail, a flag — has no name to resolve, and a help
        // token with no resolvable name prints the verb's own page.
        if let Some(name) = rest.first().filter(|name| !name.starts_with('-')) {
            let registry = load_registry_or_exit();
            return Ok(help_page(name, &registry));
        }
        return Ok(ParsedRun::VerbHelp);
    }
    let registry = load_registry_or_exit();
    parse_run_word(rest, &registry).map(|cmd| ParsedRun::Run(Box::new(cmd)))
}

/// The dispatch core of [`parse_run_args`] (the `parse_lab_word`
/// pattern), over a given registry so the decision is unit-testable
/// against fixtures without a real extensions dir: the name resolves
/// through the one lookup, the resolved extension's grammar parses
/// through the host, and everything else is a fail-closed refusal.
fn parse_run_word(rest: &[String], registry: &ActionRegistry) -> Result<RunCmd, RunUsageError> {
    let Some(name) = rest.first() else {
        return Err(RunUsageError::Verb(missing_name(registry)));
    };
    if name.starts_with('-') {
        // A flag where the name belongs: the verb's own grammar takes
        // no flag before the action name.
        return Err(RunUsageError::Verb(ParseError::unexpected("run", name)));
    }
    let tail = &rest[1..];
    match registry.find(name) {
        Some(entry) => match parse_action_args(entry, tail) {
            Ok(invocation) => Ok(RunCmd {
                entry: entry.clone(),
                invocation,
            }),
            Err(error) => Err(RunUsageError::Action(Box::new(ActionRefusal {
                error,
                usage: action_usage(entry),
                hint: action_hint(entry),
                candidates: flag_candidates(entry),
            }))),
        },
        // An unresolvable name fails closed with the pinned discovery
        // text (ADR-0429 §7) — never a fetch, never a suggestion.
        None => Err(RunUsageError::Verb(discovery(name))),
    }
}

/// The help branch's decision, with the registry the name's resolution
/// needed: a name that resolves to an installed action prints THAT
/// action's page; anything else prints the verb's own — help wins, so
/// an unresolvable name is never validated against the discovery error
/// a help exit would make unreachable.
fn help_page(name: &str, registry: &ActionRegistry) -> ParsedRun {
    match registry.find(name) {
        Some(entry) => ParsedRun::ActionHelp(action_help(entry)),
        None => ParsedRun::VerbHelp,
    }
}

/// The registry load every `run` invocation resolves through (the
/// module's one rule): a load refusal fails the verb closed through
/// the usage-error path — the refusal, the run synopsis and hint —
/// never a partial grammar or page.
pub(crate) fn load_registry_or_exit() -> ActionRegistry {
    match ActionRegistry::load() {
        Ok(registry) => registry,
        Err(e) => usage_error(RunUsageError::Verb(e.into())),
    }
}

/// The verb's listing of installed actions (ADR-0429 §1: the dynamic
/// installed-actions section): the registry's extension rows minus the
/// hidden ones — a hidden extension still parses and runs but appears
/// in no listing (ADR-0429 §2). The one listing policy the verb's help
/// page and its missing-name error both read.
pub(crate) fn installed_actions(
    registry: &ActionRegistry,
) -> impl Iterator<Item = &ExtensionEntry> {
    registry
        .extension_actions()
        .filter(|entry| !entry.command().hidden)
}

/// The bare-`run` refusal (ADR-0429 §1: the dynamic installed-actions
/// section lives on the run usage error): the missing-name message with
/// the installed listing — or, in the shipped fresh-install state, the
/// zero-installed wording naming the discovery path.
fn missing_name(registry: &ActionRegistry) -> ParseError {
    let installed: Vec<&str> = installed_actions(registry)
        .map(|entry| entry.mount_name())
        .collect();
    if installed.is_empty() {
        "run requires an action name (no actions installed; \
          relate extension install <name> adds one)"
            .into()
    } else {
        format!(
            "run requires an action name (installed: {})",
            installed.join(", ")
        )
        .into()
    }
}

/// The uninstalled-action discovery error — ADR-0429 §7's pinned text
/// (LABS-552 §4): the fail-closed refusal every unresolvable name gets,
/// naming the first-party install path. Never a fetch, never a
/// suggestion: discovery lists what is installed, it never reaches for
/// anything.
fn discovery(name: &str) -> ParseError {
    format!(
        "no installed action '{name}' — first-party extension available: \
          relate extension install {name}"
    )
    .into()
}

/// The action-scope help hint — the per-command hint convention
/// (`Run 'relate <command> --help' for more information.`) with the
/// manifest's mount name, sanitized: it is package data, not static
/// command text.
fn action_hint(entry: &ExtensionEntry) -> String {
    format!(
        "Run 'relate run {} --help' for more information.",
        sanitize_error_text(entry.mount_name())
    )
}

/// The run verb's usage-error renderer (ADR-0430 §5's exit-2 shape):
/// The run mount's usage-error dispatch, both arms through the shared
/// renderer in [`crate::flags`] (the one exit-2 shape): the verb scope is
/// the static command path — the run spec's synopsis, hint, and spec-flag
/// did-you-mean, the same assembly every command grammar uses
/// (`command_usage_error`). The action scope renders the action's OWN
/// synopsis and suggestion beside the refusal; both are manifest data,
/// and the renderer owns their sanitization (`render_usage_error` prints
/// every line through `print_error`) — this arm only supplies the text:
/// the usage/hint were composed sanitized per fragment by
/// `action_usage`'s own rule, which also serves the help page's raw
/// print, and the flag suggestion comes from the same shared assembly
/// every grammar uses (`UsageSuggestion::from_parse_error` over the
/// manifest's candidate list).
pub(crate) fn usage_error(err: RunUsageError) -> ! {
    match err {
        RunUsageError::Verb(err) => crate::flags::command_usage_error(HelpTopic::Run, &err),
        RunUsageError::Action(refusal) => {
            let ActionRefusal {
                error,
                usage,
                hint,
                candidates,
            } = *refusal;
            let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
            let suggestion = crate::flags::UsageSuggestion::from_parse_error(&error, &candidates);
            crate::flags::render_usage_error(&error.to_string(), &usage, &hint, suggestion)
        }
    }
}

/// Run the parsed `run` command: dispatch into the host's
/// `run_import`-shaped lifecycle (ADR-0430 §7) — the host-owned store,
/// the wire batches, the summary, the interactive handoff, the final
/// checkpoint. The caller (`main`'s lifecycle dispatch) reports the
/// COMMAND telemetry: command `run`, action `extension` — never the
/// installed name (ADR-0429 §9's anonymity rule).
pub(crate) fn run(cmd: RunCmd, telemetry: &mut Telemetry) -> i32 {
    crate::extensions::host::run_action(&cmd.entry, cmd.invocation, telemetry)
}

// The mount's tests live in `tests.rs` (the dir-module pattern the
// extension tree's own modules use, LABS-421) so the parse decisions,
// the discovery error, and the listing policy stay scannable in this
// file, beside their tests, under the repo's 1k-line budget.
#[cfg(test)]
mod tests;
