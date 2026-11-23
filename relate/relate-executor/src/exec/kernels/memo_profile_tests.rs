//! PROFILE's expression-memo counters (ADR-0654): the guarded-state memos'
//! sums land on the clause that ran them, the constructor memos' at the
//! statement level, a memo-free statement carries none, and a statement
//! that is not PROFILEd never reads a memo.

use relate_core::Value;
use relate_model::wire::MemoKindWire;

use super::memo::STATS_READS;
use super::memo_differential_tests::wide_graph;
use super::memo_wiring_tests::{guarded_queries, layered_graph};
use crate::exec::test_support::{exec_cypher, profiled_memos, Builder};
use crate::exec::{arm_store_profiling, disarm_store_profiling};
use crate::metrics::MemoCounters;

fn sum(records: &[crate::metrics::ClauseMetricsRecord]) -> MemoCounters {
    let mut total = MemoCounters::default();
    for record in records {
        total += record.memo;
    }
    total
}

/// A guarded quantified traversal over the layered DAG runs warm,
/// probing, hitting guard memos: the clause's record carries nonzero
/// counters, the table bytes, and the rows equal the unprofiled run's.
#[test]
fn a_guarded_traversal_reports_its_stage_memo_counters() {
    let g = layered_graph();
    let q = "MATCH (a:Hub)((m)-[:E]->(n:U)){1,}(y:V) RETURN count(DISTINCT y)";
    let (rows, records, statement) = profiled_memos(&g, q);
    let memo = sum(&records);
    assert!(memo.probes > 0, "the guard memos were probed: {memo:?}");
    assert!(memo.hits > 0, "a repeated node is a hit: {memo:?}");
    assert!(memo.hits <= memo.probes);
    assert!(memo.inserts > 0 && memo.bytes > 0, "{memo:?}");
    assert_eq!(memo.refused, 0);
    assert_eq!(memo.disabled_count, 0);
    assert_eq!(statement, Default::default(), "no constructor memo ran");
    assert_eq!(rows, exec_cypher(&g, q).rows, "PROFILE changes no answer");
    // The counters belong to the clause that ran them, not the projection.
    let owner = records
        .iter()
        .filter(|record| record.memo.probes > 0)
        .count();
    assert_eq!(owner, 1, "one operator carries the memo group");
}

/// The chain kernel's memoised target admission (a `Memoized` predicate in
/// the run's prepared state) reports through the same sums.
#[test]
fn a_chain_kernel_target_admission_reports_its_memo_counters() {
    let g = wide_graph();
    let q = "MATCH (a:U)-[:F]->()-[:F]->()-[:F]->(b:V) RETURN count(*)";
    let _ = super::take_kernel_engagements();
    let (rows, records, statement) = profiled_memos(&g, q);
    assert_eq!(
        super::take_kernel_engagements(),
        1,
        "the chain kernel served the clause"
    );
    let memo = sum(&records);
    assert!(memo.probes > 0 && memo.hits > 0, "{memo:?}");
    assert!(memo.bytes > 0 && memo.inserts > 0, "{memo:?}");
    assert_eq!(statement, Default::default());
    assert_eq!(rows, exec_cypher(&g, q).rows, "PROFILE changes no answer");
}

/// A statement with no memoised check reports no memo anywhere.
#[test]
fn a_memo_free_statement_carries_no_counters() {
    let b = Builder::new();
    let a = b.node("a", &["N"]);
    let c = b.node("c", &["N"]);
    b.rel(a, c, "E", &[]);
    let g = b.done();
    let (_, records, statement) =
        profiled_memos(&g, "MATCH (a:N)-[:E]->(c:N) RETURN count(*) AS n");
    assert_eq!(sum(&records), MemoCounters::default());
    assert_eq!(statement, Default::default());
}

/// Repeated datetime strings go through the constructor memo: the
/// statement-level record names the `datetime` kind and counts its hits;
/// no operator carries them (they serve any expression).
#[test]
fn repeated_datetime_strings_report_the_constructor_memo_at_statement_level() {
    let g = Builder::new().done();
    let q = "UNWIND range(0, 1999) AS i \
             WITH ['2024-05-05T10:00:00Z', '2023-01-01T09:00:00Z'][i % 2] AS s \
             RETURN count(datetime(s)) AS n";
    let (rows, records, statement) = profiled_memos(&g, q);
    assert_eq!(rows, vec![vec![Value::Int(2000)]]);
    assert_eq!(sum(&records), MemoCounters::default());
    assert!(statement.kinds.contains(MemoKindWire::Datetime));
    assert!(!statement.kinds.contains(MemoKindWire::NodeLabel));
    let memo = statement.counters;
    assert!(memo.probes > 0 && memo.hits > 0, "{memo:?}");
    assert_eq!(memo.inserts, 2, "two distinct strings");
    assert!(memo.bytes > 0);
}

/// The distinct-text query the next two pins share: `n` distinct datetime
/// strings, optionally beside a constant one so the memo's hit rate stays
/// above its floor.
fn distinct_datetimes(n: u32, with_constant: bool) -> String {
    let constant = if with_constant {
        "count(datetime('2024-01-01T00:00:00Z' + '')) + "
    } else {
        ""
    };
    format!(
        "UNWIND range(0, {n}) AS i RETURN {constant}count(datetime(\
         toString(date('2000-01-01') + duration({{days: i}})) + 'T00:00:00Z')) AS n"
    )
}

/// Distinct strings never hit: the memo's one review gives up, the
/// statement-level record counts it (`disabled_count`), and its table is
/// returned (the bytes read zero).
#[test]
fn a_low_repetition_constructor_memo_reports_disabled() {
    let g = Builder::new().done();
    let q = distinct_datetimes(70_000, false);
    let (rows, _, statement) = profiled_memos(&g, &q);
    assert_eq!(rows, exec_cypher(&g, &q).rows, "PROFILE changes no answer");
    assert!(statement.counters.probes > 0, "{statement:?}");
    assert_eq!(statement.counters.hits, 0, "{statement:?}");
    assert_eq!(statement.counters.disabled_count, 1, "{statement:?}");
    assert_eq!(statement.counters.bytes, 0, "the disabled table is dropped");
}

/// Past its byte ceiling a memo refuses inserts and still serves hits; the
/// refusals are counted and the rows are the unprofiled run's.
#[test]
fn a_memo_past_its_ceiling_reports_refused_with_unchanged_rows() {
    let g = Builder::new().done();
    let q = distinct_datetimes(70_000, true);
    let (rows, _, statement) = profiled_memos(&g, &q);
    assert_eq!(rows, exec_cypher(&g, &q).rows, "PROFILE changes no answer");
    let memo = statement.counters;
    assert!(memo.refused > 0, "{memo:?}");
    assert!(memo.hits > 0, "hits are still served: {memo:?}");
    assert_eq!(memo.disabled_count, 0, "{memo:?}");
    assert!(
        memo.bytes >= 8 << 20,
        "the table sits at its ceiling: {memo:?}"
    );
}

/// ADR-0654 §6: a statement that is not PROFILEd never reads a memo — the
/// stats reader runs only behind the armed PROFILE gate, whatever the memos
/// did. (Test builds default the arm flag on; this disarms it explicitly.)
#[test]
fn a_non_profile_run_never_reads_a_memo() {
    let g = layered_graph();
    struct Rearm;
    impl Drop for Rearm {
        fn drop(&mut self) {
            arm_store_profiling();
        }
    }
    disarm_store_profiling();
    let _rearm = Rearm;
    let before = STATS_READS.with(std::cell::Cell::get);
    for q in guarded_queries() {
        exec_cypher(&g, &q);
    }
    exec_cypher(
        &g,
        "UNWIND range(0, 99) AS i RETURN count(datetime(['2024-05-05T10:00:00Z'][i % 1]))",
    );
    assert_eq!(
        STATS_READS.with(std::cell::Cell::get),
        before,
        "a disarmed run read no memo"
    );
    arm_store_profiling();
    let q = "MATCH (a:Hub)((m)-[:E]->(n:U)){1,}(y:V) RETURN count(DISTINCT y)";
    let _ = profiled_memos(&g, q);
    assert!(
        STATS_READS.with(std::cell::Cell::get) > before,
        "the armed PROFILE run reads its memos"
    );
}
