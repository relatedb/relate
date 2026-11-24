//! ADR-0468's scan/filter/project chain (LABS-359): the differential
//! contract — every query the chain takes answers exactly what the
//! per-row path answers — plus the engagement gate's declines, the null
//! semantics, and the error parity. The openCypher TCK remains the wide
//! net; these pin the chain's own behaviors.

use std::collections::BTreeMap;

use relate_core::Value;

use crate::graph::Graph;

use super::batch::{take_batch_engagements, with_batch_disabled};
use super::take_zone_skips;
use super::test_support::{
    column, error_of, exec_autocommit, int_cell, string_cell, try_exec_cypher_params, Builder,
};

/// The `Comment` population exercising the filter's whole vocabulary:
/// present, missing (null), and mixed-type properties, plus one `Root`
/// with a `T` edge for the traversal-decline shapes.
fn comments() -> Graph {
    let g = Builder::new().done();
    exec_autocommit(
        &g,
        "CREATE (:Comment {id: 'short', length: 10}),
              (:Comment {id: 'long', length: 2000}),
              (:Comment {id: 'longer', length: 4000}),
              (:Comment {id: 'missing'}),
              (:Comment {id: 'textual', length: 'many'}),
              (:Comment {id: 'flagged', length: 3000, archived: true}),
              (:Root {id: 'root'})",
    )
    .unwrap();
    exec_autocommit(
        &g,
        "MATCH (c:Comment {id: 'long'}), (r:Root) CREATE (c)-[:T]->(r)",
    )
    .unwrap();
    g
}

/// The differential core: `cypher` must answer identically with the batch
/// chain on and forced off, and (when `engages`) the chain must have
/// taken it exactly once. Returns the batch-path result.
fn differential(g: &Graph, cypher: &str, engages: bool) -> relate_core::Result<super::QueryResult> {
    let batched = super::test_support::try_exec_cypher(g, cypher)?;
    let engagements = take_batch_engagements();
    assert_eq!(
        engagements,
        u32::from(engages),
        "engagement for {cypher}: expected {}, took {}",
        u32::from(engages),
        engagements
    );
    let per_row = with_batch_disabled(|| super::test_support::try_exec_cypher(g, cypher))?;
    assert_eq!(
        batched.columns, per_row.columns,
        "columns diverge for {cypher}"
    );
    assert_eq!(batched.rows, per_row.rows, "rows diverge for {cypher}");
    Ok(batched)
}

/// The `id` strings of result column `name`, in result order.
fn ids(result: &super::QueryResult, name: &str) -> Vec<String> {
    column(result, name, string_cell)
}

/// The acceptance shape and its filter vocabulary: the batch chain's
/// answers are the per-row path's, and the counts are the filtered
/// population's.
#[test]
fn where_filtering_and_count_match_the_row_path() {
    let g = comments();
    let counted = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > 1000 RETURN count(c.id)",
        true,
    )
    .unwrap();
    assert_eq!(counted.rows, vec![vec![Value::Int(3)]]);
    // A string `length` compares null against the integer bound and
    // drops; a missing `length` is a null slot and drops the same way.
    let filtered = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > 1000 RETURN c.id LIMIT 99",
        true,
    )
    .unwrap();
    let mut kept = ids(&filtered, "c.id");
    kept.sort();
    assert_eq!(kept, vec!["flagged", "long", "longer"]);
}

/// The clause's output is the binding column and nothing else: the same
/// order the per-row path keeps (the differential's row equality), every
/// candidate exactly once, and no scratch property column leaking into
/// the result's shape.
#[test]
fn scan_results_keep_the_scan_order_and_bind_the_variable() {
    let g = comments();
    let batched = differential(&g, "MATCH (c:Comment) RETURN c.id LIMIT 99", true).unwrap();
    // The LIMIT follower keeps the scan order (no sort); the ids are the
    // whole population, exactly once each.
    let mut kept = ids(&batched, "c.id");
    assert_eq!(kept.len(), 6);
    kept.sort();
    assert_eq!(
        kept,
        vec!["flagged", "long", "longer", "missing", "short", "textual"]
    );
    assert_eq!(batched.columns, vec!["c.id".to_string()]);
}

/// Null predicates read the bitmap, and a null bound drops every row.
#[test]
fn null_predicates_read_the_bitmap() {
    let g = comments();
    let is_null = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length IS NULL RETURN c.id LIMIT 99",
        true,
    )
    .unwrap();
    assert_eq!(ids(&is_null, "c.id"), vec!["missing"]);
    let not_null = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length IS NOT NULL RETURN count(c.id)",
        true,
    )
    .unwrap();
    assert_eq!(not_null.rows, vec![vec![Value::Int(5)]]);
    let null_bound = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > null RETURN c.id LIMIT 99",
        true,
    )
    .unwrap();
    assert!(null_bound.rows.is_empty());
}

/// Several conjuncts share the batch, short-circuiting per conjunct: a
/// row the first drops never reaches the second, and one key read by two
/// conjuncts is one scratch column.
#[test]
fn several_conjuncts_short_circuit_like_the_row_path() {
    let g = comments();
    let result = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > 1000 AND c.archived IS NULL RETURN c.id LIMIT 99",
        true,
    )
    .unwrap();
    // `flagged` carries `archived: true` (IS NULL drops it); `long` and
    // `longer` carry no `archived` (a null slot — IS NULL keeps them).
    assert_eq!(ids(&result, "c.id"), vec!["long", "longer"]);
    let counted = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > 1000 AND c.length < 3000 RETURN count(c.id)",
        true,
    )
    .unwrap();
    assert_eq!(counted.rows, vec![vec![Value::Int(1)]]);
}

/// The bound vocabulary beyond ordering: strings, lists, parameters.
#[test]
fn string_list_and_parameter_bounds_match_the_row_path() {
    let g = comments();
    let prefixed = differential(
        &g,
        "MATCH (c:Comment) WHERE c.id STARTS WITH 'long' RETURN c.id LIMIT 99",
        true,
    )
    .unwrap();
    let mut kept = ids(&prefixed, "c.id");
    kept.sort();
    assert_eq!(kept, vec!["long", "longer"]);
    differential(
        &g,
        "MATCH (c:Comment) WHERE c.id IN ['short', 'textual'] RETURN count(c.id)",
        true,
    )
    .unwrap();
    // A parameter bound is row-invariant and evaluates once per chunk.
    let params = BTreeMap::from([("min".to_string(), Value::Int(2000))]);
    let batched = try_exec_cypher_params(
        &g,
        "MATCH (c:Comment) WHERE c.length >= $min RETURN count(c.id)",
        &params,
    )
    .unwrap();
    assert_eq!(take_batch_engagements(), 1);
    assert_eq!(batched.rows, vec![vec![Value::Int(3)]]);
}

/// Error parity: an operand only the shared `binary` rejects at
/// evaluation time (an invalid regular expression — `IN 5` is rejected
/// at plan time, before either path runs) errors on both paths with the
/// same message, and an empty population never evaluates it at all.
#[test]
fn error_parity_on_a_bad_bound_operand() {
    let g = comments();
    let batched = error_of(super::test_support::try_exec_cypher(
        &g,
        "MATCH (c:Comment) WHERE c.id =~ '[' RETURN c.id LIMIT 99",
    ));
    let per_row = with_batch_disabled(|| {
        error_of(super::test_support::try_exec_cypher(
            &g,
            "MATCH (c:Comment) WHERE c.id =~ '[' RETURN c.id LIMIT 99",
        ))
    });
    assert_eq!(batched, per_row);
    // Drain the pair's engagement count (the errored run engaged once
    // before failing) so the differential below reads its own.
    let _ = take_batch_engagements();
    differential(
        &g,
        "MATCH (c:Nope) WHERE c.id =~ '[' RETURN c.id LIMIT 99",
        true,
    )
    .unwrap();
}

/// The engagement gate's declines: every shape the chain cannot prove
/// keeps the per-row path, answering identically.
#[test]
fn the_gate_declines_every_unsupported_shape() {
    let g = comments();
    for cypher in [
        // A traversal is not a scan.
        "MATCH (c:Comment)-[:T]->(:Root) WHERE c.length > 1000 RETURN count(c.id)",
        // OPTIONAL keeps the per-row null-binding arm.
        "OPTIONAL MATCH (c:Comment) WHERE c.length > 1000 RETURN count(c.id)",
        // A pattern property map stays with node_ok's map check.
        "MATCH (c:Comment {length: 2000}) RETURN count(c.id)",
        // OR / NOT keep the per-row tree walk.
        "MATCH (c:Comment) WHERE c.length > 1000 OR c.archived IS NULL RETURN count(c.id)",
        "MATCH (c:Comment) WHERE NOT c.length > 1000 RETURN count(c.id)",
        // Two properties of the row compared: the bound is not
        // row-invariant.
        "MATCH (c:Comment) WHERE c.length > c.archived RETURN count(c.id)",
        // A function over the row.
        "MATCH (c:Comment) WHERE size(c.id) > 3 RETURN count(c.id)",
        // Bindings carried into the clause: not the unbound seed row.
        "UNWIND [1000, 3000] AS min MATCH (c:Comment) WHERE c.length > min RETURN count(c.id)",
        // A sink projection runs in the match sink.
        "MATCH (c:Comment) WHERE c.length > 1000 RETURN c.id",
        "MATCH (c:Comment) WHERE c.length > 1000 RETURN c.id ORDER BY c.id",
        // A second pattern in the same MATCH.
        "MATCH (c:Comment), (r:Root) WHERE c.length > 1000 RETURN count(c.id)",
        // A named path.
        "MATCH p = (c:Comment) WHERE c.length > 1000 RETURN count(p)",
        // A mirrored comparison keeps the per-row spelling.
        "MATCH (c:Comment) WHERE 1000 < c.length RETURN count(c.id)",
    ] {
        differential(&g, cypher, false).unwrap();
    }
}

/// A named `DISTINCT` feeds the clause's first-seen pass (ADR-0555 §3,
/// ADR-0660), so the chain serves the shape unbounded and pushes each row
/// into the seen-set — the answer is the per-row path's, and no `LIMIT`
/// rides the scan (a distinct suffix's `want` is the distinct cap's, which a
/// `WHERE`-bearing clause takes from the walker).
#[test]
fn a_distinct_fold_opens_the_gate_for_a_distinct_suffix() {
    let g = comments();
    differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > 1000 RETURN DISTINCT c.id LIMIT 2",
        true,
    )
    .unwrap();
}

/// A nondeterministic bound keeps its per-row evaluation (its value may
/// differ per row), so the chain declines — engagement only, no row
/// comparison (two per-row runs draw different values).
#[test]
fn a_nondeterministic_bound_declines() {
    let g = comments();
    let _ = super::test_support::try_exec_cypher(
        &g,
        "MATCH (c:Comment) WHERE c.length > toInteger(rand() * 100000) RETURN count(c.id)",
    )
    .unwrap();
    assert_eq!(take_batch_engagements(), 0);
}

/// A population past `CHUNK_ROWS` (512): the chain batches, compacts per
/// chunk, and the boundary still answers the per-row path.
#[test]
fn the_chain_handles_more_than_one_chunk() {
    let g = Builder::new().done();
    let mut create = String::from("CREATE ");
    for i in 0..1200 {
        if i > 0 {
            create.push_str(", ");
        }
        create.push_str(&format!("(:Bulk {{n: {i}}})"));
    }
    exec_autocommit(&g, &create).unwrap();
    let counted =
        differential(&g, "MATCH (b:Bulk) WHERE b.n > 100 RETURN count(b.n)", true).unwrap();
    assert_eq!(counted.rows, vec![vec![Value::Int(1099)]]);
    let paged = differential(
        &g,
        "MATCH (b:Bulk) WHERE b.n > 1195 RETURN b.n LIMIT 99",
        true,
    )
    .unwrap();
    let values = column(&paged, "b.n", int_cell);
    // Creation order is id order is scan order: the page keeps it.
    assert_eq!(values, (1196..1200).collect::<Vec<_>>());
}

/// LABS-645: the zone skip is measured and the answer is unchanged. The
/// Bulk population is one typed Int column over three chunks —
/// `[0, 511]`, `[512, 1023]`, `[1024, 1199]` in scan order — so every
/// skip decision below names its chunks. The differential (batch chain
/// vs the per-row path forced off) pins the answers; the counter pins
/// the skips — the per-row run the differential makes counts none.
#[test]
fn zone_skips_are_measured_and_answers_unchanged() {
    let g = Builder::new().done();
    let mut create = String::from("CREATE ");
    for i in 0..1200 {
        if i > 0 {
            create.push_str(", ");
        }
        create.push_str(&format!("(:Bulk {{n: {i}}})"));
    }
    exec_autocommit(&g, &create).unwrap();
    // `> 1195`: the first two chunks' zones ([0, 511], [512, 1023])
    // prove no match and skip; the third ([1024, 1199]) holds
    // 1196..=1199 and walks. Two skips, four rows, scan order kept.
    let paged = differential(
        &g,
        "MATCH (b:Bulk) WHERE b.n > 1195 RETURN b.n LIMIT 99",
        true,
    )
    .unwrap();
    let values = column(&paged, "b.n", int_cell);
    assert_eq!(values, (1196..1200).collect::<Vec<_>>());
    assert_eq!(take_zone_skips(), 2, "the two provable chunks skipped");
    // `IS NULL` over the null-free column: the zone's null-presence
    // fact proves no row satisfies it — every chunk skips.
    let nulled = differential(
        &g,
        "MATCH (b:Bulk) WHERE b.n IS NULL RETURN count(b.n)",
        true,
    )
    .unwrap();
    assert_eq!(nulled.rows, vec![vec![Value::Int(0)]]);
    assert_eq!(take_zone_skips(), 3, "every chunk skipped");
    // A cross-kind bound: the Int zone proves the comparison holds for
    // no value (every row's comparison is null), all three chunks skip.
    let cross_kind =
        differential(&g, "MATCH (b:Bulk) WHERE b.n > 'x' RETURN count(b.n)", true).unwrap();
    assert_eq!(cross_kind.rows, vec![vec![Value::Int(0)]]);
    assert_eq!(take_zone_skips(), 3, "every chunk skipped");
    // A Utf8 zone: a string bound past the `id` column's max skips the
    // one chunk the six-comment population makes.
    let comments = comments();
    let past = differential(
        &comments,
        "MATCH (c:Comment) WHERE c.id > 'zzz' RETURN count(c.id)",
        true,
    )
    .unwrap();
    assert_eq!(past.rows, vec![vec![Value::Int(0)]]);
    assert_eq!(take_zone_skips(), 1, "the one chunk skipped");
}

/// LABS-645: the fallback column keeps null-presence only — no min/max
/// over heterogeneous values — so a conjunct over the mixed-type
/// `length` column (Ints, a String, a null) never zone-skips: the row
/// walk answers, exactly as the per-row path does.
#[test]
fn fallback_columns_never_skip() {
    let g = comments();
    // `length` mixes Int and String: the fallback column, no bounds, no
    // skip — the counter stays at zero and the answer is the filtered
    // population's.
    let counted = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > 1000 RETURN count(c.id)",
        true,
    )
    .unwrap();
    assert_eq!(counted.rows, vec![vec![Value::Int(3)]]);
    assert_eq!(
        take_zone_skips(),
        0,
        "the fallback column has no zone facts"
    );
    // A bound past anything the column holds still walks: without
    // bounds there is no proof, never an estimate.
    let past = differential(
        &g,
        "MATCH (c:Comment) WHERE c.length > 4000 RETURN count(c.id)",
        true,
    )
    .unwrap();
    assert_eq!(past.rows, vec![vec![Value::Int(0)]]);
    assert_eq!(take_zone_skips(), 0, "no bounds, no skip");
}
