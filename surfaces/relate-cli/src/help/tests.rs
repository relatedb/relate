//! The command-table and top-level page tests (kept out of `mod.rs` under
//! the file-size budget; per-command page tests live beside their page).

use super::*;

#[test]
fn each_options_entry_renders_whole_and_in_table_order() {
    // One atomic entry per flag: its headline and every continuation line
    // appear contiguously, in table order — a mid-block insertion cannot
    // separate a continuation from its flag (the --width/--max-rows drift
    // this pins), and no entry can vanish.
    let help = top_level_with(crate::agent_mode::FeedbackSurface::Human, true);
    let mut cursor = 0;
    for entry in options::OPTION_ENTRIES {
        let block = options::entry_text(entry, "QUERY");
        // The entry's flag column carries the names its `flags` list
        // documents (the reverse is not derivable: `--format`'s
        // description follows its flag column after a single space, and the
        // `--` row's column is the separator itself).
        for flag in entry.flags {
            assert!(
                block.contains(flag),
                "{flag:?} is not spelled in its own entry: {block:?}"
            );
        }
        let at = help[cursor..]
            .find(&block)
            .unwrap_or_else(|| panic!("entry not rendered whole, in order:\n{block}"));
        cursor += at + block.len();
    }
}

#[test]
fn every_placeholder_in_the_table_is_substituted() {
    // A typo'd `{token}` in an entry would otherwise render literally with
    // nothing tying the table to the substituter: run each line through the
    // canonical `substitute` and require no brace survives (no provider can
    // emit one).
    for entry in options::OPTION_ENTRIES {
        for line in options::entry_lines(entry) {
            let filled = options::substitute(line, "QUERY");
            assert!(
                !filled.contains('{') && !filled.contains('}'),
                "unsubstituted placeholder syntax in {line:?}: {filled:?}"
            );
        }
    }
}

#[test]
fn the_query_page_documents_the_session_vocabulary() {
    // The query session answers the same flags the top-level run documents
    // (`--version` included: the parse loop answers it after the verb too),
    // and its page lists every one of them.
    let help = crate::help::command_help(crate::help::HelpTopic::Query);
    for flag in session_flags() {
        assert!(help.contains(flag), "{flag} missing from query --help");
    }
}

/// Every help page this build renders, one row per page: its name, the
/// page with examples on, and the examples slice that page carries (the
/// top level's own for the top row). The one inventory every page-level
/// test iterates — the examples contract and the no-internal-ids guard
/// walk the same list, so a page renderer added anywhere else sits
/// outside every guard until it joins this one.
fn every_help_page() -> Vec<(&'static str, String, &'static [crate::examples::Example])> {
    let mut pages = vec![(
        "top level",
        top_level_with(crate::agent_mode::FeedbackSurface::Human, true),
        crate::examples::TOP_LEVEL,
    )];
    for spec in commands() {
        pages.push((
            spec.word,
            command_help_with(spec.topic, true),
            spec.examples,
        ));
    }
    pages
}

#[test]
fn every_help_page_carries_one_or_two_runnable_looking_examples() {
    // The top level and every compiled-in command: an `Examples:` section
    // with at most two entries, each a `# comment` above a `relate`
    // command line. Few on purpose; the e2e suite runs the safe ones.
    for (name, page, examples) in every_help_page() {
        assert!(
            (1..=2).contains(&examples.len()),
            "{name}: {} examples, want 1-2",
            examples.len()
        );
        assert_eq!(page.matches("\nExamples:\n").count(), 1, "{name}:\n{page}");
        for (comment, command) in examples {
            assert!(!comment.is_empty(), "{name}: empty comment");
            assert!(command.starts_with("relate "), "{name}: {command}");
            let block = format!("  # {comment}\n  {command}\n");
            assert!(page.contains(&block), "{name}: {block:?} missing:\n{page}");
        }
    }
}

#[test]
fn dropping_the_examples_leaves_the_rest_of_every_page_unchanged() {
    // The top level keeps its pointer line last either way; the page
    // without examples is the page as it was before them.
    let pointer = "Run 'relate <COMMAND> --help' for details on a command.\n";
    let human = crate::agent_mode::FeedbackSurface::Human;
    let (with, without) = (top_level_with(human, true), top_level_with(human, false));
    assert!(with.contains("\nExamples:\n") && !without.contains("Examples:"));
    let head = without.strip_suffix(pointer).expect("pointer line");
    assert!(with.starts_with(head), "the head of the page is unchanged");
    assert!(with.ends_with(pointer), "the pointer line is unchanged");
    // A command page only gains a suffix.
    for spec in commands() {
        let (on, off) = (
            command_help_with(spec.topic, true),
            command_help_with(spec.topic, false),
        );
        assert!(!off.contains("Examples:"), "{}", spec.word);
        assert!(on.starts_with(&off), "{}: only a suffix", spec.word);
    }
}

#[test]
fn every_command_example_is_for_its_own_command() {
    for spec in commands() {
        for (_, command) in spec.examples {
            assert!(
                command.starts_with(&format!("relate {} ", spec.word)),
                "{}: {command}",
                spec.word
            );
        }
    }
}

/// Flags every compiled-in command accepts without declaring them in its
/// table — the help pre-scan the parser runs before any grammar, so a page
/// may print them.
const UNIVERSAL_FLAGS: &[&str] = &["--help"];

/// Every `--long-flag` token a help page prints, in first-seen order — the
/// reverse-direction sweep's reader (`--flag`, `--from`, and the flags a page
/// spells inside prose alike).
fn long_flags(page: &str) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    let mut rest = page;
    while let Some(at) = rest.find("--") {
        let tail = &rest[at + 2..];
        let len = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(tail.len());
        if len > 0 {
            let flag = format!("--{}", &tail[..len]);
            if !flags.contains(&flag) {
                flags.push(flag);
            }
        }
        rest = &tail[len..];
    }
    flags
}

#[test]
fn command_flags_are_all_on_their_help_page() {
    for spec in commands() {
        let help = (spec.help)();
        for flag in spec.flags.iter() {
            assert!(
                help.contains(flag),
                "{flag} missing from {} help",
                spec.word
            );
        }
        // The reverse direction: every `--flag` the page prints is a flag
        // the parser accepts — a page can document no flag the grammar
        // rejects (the `list --available` drift this pin was added for).
        for flag in long_flags(&help) {
            assert!(
                spec.flags.contains(&flag.as_str()) || UNIVERSAL_FLAGS.contains(&flag.as_str()),
                "{} help prints {flag}, which is not in its flag list",
                spec.word
            );
        }
        // Lifecycle pages omit the query-run options: the reverse sweep
        // above already pins that a page prints no flag outside its own
        // `spec.flags`, and only `query` (pre-verb options, ADR-0200) and
        // `serve` (its own storage flag, ADR-0470) list
        // `--durability-mode` — a lifecycle page cannot print it without
        // failing that sweep.
    }
}

#[test]
fn top_level_starts_with_both_usage_lines_and_ends_with_the_footer() {
    let help = top_level(crate::agent_mode::FeedbackSurface::Human);
    assert!(help.starts_with(
        "Usage: relate [OPTIONS] [--database PATH] [QUERY]\n       relate <COMMAND> [ARGS]\n"
    ));
    assert_eq!(
        help.lines().last(),
        Some("Run 'relate <COMMAND> --help' for details on a command.")
    );
}

#[test]
fn top_level_commands_follow_the_build() {
    let help = top_level(crate::agent_mode::FeedbackSurface::Human);
    let listed: Vec<&CommandSpec> = commands()
        .iter()
        .filter(|spec| spec.topic.surface() != Some(crate::agent_mode::FeedbackSurface::Agent))
        .collect();
    let width = command_column_width(&listed);
    // The gate half is the per-command coverage: only the compiled-in
    // decision per word is asserted against the page (the byte-exact
    // rendering, blurbs included, is derived from COMMANDS and pinned by
    // commands_table_is_the_single_source_of_truth below, so this table
    // never copies a blurb).
    for (compiled_in, word) in [
        (true, "query"),
        (true, "clone"),
        (cfg!(feature = "info"), "info"),
        (cfg!(feature = "pull"), "pull"),
        (cfg!(feature = "push"), "push"),
        (cfg!(feature = "skill"), "skill"),
        (cfg!(feature = "upgrade"), "upgrade"),
        (cfg!(feature = "telemetry"), "telemetry"),
        (true, "feedback"),
        (false, "agent-feedback"),
        (cfg!(feature = "extensions"), "run"),
        // `extension` is reserved in COMMAND_WORDS in every build but
        // listed only where compiled in (ADR-0431 §3) — the row must
        // never drift out of this gate.
        (cfg!(feature = "extensions"), "extension"),
        (cfg!(feature = "serve"), "serve"),
        (cfg!(feature = "agent-context"), "agent-context"),
    ] {
        let line_prefix = format!("  {word:<width$}");
        assert_eq!(
            help.contains(&line_prefix),
            compiled_in,
            "{word} (compiled_in={compiled_in}):\n{help}"
        );
    }
}

/// Each mode's top level lists only its own feedback surface (ADR-0588 §3).
#[test]
fn the_commands_section_lists_only_the_runs_feedback_surface() {
    use crate::agent_mode::FeedbackSurface;
    let words = |surface| -> Vec<String> {
        top_level(surface)
            .lines()
            .skip_while(|line| *line != "Commands:")
            .skip(1)
            .take_while(|line| !line.is_empty())
            .map(|line| {
                line.split_whitespace()
                    .next()
                    .expect("a command row has a word")
                    .to_string()
            })
            .collect()
    };
    let human = words(FeedbackSurface::Human);
    assert!(human.iter().any(|word| word == "feedback"));
    assert!(!human.iter().any(|word| word == "agent-feedback"));
    let agent = words(FeedbackSurface::Agent);
    assert!(agent.iter().any(|word| word == "agent-feedback"));
    assert!(!agent.iter().any(|word| word == "feedback"));
}

#[test]
fn commands_table_is_the_single_source_of_truth() {
    // Each compiled-in topic exactly once, words unique, and every help
    // page opening with its shared synopsis.
    for (i, spec) in commands().iter().enumerate() {
        assert!(!commands()[..i].iter().any(|s| s.topic == spec.topic));
        assert!(!commands()[..i].iter().any(|s| s.word == spec.word));
        let help = (spec.help)();
        assert_single_usage(&help, &spec.usage);
    }
    // The Commands: section renders the table in order through the one
    // shared row builder, less the other mode's feedback surface.
    let help = top_level(crate::agent_mode::FeedbackSurface::Human);
    let section: Vec<&str> = help
        .lines()
        .skip_while(|line| *line != "Commands:")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    let listed: Vec<&CommandSpec> = commands()
        .iter()
        .filter(|spec| spec.topic.surface() != Some(crate::agent_mode::FeedbackSurface::Agent))
        .collect();
    let width = command_column_width(&listed);
    let expected: Vec<String> = listed
        .into_iter()
        .map(|spec| command_row(spec, width))
        .collect();
    assert_eq!(section, expected);
}

#[test]
fn top_level_omits_lifecycle_flags() {
    let help = top_level(crate::agent_mode::FeedbackSurface::Human);
    for absent in [
        // The skill verb's value-taking harness selector (`--agent NAME`) —
        // not the substring `--agent`, which the run-level `--agent-mode`
        // (ADR-0407) legitimately contains.
        "--agent NAME",
        "--to-snapshot-format",
        "--allow-plaintext",
        "--max-content-bytes",
        "--include-hidden",
        "--from FILE",
        "--to CREDS",
        "--rel-mode MODE",
        "--unkeyed-labels ACTION",
        "--http PORT",
        "--mcp PORT|stdio",
        "--auth USER:PASSWORD",
    ] {
        assert!(
            !help.contains(absent),
            "{absent:?} must not appear:\n{help}"
        );
    }
}

#[test]
fn top_level_keeps_the_query_run_options() {
    let help = top_level(crate::agent_mode::FeedbackSurface::Human);
    for present in [
        "-h, --help",
        "--version",
        "-d, --database PATH",
        "--json",
        "--csv",
        "--markdown",
        "--jsonl",
        "--noheader",
        "--mode MODE",
        "--param N=V",
        "--snapshot-format F",
        "--markdown-body-properties LIST",
        "--wal-dir DIR",
        "--checkpoint-interval-secs N",
        "--durability-mode MODE",
        "--logical-graph-size-limit SIZE",
        "--expression-memory-limit SIZE",
        "--concurrent-logical-workspace-limit SIZE",
        "--max-traversal-depth N",
        "--timeout DURATION",
        "--read-only[=FRESH]",
        "--load-csv-dir PATH",
        "--namespace NAME",
        "-c, --command Q",
        "`;`-separated list",
    ] {
        assert!(help.contains(present), "{present:?} missing:\n{help}");
    }
    assert_eq!(
        help.contains("--fetch-params N=SRC"),
        cfg!(feature = "fetch-params")
    );
    assert_eq!(
        help.contains("readline-style editing"),
        cfg!(feature = "repl")
    );
}

#[test]
fn hints_name_the_canonical_command() {
    assert_eq!(
        HelpScope::TopLevel.hint(),
        "Run 'relate --help' for more information."
    );
    assert_eq!(
        HelpScope::Command(HelpTopic::Clone).hint(),
        "Run 'relate clone --help' for more information."
    );
}

/// No internal record identifier (an `ADR-*` decision record or a `LABS-*`
/// tracker card) rides any help page: the pages ship inside the public
/// binary, and those identifiers resolve only inside the repository, so a
/// user reading them is noise that erodes trust in the rest of the text.
/// The reasoning the identifiers pointed at stays in the repo, where the
/// links actually resolve. Walks the shared page inventory (examples
/// included) — a page carrying one fails here.
#[test]
fn no_help_page_carries_internal_record_ids() {
    for (name, page, _) in every_help_page() {
        assert!(
            !page.contains("ADR-") && !page.contains("LABS-"),
            "{name}: internal record id on the page:\n{page}"
        );
    }
}
