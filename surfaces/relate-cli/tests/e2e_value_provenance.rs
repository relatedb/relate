//! The documented value-provenance demo, executed end to end against the
//! CLI binary (`docs/api/cli/examples/value_provenance.md`, ADR-0610):
//! three simulated agents write facts into one file store, each
//! invocation carrying its own `--tx-meta` map; the readbacks return
//! `provenance()` beside the values; one statement retracts everything
//! the scraper wrote through `CALL db.transactions()`; the supervisor's
//! own retract record carries its metadata. Every outcome is asserted,
//! never printed — the example's expected outputs are this suite's
//! assertions (AGENTS.md §2: examples are tests).
//!
//! Each CLI invocation is one open of the file store, and the retained
//! transaction records persist across them (the `transactions/`
//! checkpoint family) — that is what the retract's record listing reads.
//! `provenance()` is asked only where its attribution is exact in every
//! retained-history shape: a fact read in the open that wrote it, and a
//! fact written by the store's first commit — so the assertions stay
//! green when retained version chains persist across opens, and while
//! they collapse to the restored row's birth commit (ADR-0610's
//! retention consequence).
//!
//! Every invocation goes through the shared [`support::run_cli`] harness,
//! which scrubs the ambient `RELATE_PREVIEW` before a gate-on value is
//! applied, so a developer shell that exports the opt-in cannot flip a
//! gate-off assertion into a gate-on one.

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

/// The gate-on environment: the `value-provenance` opt-in the metadata
/// channel requires (read once, at open).
const GATE_ON: &[(&str, &str)] = &[("RELATE_PREVIEW", "value-provenance")];

/// A fresh tempdir and the store path inside it, in the argv form the CLI
/// expects. The returned guard must outlive every run against the store.
fn fresh_store() -> (tempfile::TempDir, String) {
    let dir = support::tempdir();
    let store = dir.path().join("graph.db");
    let store = store.to_str().expect("utf-8 temp path").to_string();
    (dir, store)
}

/// One gate-on statement session against `db` in the suite's JSONL output
/// mode, with the given `--tx-meta` assignments and either a stdin script
/// of `;`-separated statements or a one-shot positional statement.
fn run_session(
    db: &str,
    tx_meta: &[&str],
    script: Option<&str>,
    statement: Option<&str>,
) -> (std::process::ExitStatus, String, String) {
    let mut args = vec!["query", "--database", db, "--mode", "jsonl"];
    for pair in tx_meta {
        args.push("--tx-meta");
        args.push(pair);
    }
    if let Some(statement) = statement {
        args.push(statement);
    }
    let mut opts = Opts::new(&args).with_envs(GATE_ON);
    if let Some(script) = script {
        opts = opts.with_stdin(script.as_bytes());
    }
    run_cli(opts)
}

#[test]
fn three_agents_write_with_distinct_tx_meta_and_one_statement_retracts_one_agent() {
    let (dir, db) = fresh_store();

    // The ingest agent: its own invocation, its own map. The write has no
    // RETURN, so jsonl prints nothing — exit 0 is the acknowledgement.
    let (status, stdout, stderr) = run_session(
        &db,
        &["agent=ingest", "source=directory"],
        None,
        Some(
            "CREATE (:Person {name: 'Ada', role: 'engineer'}), \
             (:Person {name: 'Grace', role: 'mathematician'}), \
             (:Company {name: 'Acme'});",
        ),
    );
    assert!(status.success(), "the ingest write failed: {stderr}");
    assert_eq!(stdout, "", "a zero-row write prints nothing in jsonl");

    // The curator agent: links Ada to Acme, then reads its own link back
    // — the value it just wrote beside the provenance of that value,
    // both from the open that wrote them.
    let (status, stdout, stderr) = run_session(
        &db,
        &["agent=curator"],
        Some(
            "MATCH (p:Person {name: 'Ada'}), (c:Company {name: 'Acme'})\n\
             CREATE (p)-[:WORKS_FOR {since: 2026}]->(c);\n\
             MATCH (p:Person {name: 'Ada'})-[r:WORKS_FOR]->(c)\n\
             RETURN p.name AS person, c.name AS employer, r.since AS since, \
             provenance(r, 'since') AS sinceOrigin;",
        ),
        None,
    );
    assert!(status.success(), "the curator session failed: {stderr}");
    assert_eq!(
        stdout,
        "{\"employer\":\"Acme\",\"person\":\"Ada\",\"since\":2026,\
         \"sinceOrigin\":{\"commitTs\":2,\"lsn\":2,\"metadata\":{\"agent\":\"curator\"},\"value\":2026}}\n",
        "the link's value and its provenance, side by side"
    );

    // The scraper agent: bulk-adds tags, then reads the whole graph back —
    // every fact with the transaction that wrote it beside the value. The
    // persons and the company were written by the store's first commit;
    // the tags by this open's own write.
    let (status, stdout, stderr) = run_session(
        &db,
        &["agent=scraper", "url=wiki"],
        Some(
            "CREATE (:Tag {name: 'pioneer'}), (:Tag {name: 'analyst'});\n\
             MATCH (n)\n\
             RETURN labels(n) AS labels, n.name AS name, \
             provenance(n, 'name') AS nameOrigin\n\
             ORDER BY name;",
        ),
        None,
    );
    assert!(status.success(), "the scraper session failed: {stderr}");
    assert_eq!(
        stdout,
        concat!(
            "{\"labels\":[\"Company\"],\"name\":\"Acme\",\"nameOrigin\":",
            "{\"commitTs\":1,\"lsn\":1,\"metadata\":{\"agent\":\"ingest\",\"source\":\"directory\"},\"value\":\"Acme\"}}\n",
            "{\"labels\":[\"Person\"],\"name\":\"Ada\",\"nameOrigin\":",
            "{\"commitTs\":1,\"lsn\":1,\"metadata\":{\"agent\":\"ingest\",\"source\":\"directory\"},\"value\":\"Ada\"}}\n",
            "{\"labels\":[\"Person\"],\"name\":\"Grace\",\"nameOrigin\":",
            "{\"commitTs\":1,\"lsn\":1,\"metadata\":{\"agent\":\"ingest\",\"source\":\"directory\"},\"value\":\"Grace\"}}\n",
            "{\"labels\":[\"Tag\"],\"name\":\"analyst\",\"nameOrigin\":",
            "{\"commitTs\":3,\"lsn\":3,\"metadata\":{\"agent\":\"scraper\",\"url\":\"wiki\"},\"value\":\"analyst\"}}\n",
            "{\"labels\":[\"Tag\"],\"name\":\"pioneer\",\"nameOrigin\":",
            "{\"commitTs\":3,\"lsn\":3,\"metadata\":{\"agent\":\"scraper\",\"url\":\"wiki\"},\"value\":\"pioneer\"}}\n",
        ),
        "every fact with the transaction that wrote it: {stdout}"
    );

    // The supervisor: one statement retracts everything the scraper wrote
    // (db.transactions lists the retained records, the WHERE keeps one
    // agent's commits, their changed keys name the touched elements), and
    // the supervisor's own invocation metadata makes the retraction
    // itself part of the audit surface. The curated graph stands.
    let (status, stdout, stderr) = run_session(
        &db,
        &["agent=supervisor"],
        Some(
            "CALL db.transactions() YIELD commitTs, metadata, changedKeys\n\
             WHERE metadata.agent = 'scraper'\n\
             UNWIND changedKeys AS ck\n\
             WITH collect(DISTINCT ck.elementId) AS touched\n\
             MATCH (n) WHERE elementId(n) IN touched\n\
             DETACH DELETE n\n\
             RETURN count(n) AS retracted;\n\
             MATCH (n:Tag) RETURN count(n) AS tagsLeft;\n\
             MATCH (n) RETURN labels(n)[0] AS label, count(n) AS standing ORDER BY label;\n\
             MATCH ()-[r:WORKS_FOR]->() RETURN count(r) AS links;\n\
             CALL db.transactions() YIELD commitTs, metadata, changedKeys\n\
             WHERE metadata.agent = 'supervisor'\n\
             RETURN commitTs, metadata, changedKeys;",
        ),
        None,
    );
    assert!(status.success(), "the supervisor session failed: {stderr}");
    assert_eq!(
        stdout,
        concat!(
            "{\"retracted\":2}\n",
            "{\"tagsLeft\":0}\n",
            "{\"label\":\"Company\",\"standing\":1}\n",
            "{\"label\":\"Person\",\"standing\":2}\n",
            "{\"links\":1}\n",
            "{\"changedKeys\":[{\"elementId\":\"4:rl-id:n3\",\"key\":\"name\"},",
            "{\"elementId\":\"4:rl-id:n4\",\"key\":\"name\"}],",
            "\"commitTs\":4,\"metadata\":{\"agent\":\"supervisor\"}}\n",
        ),
        "the retract removed exactly the scraper's writes, the curated \
         graph stands, and the supervisor's own record names what it \
         deleted: {stdout}"
    );

    drop(dir);
}
