//! LABS-1731: keyed MATCH lookups no declared index covers serve from
//! the lazy equality postings (ADR-0614). The ticket repro — a bulk
//! load's `UNWIND … MATCH (a:El {idx: r.f})` endpoint resolution — ran
//! a per-row label scan whenever no `CREATE RANGE INDEX` covered the
//! pair, making statement-based relationship loads O(rows × label).
//! This file pins the serve: the postings answer exactly what a
//! declared index and the scan answer (numeric equivalence included),
//! stale postings filter through the re-check, a named-zone-datetime
//! bound declines so the un-indexed answer keeps `=` semantics (an
//! offset-only datetime serves — its identity is a bijection of its
//! `=` class), the bulk shape's reads scale with the rows, and the
//! missing-index notification stays truthful.

use crate::common;

use common::{connection, has_notification, plan, run, scalar_one, tree_metric};
use relate_model::wire::NotificationKind;
use relate_test_support::temporal::{DST_OVERLAP_FIRST, DST_OVERLAP_SECOND};

/// The `:El` rows every store shares: one per key kind (int, float,
/// string), plus an `:Other` row sharing the int key — the label check
/// must keep it out of every `:El` lookup.
fn seed(conn: &mut relate::Connection) {
    run(
        conn,
        "CREATE (:El {idx: 1}), (:El {idx: 1.5}), (:El {idx: 'x'}), \
         (:Other {idx: 1})",
    );
}

/// The single-row integer `count(…)` of `stmt`, which must succeed.
fn count(conn: &mut relate::Connection, stmt: &str) -> i64 {
    scalar_one(conn, stmt).as_int().expect("count")
}

/// The same value must answer the same count under the inline-map form
/// and the WHERE form — the two serve routes' shared assertion.
fn assert_both_forms(conn: &mut relate::Connection, prop: &str, value: &str, expected: i64) {
    let map = format!("MATCH (a:El {{{prop}: {value}}}) RETURN count(a) AS n");
    let where_form = format!("MATCH (a:El) WHERE a.{prop} = {value} RETURN count(a) AS n");
    for stmt in [&map, &where_form] {
        assert_eq!(count(conn, stmt), expected, "{stmt}");
    }
}

/// The postings serve answers exactly what a declared index answers —
/// inline-map and WHERE form, numeric equivalence (`1` and `1.0` are
/// one identity under both `=` and the postings key) and the label
/// check included.
#[test]
fn postings_serve_answers_match_the_declared_index() {
    let mut unindexed = connection();
    seed(&mut unindexed);
    let mut declared = connection();
    run(&mut declared, "CREATE INDEX el_idx FOR (n:El) ON (n.idx)");
    seed(&mut declared);

    for (key, expected) in [("1", 1i64), ("1.0", 1), ("1.5", 1), ("'x'", 1), ("2", 0)] {
        for conn in [&mut unindexed, &mut declared] {
            assert_both_forms(conn, "idx", key, expected);
        }
    }
}

/// ADR-0063's superset contract at the lookup: a posting left stale by
/// a later write costs a read, never a wrong answer — the re-check
/// filters it. Value moves, deletes, and label removals all leave a
/// stale id behind in the built pair.
#[test]
fn stale_postings_filter_through_the_recheck() {
    let mut conn = connection();
    run(
        &mut conn,
        "CREATE (:El {idx: 1, tag: 'a'}), (:El {idx: 2, tag: 'b'})",
    );
    // Builds the (El, idx) pair on first use.
    assert_eq!(
        count(&mut conn, "MATCH (a:El {idx: 1}) RETURN count(a) AS n"),
        1
    );
    // Move the value: node a's stale idx-1 posting must not answer.
    run(&mut conn, "MATCH (a:El {tag: 'a'}) SET a.idx = 2");
    assert_eq!(
        count(&mut conn, "MATCH (a:El {idx: 1}) RETURN count(a) AS n"),
        0,
        "the moved-off key no longer matches"
    );
    assert_eq!(
        count(&mut conn, "MATCH (a:El {idx: 2}) RETURN count(a) AS n"),
        2,
        "both nodes carry idx 2 now"
    );
    // Delete one: its stale posting must not answer either.
    run(&mut conn, "MATCH (b:El {tag: 'b'}) DELETE b");
    assert_eq!(
        count(&mut conn, "MATCH (a:El {idx: 2}) RETURN count(a) AS n"),
        1
    );
    // Drop the label: the node stays in the store and the bucket, but
    // the re-check's label half filters it.
    run(&mut conn, "CREATE (:El {idx: 3, tag: 'c'})");
    assert_eq!(
        count(&mut conn, "MATCH (a:El {idx: 3}) RETURN count(a) AS n"),
        1
    );
    run(&mut conn, "MATCH (c:El {tag: 'c'}) REMOVE c:El");
    assert_eq!(
        count(&mut conn, "MATCH (a:El {idx: 3}) RETURN count(a) AS n"),
        0,
        "the label-less node no longer matches"
    );
}

/// ADR-0614's decline: a named-zone-datetime bound's identity is
/// narrower than its `=` class, so without an index both routes scan
/// and answer `=` — BOTH fall-back halves (the centralized
/// `relate-test-support` fixture) — exactly as before the serve; a
/// declared index answers identity (the one half), the recorded
/// Neo4j-parity behaviour.
#[test]
fn zoned_datetime_overlap_keeps_the_scan_answer_without_an_index() {
    let mut unindexed = connection();
    run(
        &mut unindexed,
        &format!("CREATE (:El {{at: {DST_OVERLAP_FIRST}}}), (:El {{at: {DST_OVERLAP_SECOND}}})"),
    );
    for half in [DST_OVERLAP_FIRST, DST_OVERLAP_SECOND] {
        assert_both_forms(&mut unindexed, "at", half, 2);
    }

    let mut declared = connection();
    run(&mut declared, "CREATE INDEX el_at FOR (n:El) ON (n.at)");
    run(
        &mut declared,
        &format!("CREATE (:El {{at: {DST_OVERLAP_FIRST}}}), (:El {{at: {DST_OVERLAP_SECOND}}})"),
    );
    let stmt = format!("MATCH (a:El {{at: {DST_OVERLAP_FIRST}}}) RETURN count(a) AS n");
    assert_eq!(
        count(&mut declared, &stmt),
        1,
        "a declared index answers identity: the one half"
    );
}

/// The decline is named-zone only (ADR-0614): an offset-only datetime
/// resolves to `ZoneId::Offset`, whose (instant, offset) identity is a
/// bijection of its `=` class (wall clock, offset) — so it serves, and
/// the `Z` and `+00:00` spellings of one wall clock find each other
/// through the postings. PROFILE pins the serve's reads: the matches'
/// buckets, never the decoys a declined scan would examine.
#[test]
fn offset_only_datetimes_serve_without_declining() {
    let spellings = [
        "datetime('2020-10-25T02:30Z')",
        "datetime('2020-10-25T02:30+00:00')",
    ];
    let row = |spelling: &str| format!("(:El {{at: {spelling}}})");
    let mut conn = connection();
    run(
        &mut conn,
        &format!(
            "CREATE {}, {} \
             UNWIND range(1, 50) AS i CREATE (:El {{at: datetime('2020-10-25T03:30Z'), i: i}})",
            row(spellings[0]),
            row(spellings[1]),
        ),
    );
    for spelling in spellings {
        assert_both_forms(&mut conn, "at", spelling, 2);
    }
    let profiled = run(
        &mut conn,
        &format!(
            "PROFILE MATCH (a:El {{at: {}}}) RETURN count(a) AS n",
            spellings[0]
        ),
    );
    assert_eq!(
        profiled.rows[0][0].as_int(),
        Some(2),
        "the profiled statement answers"
    );
    let reads = tree_metric(plan(&profiled), |m| m.reads.node_reads);
    assert!(
        reads < 20,
        "the offset-only serve reads its buckets, not the fifty decoys: {reads}"
    );
}

/// The ticket's bulk shape, PROFILEd: the keyed endpoint resolution of
/// an `UNWIND` load reads the rows' buckets and the one pair build —
/// never rows × label. Two hundred and fifty rows resolve two keyed
/// lookups each over a bucket of one: exactly 500 node reads, where the
/// quadratic shape this replaces reads at least rows × nodes
/// (500 × 250 = 125,000).
#[test]
fn unwind_keyed_load_reads_scale_with_the_rows() {
    let mut conn = connection();
    run(&mut conn, "UNWIND range(0, 249) AS i CREATE (:El {idx: i})");
    let profiled = run(
        &mut conn,
        "PROFILE UNWIND range(0, 249) AS i \
         MATCH (a:El {idx: i}), (b:El {idx: (i + 1) % 250}) \
         RETURN count(a) AS n",
    );
    assert_eq!(
        profiled.rows[0][0].as_int(),
        Some(250),
        "every row resolves its endpoints"
    );
    assert_eq!(
        tree_metric(plan(&profiled), |m| m.reads.node_reads),
        500,
        "one read per row per keyed lookup"
    );
}

/// The notification stays truthful: the serve is a runtime fallback,
/// not a declared index — `EXPLAIN` keeps recommending one (declaring
/// it still buys ordered postings and no lazy build), and stops once it
/// exists.
#[test]
fn missing_index_notification_stays_truthful() {
    let mut conn = connection();
    seed(&mut conn);
    let explained = run(
        &mut conn,
        "EXPLAIN MATCH (a:El {idx: 1}) RETURN count(a) AS n",
    );
    assert!(
        has_notification(&explained, NotificationKind::MissingIndex),
        "no RANGE index covers (El, idx)"
    );
    run(&mut conn, "CREATE INDEX el_idx FOR (n:El) ON (n.idx)");
    let explained = run(
        &mut conn,
        "EXPLAIN MATCH (a:El {idx: 1}) RETURN count(a) AS n",
    );
    assert!(
        !has_notification(&explained, NotificationKind::MissingIndex),
        "the declared pair answers it"
    );
}
