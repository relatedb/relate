//! End-to-end tests for query notifications in the CLI (ADR-0363): the
//! table mode's one `Notifications:` block (with the plan-warning kinds
//! filtered) and the envelope's complete `notifications` array.

use crate::support;

use support::{run_cli, tempdir, CliRunOptions as Opts};

/// The Cartesian-product message — the plan table's shared vocabulary text
/// (ADR-0363 decision 1), and the description the notification carries.
const CARTESIAN_MESSAGE: &str = "the MATCH patterns share no join anchor";

/// A throwaway store holding one `:Person` and one `:City` node, so the
/// two-pattern `MATCH` plans as the disconnected patterns that raise the
/// Cartesian-product warning. Returns the database path.
fn seeded_store() -> (tempfile::TempDir, String) {
    let dir = tempdir();
    let path = dir.path().join("notifications.rl");
    let db = path.to_str().expect("tempdir path is UTF-8").to_string();
    let (status, _, stderr) = run_cli(Opts::new(&[
        "--database",
        &db,
        "CREATE (:Person); CREATE (:City)",
    ]));
    assert!(status.success(), "seeding failed: {stderr}");
    (dir, db)
}

#[test]
fn explain_table_renders_the_cartesian_warning_only_in_the_plan_table() {
    // ADR-0363 decision 8: the three plan-warning kinds render in the plan
    // table's `Warnings` column and footer only — never repeated in the
    // `Notifications:` block. A table whose whole list is a plan-warning kind
    // therefore has no block at all.
    let (_dir, db) = seeded_store();
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "table",
        "--database",
        &db,
        "EXPLAIN MATCH (a:Person), (b:City) RETURN a, b",
    ]));
    assert!(status.success(), "EXPLAIN failed: {stderr}");
    // LABS-373: table mode keeps stdout for the data (an EXPLAIN has none)
    // and prints the plan table on stderr.
    assert!(stdout.is_empty(), "EXPLAIN has no data table: {stdout}");
    assert!(
        stderr.contains("Warnings"),
        "the plan table's warnings column: {stderr}"
    );
    assert!(
        stderr.lines().any(|line| line.starts_with("Warnings: Id ")),
        "the plan table's warnings footer: {stderr}"
    );
    let occurrences = stderr.matches(CARTESIAN_MESSAGE).count();
    assert_eq!(
        occurrences, 2,
        "the plan table's column cell and footer are its only renderings: {stderr}"
    );
    assert!(
        !stderr.contains("Notifications:"),
        "the plan-warning kind is not repeated in a notification block: {stderr}"
    );
}

#[test]
fn explain_table_renders_the_notification_block_after_the_plan_table() {
    // ADR-0363 decision 8: the block follows the plan table on stderr and
    // carries every kind the plan table does not already render. An unknown
    // label is statement-scoped, so only the block can carry it.
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "table",
        "EXPLAIN MATCH (n:Nope) RETURN n",
    ]));
    assert!(status.success(), "EXPLAIN failed: {stderr}");
    assert!(stdout.is_empty(), "EXPLAIN has no data table: {stdout}");
    let block_start = stderr
        .find("Notifications:\n")
        .unwrap_or_else(|| panic!("the block follows the plan table: {stderr}"));
    let block = &stderr[block_start..];
    assert!(
        block.contains("Neo.ClientNotification.Statement.UnknownLabelWarning"),
        "the unknown label rides the block: {stderr}"
    );
    assert!(
        block.contains("Remedy: Check the spelling"),
        "the block carries the remedy: {stderr}"
    );
}

#[test]
fn explain_envelope_carries_the_complete_notification_list() {
    // ADR-0363 decision 9: the envelope's always-present `notifications`
    // array carries the native keys (code, gql_status, severity, category,
    // title, description, remedy, operator_id when present) — the complete
    // list once, unfiltered.
    let (_dir, db) = seeded_store();
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "envelope",
        "--database",
        &db,
        "EXPLAIN MATCH (a:Person), (b:City) RETURN a, b",
    ]));
    assert!(status.success(), "EXPLAIN failed: {stderr}");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"));
    assert_eq!(envelope["plan"]["operatorType"], "ProduceResults");
    let list = envelope["notifications"]
        .as_array()
        .unwrap_or_else(|| panic!("notifications is an array: {stdout}"));
    let [notification] = list.as_slice() else {
        panic!("the disconnected MATCH raises one notification: {stdout}");
    };
    assert_eq!(
        notification["code"],
        "Neo.ClientNotification.Statement.CartesianProduct"
    );
    assert_eq!(notification["gql_status"], "03N90");
    assert_eq!(notification["severity"], "INFORMATION");
    assert_eq!(notification["category"], "PERFORMANCE");
    assert_eq!(notification["title"], "Cartesian product");
    assert_eq!(notification["description"], CARTESIAN_MESSAGE);
    assert!(
        notification["remedy"]
            .as_str()
            .unwrap_or_else(|| panic!("remedy is a string: {stdout}"))
            .contains("join anchor"),
        "the remedy names the action: {stdout}"
    );
    assert!(
        notification["operator_id"].is_number(),
        "the warning is operator-scoped: {stdout}"
    );
}

#[test]
fn a_plain_query_envelope_carries_an_empty_notification_array() {
    // A plain run builds no plan tree, so the list is empty — the key is
    // still present (ADR-0363 decision 9), never omitted.
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "envelope",
        "MATCH (n:Nope) RETURN n",
    ]));
    assert!(status.success(), "{stderr}");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"));
    assert_eq!(envelope["notifications"], serde_json::json!([]));
}
