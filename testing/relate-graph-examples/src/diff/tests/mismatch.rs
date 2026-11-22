use super::*;
use relate_model::CypherValue;

/// F-1-11: an order-only list difference is `UpToListOrder` carrying
/// the strict failure text, not a diff.
#[test]
fn list_order_only_difference_is_up_to_list_order() {
    let left = row_json(serde_json::json!([1, 2]));
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Int(2),
        CypherValue::Int(1),
    ]));
    match compare_rows(&left, &right, StatementOrder::Unordered) {
        Relation::UpToListOrder(strict) => {
            assert!(strict.contains("with no relate match"), "{strict}");
        }
        other => panic!("expected UpToListOrder, got {other:?}"),
    }
}

/// F-1-11: with `ordered`, an `ORDER BY` tie (rows equal but swapped)
/// is `UpToListOrder` too; the strict text is the positional one.
#[test]
fn ordered_row_ties_are_up_to_list_order() {
    let (left, right) = swapped_rows();
    match compare_rows(&left, &right, StatementOrder::Ordered) {
        Relation::UpToListOrder(strict) => {
            assert!(strict.contains("row 0 differs"), "{strict}");
        }
        other => panic!("expected UpToListOrder, got {other:?}"),
    }
}

/// F-1-11: the strict row-count failure text survives in `Different`.
#[test]
fn row_count_mismatch_keeps_the_strict_text() {
    let left = vec![serde_json::json!({"n": 1}), serde_json::json!({"n": 2})];
    let right = row_cypher(CypherValue::Int(1));
    let strict = expect_different(&left, &right, StatementOrder::Unordered);
    assert_eq!(strict, "row count differs: neo4j 2 vs relate 1");
}

/// A row mismatch renders both sides as compact serde_json — never
/// Rust Debug (`Array [String("…")]`), which the relate side used
/// to produce via `{:?}`.
#[test]
fn mismatch_messages_render_compact_json_not_rust_debug() {
    let left = row_json(serde_json::json!(["a", "b"]));
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Str("x".into()),
        CypherValue::Str("y".into()),
    ]));
    let msg = expect_different(&left, &right, StatementOrder::Unordered);
    assert!(!msg.contains("Array ["), "{msg}");
    assert!(!msg.contains("String("), "{msg}");
    assert!(msg.contains(r#"["a","b"]"#), "{msg}");
    assert!(msg.contains(r#"["x","y"]"#), "{msg}");
}

/// A wide row cannot flood the report: each side of a mismatch is
/// elided at the report's cap with the `…` marker.
#[test]
fn mismatch_messages_elide_long_rows() {
    let long = "x".repeat(ELIDE_AT * 2);
    let left = row_json(serde_json::json!([long]));
    let right = row_cypher(CypherValue::List(vec![CypherValue::Str("y".into())]));
    let msg = expect_different(&left, &right, StatementOrder::Unordered);
    assert!(msg.contains('…'), "{msg}");
    assert!(msg.len() < ELIDE_AT * 3, "{}", msg.len());
}

/// Both sides of a no-match message render row by row, each in a `row N:`
/// frame naming the row's result-set index — never one nested list whose
/// outer brackets read as an extra value-nesting level (the LABS-1299
/// misdiagnosis) — and never more rows per side than [`ROWS_SHOWN`].
#[test]
fn no_match_message_frames_rows_one_by_one() {
    let left = row_json(serde_json::json!(["a", "b"]));
    let right = row_cypher(CypherValue::List(vec![CypherValue::Str("x".into())]));
    let msg = expect_different(&left, &right, StatementOrder::Unordered);
    assert!(msg.contains("with no relate match"), "{msg}");
    assert!(
        msg.contains("neo4j rows with no relate match (1): row 0:"),
        "{msg}"
    );
    assert!(msg.contains("relate rows left over (1): row 0:"), "{msg}");
    assert!(!msg.contains("relate rows: ["), "{msg}");
}

/// The difference the message reports is the completed bag match's, not a
/// mid-loop snapshot: left rows that match consume their right rows even
/// after an earlier left row failed, so the frames name exactly the rows
/// left over at the end, by their true result-set indices.
#[test]
fn no_match_message_reports_the_completed_difference() {
    // Left row 0 matches nothing; rows 1-4 consume relate rows 0-3, so
    // the leftover is relate row 4 — not the five a snapshot would show.
    let left = rows_json(&[
        serde_json::json!(["a", "b"]),
        serde_json::json!("x"),
        serde_json::json!("y"),
        serde_json::json!("z"),
        serde_json::json!("w"),
    ]);
    let right = rows_cypher(&["x", "y", "z", "w", "v"]);
    let msg = expect_different(&left, &right, StatementOrder::Unordered);
    assert!(
        msg.contains("neo4j rows with no relate match (1): row 0:"),
        "{msg}"
    );
    assert!(msg.contains("relate rows left over (1): row 4:"), "{msg}");
}

/// With more rows on a side than [`ROWS_SHOWN`], the message names the
/// rest by count instead of rendering them.
#[test]
fn no_match_message_names_the_unrendered_rows_by_count() {
    // No left row matches any right row, so both sides of the difference
    // hold all five rows.
    let left = rows_json(&[
        serde_json::json!(["a", "b"]),
        serde_json::json!(["c", "d"]),
        serde_json::json!(["e", "f"]),
        serde_json::json!(["g", "h"]),
        serde_json::json!(["i", "j"]),
    ]);
    let right = rows_cypher(&["x", "y", "z", "w", "v"]);
    let msg = expect_different(&left, &right, StatementOrder::Unordered);
    assert!(msg.contains("neo4j rows with no relate match (5)"), "{msg}");
    assert!(msg.contains("relate rows left over (5)"), "{msg}");
    assert!(msg.contains("row 2:"), "{msg}");
    assert!(!msg.contains("row 3:"), "{msg}");
    assert!(msg.contains("(+2 more)"), "{msg}");
}

/// A frame's `row N` is the row's index in its result set, not its
/// position among the still-unmatched rows: after an earlier successful
/// match removes an entry from the pool, the frame keeps naming the
/// original index (the drift the round-1 review caught).
#[test]
fn no_match_message_names_true_result_indices() {
    let left = rows_json(&[serde_json::json!("a"), serde_json::json!("c")]);
    let right = rows_cypher(&["a", "b"]);
    let msg = expect_different(&left, &right, StatementOrder::Unordered);
    // Relate row 0 matched the first reference row; the row left
    // over is result-set row 1, whatever the pool order is.
    assert!(msg.contains("relate rows left over (1): row 1:"), "{msg}");
}

/// A rendered JSON payload at the cap is untouched; one past the cap is
/// cut in the middle — head and tail kept, the cut size named — so a
/// deep difference at either end stays visible (LABS-1299).
#[test]
fn elide_cuts_the_middle_and_names_the_cut() {
    let exact = "x".repeat(ELIDE_AT);
    assert_eq!(elide(&exact), exact);
    let out = elide(&"x".repeat(ELIDE_AT + 10));
    assert!(out.contains("…(+10 chars)…"), "{out}");
    // Only the head and tail shares survive the cut.
    assert_eq!(out.chars().filter(|c| *c == 'x').count(), ELIDE_AT, "{out}");
    let tagged = format!(
        "{}{}{}",
        "h".repeat(ELIDE_HEAD),
        "…".repeat(50),
        "t".repeat(ELIDE_TAIL)
    );
    let out = elide(&tagged);
    assert!(out.starts_with(&"h".repeat(ELIDE_HEAD)), "{out}");
    assert!(out.ends_with(&"t".repeat(ELIDE_TAIL)), "{out}");
}
