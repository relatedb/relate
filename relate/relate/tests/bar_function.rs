//! The engine-only `bar(value, min, max, width)` text-bar function: callable
//! in every build, never listed by `SHOW FUNCTIONS` (ADR-0542).

mod common;

use common::connection;

#[test]
fn bar_is_callable_and_renders_blocks() {
    let mut conn = connection();
    let rows = conn
        .query("RETURN bar(5, 0, 10, 10) AS half, bar(1.25, 0, 10, 4) AS part, bar(null, 0, 10, 4) AS n")
        .unwrap();
    let row = &rows.rows[0];
    assert_eq!(row[0].as_string(), Some("█████"));
    assert_eq!(row[1].as_string(), Some("▌"));
    assert!(row[2].is_null());
}

#[test]
fn bar_rejects_a_bad_range_and_a_wrong_arity() {
    let mut conn = connection();
    let error = conn
        .query("RETURN bar(1, 5, 5, 4)")
        .unwrap_err()
        .to_string();
    assert!(error.contains("InvalidArgumentValue"), "{error}");
    let error = conn.query("RETURN bar(1, 0, 10)").unwrap_err().to_string();
    assert!(error.contains("InvalidNumberOfArguments"), "{error}");
}

#[test]
fn show_functions_lists_no_bar_row() {
    let mut conn = connection();
    let rows = conn.query("SHOW FUNCTIONS YIELD name RETURN name").unwrap();
    let listed: Vec<String> = rows
        .rows
        .iter()
        .map(|row| {
            row[0]
                .as_string()
                .expect("string name")
                .to_ascii_lowercase()
        })
        .collect();
    assert!(listed.iter().any(|name| name == "coll.sort"), "control row");
    assert!(!listed.iter().any(|name| name == "bar"), "bar is unlisted");
}
