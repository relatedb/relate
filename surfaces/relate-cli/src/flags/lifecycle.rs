//! Lifecycle-verb parsing (ADR-0069). `query` stays in the session parser;
//! every other first-positional command word is parsed here.

use crate::help::HelpTopic;
// The direct `usage_error` sites in this module are the pull/push
// `--namespace` refusals; every other exit goes through
// `command_usage_error`, which needs no `HelpScope` of its own.
#[cfg(any(feature = "pull", feature = "push"))]
use crate::help::HelpScope;
use crate::parse_error::ParseError;

use super::command_usage_error;
#[cfg(any(feature = "pull", feature = "push"))]
use super::usage_error;

/// Every command word (ADR-0069, ADR-0200). The set is the same in every
/// build (ADR-0104): a word whose feature is compiled out is still a
/// command, so the same argv never silently opens a database of that name.
/// `query` is a statement-session verb in every bundle; the rest are
/// exit-after lifecycle verbs. `run` (ADR-0429 §1) is the one reserved
/// word of the extension program's first stratum — an installed action
/// name is never a reserved word, so the top-level grammar stays closed
/// across installs. Read crate-wide through the re-export in
/// `flags.rs`: the extension registry's collision refusal (ADR-0429 §6)
/// checks an installed name against this vocabulary.
pub(crate) const COMMAND_WORDS: &[&str] = &[
    "query",
    "clone",
    "skill",
    "upgrade",
    "update",
    "telemetry",
    "feedback",
    "agent-feedback",
    "pull",
    "push",
    "run",
    "info",
    "serve",
    "agent-context",
    "extension",
];

/// The word `name` spells when it is a reserved command word, `None`
/// otherwise — the one lookup behind every collision refusal of the
/// extension program: the registry's load-time check, the install flow's
/// pre-flight, and `extension init`'s name rule (ADR-0429 §6). Beside the
/// vocabulary it reads, so the accepted values and the refusals cannot
/// drift apart.
#[cfg(feature = "extensions")]
pub(crate) fn reserved_word(name: &str) -> Option<&'static str> {
    COMMAND_WORDS.iter().copied().find(|word| *word == name)
}

/// A parsed positional lifecycle command (ADR-0069). Lifecycle verbs are
/// recognized only as the first positional argument to the CLI.
#[derive(Debug)]
pub(crate) enum LifecycleCommand {
    /// `relate skill install|remove [--agent NAME] [--force]`
    #[cfg(feature = "skill")]
    Skill { args: crate::skill_cmd::SkillArgs },
    /// `relate upgrade [--yes] [--check]` (`update` is an alias)
    #[cfg(feature = "upgrade")]
    Upgrade {
        opts: crate::upgrade_cmd::UpgradeOpts,
    },
    /// `relate telemetry on|off|status` (ADR-0079 §6)
    #[cfg(feature = "telemetry")]
    Telemetry {
        action: crate::telemetry_cmd::TelemetryAction,
    },
    /// `relate feedback [TEXT | enable | disable]` (ADR-0588 §3): the
    /// person's channel — a prefilled GitHub issue and the agent grant. The
    /// grammar runs at dispatch, after the mode gate: the surface's absence
    /// in agent mode must answer as an unknown command, never as this
    /// grammar's usage error.
    Feedback { args: Vec<String> },
    /// `relate agent-feedback [flags]` (ADR-0588 §3): the agent's channel —
    /// one consented report under the person's standing grant. Present only
    /// in agent mode; `main` gates it, then parses the flags.
    AgentFeedback { args: Vec<String> },
    /// `relate pull [--from FILE] --query Q [--param N=V] --database PATH` (ADR-0080,
    /// ADR-0158: `Q` may be a statement list, file, or `-`)
    #[cfg(feature = "pull")]
    Pull { cmd: crate::pull_cmd::PullCmd },
    /// `relate push --to CREDS --database PATH [scope] [policies]`
    /// (ADR-0401): pull's mirror, constraint-keyed MERGE, batched UNWIND.
    #[cfg(feature = "push")]
    Push { cmd: crate::push_cmd::PushCmd },
    /// `relate clone [--wal-dir DIR] [--to-snapshot-format F] SOURCE DEST`
    /// (ADR-0156, ADR-0282, ADR-0163). Built into every CLI bundle.
    Clone { cmd: crate::clone_cmd::CloneCmd },
    /// `relate run <name> [flags]` (ADR-0429 §1, LABS-552): dispatch
    /// an installed extension action through the host (ADR-0430 §7).
    #[cfg(feature = "extensions")]
    Run { cmd: crate::extensions::run::RunCmd },
    /// `relate info --database PATH [--json]` (ADR-0165, ADR-0200): cold store-metadata
    /// read.
    #[cfg(feature = "info")]
    Info { cmd: crate::info_cmd::InfoCmd },
    /// `relate serve --database PATH [--http PORT] [--mcp PORT|stdio] [--bolt PORT]`
    /// (ADR-0232): host the network surfaces until SIGINT/SIGTERM.
    #[cfg(feature = "serve")]
    Serve { cmd: crate::serve_args::ServeCmd },
    /// `relate agent-context [COMMAND]` (ADR-0242): the compact
    /// discovery readout; `None` prints the index.
    #[cfg(feature = "agent-context")]
    AgentContext {
        command: Option<&'static crate::help::CommandSpec>,
    },
    /// `relate extension install|list|remove` (ADR-0431): the
    /// extension lifecycle verbs.
    #[cfg(feature = "extensions")]
    Extension {
        action: crate::extension_cmd::ExtensionAction,
    },
    /// `-h`/`--help` after a compiled-in command word: print that command's
    /// help page to stdout and exit 0. Nothing after the word is parsed or
    /// validated, and nothing runs.
    Help { topic: HelpTopic },
    /// A pre-scanned `run <name> --help` page: the resolved action's own
    /// help (manifest data, rendered at parse — no static topic carries
    /// it), printed through the same help-exit path as every command's
    /// page: the two help events, no COMMAND event, nothing runs. The
    /// verb's own `--help` rides the ordinary [`Self::Help`] topic.
    #[cfg(feature = "extensions")]
    RunHelp { page: String },
    /// A command word whose feature is compiled out of this build (ADR-0104).
    /// The word is the static [`COMMAND_WORDS`] member the user typed.
    NotIncluded { command: &'static str },
}

impl LifecycleCommand {
    /// Whether this command is help-shaped — the `command_topic` `None`
    /// set (`Help`, `NotIncluded`, `RunHelp`): a page to print, never a
    /// command with a query of its own. The end-of-parse conflict stage
    /// exempts these exactly as it exempts `--help`/`--version` (round 4):
    /// a help page is a no-side-effect exit, so no conflict can precede it.
    pub(crate) fn is_help_shaped(&self) -> bool {
        self.command_topic().is_none()
    }

    /// The topic of the parsed command itself, or `None` for the
    /// help-shaped commands — `Help` (whose topic names the page to
    /// print, not a command to run), `NotIncluded` (whose page is
    /// compiled out with the feature), and `RunHelp` (a pre-scanned
    /// action page, not a command of its own). The variant→topic
    /// association lives here exactly once; every other command
    /// metadata read goes through the topic's [`crate::help`]
    /// descriptor. The help-shaped arms are ungated — their variants
    /// exist in every build — so only the arms of feature-gated
    /// variants carry a gate.
    fn command_topic(&self) -> Option<HelpTopic> {
        match self {
            Self::Help { .. } | Self::NotIncluded { .. } => None,
            #[cfg(feature = "skill")]
            Self::Skill { .. } => Some(HelpTopic::Skill),
            #[cfg(feature = "upgrade")]
            Self::Upgrade { .. } => Some(HelpTopic::Upgrade),
            #[cfg(feature = "telemetry")]
            Self::Telemetry { .. } => Some(HelpTopic::Telemetry),
            Self::Feedback { .. } => Some(HelpTopic::Feedback),
            Self::AgentFeedback { .. } => Some(HelpTopic::AgentFeedback),
            #[cfg(feature = "pull")]
            Self::Pull { .. } => Some(HelpTopic::Pull),
            #[cfg(feature = "push")]
            Self::Push { .. } => Some(HelpTopic::Push),
            Self::Clone { .. } => Some(HelpTopic::Clone),
            #[cfg(feature = "extensions")]
            Self::Run { .. } => Some(HelpTopic::Run),
            #[cfg(feature = "info")]
            Self::Info { .. } => Some(HelpTopic::Info),
            #[cfg(feature = "serve")]
            Self::Serve { .. } => Some(HelpTopic::Serve),
            #[cfg(feature = "agent-context")]
            Self::AgentContext { .. } => Some(HelpTopic::AgentContext),
            #[cfg(feature = "extensions")]
            Self::Extension { .. } => Some(HelpTopic::Extension),
            #[cfg(feature = "extensions")]
            Self::RunHelp { .. } => None,
        }
    }

    /// The command word as diagnostics and telemetry report it (`update`
    /// reports "upgrade", ADR-0079 §4). Static because
    /// [`crate::telemetry::Event::Command`] stores `&'static str`.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Help { topic } => topic.word(),
            // The action's page is manifest data, so the word is spelled
            // here once — the static reserved word the verb owns.
            #[cfg(feature = "extensions")]
            Self::RunHelp { .. } => "run",
            Self::NotIncluded { command } => command,
            _ => self
                .command_topic()
                .expect("a parsed command has a topic")
                .word(),
        }
    }

    /// The parse-tail conflict decision (ADR-0306 §6): the one answer for
    /// a query or a pre-verb access flag (`--read-only`, ADR-0480's one
    /// spelling) riding a lifecycle command line, where the command,
    /// query, and access slot are all final. The canonical policy
    /// statement is the `CommandSpec` descriptor row (`post_verb_access`,
    /// `rejects_query`); this decision only reads it: a `post_verb_access`
    /// row answers a pre-verb sighting with the command's own message
    /// naming the post-verb spelling (from parse_error's one `ACCESS_FLAG`
    /// fact — no message here hand-types it), a `rejects_query` row
    /// answers a query or any access flag with the shared message, and
    /// both riders are only meaningful to a query run — a command like
    /// `clone`, which writes, must not silently drop the user's read-only
    /// fence. `access` is the sighting slot itself (`None` = no flag), so
    /// no caller resolves a default. `None` for a clean combination, a
    /// command that runs the query engine itself, and `Help`, which wins
    /// before the check. Pure on its inputs so the decision is
    /// unit-tested at this level; the end-to-end exit-2 pins are the
    /// e2e suite's.
    pub(crate) fn conflict_error(
        &self,
        query: Option<&str>,
        access: Option<relate::OpenMode>,
    ) -> Option<(HelpTopic, String)> {
        let topic = self.command_topic()?;
        let spec = topic.spec();
        // The one rider predicate: a pre-verb access sighting. The slot's
        // only writer parks a read-only freshness, so supplied-ness is the
        // whole fact here — the mode belongs to the open-time resolution
        // (`parse_error::resolve_access`), not to this decision.
        let pre_verb_sighting = access.is_some();
        let flag = crate::parse_error::ACCESS_FLAG;
        // The post-verb half first — a plain early return, so the
        // precedence is structural, not call order.
        if spec.post_verb_access && pre_verb_sighting {
            return Some((
                topic,
                format!(
                    "{} takes {flag} after the verb, not before it (relate {} {flag} …)",
                    spec.word, spec.word
                ),
            ));
        }
        if spec.rejects_query && (query.is_some() || pre_verb_sighting) {
            return Some((
                topic,
                format!(
                    "{} cannot be combined with a query or an access flag ({flag})",
                    spec.word
                ),
            ));
        }
        None
    }
}

/// Parse a lifecycle command: `word` is the static [`COMMAND_WORDS`] member
/// the caller matched at the parse site, `rest` the arguments after it. A
/// word whose feature is not compiled in becomes
/// [`LifecycleCommand::NotIncluded`] without looking at its arguments.
pub(super) fn parse_lifecycle_command(
    word: &'static str,
    rest: &[String],
    namespace: Option<relate::Namespace>,
) -> LifecycleCommand {
    // Only the pull and push arms consume `namespace`; discard it
    // explicitly in builds without either feature so the signature reads
    // the same in every bundle.
    #[cfg(not(any(feature = "pull", feature = "push")))]
    let _ = namespace;
    // Resolve command-word aliases (`update` → `upgrade`, ADR-0069) through
    // the one alias table in help.rs, so the fact lives in one place. Done
    // only when the alias's target is compiled in: without `upgrade`, the
    // typed word reaches `NotIncluded` unchanged.
    #[cfg(feature = "upgrade")]
    let word = crate::help::canonical_word(word);
    match word {
        #[cfg(feature = "skill")]
        "skill" => help_or(HelpTopic::Skill, rest, || parse_skill_command(rest)),
        #[cfg(feature = "upgrade")]
        "upgrade" => help_or(HelpTopic::Upgrade, rest, || parse_upgrade_command(rest)),
        #[cfg(feature = "telemetry")]
        "telemetry" => help_or(HelpTopic::Telemetry, rest, || parse_telemetry_command(rest)),
        "feedback" => help_or(HelpTopic::Feedback, rest, || LifecycleCommand::Feedback {
            args: rest.to_vec(),
        }),
        "agent-feedback" => help_or(HelpTopic::AgentFeedback, rest, || {
            LifecycleCommand::AgentFeedback {
                args: rest.to_vec(),
            }
        }),
        #[cfg(feature = "pull")]
        "pull" => help_or(HelpTopic::Pull, rest, || {
            parse_pull_command(rest, namespace)
        }),
        #[cfg(feature = "push")]
        "push" => help_or(HelpTopic::Push, rest, || {
            parse_push_command(rest, namespace)
        }),
        "clone" => help_or(HelpTopic::Clone, rest, || parse_clone_command(rest)),
        #[cfg(feature = "extensions")]
        "run" => parse_run_command(rest),
        #[cfg(feature = "info")]
        "info" => help_or(HelpTopic::Info, rest, || parse_info_command(rest)),
        #[cfg(feature = "serve")]
        "serve" => help_or(HelpTopic::Serve, rest, || parse_serve_command(rest)),
        #[cfg(feature = "agent-context")]
        "agent-context" => help_or(HelpTopic::AgentContext, rest, || {
            parse_agent_context_command(rest)
        }),
        #[cfg(feature = "extensions")]
        "extension" => help_or(HelpTopic::Extension, rest, || {
            parse_args_or_usage_error(
                HelpTopic::Extension,
                rest,
                crate::extension_cmd::parse_extension_args,
                |action| LifecycleCommand::Extension { action },
            )
        }),
        // The caller matched `word` against COMMAND_WORDS, so a verb whose
        // feature is compiled out lands here with its static word — and its
        // `-h`/`--help` is ignored: a compiled-out command keeps the
        // not-included error (ADR-0104).
        word => LifecycleCommand::NotIncluded { command: word },
    }
}

/// `-h`/`--help` anywhere in `rest`. A flag *value* that is literally
/// `--help` reads as help too — the accepted trade-off of a pre-scan
/// without per-command grammar.
///
/// `pub(crate)`: this is the one help-pre-scan policy (`help_or`'s rule);
/// the `run` mount's dynamic help decision reads it through the
/// re-export in `flags.rs`, the way the registry reads `COMMAND_WORDS`.
pub(crate) fn wants_help(rest: &[String]) -> bool {
    rest.iter().any(|arg| arg == "-h" || arg == "--help")
}

/// `-h`/`--help` anywhere in a compiled-in command's argument tail wins:
/// the command prints its help page and executes nothing. The pre-scan runs
/// before the sub-parser, so an unknown flag before the help token never
/// errors.
pub(super) fn help_or(
    topic: HelpTopic,
    rest: &[String],
    parse: impl FnOnce() -> LifecycleCommand,
) -> LifecycleCommand {
    if wants_help(rest) {
        LifecycleCommand::Help { topic }
    } else {
        parse()
    }
}

/// Parse the arguments after a first-positional `skill` verb into a
/// [`LifecycleCommand`]. `skill_cmd` owns the `install|remove [--agent NAME]
/// [--force]` grammar; the shared wrapper maps its `Err` onto the
/// usage-error convention.
#[cfg(feature = "skill")]
fn parse_skill_command(rest: &[String]) -> LifecycleCommand {
    parse_args_or_usage_error(
        HelpTopic::Skill,
        rest,
        crate::skill_cmd::parse_skill_args,
        |args| LifecycleCommand::Skill { args },
    )
}

/// Parse the arguments after a first-positional `upgrade` (or its `update`
/// alias) verb into a [`LifecycleCommand`]. `upgrade_cmd` owns the
/// `[--yes] [--check]` grammar; the shared wrapper maps its `Err` onto the
/// usage-error convention.
#[cfg(feature = "upgrade")]
fn parse_upgrade_command(rest: &[String]) -> LifecycleCommand {
    parse_args_or_usage_error(
        HelpTopic::Upgrade,
        rest,
        crate::upgrade_cmd::parse_upgrade_args,
        |opts| LifecycleCommand::Upgrade { opts },
    )
}

/// Parse the arguments after a first-positional `telemetry` verb into a
/// [`LifecycleCommand`]. The grammar is exactly `telemetry on|off|status`;
/// usage errors print `Error: ...` plus the telemetry synopsis and hint to
/// stderr and exit(2), like the rest of this hand-rolled parser.
#[cfg(feature = "telemetry")]
fn parse_telemetry_command(rest: &[String]) -> LifecycleCommand {
    parse_args_or_usage_error(
        HelpTopic::Telemetry,
        rest,
        crate::telemetry_cmd::parse_action,
        |action| LifecycleCommand::Telemetry { action },
    )
}

/// The shared parse-wrapper idiom for the lifecycle verbs whose `<verb>_cmd`
/// module owns the argument grammar: `Ok` becomes the verb's
/// [`LifecycleCommand`] variant, `Err` maps onto the shared usage-error
/// convention (`Error: ...`, the command's synopsis, and its hint; exit 2).
fn parse_args_or_usage_error<T>(
    topic: HelpTopic,
    rest: &[String],
    parse: impl FnOnce(&[String]) -> Result<T, ParseError>,
    variant: impl FnOnce(T) -> LifecycleCommand,
) -> LifecycleCommand {
    match parse(rest) {
        Ok(cmd) => variant(cmd),
        // The command-scope suggestion policy (the command's own flag
        // candidates, no top-level alias spellings) lives once in
        // `command_usage_error`, shared with the `run` mount's verb arm.
        Err(err) => command_usage_error(topic, &err),
    }
}

#[cfg(feature = "pull")]
fn parse_pull_command(rest: &[String], namespace: Option<relate::Namespace>) -> LifecycleCommand {
    // ADR-0118 §3: pull always writes neo4j and does not accept --namespace,
    // whatever its position — pre-verb it was already consumed by the global
    // parse (this parameter), post-verb parse_pull_args rejects it as an
    // unknown flag. One policy, one place.
    if namespace.is_some() {
        usage_error(
            HelpScope::Command(HelpTopic::Pull),
            "--namespace is not valid with pull (pull always writes the neo4j namespace)",
            None,
        );
    }
    parse_args_or_usage_error(
        HelpTopic::Pull,
        rest,
        crate::pull_cmd::parse_pull_args,
        |cmd| LifecycleCommand::Pull { cmd },
    )
}

/// Parse the arguments after a first-positional `push` verb (ADR-0401).
/// Like pull (ADR-0118 §3), push always works in the `neo4j` namespace and
/// rejects `--namespace` wherever it appears; `push_cmd` owns the grammar
/// and the shared wrapper maps its `Err` onto the usage-error convention.
#[cfg(feature = "push")]
fn parse_push_command(rest: &[String], namespace: Option<relate::Namespace>) -> LifecycleCommand {
    if namespace.is_some() {
        usage_error(
            HelpScope::Command(HelpTopic::Push),
            "--namespace is not valid with push (push always reads the neo4j namespace)",
            None,
        );
    }
    parse_args_or_usage_error(
        HelpTopic::Push,
        rest,
        crate::push_cmd::parse_push_args,
        |cmd| LifecycleCommand::Push { cmd },
    )
}

fn parse_clone_command(rest: &[String]) -> LifecycleCommand {
    parse_args_or_usage_error(
        HelpTopic::Clone,
        rest,
        crate::clone_cmd::parse_clone_args,
        |cmd| LifecycleCommand::Clone { cmd },
    )
}

/// Parse the arguments after a first-positional `run` verb (ADR-0429 §1,
/// LABS-552): the extension mount owns every decision — the registry
/// resolution, the `-h`/`--help` pre-scan, the pinned discovery
/// refusal, the action's own grammar — through
/// `extensions::run::parse_run_args`; this layer only maps its answer
/// onto the lifecycle command. The verb is not the shared
/// [`parse_args_or_usage_error`] wrapper: the action scope's synopsis
/// and did-you-mean candidates are manifest data (dynamic, sanitized),
/// which the wrapper's static renderer cannot print, so the mount owns
/// its usage-error renderer too.
#[cfg(feature = "extensions")]
fn parse_run_command(rest: &[String]) -> LifecycleCommand {
    use crate::extensions::run::ParsedRun;
    match crate::extensions::run::parse_run_args(rest) {
        Ok(ParsedRun::Run(cmd)) => LifecycleCommand::Run { cmd },
        Ok(ParsedRun::ActionHelp(page)) => LifecycleCommand::RunHelp { page },
        Ok(ParsedRun::VerbHelp) => LifecycleCommand::Help {
            topic: HelpTopic::Run,
        },
        Err(err) => crate::extensions::run::usage_error(err),
    }
}

/// Parse the arguments after a first-positional `info` verb (ADR-0165). The
/// grammar is `info --database PATH [--json]`; `info_cmd` owns it and the shared
/// wrapper maps its `Err` onto the usage-error convention.
#[cfg(feature = "info")]
fn parse_info_command(rest: &[String]) -> LifecycleCommand {
    parse_args_or_usage_error(
        HelpTopic::Info,
        rest,
        crate::info_cmd::parse_info_args,
        |cmd| LifecycleCommand::Info { cmd },
    )
}

/// Parse the arguments after a first-positional `serve` verb; `serve_cmd`
/// owns the `--database PATH [--http PORT] [--mcp PORT|stdio] [--bolt PORT]
/// [--read-only[=FRESHNESS]] [--auth ...]` grammar (ADR-0232, ADR-0306,
/// the ADR-0480 spelling).
#[cfg(feature = "serve")]
fn parse_serve_command(rest: &[String]) -> LifecycleCommand {
    parse_args_or_usage_error(
        HelpTopic::Serve,
        rest,
        crate::serve_args::parse_serve_args,
        |cmd| LifecycleCommand::Serve { cmd },
    )
}

/// Parse the arguments after a first-positional `agent-context` verb into a
/// [`LifecycleCommand`]; `agent_context_cmd` owns the grammar, the shared
/// wrapper maps its `Err` onto the usage-error convention.
#[cfg(feature = "agent-context")]
fn parse_agent_context_command(rest: &[String]) -> LifecycleCommand {
    parse_args_or_usage_error(
        HelpTopic::AgentContext,
        rest,
        crate::agent_context_cmd::parse_agent_context_args,
        |command| LifecycleCommand::AgentContext { command },
    )
}
