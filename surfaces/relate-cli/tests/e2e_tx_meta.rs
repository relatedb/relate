//! The `--tx-meta` flag end to end through the CLI binary (ADR-0610):
//! the CLI's transaction-metadata channel — one `KEY=VALUE` pair per
//! flag, repeatable, merged in argv order — seeded through the embedded
//! `Connection::set_tx_metadata` channel before the session's first
//! statement. With `RELATE_PREVIEW=value-provenance` the seeded map is
//! readable back through `CALL tx.getMetaData` (the live per-transaction
//! map) until the next transaction boundary serializes it into that
//! commit's record and clears it; without the opt-in the run fails with
//! ADR-0371's authored preview error — the embedded channel's loud
//! refusal, never a silent drop.
//!
//! The retained record's query-surface readback (`provenance()`,
//! `history()`, `CALL db.transactions`) is the value-provenance
//! query-surface change's own; this suite pins the CLI channel that
//! feeds it. Every invocation goes through the shared [`support::run_cli`]
//! harness, which scrubs the ambient `RELATE_PREVIEW` before a gate-on
//! value is applied, so a developer shell that exports the opt-in
//! cannot turn a gate-off assertion into a gate-on one.

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

/// The gate-on environment: the `value-provenance` opt-in the metadata
/// channel requires (read once, at open).
const GATE_ON: &[(&str, &str)] = &[("RELATE_PREVIEW", "value-provenance")];

/// ADR-0371's authored disabled-feature text for the `value-provenance`
/// row; the gate-off assertion pins it byte-for-byte.
const DISABLED: &str = "semantic error: SemanticError: preview feature 'value-provenance' is not \
     enabled: set RELATE_PREVIEW=value-provenance";

/// A fresh tempdir and the store path inside it, in the argv form the CLI
/// expects. The returned guard must outlive every run against the store.
fn fresh_store() -> (tempfile::TempDir, String) {
    let dir = support::tempdir();
    let store = dir.path().join("store");
    let store = store.to_str().expect("utf-8 temp path").to_string();
    (dir, store)
}

/// One gate-on statement run against `db` in the suite's JSONL output
/// mode, with the given `--tx-meta` assignments and (optionally) a stdin
/// script instead of a statement argument.
fn run_tx_meta(
    db: &str,
    tx_meta: &[&str],
    script: Option<&str>,
) -> (std::process::ExitStatus, String, String) {
    let mut args = vec!["query", "--database", db, "--mode", "jsonl"];
    for pair in tx_meta {
        args.push("--tx-meta");
        args.push(pair);
    }
    let mut opts = Opts::new(&args).with_envs(GATE_ON);
    if let Some(script) = script {
        opts = opts.with_stdin(script.as_bytes());
    }
    run_cli(opts)
}

#[test]
fn tx_meta_reads_back_through_the_live_map_and_clears_at_the_boundary() {
    // The seeded map is the session's next transaction's metadata: the
    // live map reads it back before the write, the write commits it into
    // its record, and the map is cleared after the boundary (ADR-0610's
    // "cleared after"). A bare word decodes as a string, a JSON number
    // as an integer — `--param`'s value grammar.
    let (dir, db) = fresh_store();
    let script = "CALL tx.getMetaData() YIELD metadata RETURN metadata;\n\
                  CREATE (:Person {name: 'ada'});\n\
                  CALL tx.getMetaData() YIELD metadata RETURN metadata;";
    let (status, stdout, stderr) = run_tx_meta(&db, &["app=loader", "run=4711"], Some(script));
    assert!(status.success(), "the gated run failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            r#"{"metadata":{"app":"loader","run":4711}}"#,
            r#"{"metadata":{}}"#,
        ],
        "the live map before the write, then the cleared map after it \
         (the zero-row CREATE prints nothing in jsonl): {stdout}"
    );
    drop(dir);
}

#[test]
fn tx_meta_merges_repeated_keys_with_the_later_value_winning() {
    // ADR-0610's merge order: the channels merge in call order, later
    // keys win — argv order for the flag.
    let (dir, db) = fresh_store();
    let (status, stdout, stderr) = run_tx_meta(
        &db,
        &["app=one", "app=two"],
        Some("CALL tx.getMetaData() YIELD metadata RETURN metadata;"),
    );
    assert!(status.success(), "{stderr}");
    assert_eq!(stdout, "{\"metadata\":{\"app\":\"two\"}}\n", "{stdout}");
    drop(dir);
}

#[test]
fn tx_meta_without_the_opt_in_fails_the_authored_preview_error() {
    // The off semantics the ADR records for the embedded channel: on a
    // store opened without the opt-in the seed fails with the authored
    // preview error instead of silently dropping the map (exit 6, the
    // statement-time semantic class). The harness scrubs any ambient
    // opt-in, so this run is genuinely gate-off.
    let (dir, db) = fresh_store();
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "query",
        "--database",
        &db,
        "--mode",
        "jsonl",
        "--tx-meta",
        "app=loader",
        "CREATE (:Person {name: 'ada'});",
    ]));
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(stderr.contains(DISABLED), "{stderr}");
    drop(dir);
}

#[test]
fn tx_meta_usage_errors_name_the_flag_and_the_value_grammar() {
    // A missing value and a pair with no `=` are usage errors (exit 2)
    // before any store is opened — the shared value-flag contract.
    let (status, _stdout, stderr) = run_cli(Opts::new(&["query", "RETURN 1", "--tx-meta"]));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("--tx-meta requires a value (key=value)"),
        "{stderr}"
    );

    let (status, _stdout, stderr) =
        run_cli(Opts::new(&["query", "RETURN 1", "--tx-meta", "novalue"]));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("Error: invalid --tx-meta value: expected name=value"),
        "{stderr}"
    );
}
