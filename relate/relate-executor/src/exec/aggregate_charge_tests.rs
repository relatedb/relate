//! The aggregate path's memory-charge family (ADR-0463 rounds 3/6,
//! ADR-0499's flattened exact charge, and LABS-781's group-population
//! running total) — extracted from `aggregate_tests.rs` so that file
//! keeps the fold-semantics tests and the charge tests grow in their own
//! module. The fold tests drive `fold_aggregate_call`'s charge protocol:
//! the slot buffer for every kind, and the exact `owned_heap` running
//! total — for `collect`, whose result retains the vals, and for the
//! transient folds alike, whose vals hold the payloads until the fold
//! completes (the security round's nested-constructed shape must keep
//! refusing). The LABS-781 pins drive the group map's own population:
//! the container the fold's groups retain while they fill.

use std::collections::BTreeMap;

use super::aggregate::{agg_eval, AggScope};
use super::test_support::{
    agg_fn, agg_fn_over, assert_crossing_element, assert_crossing_row, assert_expression_cap,
    group_with_vals, group_with_xs, try_exec_cypher,
};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use relate_core::Value;
use relate_ir::{Expr, SINGLE_INPUT_AGGREGATES};

/// ADR-0463 round 3: the per-group `vals` vector is one allocation of
/// `group.len()` slots for *every* aggregate — the loop derives the kind
/// list from the canonical `SINGLE_INPUT_AGGREGATES` table (plus `count`,
/// whose keyword path the table omits by contract), so a kind added to
/// the vocabulary is covered here by construction. A large single group
/// must hit the typed per-allocation refusal, instead of growing the
/// vector into the allocator's lap.
#[test]
fn aggregate_vals_buffer_is_charged_for_each_aggregate_kind() {
    let cap = 64;
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let graph = ExecGraph::read(&g);
    let group = group_with_xs(&(0..100).collect::<Vec<_>>());
    let names = ["count"]
        .into_iter()
        .chain(SINGLE_INPUT_AGGREGATES.iter().copied());
    for name in names {
        let error = agg_eval(
            &graph,
            AggScope::of_group_with_import(&group, None),
            &agg_fn(name),
            &BTreeMap::new(),
            None,
        )
        .unwrap_err();
        assert_expression_cap(&error, cap);
    }
}

/// ADR-0463 round 6: the vals container's slot buffer is charged exactly
/// once — by `retain_slots`'s construction — not again by a standalone
/// `charge_value_buffer`. Integer payloads add nothing, so the recorded
/// figure is exactly the buffer of `group.len()` slots.
#[test]
fn aggregate_vals_buffer_is_charged_exactly_once() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let group = group_with_xs(&(0..10).collect::<Vec<_>>());
    agg_eval(
        &graph,
        AggScope::of_group_with_import(&group, None),
        &agg_fn("sum"),
        &BTreeMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(
        graph.expression_built_bytes(),
        crate::workspace::value_buffer_bytes(group.len())
    );
}

/// LABS-802 / ADR-0499: the transient folds keep the exact cumulative
/// charge. A payload the row still shares (a bare variable read — the
/// common `count(n)` shape) adds nothing and a tight cap passes; the
/// moment the operand *constructs* its value per row, the exact walk
/// descends into it and the same cap refuses — even one nesting level
/// deep (`[x + pad]`, the security round's `count([$p + $p])` class that
/// any fixed-width estimate misses) and through `collect` alike.
#[test]
fn transient_folds_keep_the_exact_cumulative_charge() {
    let pad = "p".repeat(200);
    let shared = group_with_vals(
        &(0..100)
            .map(|_| Value::String(pad.as_str().into()))
            .collect::<Vec<_>>(),
    );
    let constructed = group_with_vals(
        &(0..100)
            .map(|i| Value::String(i.to_string().into()))
            .collect::<Vec<_>>(),
    );
    // The cap sits between the slot buffer plus one shared-check per row
    // (nothing) and the slot buffer plus one ~217-byte constructed element
    // per row.
    let cap = crate::workspace::value_buffer_bytes(shared.len()) + 4_000;
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let graph = ExecGraph::read(&g);
    // The shared operand passes: the row still holds the payload, so the
    // exact walk is one `Arc` count read per row — the O(1) common case.
    let count = agg_eval(
        &graph,
        AggScope::of_group_with_import(&shared, None),
        &agg_fn("count"),
        &BTreeMap::new(),
        None,
    )
    .expect("a shared operand charges nothing per row");
    assert_eq!(count, Value::Int(100));
    // Every constructed-operand fold over the group refuses through the
    // exact running total — the transient `count` (whose walk descends
    // into the list's element, where a fixed-width figure would see only
    // the list's own slot buffer) and the retaining `collect` alike.
    let constructed_refuses = |name: &str| {
        // `[x + pad]`: one fresh list per row whose element is a
        // constructed ~217-byte string.
        let error = agg_eval(
            &graph,
            AggScope::of_group_with_import(&constructed, None),
            &agg_fn_over(
                name,
                Expr::List(vec![Expr::Binary {
                    op: relate_ir::BinaryOp::Add,
                    left: Box::new(Expr::Variable("x".into())),
                    right: Box::new(Expr::String(pad.clone())),
                }]),
            ),
            &BTreeMap::new(),
            None,
        )
        .unwrap_err();
        assert_expression_cap(&error, cap);
    };
    constructed_refuses("count");
    constructed_refuses("collect");
}

/// LABS-802 / ADR-0499, end to end: the repro family keeps its exact
/// bounds while the flattened charge chain takes the per-row overhead off
/// the profile. A count over small per-row constructions completes (the
/// long-streaming bulk-load shape); the same count over ~2 KB
/// constructions — and the security round's one-level-nested form —
/// refuses through the vals container's exact running total.
#[test]
fn a_transient_fold_over_constructed_operands_stays_exact() {
    let pad = "p".repeat(2_000);
    let cap = 60_000;
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    // Small per-row constructions accumulate far under the cap: completes.
    let counted = try_exec_cypher(
        &g,
        "UNWIND range(1, 100) AS i RETURN count(toString(i)) AS c",
    )
    .expect("small per-row constructions stay under the cap");
    assert_eq!(counted.rows[0][0], Value::Int(100));
    // The ~2 KB per-row constructions refuse — flat, and nested one level
    // deep in a constructed container (the exact walk descends where a
    // fixed-width figure would see only the container's slot buffer).
    let refuses = |query: String, why: &str| {
        let error = try_exec_cypher(&g, &query).err().expect(why);
        assert_expression_cap(&error, cap);
    };
    refuses(
        format!("UNWIND range(1, 100) AS i RETURN count(toString(i) + '{pad}') AS c"),
        "the transient fold's running total must bound the payloads",
    );
    refuses(
        format!("UNWIND range(1, 100) AS i RETURN count([toString(i) + '{pad}']) AS c"),
        "a nested constructed payload must not escape the exact walk",
    );
}

/// LABS-781's repro shape: one `:A` root, `m_ids` `:M` middles on `E`
/// rels, each middle fanning to `leaves` `:B` leaves — a fixed two-hop
/// pattern whose match cardinality is `m_ids * leaves` rows from a small
/// graph, so a group-cap test sizes its input by row count, not node
/// count.
fn two_hop_fan(m_ids: i64, leaves: i64) -> Graph {
    let builder = crate::exec::test_support::Builder::new();
    let a = builder.anon(&["A"]);
    for _ in 0..m_ids {
        let m = builder.anon(&["M"]);
        builder.rel(a, m, "E", &[]);
        for _ in 0..leaves {
            let b = builder.anon(&["B"]);
            builder.rel(m, b, "E", &[]);
        }
    }
    builder.done()
}

/// The one query every LABS-781 group pin runs: an aggregate the
/// pattern-agg folds cannot stream (`collect`, not a group count), so the
/// match materializes its rows and [`super::aggregate::aggregate_ex`]
/// populates one keyless group with them.
const TWO_HOP_COLLECT: &str = "MATCH (a:A)-[:E]->(m:M)-[:E]->(b:B) RETURN size(collect(b)) AS c";

/// LABS-781: the group map is a retaining container. A pattern scan
/// feeding one aggregate group refuses at ~the cap while the rows land —
/// the same per-element running total the comprehension, `collect`, and
/// non-aggregating projection containers follow. Before the fix the
/// group populated uncharged and only the fold's whole-size slot
/// pre-charge saw it, reporting the entire group (49x the configured cap
/// on this shape); the pin bounds the overshoot at one landed row.
#[test]
fn an_aggregating_group_is_capped_by_its_running_total() {
    let cap = 64_000;
    let mut g = two_hop_fan(4_000, 12);
    g.configure_expression_memory_limit(Some(cap));
    let error = try_exec_cypher(&g, TWO_HOP_COLLECT)
        .err()
        .expect("a group over the cap must be refused while it populates");
    let used = assert_expression_cap(&error, cap);
    // Small overshoot: the row that crossed the cap — one generous
    // 16-binding row figure bounds any row this shape lands — never the
    // whole group's slot buffer.
    assert_crossing_row(used, cap, 16);
}

/// LABS-781 end to end at the default cap: a three-hop pattern over a
/// tiny shared-middle graph (50 x 50 x 1200 = 3M rows) feeds one
/// aggregate group; every landed row retains its value buffer, so the
/// running total crosses the 256 MiB default cap mid-population and the
/// refusal reports the crossing row — not the multi-gigabyte whole-group
/// figure the fold-time pre-charge produced on the wire repro.
#[test]
fn an_aggregating_group_populated_by_a_large_pattern_scan_refuses_near_the_default_cap() {
    let builder = crate::exec::test_support::Builder::new();
    let a = builder.anon(&["A"]);
    let m1s: Vec<_> = (0..50)
        .map(|_| {
            let m = builder.anon(&["M1"]);
            builder.rel(a, m, "E", &[]);
            m
        })
        .collect();
    let m2s: Vec<_> = (0..50).map(|_| builder.anon(&["M2"])).collect();
    for m1 in &m1s {
        for m2 in &m2s {
            builder.rel(*m1, *m2, "E", &[]);
        }
    }
    let bs: Vec<_> = (0..1_200).map(|_| builder.anon(&["B"])).collect();
    for m2 in &m2s {
        for b in &bs {
            builder.rel(*m2, *b, "E", &[]);
        }
    }
    let g = builder.done();
    let error = try_exec_cypher(
        &g,
        "MATCH (a:A)-[:E]->(m1:M1)-[:E]->(m2:M2)-[:E]->(b:B) RETURN size(collect(b)) AS c",
    )
    .err()
    .expect("a group over the default cap must be refused while it populates");
    let cap = crate::memory::EXPRESSION_BUDGET_BYTES;
    let used = assert_expression_cap(&error, cap);
    // Small overshoot at the 256 MiB cap: one landed row (a generous
    // 8-binding figure bounds this shape's narrow rows), never the
    // whole group.
    assert_crossing_row(used, cap, 8);
}

/// LABS-781 (stellar round): a new group's landing charges its key to
/// the running total — the map slot's growth plus the key's slots and
/// owned payload — so a `GROUP BY` over a constructed key expression
/// cannot retain unbounded key bytes under the cap. The cap sits between
/// the rows-and-entries total (which passes) and the keys' payloads
/// (which cross it), so the refusal is the keys' own.
#[test]
fn a_grouped_aggregation_is_capped_by_its_keys_running_total() {
    let cap = 50_000;
    let pad = "p".repeat(2_000);
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let query =
        format!("UNWIND range(1, 200) AS i RETURN toString(i) + '{pad}' AS k, count(*) AS c");
    let error = try_exec_cypher(&g, &query)
        .err()
        .expect("the retained keys must cross the cap");
    let used = assert_expression_cap(&error, cap);
    // The crossing element is one group's landing, and the bound drives
    // this pin's own real figures: the cap crosses by the 25th group
    // (each key alone is at least the ~2 KB pad), so the map never
    // passes its 32nd reserved slot — the largest slot-growth step a
    // landing can hit is the 16-entry boundary — and `200` is the
    // longest `toString` argument, so `"200" + pad` is the largest key
    // any landing charges. The entry delta carries that key's own
    // payload and slot buffer beside the step; one generous 2-slot row
    // figure bounds the landed row beside it.
    let one_landing = crate::workspace::group_entry_insert_delta(
        16,
        &[crate::group_key::GroupKey::new(&Value::String(
            format!("200{pad}").as_str().into(),
        ))],
    ) + crate::workspace::retained_rows_bytes(1, 2);
    assert_crossing_element(used, cap, one_landing);
}

/// LABS-781 (rust-review round): the row charge runs inside the entry
/// arms because the probe `GroupKey`'s `Arc` clone shares the row's key
/// payload — a duplicate-key row landing in an occupied group must be
/// charged in full (the probe is dropped by then), never skipped as a
/// shared payload. One million rows over one thousand duplicate keys,
/// each row solely owning its ~190-byte string: every producer stays
/// under the default cap, and the group's running total — the duplicate
/// rows' payloads plus buffers — crosses it mid-population.
#[test]
fn an_aggregating_group_of_duplicate_key_rows_is_capped_by_its_running_total() {
    let pad = "p".repeat(170);
    let query = format!(
        "UNWIND [x IN range(1, 1000000) | toString(x % 1000) + '{pad}'] AS k RETURN k, count(*) AS c"
    );
    let error = try_exec_cypher(&Graph::default(), &query)
        .err()
        .expect("the duplicate-key rows' payloads must cross the cap");
    let cap = crate::memory::EXPRESSION_BUDGET_BYTES;
    let used = assert_expression_cap(&error, cap);
    // One landed row: its ~190-byte key payload plus its buffers.
    let one_landing =
        relate_core::str_logical_bytes(&pad) + crate::workspace::retained_rows_bytes(1, 2);
    assert_crossing_element(used, cap, one_landing);
}

/// LABS-781 (security round): the streaming aggregate folds build the
/// same key-retaining group tables the match-then-aggregate path does,
/// so the memory-exhaustion class must not be reachable around the
/// group map through the fold dispatch. A one-hop fold over 4,000
/// distinct constructed keys (~620 bytes each) refuses while the groups
/// land — before the fold's table was charged, this query completed
/// unrefused at any group count.
#[test]
fn a_streaming_fold_group_table_is_capped_by_its_running_total() {
    let cap = 64_000;
    let mut g = super::test_support::hub_out(4_000);
    g.configure_expression_memory_limit(Some(cap));
    let pad = "p".repeat(600);
    let query =
        format!("MATCH (a:A)-[:E]->(b:B) RETURN toString(b.k) + '{pad}' AS k, count(*) AS c");
    let error = try_exec_cypher(&g, &query)
        .err()
        .expect("the fold's group-table landings must cross the cap");
    let used = assert_expression_cap(&error, cap);
    // The crossing element is one group's landing. The bound drives the
    // pin's real figures: each key alone is at least the ~616-byte pad,
    // so the cap crosses before the 128th group and the largest
    // slot-growth step a landing can hit is the 64-entry boundary —
    // `group_entry_insert_delta(64, ..)` carries that step beside the
    // largest key this pin builds (`"3999"` is the longest `k`), and
    // `value_buffer_bytes(1)` is the one-element key-half `vals` buffer
    // the table retains beside the key.
    let one_landing = crate::exec::test_support::group_landing_bound(
        64,
        &Value::String(format!("3999{pad}").as_str().into()),
    );
    assert_crossing_element(used, cap, one_landing);
}

/// LABS-781 (security round, folded from the bindings session's
/// verification branch): the one-hop pattern fold serves a keyed count
/// itself — its group table is the retaining container, not the eager
/// aggregate's — so a `GROUP BY` over constructed key expressions must
/// refuse at ~the cap there too. Forty distinct names with a ~2 KB pad
/// in the key cross a 50 KB cap at a group's landing; before the fix
/// the fold's groups populated uncharged (the wire repro's class was
/// reachable through the fold dispatch).
#[test]
fn a_one_hop_fold_served_grouped_count_is_capped_by_its_keys_running_total() {
    let cap = 50_000;
    let pad = "p".repeat(2_000);
    let builder = crate::exec::test_support::Builder::new();
    for i in 0..40 {
        let a = builder.prop_node(
            &format!("a{i}"),
            &["A"],
            &[("name", Value::String(format!("n{i}").into()))],
        );
        let b = builder.node(&format!("b{i}"), &["B"]);
        builder.edge(a, b);
    }
    let mut g = builder.done();
    g.configure_expression_memory_limit(Some(cap));
    let query = format!("MATCH (a:A)-[:E]->(b:B) RETURN a.name + '{pad}' AS k, count(*) AS c");
    let error = try_exec_cypher(&g, &query)
        .err()
        .expect("the fold-served group's retained keys must cross the cap");
    let used = assert_expression_cap(&error, cap);
    // The crossing element is one group's landing, and the bound drives
    // the pin's real figures: each key alone is at least the ~2 KB pad,
    // so the cap crosses before the 32nd group and the largest
    // slot-growth step a landing can hit is the 16-entry boundary;
    // `group_entry_insert_delta(16, ..)` carries that step beside the
    // largest key this pin builds (`"n39"` is the longest name), and
    // `value_buffer_bytes(1)` is the one-element key-half vals buffer
    // the table retains beside the key.
    let one_landing = crate::exec::test_support::group_landing_bound(
        16,
        &Value::String(format!("n39{pad}").as_str().into()),
    );
    assert_crossing_element(used, cap, one_landing);
}

/// LABS-781 (rust round 11): a fold-served `count(DISTINCT ..)` over a
/// constructed operand retains one payload per distinct value in the
/// seat's `DistinctSet` — the set's structure rides its workspace
/// reservation, but the expression budget must see the payloads. Forty
/// distinct ~2 KB constructed operands cross a 50 KB cap while the set
/// grows; before the charge this shape escaped every running total.
#[test]
fn a_fold_served_distinct_count_is_capped_by_its_running_total() {
    let cap = 50_000;
    let pad = "p".repeat(2_000);
    let builder = crate::exec::test_support::Builder::new();
    for i in 0..40 {
        let a = builder.prop_node(
            &format!("a{i}"),
            &["A"],
            &[("name", Value::String(format!("n{i}").into()))],
        );
        let b = builder.node(&format!("b{i}"), &["B"]);
        builder.edge(a, b);
    }
    let mut g = builder.done();
    g.configure_expression_memory_limit(Some(cap));
    let query = format!("MATCH (a:A)-[:E]->(b:B) RETURN count(DISTINCT a.name + '{pad}') AS c");
    let error = try_exec_cypher(&g, &query)
        .err()
        .expect("the distinct set's retained payloads must cross the cap");
    let used = assert_expression_cap(&error, cap);
    // The crossing element is one distinct value's landing: its ~2 KB
    // payload (the set's structural growth is the token ledger's), the
    // largest key this pin constructs.
    let one_landing = relate_core::str_logical_bytes(&format!("n39{pad}"));
    assert_crossing_element(used, cap, one_landing);
}

/// LABS-781 (security round, folded from the bindings session's
/// verification branch and reworked per the stellar round): the
/// fixed-length path fold's group table is the same retaining
/// container — a keyed count over a two-hop pattern must refuse at
/// ~the cap while its groups land. The key is a bare `a.name`
/// projection (the `var.prop` shape `compile_path`'s key plan serves —
/// a computed key expression would decline the fold and test the eager
/// group map instead), with the ~2 KB pad stored in the property.
#[test]
fn a_path_fold_served_grouped_count_is_capped_by_its_keys_running_total() {
    let cap = 50_000;
    let pad = "p".repeat(2_000);
    let builder = crate::exec::test_support::Builder::new();
    let ms: Vec<_> = (0..4)
        .map(|j| builder.node(&format!("m{j}"), &["M"]))
        .collect();
    for i in 0..40 {
        let a = builder.prop_node(
            &format!("a{i}"),
            &["A"],
            &[("name", Value::String(format!("n{i}{pad}").into()))],
        );
        let b = builder.node(&format!("b{i}"), &["B"]);
        builder.edge(a, ms[i % ms.len()]);
        builder.edge(ms[i % ms.len()], b);
    }
    let mut g = builder.done();
    g.configure_expression_memory_limit(Some(cap));
    let query = "MATCH (a:A)-[:E]->(:M)-[:E]->(b:B) RETURN a.name AS k, count(*) AS c";
    let error = try_exec_cypher(&g, query)
        .err()
        .expect("the path fold's retained keys must cross the cap");
    let used = assert_expression_cap(&error, cap);
    // The same real-figure bound as the one-hop pin: the cap crosses
    // before the 32nd group, the largest step is the 16-entry boundary,
    // and `"n39" + pad` is the largest name.
    let one_landing = crate::exec::test_support::group_landing_bound(
        16,
        &Value::String(format!("n39{pad}").as_str().into()),
    );
    assert_crossing_element(used, cap, one_landing);
}

/// LABS-781 (rust round 7): the aggregating projection's output
/// container is one retaining container too — every emitted group row
/// carries its seed bindings, projected values, aggregate results and
/// parked ORDER BY keys. Half a million groups whose parked keys are
/// ~2 KB constructed strings cross the default cap while the groups
/// land in the output, after the group table itself (integer keys,
/// ~125 MB) stays comfortably under it — before this charge the same
/// query retained every parked payload unrefused.
#[test]
fn an_aggregating_projection_result_is_capped_by_its_running_total() {
    let pad = "p".repeat(2_000);
    let query = format!(
        "UNWIND range(1, 1000000) AS i RETURN i % 500000 AS k, count(*) AS c \
         ORDER BY toString(k) + '{pad}'"
    );
    let error = try_exec_cypher(&Graph::default(), &query)
        .err()
        .expect("the aggregating projection's output rows must cross the cap");
    let cap = crate::memory::EXPRESSION_BUDGET_BYTES;
    let used = assert_expression_cap(&error, cap);
    // One landed output row: its ~2 KB parked order key plus a generous
    // 4-slot row figure.
    let one_landing =
        relate_core::str_logical_bytes(&pad) + crate::workspace::retained_rows_bytes(1, 4);
    assert_crossing_element(used, cap, one_landing);
}

/// LABS-781 (OPLANE_REQ-00345609, final round): the MATCH output
/// container is a retaining container — every landed row charges the
/// expression budget at the dispatch layer where every operator's rows
/// land (the frontier family's emission sites and the walker all feed
/// that one point; the kernels stay untouched). A two-hop scan into a
/// plain projection refuses at ~the cap with the crossing row's
/// figure.
#[test]
fn a_match_populated_container_is_capped_by_its_running_total() {
    let cap = 64_000;
    let mut g = two_hop_fan(4_000, 12);
    g.configure_expression_memory_limit(Some(cap));
    let error = try_exec_cypher(&g, "MATCH (a:A)-[:E]->(m:M)-[:E]->(b:B) RETURN b")
        .err()
        .expect("the match-populated container must be refused at the cap");
    let used = assert_expression_cap(&error, cap);
    // One landed row: a generous 16-binding row figure bounds this
    // shape's narrow rows.
    assert_crossing_row(used, cap, 16);
}

/// The no-false-positive form: the same scan under a cap the container
/// fits in completes with the exact row count.
#[test]
fn a_match_populated_container_under_the_cap_completes() {
    let mut g = two_hop_fan(4_000, 12);
    g.configure_expression_memory_limit(Some(20_000_000));
    let result = try_exec_cypher(&g, "MATCH (a:A)-[:E]->(m:M)-[:E]->(b:B) RETURN b")
        .expect("a match-populated container under the cap completes");
    assert_eq!(result.rows.len(), 48_000);
}

/// LABS-781's no-false-positive pin: the same two-hop scan under a cap
/// the group fits in completes — the population charge never refuses a
/// container the cap admits.
#[test]
fn an_aggregating_group_under_the_cap_completes() {
    let mut g = two_hop_fan(4_000, 12);
    g.configure_expression_memory_limit(Some(20_000_000));
    let result = try_exec_cypher(&g, TWO_HOP_COLLECT).expect("a group under the cap completes");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::Int(48_000));
}
