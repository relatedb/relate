//! The aggregate path's memory-charge family (ADR-0463 rounds 3/6,
//! ADR-0499's flattened exact charge, and LABS-781's group-population
//! running total) — extracted from `aggregate_tests.rs` so that file
//! keeps the fold-semantics tests and the charge tests grow in their own
//! module. The fold tests drive the streaming accumulators' charge
//! protocol: a non-retaining aggregate (`count`, `sum`, `avg`, `min`,
//! `max`, `stDev`, `stDevP`) keeps constant-size state and is charged
//! nothing per row, while `collect`, the percentiles and DISTINCT calls
//! retain their operands and charge the exact `owned_heap` running total
//! (the security round's nested-constructed shape must keep refusing
//! there). The LABS-781 pins drive the group map's own population: the
//! container holding one seed row and one accumulator set per group.

use std::collections::BTreeMap;

use super::test_support::{
    agg_eval_rows, agg_fn, agg_fn_over, assert_crossing_element, assert_crossing_row,
    assert_expression_cap, group_with_vals, group_with_xs, try_exec_cypher,
};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use relate_core::Value;
use relate_ir::{classify_aggregate, Expr, SINGLE_INPUT_AGGREGATES};

use crate::eval::StreamFold;

/// A non-retaining aggregate keeps constant-size state: the group's row
/// count never reaches the expression budget. Every kind in the canonical
/// `SINGLE_INPUT_AGGREGATES` table (plus `count`, whose keyword path the
/// table omits by contract) is classified by whether it has a streaming
/// accumulator — so a kind added to the vocabulary is covered by
/// construction — and a 100,000-row group passes a 1 KiB cap for every
/// streaming kind while every retaining kind refuses with the typed
/// per-allocation error, instead of growing its operand buffer into the
/// allocator's lap.
#[test]
fn only_retaining_aggregates_charge_per_row() {
    let cap = 1_024;
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let graph = ExecGraph::read(&g);
    let group = group_with_xs(&(0..100_000).collect::<Vec<_>>());
    let names = ["count"]
        .into_iter()
        .chain(SINGLE_INPUT_AGGREGATES.iter().copied());
    for name in names {
        let kind = classify_aggregate(name).expect("the table names aggregate kinds");
        let result = agg_eval_rows(&graph, &group, &agg_fn(name), &BTreeMap::new());
        if StreamFold::for_kind(kind).is_some() {
            result.unwrap_or_else(|error| panic!("{name} streams, yet refused: {error}"));
        } else {
            assert_expression_cap(&result.unwrap_err(), cap);
        }
    }
}

/// A DISTINCT call that has a streaming accumulator keeps no operand per
/// distinct key: the seen-set's keys are the only per-value state, so a
/// 100,000-distinct-value group passes a cap that `collect(DISTINCT)`
/// (which retains its operands) crosses.
#[test]
fn distinct_streaming_aggregates_retain_no_operands() {
    let cap = 64 * 1_024;
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let graph = ExecGraph::read(&g);
    let group = group_with_xs(&(0..100_000).collect::<Vec<_>>());
    let distinct = |name: &str| Expr::Function {
        name: name.into(),
        distinct: true,
        args: vec![Expr::Variable("x".into())],
    };
    for name in ["count", "sum", "avg", "stDev", "min", "max"] {
        agg_eval_rows(&graph, &group, &distinct(name), &BTreeMap::new())
            .unwrap_or_else(|error| panic!("{name}(DISTINCT) streams, yet refused: {error}"));
    }
    let error = agg_eval_rows(&graph, &group, &distinct("collect"), &BTreeMap::new()).unwrap_err();
    assert_expression_cap(&error, cap);
}

/// A DISTINCT call folds only first-seen operands through its seen-set, so
/// its state is proportional to the distinct values: `count(DISTINCT x)`
/// over 200,000 rows of 10 distinct values stays under a 64 KiB cap that a
/// non-DISTINCT `collect` and a `collect(DISTINCT)` over distinct operands
/// both cross.
#[test]
fn distinct_state_is_proportional_to_distinct_values() {
    let cap = 64 * 1_024;
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let graph = ExecGraph::read(&g);
    let few = group_with_xs(&(0..200_000).map(|i| i % 10).collect::<Vec<_>>());
    let distinct = |name: &str| Expr::Function {
        name: name.into(),
        distinct: true,
        args: vec![Expr::Variable("x".into())],
    };
    let counted = agg_eval_rows(&graph, &few, &distinct("count"), &BTreeMap::new())
        .expect("ten distinct values fit the cap");
    assert_eq!(counted, Value::Int(10));
    let collected = agg_eval_rows(&graph, &few, &distinct("collect"), &BTreeMap::new())
        .expect("ten distinct values fit the cap");
    assert!(matches!(collected, Value::List(items) if items.len() == 10));
    let error = agg_eval_rows(&graph, &few, &agg_fn("collect"), &BTreeMap::new()).unwrap_err();
    assert_expression_cap(&error, cap);
    let many = group_with_xs(&(0..200_000).collect::<Vec<_>>());
    let error = agg_eval_rows(&graph, &many, &distinct("collect"), &BTreeMap::new()).unwrap_err();
    assert_expression_cap(&error, cap);
}

/// LABS-802 / ADR-0499: a retaining fold keeps the exact cumulative charge.
/// A payload the row still shares (a bare variable read — the common
/// `collect(n)` shape) adds nothing and a tight cap passes; the moment the
/// operand *constructs* its value per row, the exact walk descends into it
/// and the same cap refuses — even one nesting level deep (`[x + pad]`, the
/// security round's `collect([$p + $p])` class that any fixed-width
/// estimate misses). A streaming `count` over the same constructions
/// retains none of them — each value is dropped as the row is spent, and
/// the per-allocation cap still bounds any one construction.
#[test]
fn retaining_folds_keep_the_exact_cumulative_charge() {
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
    let collected = agg_eval_rows(&graph, &shared, &agg_fn("collect"), &BTreeMap::new())
        .expect("a shared operand charges nothing per row");
    assert!(matches!(collected, Value::List(items) if items.len() == 100));
    // The retaining `collect` over constructed operands refuses through
    // the exact running total, whose walk descends into the list's element
    // where a fixed-width figure would see only the list's own slot buffer.
    let constructed_over = |name: &str| {
        // `[x + pad]`: one fresh list per row whose element is a
        // constructed ~217-byte string.
        agg_eval_rows(
            &graph,
            &constructed,
            &agg_fn_over(
                name,
                Expr::List(vec![Expr::Binary {
                    op: relate_ir::BinaryOp::Add,
                    left: Box::new(Expr::Variable("x".into())),
                    right: Box::new(Expr::String(pad.clone())),
                }]),
            ),
            &BTreeMap::new(),
        )
    };
    assert_expression_cap(&constructed_over("collect").unwrap_err(), cap);
    // The streaming count keeps nothing of them and completes under the
    // same cap.
    assert_eq!(constructed_over("count").unwrap(), Value::Int(100));
}

/// LABS-802 / ADR-0499, end to end: the retaining fold keeps its exact
/// bounds while a streaming fold over the same operands holds nothing. A
/// count over per-row constructions — small or ~2 KB, flat or nested one
/// level deep — completes under a 60 KB cap (it keeps one counter, and the
/// per-allocation cap bounds any one construction); `collect` over the same
/// constructions refuses through its container's exact running total, the
/// nested form included.
#[test]
fn a_retaining_fold_over_constructed_operands_stays_exact() {
    let pad = "p".repeat(2_000);
    let cap = 60_000;
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(cap));
    let counted = |query: String| {
        let result =
            try_exec_cypher(&g, &query).expect("a streaming count retains no per-row construction");
        assert_eq!(result.rows[0][0], Value::Int(100));
    };
    counted("UNWIND range(1, 100) AS i RETURN count(toString(i)) AS c".to_string());
    counted(format!(
        "UNWIND range(1, 100) AS i RETURN count(toString(i) + '{pad}') AS c"
    ));
    counted(format!(
        "UNWIND range(1, 100) AS i RETURN count([toString(i) + '{pad}']) AS c"
    ));
    // The ~2 KB per-row constructions refuse in the retaining `collect` —
    // flat, and nested one level deep in a constructed container (the exact
    // walk descends where a fixed-width figure would see only the
    // container's slot buffer).
    let refuses = |query: String, why: &str| {
        let error = try_exec_cypher(&g, &query).err().expect(why);
        assert_expression_cap(&error, cap);
    };
    refuses(
        format!("UNWIND range(1, 100) AS i RETURN size(collect(toString(i) + '{pad}')) AS c"),
        "the retaining fold's running total must bound the payloads",
    );
    refuses(
        format!("UNWIND range(1, 100) AS i RETURN size(collect([toString(i) + '{pad}'])) AS c"),
        "a nested constructed payload must not escape the exact walk",
    );
}

/// What a streaming `min`/`max` keeps — one operand value per group — is
/// charged: two hundred groups each holding a ~2 KB constructed extreme
/// cross a 100 KB cap while the groups land, where the uncharged payloads
/// let a statement retain many multiples of the cap.
#[test]
fn a_held_extreme_is_charged_to_the_running_total() {
    let pad = "p".repeat(2_000);
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(100_000));
    let query =
        format!("UNWIND range(1, 400) AS i RETURN i % 200 AS g, min(toString(i) + '{pad}') AS m");
    let error = try_exec_cypher(&g, &query)
        .err()
        .expect("the held extremes' payloads must cross the cap");
    assert_expression_cap(&error, 100_000);
}

/// The retained operands of every group are alive together, so they share
/// one running total: a hundred groups each collecting a second ~2 KB value
/// (the first rides its group's seed row, already charged) cross a cap that
/// no single group's container approaches. A per-group total would pass it,
/// and a value cloned from the spent input row would escape the charge.
#[test]
fn retained_operands_across_groups_share_one_running_total() {
    let pad = "p".repeat(2_000);
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(250_000));
    let query = format!(
        "UNWIND range(1, 200) AS i WITH i % 100 AS g, toString(i) + '{pad}' AS k \
         RETURN g, collect(k) AS c"
    );
    let error = try_exec_cypher(&g, &query)
        .err()
        .expect("the groups' retained operands must cross the shared cap");
    assert_expression_cap(&error, 250_000);
}

/// The reported repro class, end to end: a grouped aggregate over a
/// generated stream holds no per-row state, so a count over a million
/// duplicate-key rows completes under a cap far below the rows' own size
/// (the groups' state — one seed row and one counter each — is what the
/// cap meters), where retaining every row refused at the default cap.
#[test]
fn a_streamed_grouped_count_holds_no_per_row_state() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(100 * 1024 * 1024));
    let result = try_exec_cypher(
        &g,
        "UNWIND range(1, 1000000) AS i RETURN i % 4 AS k, count(*) AS c, sum(i) AS s, \
         avg(i) AS a, min(i) AS lo, max(i) AS hi ORDER BY k",
    )
    .expect("four groups of constant-size accumulators fit a 100 MiB cap");
    assert_eq!(result.rows.len(), 4);
    assert_eq!(result.rows[0][1], Value::Int(250_000));
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

/// LABS-781 (rust-review round), restated for streaming aggregation: a
/// duplicate-key row joining an occupied group is spent once the group's
/// accumulators have it — nothing about it is retained, however many
/// duplicate rows arrive. One million rows over one thousand duplicate
/// keys, each solely owning its ~190-byte string (190 MB of payload the
/// old group map held and refused at the default cap), now complete: the
/// group map holds one seed row per distinct key.
#[test]
fn an_aggregating_group_of_duplicate_key_rows_retains_no_row() {
    let pad = "p".repeat(170);
    let query = format!(
        "UNWIND [x IN range(1, 1000000) | toString(x % 1000) + '{pad}'] AS k RETURN k, count(*) AS c"
    );
    let result = try_exec_cypher(&Graph::default(), &query)
        .expect("duplicate-key rows are spent by the counter, never retained");
    assert_eq!(result.rows.len(), 1000);
    assert!(result.rows.iter().all(|row| row[1] == Value::Int(1000)));
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
