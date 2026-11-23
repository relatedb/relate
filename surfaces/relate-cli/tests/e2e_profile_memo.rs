//! End-to-end pins for PROFILE's expression-memo column (ADR-0654): the
//! full pipeline (flags -> REPL -> engine -> formatter) shows a `Memo`
//! column of `hits/probes` only when the statement used a memo, and the
//! machine modes carry the same group.

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

/// A string-fed `datetime(...)` over two distinct texts: the constructor
/// memo serves every repeat after its warm-up (the executor pins the exact
/// counts; this checks the surfaces carry them).
const REPEATED: &str = "UNWIND range(0, 1999) AS i \
     WITH ['2024-05-05T10:00:00Z', '2023-01-01T09:00:00Z'][i % 2] AS s \
     RETURN count(datetime(s)) AS n";

/// The `Memo` cell of the plan table's first operator row.
fn memo_cell(plan_table: &str) -> String {
    let mut lines = plan_table.lines().filter(|l| l.starts_with('|'));
    let header: Vec<&str> = lines.next().expect("a header row").split('|').collect();
    let column = header
        .iter()
        .position(|h| h.trim() == "Memo")
        .unwrap_or_else(|| panic!("a Memo column: {plan_table}"));
    let row: Vec<&str> = lines.next().expect("the root row").split('|').collect();
    row[column].trim().to_string()
}

#[test]
fn the_plan_table_shows_a_memo_column_when_a_memo_was_used() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "table",
        &format!("PROFILE {REPEATED}"),
    ]));
    assert!(status.success(), "exited non-zero: {stderr}");
    let cell = memo_cell(&stderr);
    let (hits, probes) = cell.split_once('/').expect("a hits/probes cell");
    let (hits, probes): (u64, u64) = (hits.parse().unwrap(), probes.parse().unwrap());
    assert!(hits > 0 && hits <= probes, "{cell}");
}

#[test]
fn a_memo_free_profile_has_no_memo_column() {
    let (status, _stdout, stderr) =
        run_cli(Opts::new(&["--mode", "table", "PROFILE RETURN 1 AS n"]));
    assert!(status.success(), "exited non-zero: {stderr}");
    assert!(
        stderr.contains("Operator"),
        "the plan table printed: {stderr}"
    );
    // `Memo` alone, not the `Memory (Bytes)` column that shares the prefix.
    assert!(!stderr.contains(" Memo "), "no Memo column: {stderr}");
}

#[test]
fn the_json_plan_carries_the_memo_group_and_root_kinds() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "json",
        &format!("PROFILE {REPEATED}"),
    ]));
    assert!(status.success(), "exited non-zero: {stderr}");
    assert!(stdout.contains("\"MemoHits\""), "{stdout}");
    assert!(stdout.contains("\"MemoBytes\""), "{stdout}");
    assert!(stdout.contains("\"MemoKinds\":[\"datetime\"]"), "{stdout}");
}
