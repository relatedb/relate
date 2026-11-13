//! The query-run options block (ADR-0200): one atomic entry per flag, the
//! table the top-level `--help` and `query --help` render from and the
//! vocabulary their "did you mean" suggestions derive from.

use std::sync::OnceLock;

use crate::flags::codec_names;

/// One entry of the query-run options block: the headline line and its
/// continuation lines, kept together so a mid-block insertion cannot
/// separate a continuation from its flag, plus the long-flag names the
/// entry documents. [`session_flags`] derives the suggestion vocabulary
/// from this one table.
pub(crate) struct OptionEntry {
    /// The headline line (no trailing newline). `{modes}` and `{formats}`
    /// `{slot}` are the build- and caller-dependent values [`substitute`]
    /// fills.
    pub(crate) headline: &'static str,
    /// The entry's continuation lines, each rendered under its headline.
    pub(crate) continuation: &'static [&'static str],
    /// The long flags this entry documents (empty for the `--` row and the
    /// REPL note, which document none).
    pub(crate) flags: &'static [&'static str],
}

/// The `--database` entry's target-axis continuation lines (LABS-903,
/// ADR-0500): the flag names the store to open — never a URL; a remote
/// store is a `--load` source. Stated in every build, so the axis split is
/// wherever the flag is listed.
const DATABASE_TARGET_LINES: &[&str] = &[
    "                       The store to open — never a URL: a remote store is a --load",
    "                       source",
];

/// The `--load` entry's continuation lines (LABS-903, ADR-0500): the
/// two-axis grammar's dispositions, plus the remote fetch story exactly
/// when the build compiles its fetch — the `--fetch-params` entry's
/// precedent — while the flag itself (and its URL parsing) exists in every
/// build. The base lines are restated per arm (a const slice cannot spread
/// another const slice), the price of one gated continuation slot.
#[cfg(feature = "remote-database")]
const LOAD_LINES: &[&str] = &[
    "                       https URL. With :memory: (the default target) the source is",
    "                       turned into the in-memory store: a read-only open that releases",
    "                       the source after the load (nothing is held open or locked, so",
    "                       it can be deleted or opened writable underneath). With a",
    "                       --database path the source is cloned there and the store opens",
    "                       normally (the target must not exist yet). --read-only=live is",
    "                       refused",
    "                       An https URL is fetched to a private temp copy — a GitHub blob",
    "                       or raw link is rewritten to its raw form; a remote folder store",
    "                       is not supported. Nothing is kept: the memory target releases",
    "                       the copy after the load, a path target removes it after the",
    "                       clone",
];
#[cfg(not(feature = "remote-database"))]
const LOAD_LINES: &[&str] = &[
    "                       https URL. With :memory: (the default target) the source is",
    "                       turned into the in-memory store: a read-only open that releases",
    "                       the source after the load (nothing is held open or locked, so",
    "                       it can be deleted or opened writable underneath). With a",
    "                       --database path the source is cloned there and the store opens",
    "                       normally (the target must not exist yet). --read-only=live is",
    "                       refused",
    "                       An https URL parses here and fails closed at the fetch — enable",
    "                       the remote-database feature to fetch one",
];

/// The query-run options block, one atomic entry per flag (ADR-0200).
/// `--fetch-params` and the REPL note are listed only in builds that
/// document them, like the help page does.
pub(crate) const OPTION_ENTRIES: &[OptionEntry] = &[
    OptionEntry {
        headline: "  -h, --help    Show this help and exit",
        continuation: &[],
        flags: &["--help"],
    },
    OptionEntry {
        headline: "  --version     Show version and license and exit (with --json: one JSON object)",
        continuation: &[],
        flags: &["--version"],
    },
    OptionEntry {
        headline:
            "  -d, --database PATH  Database directory, .rl bundle, or :memory: (default :memory:)",
        continuation: DATABASE_TARGET_LINES,
        flags: &["--database"],
    },
    OptionEntry {
        headline: "  --json        Output as JSON (indented on a terminal, compact when piped)",
        continuation: &[],
        flags: &["--json"],
    },
    OptionEntry {
        headline: "  --csv         Output as CSV",
        continuation: &[],
        flags: &["--csv"],
    },
    OptionEntry {
        headline: "  --markdown    Output as Markdown",
        continuation: &[],
        flags: &["--markdown"],
    },
    OptionEntry {
        headline: "  --jsonl       Output as JSON Lines",
        continuation: &[],
        flags: &["--jsonl"],
    },
    OptionEntry {
        headline: "  --toon        Output as TOON (one header line, one line per row)",
        continuation: &[],
        flags: &["--toon"],
    },
    OptionEntry {
        headline: "  --noheader    Disable column headers",
        continuation: &[],
        flags: &["--noheader"],
    },
    OptionEntry {
        headline: "  --mode MODE   Set output mode ({modes})",
        continuation: &[
            "                default: table on a terminal, jsonl when stdout is piped or redirected",
            "                (toon instead of jsonl when agent mode is on)",
        ],
        flags: &["--mode"],
    },
    OptionEntry {
        headline: "  --format MODE Alias of --mode",
        continuation: &[],
        flags: &["--format"],
    },
    OptionEntry {
        headline: "  --pretty      Indent JSON output even when stdout is not a terminal",
        continuation: &[],
        flags: &["--pretty"],
    },
    OptionEntry {
        headline: "  --timings     Append the per-statement timing line to --jsonl output (default off)",
        continuation: &[],
        flags: &["--timings"],
    },
    OptionEntry {
        headline: "  --param N=V   Bind a Cypher parameter (repeatable; V parsed as JSON scalar, bare words are strings)",
        continuation: &[],
        flags: &["--param"],
    },
    #[cfg(feature = "fetch-params")]
    OptionEntry {
        headline: "  --fetch-params N=SRC  Fetch a URL or read a file (http, https, file path), parse it as JSON, and bind it as the parameter N (repeatable)",
        continuation: &[],
        flags: &["--fetch-params"],
    },
    OptionEntry {
        headline: "  --snapshot-format F  Snapshot codec ({formats}); empty directory only",
        continuation: &[],
        flags: &["--snapshot-format"],
    },
    OptionEntry {
        headline: "  --markdown-body-properties LIST  Markdown vault body properties (LIST or",
        continuation: &[
            "                       Label=LIST; entries separated by ';'; first bare, rest as",
            "                       ## sections; default: body)",
        ],
        flags: &["--markdown-body-properties"],
    },
    OptionEntry {
        headline: "  --wal-dir DIR        WAL directory (default <DATABASE>/wal)",
        continuation: &[],
        flags: &["--wal-dir"],
    },
    OptionEntry {
        headline: "  --checkpoint-interval-secs N  Timed checkpoint period; 0 disables",
        continuation: &[],
        flags: &["--checkpoint-interval-secs"],
    },
    OptionEntry {
        headline:
            "  --durability-mode MODE  WAL durability (full|everysecond|snapshot; default full,",
        continuation: &[
            "                       RELATE_DURABILITY_MODE)",
        ],
        flags: &["--durability-mode"],
    },
    OptionEntry {
        headline: "  --logical-graph-size-limit SIZE  Live graph + index cap (bytes or mb/mib/gb/gib)",
        continuation: &[],
        flags: &["--logical-graph-size-limit"],
    },
    OptionEntry {
        headline: "  --expression-memory-limit SIZE  Per-value expression build cap (bytes, mb/mib/gb/gib, or none)",
        continuation: &[
            "                       Default: 256 MiB. Guards against one statement",
            "                       building a pathological single value; raise it, or",
            "                       pass none for known bulk pipelines",
        ],
        flags: &["--expression-memory-limit"],
    },
    OptionEntry {
        headline: "  --concurrent-logical-workspace-limit SIZE  Shared logical-workspace pool (bytes or mb/mib/gb/gib)",
        continuation: &[
            "                       A positive value turns admission on. Omit, or an",
            "                       unset RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT,",
            "                       leaves it off. 0 is rejected",
        ],
        flags: &["--concurrent-logical-workspace-limit"],
    },
    OptionEntry {
        headline: "  --max-traversal-depth N  Largest explicit traversal hop bound (1-2000; default 500)",
        continuation: &[
            "                       A query whose explicit hop bound exceeds this fails at",
            "                       admission with TraversalDepthExceeded. This is a safety",
            "                       limit, not a database limit: raising it risks a stack",
            "                       overflow on small-stack platforms.",
            "                       RELATE_MAX_TRAVERSAL_DEPTH sets the default",
        ],
        flags: &["--max-traversal-depth"],
    },
    OptionEntry {
        headline: "  --safe               Refuse agent-typical dangerous statement shapes; --no-safe",
        continuation: &[
            "                       negates it. Refuses a new named node without a label, and",
            "                       a MATCH read without LIMIT or a reducing aggregate",
            "                       (collect() does not count). A policy, not a database",
            "                       limit. RELATE_SAFE_MODE=1/0 sets the default; off",
            "                       unless asked.",
        ],
        flags: &["--safe", "--no-safe"],
    },
    OptionEntry {
        headline: "  --agent-mode         Force agent ergonomics on (the row cap's default and the",
        continuation: &[
            "                       toon piped default); pairs with --max-rows.",
            "                       --no-agent-mode turns them off (the -mode suffix stays",
            "                       clear of the skill-install harness selector).",
            "                       Precedence: the flag, then RELATE_AGENT_MODE=1/0,",
            "                       then harness detection (piped runs only). The explicit",
            "                       acts print a one-line announcement; detection stays silent",
        ],
        flags: &["--agent-mode", "--no-agent-mode"],
    },
    OptionEntry {
        headline: "  --max-rows N         Print at most N rows per statement; 0 = unlimited.",
        continuation: &[
            "                       Default: 100 under a detected agent harness with stdout",
            "                       not a terminal, unlimited otherwise (RELATE_MAX_ROWS)",
        ],
        flags: &["--max-rows"],
    },
    OptionEntry {
        headline: "  --width N            Wrap the human table to N display columns; the widest",
        continuation: &[
            "                       columns shrink and their cells wrap over lines.",
            "                       Default: the terminal width, else an exported COLUMNS",
            "                       (both floored at 20), else the natural width",
        ],
        flags: &["--width"],
    },
    OptionEntry {
        headline: "  --timeout DURATION   Cancel a statement that runs longer (500ms, 10s, 2m;",
        continuation: &["                       bare number = seconds; 0 disables). Exit code 7"],
        flags: &["--timeout"],
    },
    OptionEntry {
        headline: "  --load SOURCE        Load a store's content from a .rl path, a directory, or an",
        continuation: LOAD_LINES,
        flags: &["--load"],
    },
    OptionEntry {
        headline:
            "  --read-only[=FRESH]  Open an existing database without writing or checkpointing.",
        continuation: &[
            "                       FRESH is snapshot (default: a frozen snapshot at open) or",
            "                       live (tail the WAL and follow the writer's commits;",
            "                       refused for .rl and :memory:). Attach the value",
            "                       (--read-only=live); once per run.",
            "                       A native directory can be open beside a writer. A .rl file",
            "                       opens only while no writer holds it. :memory: rejects writes",
            "                       and creates nothing",
        ],
        flags: &["--read-only"],
    },
    OptionEntry {
        headline: "  --load-csv-dir PATH  Allow local LOAD CSV under this directory (default denied; overrides RELATE_LOAD_CSV_DIR)",
        continuation: &[],
        flags: &["--load-csv-dir"],
    },
    OptionEntry {
        headline: "  --namespace NAME     Session namespace (neo4j|system|system_ontologies; default neo4j)",
        continuation: &[
            "                       system_ontologies is visible only under",
            "                       RELATE_PREVIEW=ontology: without it the store",
            "                       opens but the session connect fails with",
            "                       'Graph not found: system_ontologies' (exit 6) and",
            "                       SHOW DATABASES lists the two Community Edition",
            "                       rows; with it the namespace is visible but not",
            "                       queryable (only the administration commands run",
            "                       from there)",
        ],
        flags: &["--namespace"],
    },
    OptionEntry {
        headline: "  -c, --command Q      One-shot statement or `;`-separated list (same as positional {slot})",
        continuation: &[],
        flags: &["--command"],
    },
    OptionEntry {
        headline: "  --            Treat all following arguments as positional",
        continuation: &[],
        flags: &[],
    },
    #[cfg(feature = "repl")]
    OptionEntry {
        headline: "On a TTY, the interactive shell supports readline-style editing (arrow keys, Home/End).",
        continuation: &[],
        flags: &[],
    },
];

/// Every long flag the query session documents, for "did you mean"
/// suggestions on an unknown flag — derived from [`OPTION_ENTRIES`], the one
/// table the help page renders from. A test pins it against the help page and
/// `tests/e2e_help.rs` feeds each entry to the real parser.
pub(crate) fn session_flags() -> &'static [&'static str] {
    static FLAGS: OnceLock<Vec<&'static str>> = OnceLock::new();
    FLAGS.get_or_init(|| {
        OPTION_ENTRIES
            .iter()
            .flat_map(|entry| entry.flags)
            .copied()
            .collect()
    })
}

/// A placeholder's replacement-text provider, called with the command slot
/// (the one caller-supplied value; the build-dependent providers ignore it).
pub(crate) type PlaceholderProvider = fn(&str) -> String;

/// One placeholder token an [`OptionEntry`] line may carry and the provider
/// of its replacement text: the command slot is the one caller-supplied
/// value, the others are build-dependent. The vocabulary is this table — a
/// new placeholder is one row, and a test asserts every `{…}` token in the
/// entries is a known row.
pub(crate) const PLACEHOLDERS: &[(&str, PlaceholderProvider)] = &[
    ("{slot}", str::to_string),
    ("{modes}", |_| relate_formatter::Mode::names()),
    // The flag's values — the codec names, from the one vocabulary list
    // (flags::snapshot_format).
    ("{formats}", |_| codec_names("|")),
];

/// Fill an entry line's build- and caller-dependent placeholders from
/// [`PLACEHOLDERS`].
pub(crate) fn substitute(line: &str, command_slot: &str) -> String {
    let mut line = line.to_string();
    for (token, provider) in PLACEHOLDERS {
        if line.contains(token) {
            line = line.replace(token, &provider(command_slot));
        }
    }
    line
}

/// An entry's lines, headline first: the one traversal of "what lines make
/// up an entry", walked by [`entry_text`] and the placeholder test.
pub(crate) fn entry_lines(entry: &OptionEntry) -> impl Iterator<Item = &'static str> {
    std::iter::once(entry.headline).chain(entry.continuation.iter().copied())
}

/// One entry rendered whole: its substituted headline, then each
/// continuation line, newline-terminated — the one builder both
/// [`session_options_text`] and the help test walk.
pub(crate) fn entry_text(entry: &OptionEntry, command_slot: &str) -> String {
    let mut s = String::new();
    for line in entry_lines(entry) {
        s.push_str(&substitute(line, command_slot));
        s.push('\n');
    }
    s
}

/// Shared query-run options block for top-level `--help` and `query --help`
/// (ADR-0200), rendered from [`OPTION_ENTRIES`]. `command_slot` names the
/// statement slot in the `-c` line.
pub(crate) fn session_options_text(command_slot: &str) -> String {
    let mut s = String::from("Options:\n");
    for entry in OPTION_ENTRIES {
        s.push_str(&entry_text(entry, command_slot));
    }
    s
}
