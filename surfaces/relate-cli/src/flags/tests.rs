//! Flag-parser tests (moved out of flags.rs, the skill_cmd/serve_cmd
//! dir-module pattern, to keep both under the repo's 1k-line file budget).

use super::lifecycle::{help_or, COMMAND_WORDS};
use super::*;

fn clone_command() -> LifecycleCommand {
    LifecycleCommand::Clone {
        cmd: crate::clone_cmd::CloneCmd {
            source: "a".into(),
            dest: "b".into(),
            options: Default::default(),
        },
    }
}

/// A parsed `serve` command, built through the real grammar: the surface
/// flag of whichever surface this build compiled in, through
/// `serve_args`'s canonical `any_surface_flag` test helper. Compiled
/// only beside a surface — the bare `serve` bundle (ADR-0232) has no
/// surface flag to build a serve command with, so the helper and its
/// one test are absent there.
#[cfg(feature = "surface")]
fn serve_command() -> LifecycleCommand {
    LifecycleCommand::Serve {
        cmd: crate::serve_args::parse_serve_args(&crate::args(&[
            "--database",
            "db",
            crate::serve_args::any_surface_flag(),
            "7474",
        ]))
        .expect("the serve grammar accepts this tail"),
    }
}

#[test]
fn clone_rejects_a_query_and_owns_its_verb() {
    let cmd = clone_command();
    // The descriptor row owns the policy (ADR-0306 §6): clone runs no
    // query engine, so its row flags `rejects_query`.
    assert!(HelpTopic::Clone.spec().rejects_query);
    assert_eq!(cmd.name(), "clone");
}

#[test]
fn a_rejects_query_command_conflicts_with_a_query_or_any_access_flag() {
    // The parse-tail conflict decision (ADR-0306 §6's shared check): a
    // command that runs no query engine refuses a query — the
    // pre-existing case — and a pre-verb access flag alike. A pre-verb
    // `--read-only` before `clone`, a command that writes, is the user's
    // fence, not a suggestion to drop.
    let clone = clone_command();
    let conflict = "clone cannot be combined with a query or an access flag (--read-only)";
    assert_eq!(
        clone.conflict_error(Some("RETURN 1"), None),
        Some((HelpTopic::Clone, conflict.to_string()))
    );
    for access in [
        Some(relate::OpenMode::ReadOnly),
        Some(relate::OpenMode::Follow),
    ] {
        assert_eq!(
            clone.conflict_error(None, access),
            Some((HelpTopic::Clone, conflict.to_string())),
            "{access:?}"
        );
    }
    // A clean combination conflicts with nothing.
    assert_eq!(clone.conflict_error(None, None), None);
}

#[cfg(feature = "info")]
#[test]
fn info_conflicts_with_a_preverb_access_flag() {
    // `--read-only info --database X`, the second new case of the
    // generalized check, at the decision level.
    let cmd = LifecycleCommand::Info {
        cmd: crate::info_cmd::InfoCmd {
            database: "a".into(),
            json: false,
        },
    };
    assert_eq!(
        cmd.conflict_error(None, Some(relate::OpenMode::Follow)),
        Some((
            HelpTopic::Info,
            "info cannot be combined with a query or an access flag (--read-only)".to_string()
        ))
    );
}

#[cfg(feature = "surface")]
#[test]
fn serve_rejects_a_preverb_access_flag_naming_the_postverb_spelling() {
    // ADR-0306 §6: a pre-verb `--read-only` (either freshness) may not
    // reach a writable serve; the one decision's serve arm answers it with
    // the post-verb spelling — structurally first, even when a query rides
    // the same line.
    let serve = serve_command();
    let redirect = "serve takes --read-only after the verb, not before it \
             (relate serve --read-only …)"
        .to_string();
    // Either freshness gets the same redirect — the decision keys on the
    // mode's read-only predicate, not the value (ADR-0480).
    for (query, access) in [
        (None, Some(relate::OpenMode::ReadOnly)),
        (None, Some(relate::OpenMode::Follow)),
        // The serve arm's precedence is structural, not call order: a
        // query beside the pre-verb flag still gets serve's own message
        // (and a no-flag serve with a query falls through to the shared
        // conflict, pinned below).
        (Some("RETURN 1"), Some(relate::OpenMode::ReadOnly)),
    ] {
        assert_eq!(
            serve.conflict_error(query, access),
            Some((HelpTopic::Serve, redirect.clone())),
            "{query:?} {access:?}"
        );
    }
    assert_eq!(
        serve.conflict_error(Some("RETURN 1"), None),
        Some((
            HelpTopic::Serve,
            "serve cannot be combined with a query or an access flag (--read-only)".to_string()
        ))
    );
    // A clean serve conflicts with nothing; every other command's
    // pre-verb access flags stay the shared conflict's business (pinned
    // above, for clone and info).
    assert_eq!(serve.conflict_error(None, None), None);
}

fn argv(args: &[&str]) -> Vec<String> {
    std::iter::once("relate".to_string())
        .chain(args.iter().map(|s| (*s).to_string()))
        .collect()
}

#[test]
fn query_parses_statement_and_database_in_either_order() {
    for args in [
        argv(&["query", "RETURN 2", "--database", "/tmp/triha/my-db"]),
        argv(&["query", "--database", "/tmp/triha/my-db", "RETURN 2"]),
    ] {
        let flags = parse(&args);
        assert!(flags.lifecycle.is_none(), "{args:?}");
        assert_eq!(flags.db_path, "/tmp/triha/my-db");
        assert_eq!(flags.query.as_deref(), Some("RETURN 2"));
    }
}

#[test]
fn query_defaults_database_to_memory_and_statement_to_unset() {
    let flags = parse(&argv(&["query"]));
    assert!(flags.lifecycle.is_none());
    assert!(flags.explicit_query);
    assert_eq!(flags.db_path, ":memory:");
    assert!(flags.query.is_none());

    let bare = parse(&argv(&[]));
    assert!(!bare.explicit_query);
    assert!(bare.query.is_none());
}

#[test]
fn query_accepts_command_flag_and_session_flags() {
    let flags = parse(&argv(&[
        "query",
        "-c",
        "RETURN 1",
        "--database",
        "./db",
        "--json",
    ]));
    assert_eq!(flags.db_path, "./db");
    assert_eq!(flags.query.as_deref(), Some("RETURN 1"));
    assert_eq!(flags.mode, Some(Mode::Json));
}

#[test]
fn markdown_body_properties_flag_parses_the_one_grammar() {
    // ADR-0460: the same SPEC grammar the env variable and the clone
    // flag use — bare entries as the ordered default, Label= overrides.
    let flags = parse(&argv(&[
        "--database",
        "./vault",
        "--markdown-body-properties",
        "summary,notes;Book=summary,review",
        "RETURN 1",
    ]));
    let config = flags.markdown_body_properties.expect("flag must parse");
    assert_eq!(config.default, vec!["summary", "notes"]);
    assert_eq!(
        config.per_label.get("Book").unwrap(),
        &vec!["summary", "review"]
    );
    // A bad value is the flag's usage error (exit 2) through
    // parse_value — the contract every value flag shares; the grammar's
    // own rejection cases are unit-tested on
    // `MarkdownBodyProperties::parse` in relate-storage.
}

#[test]
fn database_flag_names_a_store_even_when_the_path_is_a_command_word() {
    let dotted = parse(&argv(&["--database", "./query", "RETURN 1"]));
    assert!(dotted.lifecycle.is_none());
    assert_eq!(dotted.db_path, "./query");
    assert_eq!(dotted.query.as_deref(), Some("RETURN 1"));

    let word = parse(&argv(&["--database", "query", "RETURN 1"]));
    assert!(word.lifecycle.is_none());
    assert_eq!(word.db_path, "query");
    assert_eq!(word.query.as_deref(), Some("RETURN 1"));
}

#[test]
fn end_of_options_keeps_a_command_word_as_a_statement() {
    let flags = parse(&argv(&["--", "upgrade"]));
    assert!(flags.lifecycle.is_none());
    assert_eq!(flags.query.as_deref(), Some("upgrade"));

    let tail = parse(&argv(&["query", "clone"]));
    assert!(tail.lifecycle.is_none());
    assert_eq!(tail.query.as_deref(), Some("clone"));
}

#[test]
fn short_database_flag_is_the_store() {
    for args in [
        argv(&["-d", "/tmp/triha/my-db", "RETURN 2"]),
        argv(&["query", "-d", "/tmp/triha/my-db", "RETURN 2"]),
        argv(&["query", "RETURN 2", "-d", "/tmp/triha/my-db"]),
    ] {
        let flags = parse(&args);
        assert!(flags.lifecycle.is_none(), "{args:?}");
        assert_eq!(flags.db_path, "/tmp/triha/my-db");
        assert_eq!(flags.query.as_deref(), Some("RETURN 2"));
    }
}

#[test]
fn format_is_an_alias_of_mode_and_toon_is_a_shortcut() {
    // ADR-0239 (LABS-232).
    let flags = parse(&argv(&["--format", "json", "RETURN 1"]));
    assert_eq!(flags.mode, Some(Mode::Json));
    let flags = parse(&argv(&["query", "--format", "toon", "RETURN 1"]));
    assert_eq!(flags.mode, Some(Mode::Toon));
    let flags = parse(&argv(&["--toon", "RETURN 1"]));
    assert_eq!(flags.mode, Some(Mode::Toon));
    // Last output flag wins, across the alias family.
    let flags = parse(&argv(&["--mode", "csv", "--format", "json", "RETURN 1"]));
    assert_eq!(flags.mode, Some(Mode::Json));
}

#[test]
fn timeout_parses_a_duration_and_zero_disables() {
    // ADR-0241 (LABS-234).
    let flags = parse(&argv(&["--timeout", "250ms", "RETURN 1"]));
    assert_eq!(flags.timeout, Some(std::time::Duration::from_millis(250)));
    let flags = parse(&argv(&["query", "--timeout", "2m", "RETURN 1"]));
    assert_eq!(flags.timeout, Some(std::time::Duration::from_secs(120)));
    let flags = parse(&argv(&["--timeout", "0", "RETURN 1"]));
    assert_eq!(flags.timeout, None);
    let flags = parse(&argv(&["RETURN 1"]));
    assert_eq!(flags.timeout, None);
}

#[test]
fn query_is_a_command_word_in_every_bundle() {
    assert!(COMMAND_WORDS.contains(&"query"));
}

#[test]
fn every_compiled_in_command_word_is_a_command_word() {
    // The descriptor table never names a word the first-positional scan
    // wouldn't recognize (COMMAND_WORDS is a superset: it also lists the
    // compiled-out words and the `update` alias).
    for spec in crate::help::commands() {
        assert!(
            COMMAND_WORDS.contains(&spec.word),
            "{} missing from COMMAND_WORDS",
            spec.word
        );
    }
}

#[test]
fn snapshot_is_no_longer_a_command_word() {
    // ADR-0282: the verb shipped in internal releases only, so it breaks
    // cleanly. `snapshot` is not a command word; as a first positional
    // it is a statement.
    assert!(!COMMAND_WORDS.contains(&"snapshot"));
}

#[test]
fn not_included_keeps_its_word_and_allows_a_query_run() {
    let cmd = LifecycleCommand::NotIncluded { command: "pull" };
    // NotIncluded runs no engine and answers no conflict (ADR-0306 §6's
    // decision consults the descriptor row, which a NotIncluded word has
    // none of).
    assert_eq!(cmd.conflict_error(Some("RETURN 1"), None), None);
    assert_eq!(cmd.name(), "pull");
}

#[cfg(feature = "info")]
#[test]
fn info_rejects_a_query_and_owns_its_verb() {
    let cmd = LifecycleCommand::Info {
        cmd: crate::info_cmd::InfoCmd {
            database: "a".into(),
            json: false,
        },
    };
    assert!(HelpTopic::Info.spec().rejects_query);
    assert_eq!(cmd.name(), "info");
}

#[test]
fn help_wins_over_the_query_conflict() {
    let cmd = LifecycleCommand::Help {
        topic: HelpTopic::Clone,
    };
    assert_eq!(
        cmd.conflict_error(Some("RETURN 1"), None),
        None,
        "help must print before the query/access conflict"
    );
    // The decision agrees: even a query and an access flag together do
    // not conflict with help.
    assert_eq!(
        cmd.conflict_error(Some("RETURN 1"), Some(relate::OpenMode::Follow)),
        None
    );
    assert_eq!(cmd.name(), "clone");
}

#[test]
fn help_or_prescans_the_rest_for_both_help_spellings() {
    let topic = HelpTopic::Clone;
    for rest in [
        vec!["a".to_string(), "--help".to_string()],
        vec!["--bogus".to_string(), "-h".to_string()],
        vec!["--help".to_string()],
    ] {
        match help_or(topic, &rest, || LifecycleCommand::NotIncluded {
            command: "x",
        }) {
            LifecycleCommand::Help { topic: got } => assert_eq!(got, topic),
            other => panic!("{rest:?}: expected help, got {other:?}"),
        }
    }
    // No help token: the sub-parser runs.
    match help_or(topic, &["a".to_string()], || {
        LifecycleCommand::NotIncluded { command: "x" }
    }) {
        LifecycleCommand::NotIncluded { command } => assert_eq!(command, "x"),
        other => panic!("expected the sub-parser result, got {other:?}"),
    }
}

/// ADR-0463: the expression cap takes the shared SIZE grammar plus the
/// explicit `none` opt-out.
#[test]
fn expression_memory_limit_flag_parses_size_and_none() {
    let flags = parse(&argv(&["--expression-memory-limit", "128mib", "RETURN 1"]));
    assert_eq!(flags.expression_memory_limit, Some(128 << 20));
    let flags = parse(&argv(&["--expression-memory-limit", "none", "RETURN 1"]));
    assert_eq!(flags.expression_memory_limit, Some(0));
    let flags = parse(&argv(&["--expression-memory-limit", "0", "RETURN 1"]));
    assert_eq!(flags.expression_memory_limit, Some(0));
}

/// LABS-903 (ADR-0500): the `--load` source parses (value-taking,
/// last-one-wins, in every build), and the memory-target load implies the
/// ephemeral open through the one resolution — any other target keeps the
/// access slot's, and a bare `:memory:` `--database` is the plain memory
/// open.
#[test]
fn the_load_flag_parses_and_implies_the_ephemeral_mode_for_the_memory_target() {
    let flags = parse(&argv(&[
        "--load",
        "a.rl",
        "--load",
        "https://example.com/b.rl",
        "RETURN 1",
    ]));
    assert_eq!(
        flags.load.as_deref(),
        Some("https://example.com/b.rl"),
        "last one wins"
    );
    assert_eq!(flags.db_path, ":memory:", "the target default");
    assert_eq!(flags.open_mode(), relate::OpenMode::Ephemeral);

    // Any other target keeps the access slot's resolution. (A `--read-only`
    // beside a durable-target `--load` is a parse-time usage error — it
    // exits, so it is pinned by the e2e suite, not here.)
    let flags = parse(&argv(&[
        "--database",
        "out.rl",
        "--load",
        "a.rl",
        "RETURN 1",
    ]));
    assert_eq!(flags.open_mode(), relate::OpenMode::Writable);

    // A bare :memory: --database is the plain memory open, and a load-less
    // run never implies anything.
    let flags = parse(&argv(&["RETURN 1"]));
    assert_eq!(flags.load, None);
    assert_eq!(flags.open_mode(), relate::OpenMode::Writable);
}

#[test]
fn help_and_version_win_over_the_load_conflict() {
    // Round 4, F-2-6: the end-of-parse conflict stage is one block under
    // one precedence — `--help` (which breaks the parse loop) and
    // `--version` are no-side-effect exits the caller owns, so the
    // `--load` + `--read-only=live` conflict cannot exit 2 ahead of them.
    // The pin is that parse RETURNS with the exit flag set — the previous
    // shape died on the conflict's `usage_error` before reaching here.
    let flags = parse(&argv(&["--load", "x.rl", "--read-only=live", "--help"]));
    assert!(flags.show_help, "help must win over the --load conflict");
    let flags = parse(&argv(&["--load", "x.rl", "--read-only=live", "--version"]));
    assert!(
        flags.show_version,
        "version must win over the --load conflict"
    );
    // Without the exit flags the conflict still refuses (exit 2 through
    // `usage_error`) — pinned end-to-end by the e2e suite; here the
    // no-conflict shape stays parseable.
    let flags = parse(&argv(&["--load", "x.rl"]));
    assert!(flags.load.as_deref() == Some("x.rl"));
    // A lifecycle verb's own help page wins the same way: the conflict
    // stage exempts the help-shaped commands (`Help`/`RunHelp`/
    // `NotIncluded`) beside the two exit flags, so this parse returns
    // the page instead of exiting 2 on the conflict.
    let flags = parse(&argv(&[
        "--load",
        "x.rl",
        "--read-only=live",
        "serve",
        "--help",
    ]));
    assert!(
        flags
            .lifecycle
            .as_ref()
            .is_some_and(LifecycleCommand::is_help_shaped),
        "a verb's help page must win over the --load conflict"
    );
}

#[test]
fn snapshot_format_parses_codecs_and_parquet_wide() {
    // ADR-0534: the flag's values are the codec names plus the
    // `parquet-wide` opt-in — an option on Parquet, never a codec of its
    // own. An uncompiled choice parses and fails closed at open
    // (ADR-0105), so parsing is unconditional here.
    let flags = parse(&argv(&["--snapshot-format", "parquet-wide"]));
    assert_eq!(flags.snapshot_format, Some(SnapshotFormat::ParquetWide));
    assert_eq!(
        flags.snapshot_format.map(SnapshotFormat::codec),
        Some(relate::SnapshotCodecKind::Parquet)
    );
    assert_eq!(
        flags.snapshot_format.map(SnapshotFormat::wide_columns),
        Some(true)
    );
    // The flag's grammar owns its unknown-value error: the expected
    // list names the opt-in beside the codecs, never the codec parser's
    // narrower set.
    let err = "parquet-wdie"
        .parse::<SnapshotFormat>()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown snapshot format 'parquet-wdie'"),
        "{err}"
    );
    // A codec spelling is unchanged, case-insensitive like the codec
    // parser, and opts nothing in.
    let flags = parse(&argv(&["--snapshot-format", "JSON"]));
    assert_eq!(
        flags.snapshot_format,
        Some(SnapshotFormat::Codec(relate::SnapshotCodecKind::Json))
    );
    assert_eq!(
        flags.snapshot_format.map(SnapshotFormat::wide_columns),
        Some(false)
    );
}
