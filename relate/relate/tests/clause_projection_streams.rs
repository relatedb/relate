//! ADR-0660: `WITH`/`RETURN` consume the inter-clause stream.
//!
//! A pass-through projection — no aggregation, no `ORDER BY` — folds a
//! generated feed as its rows arrive and applies its page and `DISTINCT` on
//! the stream, so its retained total is independent of the input count: the
//! same tiny expression cap answers two million generated rows and ten
//! thousand alike. The state-holders keep their declared charge: an
//! unlimited `ORDER BY` refuses under that cap, and the ordered `LIMIT`
//! window keeps its bounded top-k heap
//! (`order_by_top_k_bounds.rs` pins the window's own envelope).

mod common;

use relate::{OpenOptions, Value};

/// A per-allocation cap generous for a small answer and impossible for the
/// two-million-row input these statements generate: only a stream that
/// never retains its input answers under it.
const CAP: u64 = 1 << 20;
/// The large input, and a small one an order of magnitude apart: the
/// pass-through shapes answer both under the same cap.
const LARGE: i64 = 2_000_000;
const SMALL: i64 = 10_000;

fn capped_db() -> relate::Connection {
    common::connection_with(OpenOptions {
        expression_memory_limit: Some(CAP),
        ..Default::default()
    })
}

/// The first column of `query` over a fresh capped store.
fn first_column(query: &str) -> Vec<Value> {
    let mut connection = capped_db();
    common::first_column(&common::run(&mut connection, query))
}

/// The pass-through `RETURN` page applies on the stream: the generator is
/// pulled only until the page is satisfied, so the two-million-row input
/// never lands.
#[test]
fn a_return_page_stops_its_generator() {
    assert_eq!(
        first_column(&format!("UNWIND range(1, {LARGE}) AS i RETURN i LIMIT 5")),
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]
    );
}

/// The pass-through `WITH` page is the same fold: the page is applied on
/// the stream it consumes, so the generator stops at the page.
#[test]
fn a_with_page_stops_its_generator() {
    assert_eq!(
        first_column(&format!(
            "UNWIND range(1, {LARGE}) AS i WITH i AS v LIMIT 5 RETURN v"
        )),
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]
    );
}

/// `WITH DISTINCT` retains only its charged seen-set and its (bounded)
/// first-seen output, so its retained total is independent of how many
/// rows produce those distinct values: the same small cap answers both
/// input sizes with the same answer, in first-seen order.
#[test]
fn with_distinct_keeps_only_its_seen_set() {
    let query = |n: i64| format!("UNWIND range(1, {n}) AS i WITH DISTINCT i % 7 AS d RETURN d");
    let expected = vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
        Value::Int(5),
        Value::Int(6),
        Value::Int(0),
    ];
    assert_eq!(first_column(&query(SMALL)), expected);
    assert_eq!(first_column(&query(LARGE)), expected);
}

/// A streamed `WITH DISTINCT` still applies its own `WHERE`: the landing
/// funnel filters each row before the first-seen gate, so the tail's
/// filter-skip for an already-deduped stream never drops the predicate.
#[test]
fn a_streamed_with_distinct_still_filters_its_where() {
    assert_eq!(
        first_column("UNWIND [1, 1, 2, 3, 3, 4] AS i WITH DISTINCT i WHERE i > 2 RETURN i"),
        vec![Value::Int(3), Value::Int(4)]
    );
}

/// A produced feed's plain `WHERE` filters per row on the stream (the
/// `pass` disposition): the predicate's rejects never land, so the same
/// small cap that answers the page shapes answers a two-million-row input
/// whose filter keeps five.
#[test]
fn a_streamed_with_where_filters_per_row() {
    assert_eq!(
        first_column(&format!(
            "UNWIND range(1, {LARGE}) AS i WITH i WHERE i > {} RETURN i",
            LARGE - 5
        )),
        vec![
            Value::Int(LARGE - 4),
            Value::Int(LARGE - 3),
            Value::Int(LARGE - 2),
            Value::Int(LARGE - 1),
            Value::Int(LARGE),
        ]
    );
}

/// A `*` projection with no aggregation is the visible-bindings
/// pass-through, and its page applies on the stream like any other.
#[test]
fn a_with_star_passes_through() {
    assert_eq!(
        first_column(&format!(
            "UNWIND range(1, {LARGE}) AS i WITH * LIMIT 5 RETURN i"
        )),
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]
    );
}

/// The ordered `LIMIT` window keeps its charged top-k heap: the generator
/// folds into the bounded window rather than the input landing, so the
/// same small cap answers two million rows.
#[test]
fn the_ordered_limit_window_keeps_its_bounded_heap() {
    assert_eq!(
        first_column(&format!(
            "UNWIND range(1, {LARGE}) AS i WITH i AS v ORDER BY v DESC LIMIT 5 RETURN v"
        )),
        vec![
            Value::Int(LARGE),
            Value::Int(LARGE - 1),
            Value::Int(LARGE - 2),
            Value::Int(LARGE - 3),
            Value::Int(LARGE - 4)
        ]
    );
}

/// An unlimited `ORDER BY` is a declared state-holder: it must hold its
/// whole input to sort it, so the same cap that the page folds under
/// refuses it — the pin that the pass-through did not quietly turn the
/// breaker into a stream.
#[test]
fn an_unlimited_order_by_stays_a_state_holder() {
    let mut connection = capped_db();
    common::refused_expression_cap(
        &mut connection,
        &format!("UNWIND range(1, {LARGE}) AS i WITH i AS v ORDER BY v RETURN v LIMIT 5"),
    );
}
