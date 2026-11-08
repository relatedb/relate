//! Scoped CLI help (ADR-0190): the top-level `--help` lists the compiled-in
//! commands and the query-run options only; every command word has its own
//! help page (`relate <COMMAND> --help`); and every usage error prints
//! just the relevant synopsis plus a hint — never the full help.

mod support;

use support::{
    commands_section, commands_section_words, compiled_commands, run_cli, tempdir,
    CliRunOptions as Opts,
};

#[cfg(feature = "telemetry")]
use support::MockMixpanel;

// --- top-level help ---

#[test]
fn test_top_level_help_commands_section_lists_exactly_compiled_in_commands() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["--help"]).in_home(home.path()));
    assert!(status.success(), "--help failed: {stderr}");
    assert!(stderr.is_empty(), "stderr must be empty: {stderr}");
    let section = commands_section(&stdout);
    let lines: Vec<&str> = section.lines().skip(1).collect();
    assert_eq!(
        commands_section_words(&stdout),
        compiled_commands(),
        "commands section:\n{stdout}"
    );
    for line in &lines {
        assert!(
            line.split_whitespace().count() >= 2,
            "every command line needs a one-line description: {line:?}"
        );
    }
    if cfg!(feature = "upgrade") {
        let upgrade = lines
            .iter()
            .find(|line| line.trim_start().starts_with("upgrade"))
            .expect("upgrade line");
        assert!(
            upgrade.contains("update"),
            "the upgrade line must name its update alias: {upgrade:?}"
        );
    }
}

/// Bare `relate` on piped stdin prints the top-level help and exits 0
/// without touching stdin (ADR-0231); the TTY half — the shell — is pinned
/// by the PTY driver in `e2e_repl.rs`.
#[test]
fn test_no_statement_prints_help_and_does_not_run_stdin() {
    let (help_status, help, help_err) = run_cli(Opts::new(&["--help"]));
    assert!(help_status.success(), "{help_err}");
    let (status, stdout, stderr) = run_cli(Opts::new(&[]).with_stdin(b"RETURN 1 AS n;\n"));
    assert!(status.success(), "{stderr}");
    assert_eq!(
        stdout, help,
        "bare relate on a non-TTY prints the top-level help"
    );
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn test_top_level_help_keeps_query_run_options_and_drops_command_flags() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["--help"]).in_home(home.path()));
    assert!(status.success(), "--help failed: {stderr}");
    for present in [
        "Usage: relate [OPTIONS] [--database PATH] [QUERY]",
        "relate <COMMAND> [ARGS]",
        "-h, --help",
        "--version",
        "--json",
        "--csv",
        "--markdown",
        "--jsonl",
        "--noheader",
        "--mode MODE",
        "--param N=V",
        "--snapshot-format F",
        "--wal-dir DIR",
        "--checkpoint-interval-secs N",
        "--durability-mode MODE",
        "--logical-graph-size-limit SIZE",
        "--concurrent-logical-workspace-limit SIZE",
        "--max-traversal-depth N",
        "--safe",
        "--read-only[=FRESH]",
        "--load-csv-dir PATH",
        "--namespace NAME",
        "-c, --command Q",
    ] {
        assert!(stdout.contains(present), "missing {present:?}:\n{stdout}");
    }
    assert_eq!(
        stdout.contains("--fetch-params N=SRC"),
        cfg!(feature = "fetch-params"),
        "--fetch-params in the top-level help:\n{stdout}"
    );
    assert_eq!(
        stdout.contains("readline-style editing"),
        cfg!(feature = "repl"),
        "readline note in the top-level help:\n{stdout}"
    );
    // Lifecycle flags live on their command's own page, never here.
    for absent in [
        // The skill verb's value-taking harness selector; the run-level
        // `--agent-mode` (ADR-0407) legitimately contains the substring.
        "--agent NAME",
        "--to-snapshot-format",
        "--allow-plaintext",
        "--max-content-bytes",
        "--include-hidden",
        "--from FILE",
    ] {
        assert!(
            !stdout.contains(absent),
            "{absent:?} must not appear in the top-level help:\n{stdout}"
        );
    }
    assert_eq!(
        stdout.lines().last(),
        Some("Run 'relate <COMMAND> --help' for details on a command.")
    );
}

// --- per-command help ---

#[test]
fn test_each_compiled_in_command_has_its_own_help() {
    let home = tempdir();
    // One row per command word: named fields (three adjacent `&[&str]`
    // fields as a tuple would let a transposition compile clean). The
    // iteration is driven by `compiled_commands()` and a word with no row
    // panics, so adding a command fails loudly here instead of being
    // silently skipped. An alias prints its canonical word's page, so it
    // shares the row's expectations. A page prints no flag outside its
    // own spec's flag list — pinned by the reverse-direction sweep in
    // `help::tests::command_flags_are_all_on_their_help_page` (the crate
    // unit suite), so the query-run options (`--durability-mode`,
    // ADR-0470) need no per-row repetition here: every command whose spec
    // does not list them is already covered.
    // The one per-command absence this suite pins, as data beside the
    // cases (not a field on every row, not a word-keyed branch in the
    // loop): `run` shares the store flag as -d, --database, so its page
    // must not carry the query session's --db spelling.
    const PINNED_ABSENCES: &[(&str, &[&str])] = &[("run", &["--db"])];

    struct HelpCase {
        word: &'static str,
        synopsis: &'static str,
        needles: &'static [&'static str],
        aliases: &'static [&'static str],
    }
    const HELP_CASES: &[HelpCase] = &[
        HelpCase {
            word: "query",
            synopsis: "Usage: relate query",
            needles: &[
                "--database",
                "ADR-0200",
                "--durability-mode",
                "-c, --command",
            ],
            aliases: &[],
        },
        HelpCase {
            word: "clone",
            synopsis: "Usage: relate clone",
            needles: &["--wal-dir", "--to-snapshot-format"],
            aliases: &[],
        },
        HelpCase {
            word: "info",
            synopsis: "Usage: relate info",
            needles: &["--json"],
            aliases: &[],
        },
        HelpCase {
            word: "pull",
            synopsis: "Usage: relate pull",
            needles: &["--from", "--query", "--param", "--allow-plaintext"],
            aliases: &[],
        },
        HelpCase {
            word: "push",
            synopsis: "Usage: relate push",
            needles: &[
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
            ],
            aliases: &[],
        },
        HelpCase {
            word: "skill",
            synopsis: "Usage: relate skill",
            needles: &["--agent", "--force", "otherwise refuse it"],
            aliases: &[],
        },
        HelpCase {
            word: "upgrade",
            synopsis: "Usage: relate upgrade",
            needles: &["--yes", "--check"],
            aliases: &["update"],
        },
        HelpCase {
            word: "telemetry",
            synopsis: "Usage: relate telemetry",
            needles: &["on|off|status"],
            aliases: &[],
        },
        HelpCase {
            // The verb's own page: the dynamic installed-actions section
            // (rendering the fresh-install zero state under the isolated
            // `$HOME`), the shared store block, and the discovery note.
            // A resolved action's page is the e2e_run suite's.
            word: "run",
            synopsis: "Usage: relate run",
            needles: &["Installed actions", "-d, --database", "ADR-0429"],
            aliases: &[],
        },
        HelpCase {
            word: "serve",
            synopsis: "Usage: relate serve",
            // ADR-0470: serve carries --durability-mode as its own storage
            // flag, so its page lists it rather than omitting the
            // query-run option. LABS-903: serve accepts `--load`, so its
            // page documents the flag like the query path's does.
            needles: &[
                "--database PATH",
                "--load SOURCE",
                "--auth USER:PASSWORD",
                "--durability-mode",
            ],
            aliases: &[],
        },
        HelpCase {
            word: "agent-context",
            synopsis: "Usage: relate agent-context",
            needles: &["COMMAND"],
            aliases: &[],
        },
        // `init` leads every build's synopsis — scaffolding is
        // dependency-free; the install line follows only when
        // `extension-install` is compiled in (ADR-0431 §3).
        HelpCase {
            word: "extension",
            synopsis: "Usage: relate extension init <name>",
            needles: &["Trust tiers", "remove", "--lang", "check <dir>"],
            aliases: &[],
        },
    ];
    // One case per compiled-in command, plus one per alias the row declares
    // (an alias shares its canonical word's expectations and feature gate).
    let mut cases: Vec<&HelpCase> = Vec::new();
    for word in compiled_commands() {
        let case = HELP_CASES
            .iter()
            .find(|c| c.word == word)
            .unwrap_or_else(|| panic!("no help-page case for command {word}"));
        cases.push(case);
    }
    // A pinned word with no case row would never match — the same silent
    // skip the row lookup above refuses. Guard the table's vocabulary.
    for (pinned_word, _) in PINNED_ABSENCES {
        assert!(
            HELP_CASES.iter().any(|c| c.word == *pinned_word),
            "PINNED_ABSENCES names {pinned_word:?}, which has no HELP_CASES row"
        );
    }
    for case in cases {
        // The pinned absences join to the row (case.word), so the check is
        // row-driven like synopsis/needles and rides every alias of the
        // case, never a bare word spelling.
        let pinned_absences: &[&str] = PINNED_ABSENCES
            .iter()
            .find(|(pinned_word, _)| case.word == *pinned_word)
            .map(|(_, absences)| *absences)
            .unwrap_or(&[]);
        for word in std::iter::once(case.word).chain(case.aliases.iter().copied()) {
            for flag in ["--help", "-h"] {
                let (status, stdout, stderr) =
                    run_cli(Opts::new(&[word, flag]).in_home(home.path()));
                assert!(status.success(), "{word} {flag} must exit 0: {stderr}");
                assert!(stderr.is_empty(), "{word} {flag} stderr: {stderr}");
                assert!(
                    stdout.starts_with(case.synopsis),
                    "{word} {flag} must start with {:?}:\n{stdout}",
                    case.synopsis
                );
                for needle in case.needles {
                    assert!(
                        stdout.contains(needle),
                        "{word} {flag} must list {needle:?}:\n{stdout}"
                    );
                }
                // The pinned absences, compared case-insensitively like the
                // needles: a spelling that must never reach this page.
                let lower = stdout.to_lowercase();
                for absence in pinned_absences {
                    assert!(
                        !lower.contains(&absence.to_lowercase()),
                        "{word} {flag} must not list {absence:?}:\n{stdout}"
                    );
                }
                // Exactly the command's own usage line: no other command's page.
                assert_eq!(
                    stdout.matches("Usage: relate").count(),
                    1,
                    "{word} {flag} must not contain another command's usage line:\n{stdout}"
                );
            }
        }
    }
}

#[test]
fn test_help_after_a_command_word_wins_and_creates_nothing() {
    let home = tempdir();
    let cwd = tempdir();
    let mut cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["clone", "--bogus", "--help"], "Usage: relate clone"),
        (vec!["clone", "a", "b", "--help"], "Usage: relate clone"),
    ];
    if cfg!(feature = "skill") {
        cases.push((vec!["skill", "install", "--help"], "Usage: relate skill"));
    }
    if cfg!(feature = "upgrade") {
        cases.push((vec!["update", "--check", "-h"], "Usage: relate upgrade"));
    }
    for (args, synopsis) in cases {
        let (status, stdout, stderr) =
            run_cli(Opts::new(&args).in_home(home.path()).in_dir(cwd.path()));
        assert!(status.success(), "{args:?} must exit 0: {stderr}");
        assert!(stderr.is_empty(), "{args:?} stderr: {stderr}");
        assert!(stdout.starts_with(synopsis), "{args:?}:\n{stdout}");
    }
    assert!(
        !cwd.path().join("a").exists(),
        "clone --help must not create the source path"
    );
    assert!(
        !cwd.path().join("b").exists(),
        "clone --help must not create the destination path"
    );
}

#[test]
fn test_help_before_the_command_word_stays_top_level() {
    for args in [vec!["--help", "clone"], vec!["-h", "clone"]] {
        let (status, stdout, stderr) = run_cli(Opts::new(&args));
        assert!(status.success(), "{args:?}: {stderr}");
        assert!(stderr.is_empty(), "{args:?}: {stderr}");
        assert!(
            stdout.starts_with("Usage: relate [OPTIONS] [--database PATH] [QUERY]"),
            "{args:?} must print the top-level help:\n{stdout}"
        );
        assert!(stdout.contains("Commands:"), "{args:?}:\n{stdout}");
    }
}

/// A compiled-out command word keeps the ADR-0104 not-included error for its
/// `--help` too: help only exists where the command does.
#[cfg(not(feature = "pull"))]
#[test]
fn test_pull_help_is_not_included_without_the_pull_feature() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["pull", "--help"]));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("Error: the 'pull' command is not included in this build of relate"),
        "{stderr}"
    );
}

// --- scoped usage errors ---

/// The two contract examples, byte for byte.
#[test]
fn test_clone_missing_positionals_usage_error_is_byte_for_byte() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["clone"]));
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        "Error: clone requires SOURCE and DEST\n\
         Usage: relate clone [--wal-dir DIR] [--to-snapshot-format F] [--markdown-body-properties LIST] SOURCE DEST\n\
         Run 'relate clone --help' for more information.\n"
    );
}

#[test]
fn test_unknown_flag_usage_error_is_byte_for_byte() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--bogus"]));
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        "Error: unknown flag: --bogus\n\
         Usage: relate [OPTIONS] [--database PATH] [QUERY]\n\
         Run 'relate --help' for more information.\n"
    );
}

/// Every `--flag` token a help page documents, in order of appearance.
fn documented_flags(help: &str) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    for word in help.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if word.starts_with("--")
            && word.len() > 2
            && word != "--help"
            && word != "--version"
            && !flags.iter().any(|f| f == word)
        {
            flags.push(word.to_string());
        }
    }
    flags
}

#[test]
fn test_every_documented_flag_is_accepted_by_the_parser() {
    // The did-you-mean candidate lists are pinned against the help pages; this
    // pins them against the parsers: a documented flag must never come back
    // as an unknown/unexpected argument, so a list entry cannot be a typo.
    let (_, top, _) = run_cli(Opts::new(&["--help"]));
    // `--database x` opens a store. Cargo's cwd for this binary is the crate
    // directory, so the probe runs under CARGO_TARGET_TMPDIR instead.
    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("documented-flags");
    std::fs::create_dir_all(&scratch).expect("create documented-flags scratch dir");
    for flag in documented_flags(&top) {
        let (_, _, stderr) = run_cli(Opts::new(&[&flag, "x", "RETURN 1"]).in_dir(&scratch));
        assert!(
            !stderr.contains("unknown flag"),
            "top-level --help documents {flag} but the parser rejects it: {stderr}"
        );
    }
    // Only commands whose grammar takes flags before any positional or action
    // word, and that do nothing on a parse failure: `clone` and `info`. `skill`
    // parses its action word first, so `skill <flag> x` fails on the flag
    // itself; `upgrade` and `telemetry` could act (network, config) if the
    // parse ever succeeded; `pull` (the network) is not run with made-up arguments. Their candidate lists are
    // covered by the unit test against their help pages.
    let dir = tempdir();
    for command in ["clone", "info"]
        .into_iter()
        .filter(|command| compiled_commands().contains(command))
    {
        let (_, help, _) = run_cli(Opts::new(&[command, "--help"]));
        for flag in documented_flags(&help) {
            let (_, _, stderr) = run_cli(Opts::new(&[command, &flag, "x"]).in_dir(dir.path()));
            assert!(
                !stderr.contains(&format!("unexpected argument to {command}: {flag}")),
                "`{command} --help` documents {flag} but the parser rejects it: {stderr}"
            );
        }
    }
}

/// The `relate ...` command lines under a help page's `Examples:` heading.
fn example_commands(help: &str) -> Vec<String> {
    help.split("\nExamples:\n")
        .nth(1)
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("relate "))
        .map(str::to_string)
        .collect()
}

/// Split an example command line into arguments: whitespace-separated, with
/// double quotes grouping (the examples use no other quoting).
fn example_args(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let (mut cur, mut quoted) = (String::new(), false);
    for c in command.trim_start_matches("relate ").chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

/// A temp directory holding `./mydata`, the store the examples name.
fn seeded_dir() -> tempfile::TempDir {
    let dir = tempdir();
    let (status, _, stderr) = run_cli(
        Opts::new(&["--database", "./mydata", "CREATE (:Person {name: 'Alice'})"])
            .in_dir(dir.path()),
    );
    assert!(status.success(), "{stderr}");
    dir
}

/// The help invocations whose pages carry examples this suite may run: the top
/// level and `clone` always, `info` in bundles that include it.
fn runnable_help_pages() -> Vec<&'static [&'static str]> {
    let mut pages: Vec<&'static [&'static str]> = vec![&["--help"], &["clone", "--help"]];
    if cfg!(feature = "info") {
        pages.push(&["info", "--help"]);
    }
    pages
}

/// Run one help example in `dir` and return its status, stdout and stderr.
fn run_example(example: &str, dir: &std::path::Path) -> (std::process::ExitStatus, String, String) {
    let args = example_args(example);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_cli(Opts::new(&refs).in_dir(dir))
}

#[test]
fn test_the_safe_help_examples_run_and_succeed() {
    // The top-level pair, `clone` and `info`: they read or copy a temp store.
    // `pull` (network), `skill` (home directory), `upgrade` (network),
    // `telemetry` (config) are not run; the
    // unit tests in `help.rs` pin their shape.
    let dir = seeded_dir();
    for args in runnable_help_pages() {
        let (status, help, stderr) = run_cli(Opts::new(args));
        assert_eq!(status.code(), Some(0), "{args:?}: {stderr}");
        let examples = example_commands(&help);
        assert!(!examples.is_empty(), "{args:?} has no example");
        for example in examples {
            let (status, stdout, stderr) = run_example(&example, dir.path());
            assert!(status.success(), "`{example}` failed: {stderr}");
            if args == ["--help"] {
                assert!(!stdout.is_empty(), "`{example}` printed nothing");
            }
        }
    }
}

#[test]
fn test_help_examples_env_variable_hides_only_the_examples() {
    let hidden = [("RELATE_HELP_EXAMPLES", "0")];
    for args in runnable_help_pages() {
        let (status, shown, stderr) = run_cli(Opts::new(args));
        assert_eq!(status.code(), Some(0), "{args:?}: {stderr}");
        assert!(shown.contains("\nExamples:\n"), "{args:?} shows examples");

        let (status, hid, _) = run_cli(Opts::new(args).with_envs(&hidden));
        assert_eq!(status.code(), Some(0), "{args:?}");
        assert!(!hid.contains("Examples:"), "{args:?}:\n{hid}");
        // Everything else on the page is still there, in the same order.
        let (head, _) = shown.split_once("\nExamples:\n").expect("examples section");
        assert!(hid.starts_with(head.trim_end()), "{args:?}: head changed");
    }
    // Other values leave the examples in.
    for value in ["1", "yes", ""] {
        let (_, page, _) =
            run_cli(Opts::new(&["--help"]).with_envs(&[("RELATE_HELP_EXAMPLES", value)]));
        assert!(page.contains("\nExamples:\n"), "{value:?}");
    }
}

#[test]
fn test_unknown_flag_near_miss_suggests_the_real_flag() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--readonly", "RETURN 1"]));
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        "Error: unknown flag: --readonly\n\
         Did you mean '--read-only'?\n\
         Usage: relate [OPTIONS] [--database PATH] [QUERY]\n\
         Run 'relate --help' for more information.\n"
    );
}

#[test]
fn test_command_unknown_flag_near_miss_suggests_the_commands_own_flag() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["clone", "--wal-dri", "x", "a", "b"]));
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        "Error: unexpected argument to clone: --wal-dri\n\
         Did you mean '--wal-dir'?\n\
         Usage: relate clone [--wal-dir DIR] [--to-snapshot-format F] [--markdown-body-properties LIST] SOURCE DEST\n\
         Run 'relate clone --help' for more information.\n"
    );
}

#[test]
fn test_a_flag_shaped_value_never_draws_a_suggestion_for_another_error() {
    // `--wal-dir` swallows `--wal-dri` as its value; the parser then fails on
    // the missing DEST, which has no rejected token, so no suggestion.
    let (status, _, stderr) = run_cli(Opts::new(&["clone", "--wal-dir", "--wal-dri", "a"]));
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.starts_with("Error: clone requires SOURCE and DEST\n"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("Did you mean"), "stderr: {stderr}");

    // The error names `--bogus`, which is nowhere near a flag; the swallowed
    // `--wal-dri` must not be suggested in its place.
    let (_, _, stderr) = run_cli(Opts::new(&[
        "clone",
        "--wal-dir",
        "--wal-dri",
        "--bogus",
        "a",
        "b",
    ]));
    assert!(
        stderr.starts_with("Error: unexpected argument to clone: --bogus\n"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("Did you mean"), "stderr: {stderr}");
}

#[test]
fn test_command_unknown_flag_without_a_near_miss_has_no_suggestion() {
    let (status, _, stderr) = run_cli(Opts::new(&["clone", "--bogus", "a", "b"]));
    assert_eq!(status.code(), Some(2));
    assert!(!stderr.contains("Did you mean"), "stderr: {stderr}");
}

#[test]
fn test_usage_errors_are_scoped_to_their_command() {
    let home = tempdir();
    const TOP: &str = "Usage: relate [OPTIONS] [--database PATH] [QUERY]";
    const TOP_HINT: &str = "Run 'relate --help' for more information.";
    const CLONE: &str =
        "Usage: relate clone [--wal-dir DIR] [--to-snapshot-format F] [--markdown-body-properties LIST] SOURCE DEST";
    const CLONE_HINT: &str = "Run 'relate clone --help' for more information.";
    const SKILL: &str = "Usage: relate skill install|remove|list|check [--agent NAME] [--force]";
    const SKILL_HINT: &str = "Run 'relate skill --help' for more information.";
    const UPGRADE: &str = "Usage: relate upgrade [--yes] [--check]";
    const UPGRADE_HINT: &str = "Run 'relate upgrade --help' for more information.";
    const TELEMETRY: &str = "Usage: relate telemetry on|off|status";
    const TELEMETRY_HINT: &str = "Run 'relate telemetry --help' for more information.";
    const PULL: &str = "Usage: relate pull [--from FILE] --query Q [--param N=V] [--allow-plaintext] --database PATH";
    const PULL_HINT: &str = "Run 'relate pull --help' for more information.";
    const PUSH: &str = "Usage: relate push --to CREDS --database PATH [--query Q | --labels L1,L2 | --types T1,T2] [--batch-size N] [--nodes-update POLICY] [--rels-update POLICY] [--updated-at NAME[,NAME]] [--rel-mode MODE] [--merge-key LABEL:PROP[,PROP]] [--rel-key TYPE:PROP[,PROP]] [--unkeyed-labels ACTION] [--dry-run] [--json] [--allow-plaintext]";
    const PUSH_HINT: &str = "Run 'relate push --help' for more information.";
    const INFO: &str = "Usage: relate info --database PATH [--json]";
    const INFO_HINT: &str = "Run 'relate info --help' for more information.";
    const AGENT_CONTEXT: &str = "Usage: relate agent-context [COMMAND]";
    const AGENT_CONTEXT_HINT: &str = "Run 'relate agent-context --help' for more information.";
    // One struct per case (named fields: the tuple form's adjacent
    // `&[&str]`/`&str` pairs let a transposition compile clean). The
    // feature gate is data, so a new command or bundle changes one row,
    // not a new `if cfg!` block.
    struct UsageCase {
        compiled_in: bool,
        args: &'static [&'static str],
        usage: &'static str,
        hint: &'static str,
    }
    let rows: &[UsageCase] = &[
        UsageCase {
            compiled_in: true,
            args: &["--bogus"],
            usage: TOP,
            hint: TOP_HINT,
        },
        UsageCase {
            compiled_in: true,
            args: &["RETURN 1", "extra"],
            usage: TOP,
            hint: TOP_HINT,
        },
        UsageCase {
            compiled_in: true,
            args: &["--namespace", "nosuch"],
            usage: TOP,
            hint: TOP_HINT,
        },
        UsageCase {
            compiled_in: true,
            args: &["clone"],
            usage: CLONE,
            hint: CLONE_HINT,
        },
        UsageCase {
            compiled_in: true,
            args: &["-c", "RETURN 1", "clone", "src", "dst"],
            usage: CLONE,
            hint: CLONE_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "skill"),
            args: &["skill"],
            usage: SKILL,
            hint: SKILL_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "skill"),
            args: &["skill", "frobnicate"],
            usage: SKILL,
            hint: SKILL_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "upgrade"),
            args: &["upgrade", "--bogus"],
            usage: UPGRADE,
            hint: UPGRADE_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "upgrade"),
            args: &["update", "--bogus"],
            usage: UPGRADE,
            hint: UPGRADE_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "telemetry"),
            args: &["telemetry", "foo"],
            usage: TELEMETRY,
            hint: TELEMETRY_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "pull"),
            args: &["pull", "./graph"],
            usage: PULL,
            hint: PULL_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "pull"),
            args: &[
                "--namespace",
                "system",
                "pull",
                "--query",
                "MATCH (n) RETURN n",
                "./graph",
            ],
            usage: PULL,
            hint: PULL_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "push"),
            args: &["push", "./graph", "--to", "n.env"],
            usage: PUSH,
            hint: PUSH_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "push"),
            args: &["push", "--bogus", "--to", "n.env", "--database", "./graph"],
            usage: PUSH,
            hint: PUSH_HINT,
        },
        // The bare `run` — the missing-name usage error prints the verb's
        // own synopsis and hint beside its dynamic listing.
        UsageCase {
            compiled_in: cfg!(feature = "extensions"),
            args: &["run"],
            usage: "Usage: relate run <name> [flags]",
            hint: "Run 'relate run --help' for more information.",
        },
        UsageCase {
            compiled_in: cfg!(feature = "info"),
            args: &["info"],
            usage: INFO,
            hint: INFO_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "info"),
            args: &["-c", "RETURN 1", "info", "db"],
            usage: INFO,
            hint: INFO_HINT,
        },
        UsageCase {
            compiled_in: cfg!(feature = "agent-context"),
            args: &["agent-context", "query", "extra"],
            usage: AGENT_CONTEXT,
            hint: AGENT_CONTEXT_HINT,
        },
    ];
    for case in rows.iter().filter(|case| case.compiled_in) {
        let args = case.args;
        let (status, stdout, stderr) = run_cli(Opts::new(args).in_home(home.path()));
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: stdout {stdout}");
        assert!(
            stderr.contains(case.usage),
            "{args:?}: missing {:?}:\n{stderr}",
            case.usage
        );
        assert!(
            stderr.contains(case.hint),
            "{args:?}: missing {:?}:\n{stderr}",
            case.hint
        );
        for leaked in ["Options:", "Commands:", "--durability-mode"] {
            assert!(
                !stderr.contains(leaked),
                "{args:?}: full help leaked {leaked:?}:\n{stderr}"
            );
        }
    }
}

// --- telemetry parity with the top-level help ---

#[cfg(feature = "telemetry")]
#[test]
fn test_command_help_emits_the_help_events_and_no_command_event() {
    let mock = MockMixpanel::start();
    let home = tempdir();
    // The help run spools its events at exit (ADR-0545 §1); the shared
    // delivery loop drains them through follow-up runs (ADR-0545 §2) —
    // plain query runs, because a ~10 ms help run's own drain loses the
    // exit race and leaves the claim for a run that lives long enough.
    let events = mock.deliver_spooled(
        || {
            let (status, stdout, stderr) = run_cli(
                Opts::new(&["clone", "--help"])
                    .in_home(home.path())
                    .with_endpoint(&mock.url()),
            );
            assert!(status.success(), "clone --help failed: {stderr}");
            assert!(stdout.starts_with("Usage: relate clone"), "{stdout}");
        },
        support::FollowupRun {
            args: &[support::DELIVERY_WORK_QUERY],
            envs: &[],
            home: Some(home.path()),
            cwd: None,
        },
        |events| {
            events
                .iter()
                .any(|e| e["event"] == "RELATE-CLI_STARTUP" && e["properties"]["kind"] == "help")
                && events.iter().any(|e| e["event"] == "RELATE-CLI_HELP")
        },
    );
    assert!(
        !events.iter().any(|e| e["event"] == "RELATE-CLI_COMMAND"),
        "per-command help must not emit COMMAND: {events:?}"
    );
}
