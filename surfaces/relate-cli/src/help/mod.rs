//! Every hand-written help page for the `relate` CLI: the top-level
//! `--help`, the per-command help pages, each command's synopsis line(s), and
//! the usage-error hint.
//!
//! [`COMMANDS`] is the single source of truth for the command set compiled
//! into this build: one [`CommandSpec`] per command carries its word,
//! synopsis, top-level blurb, query-conflict policy, and help page, and every
//! consumer — the top-level `Commands:` section, the per-command pages, the
//! usage-error texts, and `flags.rs`'s command metadata — reads it. The
//! feature gates mirror the command gates: a command compiled out of this
//! build has no spec and no topic, and its word still reaches the
//! not-included error in `flags.rs`.
//!
//! Layout: this module holds the command table, the top-level page, and the
//! shared building blocks; each command's page fn, usage consts, and page
//! test live in one file per command (`query.rs`, `clone.rs`, …).

use std::borrow::Cow;
use std::sync::OnceLock;

pub(crate) use options::session_flags;

#[cfg(feature = "agent-context")]
mod agent_context;
mod clone;
#[cfg(feature = "extensions")]
mod extension;
#[cfg(feature = "info")]
mod info;
mod options;
#[cfg(feature = "pull")]
mod pull;
#[cfg(feature = "push")]
mod push;
mod query;

use options::session_options_text;
#[cfg(feature = "extensions")]
mod run;
#[cfg(feature = "serve")]
mod serve;
#[cfg(feature = "skill")]
mod skill;
#[cfg(feature = "telemetry")]
mod telemetry;
#[cfg(feature = "upgrade")]
mod upgrade;

/// A command a help page or a usage error is about. `update` has no variant:
/// it is the `upgrade` alias and reports as `upgrade`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HelpTopic {
    Query,
    Clone,
    #[cfg(feature = "info")]
    Info,
    #[cfg(feature = "pull")]
    Pull,
    #[cfg(feature = "push")]
    Push,
    #[cfg(feature = "skill")]
    Skill,
    #[cfg(feature = "upgrade")]
    Upgrade,
    #[cfg(feature = "telemetry")]
    Telemetry,
    /// `relate run <name> [flags]` (ADR-0429 §1, LABS-552): dispatch
    /// an installed extension action. The one command whose page is
    /// machine-state-dependent (the installed-actions listing).
    #[cfg(feature = "extensions")]
    Run,
    #[cfg(feature = "serve")]
    Serve,
    #[cfg(feature = "agent-context")]
    AgentContext,
    #[cfg(feature = "extensions")]
    Extension,
}

/// The query-run synopsis: the top-level usage line and the usage line every
/// top-level parse error prints.
const TOP_LEVEL_USAGE: &str = "Usage: relate [OPTIONS] [--database PATH] [QUERY]";

/// One compiled-in lifecycle command (ADR-0069): everything the help pages,
/// the top-level `Commands:` section, and the `flags.rs` usage errors know
/// about it, in one place. [`COMMANDS`] is the only enumeration of the
/// command set; every consumer iterates it or looks a topic up in it.
#[derive(Debug)]
pub(crate) struct CommandSpec {
    /// The topic help pages and usage errors name this command by.
    pub topic: HelpTopic,
    /// The canonical command word (`update` reports as `upgrade`).
    pub word: &'static str,
    /// Other spellings that run this command (`&["update"]` for `upgrade`);
    /// empty for every other command. Read by the ADR-0230 did-you-mean
    /// fallback so an alias is suggested under its canonical word.
    pub aliases: &'static [&'static str],
    /// The synopsis line shared by the help page and the usage errors.
    /// `Cow` because `serve` assembles its surface segments per build,
    /// and `extension` assembles its lifecycle-actions' synopsis from
    /// the compiled-in actions.
    pub usage: Cow<'static, str>,
    /// The one-liner the top-level `Commands:` section prints.
    pub blurb: &'static str,
    /// Whether a query or a pre-verb access flag (`--read-only[=FRESHNESS]`,
    /// ADR-0480's one spelling) on the same command line is a usage error: the
    /// commands that run no query engine (`clone`, `info`, `serve`,
    /// `agent-context`) reject the combination (ADR-0306 §6; a serve
    /// command's pre-verb access flags get serve's own message first,
    /// naming the post-verb spelling, through
    /// `LifecycleCommand::conflict_error`).
    pub rejects_query: bool,
    /// Whether the command takes the access flag **after** its verb
    /// (ADR-0306 §6, ADR-0480's one spelling): a pre-verb sighting on that
    /// command line gets the command's own message naming the post-verb
    /// spelling — structurally first in `conflict_error`. `serve` is the
    /// one such command; the field tables the fact beside
    /// `rejects_query`, so one descriptor table owns the whole §6 policy.
    pub post_verb_access: bool,
    /// Every flag the command's parser accepts — long and short
    /// spellings alike (`-d` beside `--database`) — for "did you mean"
    /// suggestions on an unknown flag. A test pins each against its
    /// help page. `Cow` because `serve` assembles its flag list per
    /// build and `extension` derives its from the compiled-in actions
    /// (as `usage` already is).
    pub flags: Cow<'static, [&'static str]>,
    /// One or two invocations shown under `Examples:` on the command's help
    /// page, each a `(comment, command line)` pair. Kept short on purpose; a
    /// test runs the ones that are safe to run.
    pub examples: &'static [crate::examples::Example],
    /// The command's help page (the shared synopsis first).
    pub help: fn() -> String,
}

/// The command set compiled into this build, in `Commands:`-section order.
/// The feature gates mirror the `HelpTopic` gates exactly, so a compiled-out
/// command has neither a topic nor a spec.
pub(crate) fn commands() -> &'static [CommandSpec] {
    static COMMANDS: OnceLock<Vec<CommandSpec>> = OnceLock::new();
    COMMANDS.get_or_init(|| {
        vec![
    CommandSpec {
        topic: HelpTopic::Query,
        word: "query",
        aliases: &[],
        usage: query::QUERY_USAGE.into(),
        blurb: "Run a statement session (oneshot, stdin, or shell)",
        rejects_query: false,
        post_verb_access: false,
        flags: Cow::Borrowed(session_flags()),
        examples: &[(
            "Run one statement against a store",
            "relate query \"RETURN 1 AS n\" --database ./my-db",
        )],
        help: query::query_help,
    },
    CommandSpec {
        topic: HelpTopic::Clone,
        word: "clone",
        aliases: &[],
        usage: clone::CLONE_USAGE.into(),
        blurb: "Copy a database into a new store and exit",
        rejects_query: true,
        post_verb_access: false,
        flags: Cow::Borrowed(&["--wal-dir", "--to-snapshot-format", "--markdown-body-properties"]),
        examples: &[(
            "Copy a database into a new store",
            "relate clone ./mydata ./backup",
        )],
        help: clone::clone_help,
    },
    #[cfg(feature = "info")]
    CommandSpec {
        topic: HelpTopic::Info,
        word: "info",
        aliases: &[],
        usage: info::INFO_USAGE.into(),
        blurb: "Print store metadata for a database without opening it",
        rejects_query: true,
        post_verb_access: false,
        flags: Cow::Borrowed(&["--database", "--json"]),
        examples: &[(
            "Describe a database without opening it, as JSON",
            "relate info --database ./mydata --json",
        )],
        help: info::info_help,
    },
    #[cfg(feature = "pull")]
    CommandSpec {
        topic: HelpTopic::Pull,
        word: "pull",
        aliases: &[],
        usage: pull::PULL_USAGE.into(),
        blurb: "Copy a Neo4j/Aura subgraph into a database",
        rejects_query: false,
        post_verb_access: false,
        flags: Cow::Borrowed(&[
            "--from",
            "--query",
            "--param",
            "--allow-plaintext",
            "--database",
        ]),
        examples: &[(
            "Copy a Neo4j/Aura subgraph into a local database",
            "relate pull --from .env --query 'MATCH (n)-[r]->(m) RETURN n, r, m LIMIT 100' --database ./graph",
        )],
        help: pull::pull_help,
    },
    #[cfg(feature = "push")]
    CommandSpec {
        topic: HelpTopic::Push,
        word: "push",
        aliases: &[],
        usage: push::PUSH_USAGE.into(),
        blurb: "Copy a database subgraph into Neo4j/Aura",
        rejects_query: false,
        post_verb_access: false,
        flags: Cow::Borrowed(&[
            "-d",
            "--database",
            "--to",
            "--query",
            "--labels",
            "--types",
            "--batch-size",
            "--nodes-update",
            "--rels-update",
            "--updated-at",
            "--rel-mode",
            "--merge-key",
            "--rel-key",
            "--unkeyed-labels",
            "--dry-run",
            "--json",
            "--allow-plaintext",
        ]),
        examples: &[(
            "Push the whole store into an Aura instance, planning first",
            "relate push --to .env --database ./graph --dry-run",
        )],
        help: push::push_help,
    },
    #[cfg(feature = "skill")]
    CommandSpec {
        topic: HelpTopic::Skill,
        word: "skill",
        aliases: &[],
        usage: skill::SKILL_USAGE.into(),
        blurb: "Install, inspect, or remove the embedded agent skill bundle",
        rejects_query: false,
        post_verb_access: false,
        flags: Cow::Borrowed(&["--agent", "--force"]),
        examples: &[(
            "Install the agent skill into every detected agent",
            "relate skill install",
        )],
        help: skill::skill_help,
    },
    #[cfg(feature = "upgrade")]
    CommandSpec {
        topic: HelpTopic::Upgrade,
        word: "upgrade",
        aliases: &["update"],
        usage: upgrade::UPGRADE_USAGE.into(),
        blurb: "Update the relate binary (alias: update)",
        rejects_query: false,
        post_verb_access: false,
        flags: Cow::Borrowed(&["--yes", "--check"]),
        examples: &[(
            "See whether a newer release exists, without installing it",
            "relate upgrade --check",
        )],
        help: upgrade::upgrade_help,
    },
    #[cfg(feature = "telemetry")]
    CommandSpec {
        topic: HelpTopic::Telemetry,
        word: "telemetry",
        aliases: &[],
        usage: telemetry::TELEMETRY_USAGE.into(),
        blurb: "Manage anonymous usage metrics (on, off, status)",
        rejects_query: false,
        post_verb_access: false,
        flags: Cow::Borrowed(&[]),
        examples: &[(
            "See whether anonymous usage metrics are on",
            "relate telemetry status",
        )],
        help: telemetry::telemetry_help,
    },
    #[cfg(feature = "extensions")]
    CommandSpec {
        topic: HelpTopic::Run,
        word: "run",
        aliases: &[],
        usage: run::RUN_USAGE.into(),
        blurb: "Run an installed extension action into a store the host owns",
        rejects_query: false,
        post_verb_access: false,
        // The verb's own flag vocabulary: the store flag every action's
        // grammar shares. The actions' own flags are manifest data —
        // each action's page and did-you-mean derive them per name.
        flags: Cow::Borrowed(&["-d", "--database"]),
        examples: &[(
            "Run an installed action into a store the host owns",
            "relate run greet ./src -d ./graph",
        )],
        help: run::run_help,
    },
    #[cfg(feature = "serve")]
    CommandSpec {
        topic: HelpTopic::Serve,
        word: "serve",
        aliases: &[],
        usage: serve::serve_usage().into(),
        blurb: "Host the HTTP Query API and MCP surfaces on a database",
        rejects_query: true,
        post_verb_access: true,
        flags: serve::serve_flags().into(),
        examples: &[(
            "Serve the HTTP Query API and MCP on one store",
            "relate serve --database ./mydata --http 7474 --mcp 8000",
        )],
        help: serve::serve_help,
    },
    #[cfg(feature = "agent-context")]
    CommandSpec {
        topic: HelpTopic::AgentContext,
        word: "agent-context",
        aliases: &[],
        usage: agent_context::AGENT_CONTEXT_USAGE.into(),
        blurb: "Print the compact agent briefing (commands, flags, exit codes)",
        rejects_query: true,
        post_verb_access: false,
        flags: Cow::Borrowed(&[]),
        examples: &[(
            "Show one command's flags and examples",
            "relate agent-context query",
        )],
        help: agent_context::agent_context_help,
    },
    #[cfg(feature = "extensions")]
    CommandSpec {
        topic: HelpTopic::Extension,
        word: "extension",
        aliases: &[],
        usage: extension::extension_usage().into(),
        blurb: "Scaffold, install, check, list, or remove command extensions",
        rejects_query: true,
        post_verb_access: false,
        flags: extension::extension_flags().into(),
        examples: extension::examples(),
        help: extension::extension_help,
    },
        ]
    })
}

impl HelpTopic {
    /// This topic's descriptor in [`COMMANDS`]: every compiled-in topic has
    /// exactly one.
    pub(crate) fn spec(self) -> &'static CommandSpec {
        commands()
            .iter()
            .find(|spec| spec.topic == self)
            .expect("every HelpTopic has a CommandSpec")
    }

    /// The canonical command word: the usage-error hint reports it, and so
    /// does `LifecycleCommand::name` for a help exit (`update` reports
    /// `upgrade`).
    pub(crate) fn word(self) -> &'static str {
        self.spec().word
    }
}

/// The `-d` / `--database` help block every import grammar documents —
/// the extension actions' help pages (ADR-0383, ADR-0286): one store-path
/// fact, one text, so the block cannot drift between the surfaces that
/// share the flag. The headline first, its continuation lines after it.
/// Compiled when the import surface is (a build without `extensions`
/// documents no store flag).
#[cfg(feature = "extensions")]
pub(crate) const DATABASE_FLAG_HEADLINE: &str =
    "  -d, --database PATH   Database to import into (default :memory:).";
#[cfg(feature = "extensions")]
pub(crate) const DATABASE_FLAG_CONTINUATION: [&str; 2] = [
    "                         An explicit --database exits after the import; without",
    "                         it, a TTY opens the shell on the in-memory graph.",
];
/// All three lines in page order — the renderers' one traversal.
#[cfg(feature = "extensions")]
pub(crate) const DATABASE_FLAG_HELP: [&str; 3] = [
    DATABASE_FLAG_HEADLINE,
    DATABASE_FLAG_CONTINUATION[0],
    DATABASE_FLAG_CONTINUATION[1],
];

/// The scope of a usage error or a help hint: the top-level query run or one
/// command. The top-level-vs-command split lives here exactly once, so the
/// synopsis and the hint a usage error prints can never point at different
/// pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HelpScope {
    TopLevel,
    Command(HelpTopic),
}

impl HelpScope {
    /// The synopsis line a usage error at this scope prints.
    pub(crate) fn usage(self) -> &'static str {
        match self {
            Self::TopLevel => TOP_LEVEL_USAGE,
            Self::Command(topic) => command_usage(topic),
        }
    }

    /// The flag vocabulary "did you mean" suggests from at this scope: the
    /// top-level run's flags, or the command's own spec — the `query` spec
    /// borrows the same session list.
    pub(crate) fn known_flags(self) -> &'static [&'static str] {
        match self {
            Self::TopLevel => session_flags(),
            Self::Command(topic) => topic.spec().flags.as_ref(),
        }
    }

    /// The help hint a usage error at this scope prints.
    pub(crate) fn hint(self) -> String {
        match self {
            Self::TopLevel => "Run 'relate --help' for more information.".to_string(),
            Self::Command(topic) => {
                format!("Run 'relate {} --help' for more information.", topic.word())
            }
        }
    }
}

/// The canonical synopsis line for `topic`, shared by its help page and its
/// usage errors.
pub(crate) fn command_usage(topic: HelpTopic) -> &'static str {
    topic.spec().usage.as_ref()
}

/// The snapshot-codec names compiled into this build, joined by `separator`:
/// the one list the clone help page and the `--to-snapshot-format`
/// "requires" text render (the `--snapshot-format` vocabulary lives in
/// `flags::snapshot_format`).
pub(crate) fn compiled_codecs(separator: &str) -> String {
    crate::flags::codec_names(separator)
}

/// Exact-match did-you-mean aliases (ADR-0239): spellings from other CLIs
/// that the edit-distance matcher cannot bridge to a real flag. Lives with
/// the flag data; `suggest::suggest_flag` stays a pure string helper.
pub(crate) const FLAG_ALIASES: &[(&str, &str)] = &[("--output", "--format"), ("-o", "--format")];

/// Command-word aliases, as `(alias, canonical word)` pairs: the CLI's one
/// alias is `update` → `upgrade` (ADR-0069). The single source for the alias
/// fact — [`find_command`] and the lifecycle parser both resolve through
/// [`canonical_word`], so a second alias is one row here.
// Only the upgrade dispatch and the agent-context lookup resolve aliases.
#[cfg(any(feature = "upgrade", feature = "agent-context"))]
pub(crate) const COMMAND_WORD_ALIASES: &[(&str, &str)] = &[("update", "upgrade")];

/// The canonical word for a command word a user typed (an alias resolves to
/// its target; anything else is itself).
#[cfg(any(feature = "upgrade", feature = "agent-context"))]
pub(crate) fn canonical_word(word: &str) -> &str {
    COMMAND_WORD_ALIASES
        .iter()
        .find(|(alias, _)| *alias == word)
        .map(|(_, canonical)| *canonical)
        .unwrap_or(word)
}

/// The descriptor for a command word a user typed, resolving aliases
/// ([`canonical_word`]) to the canonical spec. `None` for an unknown word or
/// one compiled out of this build. Only the `agent-context` readout resolves
/// words to specs today.
#[cfg(feature = "agent-context")]
pub(crate) fn find_command(word: &str) -> Option<&'static CommandSpec> {
    let word = canonical_word(word);
    commands().iter().find(|spec| spec.word == word)
}

/// The top-level `--help`: usage, the compiled-in commands, and the
/// query-run options. Command-specific flags live on the command's own page
/// (`relate <COMMAND> --help`); the last line points there.
pub(crate) fn top_level() -> String {
    top_level_with(crate::examples::enabled())
}

/// The `Commands:`-section column width: the widest compiled-in command
/// word ("agent-context" today) plus the two-space gap, derived so a wider
/// word can never render misaligned while a test still passes.
fn command_column_width() -> usize {
    commands()
        .iter()
        .map(|spec| spec.word.len())
        .max()
        .expect("COMMANDS is non-empty (query and clone are unconditional)")
        + 2
}

/// One `Commands:`-section row for `spec` (no trailing newline): the word
/// padded to [`COMMAND_ROW_WIDTH`], then the blurb. The one builder
/// shared by the top-level `--help` and the `agent-context` index, so the
/// two listings cannot drift.
pub(crate) fn command_row(spec: &CommandSpec) -> String {
    format!(
        "  {:<width$}{}",
        spec.word,
        spec.blurb,
        width = command_column_width()
    )
}

/// [`top_level`] with the examples decision made by the caller.
fn top_level_with(examples: bool) -> String {
    let mut s = String::new();
    s.push_str(TOP_LEVEL_USAGE);
    s.push('\n');
    s.push_str("       relate <COMMAND> [ARGS]\n");
    s.push('\n');
    s.push_str("Commands:\n");
    // One line per compiled-in command.
    for spec in commands() {
        s.push_str(&command_row(spec));
        s.push('\n');
    }
    s.push('\n');
    s.push_str(&session_options_text("QUERY"));
    s.push('\n');
    s.push_str(
        "--database is a directory (WAL + snapshot), a .rl bundle, or :memory: (the default).\n",
    );
    s.push_str("With no QUERY, the interactive shell opens on a terminal (ADR-0231); on piped\n");
    s.push_str(
        "or redirected stdin this help is printed. `query` with no STATEMENT reads a script\n",
    );
    s.push_str("from stdin, or opens the same shell on a terminal.\n");
    crate::examples::page(
        s,
        crate::examples::TOP_LEVEL,
        examples,
        "Run 'relate <COMMAND> --help' for details on a command.\n",
    )
}

/// The help page for `topic`: the shared synopsis, then the command's
/// description and flags.
pub(crate) fn command_help(topic: HelpTopic) -> String {
    command_help_with(topic, crate::examples::enabled())
}

/// [`command_help`] with the examples decision made by the caller, so tests
/// need not touch the process environment.
fn command_help_with(topic: HelpTopic, examples: bool) -> String {
    let spec = topic.spec();
    crate::examples::page((spec.help)(), spec.examples, examples, "")
}

/// Every help page starts with its shared synopsis and mentions no other
/// command's `Usage:` line (exactly one occurrence overall). Shared by this
/// module's tests and the per-command page tests.
#[cfg(test)]
fn assert_single_usage(help: &str, usage: &str) {
    assert!(help.starts_with(usage), "{help}");
    assert_eq!(help.matches("Usage: relate").count(), 1, "{help}");
}

#[cfg(test)]
mod tests;
