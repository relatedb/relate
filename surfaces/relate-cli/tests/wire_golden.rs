//! Cross-surface anti-drift golden test.
//!
//! The canonical wire encoding lives in `relate_model::wire`, and the
//! golden fixtures that pin it live in `relate_model::wire::golden` -- a
//! shared, importable location every surface's test suite reaches. This test
//! proves the fixtures compile and the canonical module is reachable and
//! correct from the CLI build graph.

use relate_model::wire::{self, golden};

#[test]
fn cli_surface_reaches_canonical_wire_module() {
    golden::assert_all_match(wire::value_to_wire_json);
}

#[test]
fn cli_surface_golden_row_envelope_uses_rows_key() {
    // The canonical envelope for the all-variant golden row must use the
    // `rows` key (not the CLI server's historical `data`). Pinned here so the
    // 7.7 migration target is unambiguous from the CLI test suite.
    let (columns, row, expected) = golden::golden_row();
    let (snap, _) = golden::stats_fixture();
    let rows = wire::rows_for_result(&columns, std::slice::from_ref(&row));
    // ADR-0224: the engine stamps each column from the first row; the golden
    // row is the first row, so its envelope carries the shared derivation as
    // `columnTypes` beside `columns`.
    let column_types = golden::column_types(&row);
    let env = wire::response_envelope(
        &columns,
        &column_types,
        rows,
        wire::stats_to_wire_json(&snap),
        // ADR-0363 decision 9: the always-present notification list, empty
        // when the golden row carries none.
        &[],
        golden::FIXTURE_TIMING_STAMP,
        None,
    );
    assert_eq!(env, expected);
    assert!(env.get("rows").is_some());
    assert!(env.get("data").is_none());
    // ADR-0363: the envelope carries the empty list as an empty array.
    assert_eq!(
        env[wire::notification_keys::NOTIFICATIONS],
        serde_json::json!([])
    );
    // ADR-0224: one type name per column, same length and order as `columns`.
    assert_eq!(env["columnTypes"].as_array().unwrap().len(), columns.len());
    // ADR-0491: the timing block rides the one canonical builder — the
    // total and its split, all four keys, the cross-surface anti-drift pin.
    assert_eq!(
        env["elapsed_ms"],
        serde_json::json!(golden::FIXTURE_TIMING_STAMP.elapsed_ms)
    );
    assert_eq!(
        env["parse_ms"],
        serde_json::json!(golden::FIXTURE_TIMING_STAMP.parse_ms)
    );
    assert_eq!(
        env["plan_ms"],
        serde_json::json!(golden::FIXTURE_TIMING_STAMP.plan_ms)
    );
    assert_eq!(
        env["execute_ms"],
        serde_json::json!(golden::FIXTURE_TIMING_STAMP.execute_ms)
    );
}

#[test]
fn cli_surface_reaches_shared_write_only_envelope() {
    // Write-only statements (no result columns) must render as an empty `rows`
    // array — never `[{}]` — across every surface. The FFI assembles its rows
    // through `wire::rows_for_result`, so this shared fixture pins the
    // empty-columns path from the CLI build graph.
    let (columns, rows, stats, expected) = golden::envelope_write_only();
    let wire_rows = wire::rows_for_result(&columns, &rows);
    let env = wire::response_envelope(
        &columns,
        &[],
        wire_rows,
        wire::stats_to_wire_json(&stats),
        &[],
        golden::FIXTURE_TIMING_STAMP,
        None,
    );
    assert_eq!(env, expected);
    assert_eq!(env["rows"], serde_json::json!([]));
    // ADR-0224: no columns, so no column types.
    assert_eq!(env["columnTypes"], serde_json::json!([]));
    // ADR-0491: the timing block rides the one canonical builder here too.
    assert_eq!(
        env["parse_ms"],
        serde_json::json!(golden::FIXTURE_TIMING_STAMP.parse_ms)
    );
}

#[test]
fn column_type_names_agree_across_the_wire_envelope_and_the_ffi_path() {
    // ADR-0224 cross-surface pin: the same query must report the same
    // per-column type names everywhere they surface. The engine stamps
    // `QueryResult::column_types`; the wire envelope carries them as
    // `columnTypes`; the FFI's language clients receive exactly this
    // envelope, because `relate::query_result_to_wire` is the single
    // entry point the FFI result path calls — so pinning the engine stamp
    // against the envelope pins the FFI's names too.
    let db = relate::Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let result = conn.query("RETURN 1 AS n").unwrap();
    let engine: Vec<&str> = result
        .header
        .column_types
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(engine, ["INTEGER"]);
    let wire = relate::query_result_to_wire(&result).unwrap();
    assert_eq!(wire["columnTypes"], serde_json::json!(["INTEGER"]));
    // One type name per column, same length and order as `columns`.
    assert_eq!(
        wire["columnTypes"].as_array().unwrap().len(),
        wire["columns"].as_array().unwrap().len()
    );
    // An empty result stamps no columns and no types anywhere.
    let empty = conn.query("CREATE (:Empty)").unwrap();
    let wire_empty = relate::query_result_to_wire(&empty).unwrap();
    assert_eq!(empty.header.column_types, Vec::<String>::new());
    assert_eq!(wire_empty["columnTypes"], serde_json::json!([]));
}
