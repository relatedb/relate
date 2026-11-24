//! The PROFILE per-operator metric tests (ADR-0344 phase 2 + metric
//! fields): rows, elapsed time, `db hits` (the store-access delta: hop
//! expansions, neighbor walks and node-store reads), and
//! row-materialization `memory bytes` per clause operator — the NEXT-chain
//! stage keying, the starved-operator facts behind the never-executed
//! marker, and the estimated-vs-actual 10x skew behind the
//! `EstimationMiss` warning (their rendered forms are the boundary's,
//! pinned in `relate-formatter`, ADR-0359).

mod common;

use relate::Value;

use common::{connection, plan};

/// The hops of a whole plan tree — [`common::tree_metric`] over the
/// clause arms' hop reads (the allReduce pruning test's helper, spelled
/// once for its sibling tests).
fn tree_hops(node: &relate::PlanNode) -> u64 {
    common::tree_metric(node, |m| m.reads.hops)
}

/// The frontier levels of a whole plan tree (the `tree_hops` idiom): the
/// clause the frontier family served reports its BFS levels, every other
/// clause — and every clause the family declined — reports 0.
fn tree_frontier_levels(node: &relate::PlanNode) -> u64 {
    common::tree_metric(node, |m| m.frontier.levels)
}

/// The neighbor walks of a whole plan tree (the `tree_hops` idiom): each
/// node's metrics are its own clause arm's store reads, so the sum is
/// the statement's walk total.
fn tree_walks(node: &relate::PlanNode) -> u64 {
    common::tree_metric(node, |m| m.reads.neighbor_walks)
}

/// The rows one clause operator's PROFILE record reports — the one shape
/// the streamed-clause pins share: run the PROFILE statement, find the
/// operator's node ([`common::find_node`], ADR-0359's shared lookup), read
/// its `metrics.rows`.
fn clause_rows(conn: &mut relate::Connection, query: &str, operator: &str) -> u64 {
    let result = common::run(conn, query);
    let tree = plan(&result);
    let node =
        common::find_node(tree, operator).unwrap_or_else(|| panic!("the {operator} clause node"));
    node.metrics
        .unwrap_or_else(|| panic!("the {operator}'s metrics"))
        .rows
}

#[test]
fn profile_of_a_next_chain_carries_each_stages_own_metrics() {
    let mut conn = connection();
    // A NEXT chain with distinct per-stage cardinalities: stage 1
    // produces two rows, stage 2 aggregates them to one. Without the
    // stage in the metrics key the later stage's entries overwrite the
    // earlier stage's same-keyed ones, and stage 1's tree node would
    // report stage 2's counts (ADR-0344 phase 2).
    let result = conn
        .query("PROFILE UNWIND [1, 2] AS i RETURN i AS x NEXT RETURN count(x) AS c")
        .unwrap();
    let tree = plan(&result);
    let stage1 = tree.metrics.expect("stage 1 root metrics");
    assert_eq!(stage1.rows, 2, "stage 1 produced the two UNWIND rows");
    assert!(stage1.time_ns > 0, "{stage1:?}");
    let next = tree
        .children
        .iter()
        .find(|c| c.operator == "Next")
        .expect("the Next stage link");
    let stage2 = next.children[0].metrics.expect("stage 2 root metrics");
    assert_eq!(stage2.rows, 1, "stage 2 produced the single count row");
    assert!(stage2.time_ns > 0, "{stage2:?}");
}

/// An `UNWIND` whose follower is not a projection emits its rows as the
/// inter-clause stream; the clause reports the rows its generator produced
/// once the follower drains it. A probe of the not-yet-pulled stream would
/// report zero.
#[test]
fn profile_unwind_reports_the_rows_it_generated() {
    let mut conn = connection();
    let n = 5;
    assert_eq!(
        clause_rows(
            &mut conn,
            &format!("PROFILE UNWIND range(1, {n}) AS i CREATE (:P {{v: i}})"),
            "Unwind"
        ),
        n
    );
}

/// A `CALL { … } IN TRANSACTIONS` clause answers as a produced stream too:
/// its batches report the rows they emitted, never the empty probe of a
/// stream whose rows are not in hand.
#[test]
fn profile_in_transactions_reports_the_rows_its_batches_emitted() {
    let mut conn = connection();
    let n = 5;
    assert_eq!(
        clause_rows(
            &mut conn,
            &format!(
                "PROFILE UNWIND range(1, {n}) AS i \
                 CALL {{ WITH i CREATE (:T {{v: i}}) }} IN TRANSACTIONS OF 2 ROWS"
            ),
            "CallSubquery"
        ),
        n
    );
}

/// A graph-sensitive `CREATE` (a property expression holding a subquery)
/// resolves every row's patterns against the pre-write graph, so it takes
/// the whole-input hold — and its held form still reports the rows it
/// created, never a clause that looks like it never ran.
#[test]
fn profile_reports_a_graph_sensitive_creates_rows() {
    let mut conn = connection();
    let n = 5;
    assert_eq!(
        clause_rows(
            &mut conn,
            &format!(
                "PROFILE UNWIND range(1, {n}) AS i \
                 CREATE (:P {{v: i, c: COUNT {{ MATCH (m:P) }}}})"
            ),
            "Create"
        ),
        n
    );
}

/// LABS-375: a node scan reads the node store once per candidate it
/// examined, so its `db_hits` is nonzero — before the node reads were
/// counted the scan reported 0 while doing real work.
#[test]
fn a_node_scan_reports_its_store_reads() {
    let mut conn = connection();
    conn.query("CREATE (:Person {name: 'Alice'}), (:Person {name: 'Bob'})")
        .unwrap();
    let result = conn
        .query("PROFILE MATCH (n:Person) RETURN n.name AS name")
        .unwrap();
    let tree = plan(&result);
    let scan = tree
        .children
        .iter()
        .find(|c| c.operator == "NodeByLabelScan" || c.operator == "AllNodesScan")
        .expect("the scan operator");
    let metrics = scan.metrics.expect("the scan's metrics");
    assert_eq!(metrics.rows, 2, "{metrics:?}");
    assert!(
        metrics.db_hits >= 2,
        "one store read per candidate examined: {metrics:?}"
    );
}

/// The metric fields (ADR-0344 metric fields): `db_hits` is the clause's
/// store-access count — the delta of the executor's graph-walk counters
/// (hop expansions, neighbor walks and node-store reads) across the
/// clause's arm — and
/// `memory_bytes` is the row-materialization bytes: the summed logical
/// size (ADR-0042) of the rows the operator produced. Both deterministic
/// alongside the row count: a traversal's walks count with its rows, a
/// projection's bytes follow its values.
#[test]
fn profile_metrics_carry_db_hits_and_memory_bytes() {
    let mut conn = connection();
    conn.query(
        "CREATE (:Person {name: 'Alice'}), (:Person {name: 'Bob'}),
         (:Person {name: 'Cara'})",
    )
    .unwrap();
    conn.query(
        "MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'})
         CREATE (a)-[:KNOWS]->(b)",
    )
    .unwrap();
    let result = conn
        .query("PROFILE MATCH (a:Person)-[k:KNOWS]->(b:Person) RETURN b.name")
        .unwrap();
    let tree = plan(&result);
    // The one pattern is one Expand operator (the traversal) beneath the
    // projection.
    let expand = &tree.children[0];
    assert_eq!(expand.operator, "Expand");
    let expand_metrics = expand.metrics.expect("the expand operator's metrics");
    assert_eq!(expand_metrics.rows, 1, "the traversal produced one row");
    assert!(
        expand_metrics.db_hits > 0,
        "the traversal walked relationships: {expand_metrics:?}"
    );
    // The projection's memory bytes are the rows it produced: one row,
    // one binding `b.name`, an ADR-0042 string ('Bob': 16 + 3) beside the
    // key's own string bytes ('b.name': 16 + 6) — the deterministic sum.
    let root = tree.metrics.expect("the projection's metrics");
    assert_eq!(root.rows, 1);
    assert_eq!(root.memory_bytes, 16 + 6 + 16 + 3, "{root:?}");
    // The projection touches no store — an honestly counted zero.
    assert_eq!(root.db_hits, 0, "{root:?}");
    // The wire form carries the new args beside Rows/TimeNs, emitted
    // together with the same saturation policy.
    let Value::Map(map) = expand.to_value() else {
        panic!("to_value is a map");
    };
    let Value::Map(args) = map.get("args").expect("args") else {
        panic!("args is a map");
    };
    let Value::Int(db_hits) = args.get("DbHits").expect("DbHits") else {
        panic!("DbHits is an integer");
    };
    assert!(*db_hits > 0, "{args:?}");
    let memory = args.get("MemoryBytes").expect("MemoryBytes");
    assert!(matches!(memory, Value::Int(_)), "{args:?}");
}

/// The never-executed marker's facts: an operator whose input clause
/// produced no rows did no work — its metrics stay honestly zero, while
/// the tree's root (the projection that produced the answer) carries its
/// actual metrics. The rendered marker cells ("(never executed)", never a
/// bare zero) are the boundary's rendering — pinned in
/// `relate-formatter`'s plan-text tests (ADR-0359).
#[test]
fn profile_marks_a_starved_operator_with_zero_metrics() {
    let mut conn = connection();
    conn.query("CREATE (:Person {name: 'Alice'})").unwrap();
    let result = conn
        .query("PROFILE MATCH (n:Person) WHERE n.name = 'Nobody' MATCH (m:Person) RETURN m.name")
        .unwrap();
    let tree = plan(&result);
    // The starved MATCH: its input (the filtered MATCH) produced no rows,
    // it produced none either — the collected facts behind the marker.
    let starved = &tree.children[0].children[0];
    assert_eq!(starved.operator, "NodeByLabelScan");
    let metrics = starved.metrics.expect("the starved MATCH's metrics");
    assert_eq!(metrics.rows, 0, "{metrics:?}");
    // The root always executes — the projection that produced the answer.
    let root = tree.metrics.expect("the projection's metrics");
    assert_eq!(root.rows, 0, "{root:?}");
    assert!(root.time_ns > 0, "the root executed: {root:?}");
}

/// The estimated-vs-actual ratio facts: a scan estimated at the label
/// count whose WHERE filters to almost nothing overshoots by 10x or more
/// (the typed `EstimationMiss` warning), while a matching estimate stays
/// warning-free. The rendered ratio cells ("Est/Actual", "25.0x !", the
/// "-" unavailable spelling) are the boundary's rendering — pinned in
/// `relate-formatter`'s plan-text tests (ADR-0359).
#[test]
fn profile_flags_a_ten_x_estimate_skew_with_the_typed_warning() {
    let mut conn = connection();
    conn.query("UNWIND range(1, 25) AS i CREATE (:Person {flag: i = 1})")
        .unwrap();
    let result = conn
        .query("PROFILE MATCH (n:Person) WHERE n.flag RETURN n")
        .unwrap();
    let tree = plan(&result);
    // The scan: estimated at the label count (25), one actual row — a
    // 25.0x overshoot flagged by the typed warning.
    let scan = common::find_node(tree, "NodeByLabelScan").expect("the scan");
    assert_eq!(scan.estimated_rows, Some(25), "{tree:?}");
    let scan_metrics = scan.metrics.expect("the scan's metrics");
    assert_eq!(scan_metrics.rows, 1, "{scan_metrics:?}");
    assert_eq!(
        scan.warnings,
        [relate::PlanWarning::EstimationMiss],
        "the 25-row estimate against one actual row: a 10x overshoot: {scan:?}"
    );
    // A matching estimate: every person, no filter — no warning.
    let matched = conn.query("PROFILE MATCH (n:Person) RETURN n").unwrap();
    let matched_scan =
        common::find_node(plan(&matched), "NodeByLabelScan").expect("the matched scan");
    assert!(
        matched_scan.warnings.is_empty(),
        "a matching estimate carries no warning: {matched_scan:?}"
    );
}

#[test]
fn profile_names_the_walks_start_and_the_read_kinds() {
    // LABS-369: the Expand operator no longer swallows where the walk
    // started or how the one Db Hits total splits. A row-bound pivot
    // (ADR-0350) must name itself, and the read kinds must ride the
    // metric set (emitted together with it).
    let mut conn = connection();
    conn.query(
        "CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})\
         -[:KNOWS]->(c:Person {name: 'Carol'})",
    )
    .unwrap();
    // The pivot: the walk's SECOND element is row-bound from the first
    // clause (`b` is the pattern's index 1), so the expansion anchors on
    // it and walks its prefix backwards (ADR-0350) — the fact the opaque
    // Expand used to hide.
    let result = conn
        .query(
            "PROFILE MATCH (b:Person) WITH b \
             MATCH (a)-[k1:KNOWS]->(b)-[k2:KNOWS]->(c) RETURN a.name AS n",
        )
        .unwrap();
    let tree = plan(&result);
    let expand = common::find_node(tree, "Expand").expect("the expansion");
    let metrics = expand.metrics.expect("PROFILE metrics");
    let start = metrics.start.expect("a walk ran: its start is named");
    assert!(
        matches!(start.reason, relate_executor::StartReason::BoundPivot),
        "the row-bound element anchors the walk: {start:?}"
    );
    assert!(
        start.backward,
        "the pivot walks its prefix backwards: {start:?}"
    );
    // The read kinds ride the metric set: the walk read the store.
    assert!(
        metrics.reads.hops + metrics.reads.neighbor_walks + metrics.reads.node_reads > 0,
        "the walk read the store: {:?}",
        metrics.reads
    );
    assert_eq!(
        metrics.db_hits,
        metrics.reads.hops + metrics.reads.neighbor_walks + metrics.reads.node_reads,
        "the total is exactly the kinds' sum: {:?}",
        metrics.reads
    );
}

/// LABS-805: the hop/walk counters are PROFILE-only aggregates — the
/// walk engines tally per edge into a per-run `WalkTallies` and flush
/// once at the run's end, gated on the PROFILE arm — and the flush keeps
/// the exact per-edge totals the per-edge bumps produced: a fixed
/// two-hop count fold reports its exact walk count, and a following
/// plain statement moves no counter (its EXPLAIN tree proves no
/// PROFILE state leaked).
#[test]
fn profile_reports_the_folds_exact_walk_count() {
    let mut conn = connection();
    conn.query(
        "CREATE (a:Person {name: 'A'})-[:KNOWS]->(b:Person {name: 'B'})-[:KNOWS]->(c:Person {name: 'C'})",
    )
    .unwrap();
    let result = conn
        .query("PROFILE MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) RETURN count(*) AS n")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
    let tree = plan(&result);
    // The fold's walk: hop 1's anchors are the three Person candidates —
    // the outgoing KNOWS rels walked are A->B and B->C (2) — and hop 2
    // continues from the accepted ends: B->C from b=B, none from b=C
    // (1). Three relationships walked in total, zero hop expansions (the
    // path fold counts the `hops` kind per expansion step past the
    // first, which its anchored collect never spells).
    assert_eq!(tree_walks(tree), 3, "the fold walked three rels: {tree:?}");
    assert_eq!(
        tree_hops(tree),
        0,
        "the path fold expands no hops: {tree:?}"
    );
}

#[test]
fn an_explain_tree_names_no_start_and_carries_no_reads() {
    // The start and the read kinds are PROFILE facts: an EXPLAIN tree
    // carries neither, so a consumer never reads a fabricated zero.
    let mut conn = connection();
    let result = conn
        .query("EXPLAIN MATCH (a:Person)-[:KNOWS]->(b) RETURN b")
        .unwrap();
    let tree = plan(&result);
    let expand = common::find_node(tree, "Expand").expect("the expansion");
    assert!(expand.metrics.is_none(), "EXPLAIN carries no metrics");
}

/// The per-phase times (LABS-448): a PROFILEd walk's operators carry the
/// seek's and the walk's elapsed nanoseconds — the scan's candidates
/// resolution is seek time, the expand's relationship traversal is walk
/// time, a clause that walks nothing reports zeros — and the stamps cost
/// nothing on a non-PROFILEd statement (its metrics are simply absent).
#[test]
fn profile_reports_the_seek_and_walk_phase_times() {
    let mut conn = connection();
    conn.query("CREATE (a:Person {name: 'A'})-[:KNOWS]->(b:Person {name: 'B'})")
        .unwrap();
    // A two-element pattern: the label scan resolves candidates (seek),
    // the expansion follows the relationship (walk) — its own arm interleaves
    // per row, and the nested walk_pattern the multi-element recursion
    // performs counts inclusively into both sums.
    let result = conn
        .query("PROFILE MATCH (p:Person)-[:KNOWS]->(f:Person) RETURN f.name")
        .unwrap();
    let tree = plan(&result);
    let expand = tree
        .children
        .iter()
        .find(|c| c.operator.contains("Expand"))
        .expect("the pattern's expand operator");
    let metrics = expand.metrics.expect("the PROFILE metrics");
    assert!(
        metrics.phases.seek_ns > 0,
        "the expand resolved node candidates: {metrics:?}"
    );
    assert!(
        metrics.phases.walk_ns > 0,
        "the expand followed the relationship: {metrics:?}"
    );
    // The phases ride the whole metric block: the sum covers both phases
    // and whatever the arm spent between them.
    let phases_sum = metrics.phases.seek_ns + metrics.phases.walk_ns;
    assert!(
        phases_sum <= metrics.time_ns,
        "the inclusive phases cannot exceed the arm's wall clock: {metrics:?}"
    );
    // A non-walking clause (the projection) reports zeros, never missing
    // values — the stamps exist for every profiled clause.
    let root = tree.metrics.expect("the root's metrics");
    assert_eq!(
        root.phases,
        Default::default(),
        "the projection walks nothing"
    );
}

/// The shortest-path resolver sits inside the seek span too (LABS-448's
/// round-2 finding): a PROFILEd `MATCH SHORTEST` reports its start
/// resolution as seek time — `Nodes > 0` with `seek_ns > 0`, the kind↔phase
/// pairing holding on the shortest route, never a zero-phase read count.
#[test]
fn profile_shortest_path_reports_its_resolution_as_seek_time() {
    let mut conn = connection();
    conn.query("CREATE (a:P {n: 'a'})-[:R]->(b:P {n: 'b'})")
        .unwrap();
    let result = conn
        .query("PROFILE MATCH SHORTEST 1 (a:P {n: 'a'})-[:R]->(b:P {n: 'b'}) RETURN b.n")
        .unwrap();
    let tree = plan(&result);
    let shortest = tree
        .children
        .iter()
        .find(|c| c.operator.to_lowercase().contains("shortest"))
        .or_else(|| tree.children.first())
        .expect("the shortest operator");
    let metrics = shortest.metrics.expect("the PROFILE metrics");
    assert!(
        metrics.reads.node_reads > 0,
        "the resolver read its start candidates: {metrics:?}"
    );
    assert!(
        metrics.phases.seek_ns > 0,
        "the resolver's resolution is seek time, never a zero-phase read: {metrics:?}"
    );
}

/// The quantified expansion's hops count into the `hops` read kind behind
/// `Db Hits`, so the QPP allReduce mid-path pruning is PROFILE-visible
/// end to end: the pruned route query (the allReduce on the MATCH WHERE,
/// advanced per hop) expands strictly fewer hops than the same query's
/// post-filter spelling (the allReduce on a WITH WHERE, evaluated once per
/// complete path) — and both answer the same route count.
#[test]
fn profile_shows_the_quantified_allreduce_pruning() {
    let mut conn = connection();
    // An EV-shaped cyclic road graph: an 8-city ring of 100 km legs with a
    // 250 km chord across each triple and two charge self-loops.
    let mut create = String::from("CREATE ");
    for i in 0..8 {
        create.push_str(&format!("(c{i}:Geo {{id:'c{i}'}}), "));
    }
    for i in 0..8 {
        create.push_str(&format!(
            "(c{i})-[:ROAD {{distance_km: 100.0}}]->(c{}), ",
            (i + 1) % 8
        ));
        create.push_str(&format!(
            "(c{i})-[:ROAD {{distance_km: 250.0}}]->(c{}), ",
            (i + 3) % 8
        ));
    }
    create.push_str("(c2)-[:CHARGE {distance_km: 20.0}]->(c2), ");
    create.push_str("(c6)-[:CHARGE {distance_km: 20.0}]->(c6)");
    conn.query(&create).unwrap();

    let routes = |filter: &str| {
        format!(
            "PROFILE MATCH REPEATABLE ELEMENTS p = (a:Geo {{id:'c0'}}) \
             (()-[rels:ROAD|CHARGE]-(x:Geo)){{1,6}} (b:Geo {{id:'c5'}}) \
             {filter} RETURN count(p) AS routes"
        )
    };
    let pruned = conn
        .query(&routes(
            "WHERE allReduce(acc = 0.0, r IN rels | acc + r.distance_km, acc < 600)",
        ))
        .unwrap();
    let postfilter = conn
        .query(&routes(
            "WITH p, rels WHERE allReduce(acc = 0.0, r IN rels | acc + r.distance_km, acc < 600)",
        ))
        .unwrap();
    assert_eq!(pruned.rows, postfilter.rows, "the same route count");

    let pruned_hops = tree_hops(plan(&pruned));
    let post_hops = tree_hops(plan(&postfilter));
    assert!(pruned_hops > 0, "the quantified walk counts its hops");
    assert!(
        pruned_hops < post_hops,
        "pruned {pruned_hops} hops vs post-filter {post_hops}"
    );
}

use relate_model::wire::plan_keys;

/// ADR-0468 (LABS-359): a scan-shaped `MATCH` whose interior ran the
/// batch scan/filter/project chain says so on its metric set —
/// `batched` on the scan's node, `false` on every operator the chain
/// declined, and the fact rides the wire with the metric set (present
/// only when true, like the start trio) while the walk's start keeps
/// naming itself beside it.
#[test]
fn profile_marks_the_batch_scan_filter_chain() {
    let mut conn = connection();
    conn.query(
        "CREATE (:Comment {length: 2000}), (:Comment {length: 10}),
         (:Comment {length: 4000})",
    )
    .unwrap();
    let result = conn
        .query("PROFILE MATCH (c:Comment) WHERE c.length > 1000 RETURN count(c.length) AS n")
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Int(2)]],
        "the batch chain answers the filtered count"
    );
    let tree = plan(&result);
    let scan = common::find_node(tree, "NodeByLabelScan").expect("the scan operator");
    let metrics = scan.metrics.expect("the scan's metrics");
    assert!(metrics.batched, "the scan ran the batch chain: {metrics:?}");
    // The walk-start fact keeps naming itself (LABS-369): the chain's
    // scan starts at the pattern's only element, forward, the default.
    let start = metrics.start.expect("the scan records its start");
    assert!(matches!(
        start.reason,
        relate_executor::StartReason::IndexZero
    ));
    assert!(!start.backward);
    // Every other operator declined: the aggregation's node says so.
    let root = tree.metrics.expect("the root's metrics");
    assert!(!root.batched, "the projection never batches: {root:?}");
    // The wire emits the fact with the metric set, only when true.
    let wire = scan.to_value();
    let Value::Map(node) = &wire else {
        panic!("the wire node is a map");
    };
    let Value::Map(args) = node.get(plan_keys::ARGS).expect("the args map") else {
        panic!("the args are a map");
    };
    assert_eq!(args.get(plan_keys::BATCHED), Some(&Value::Bool(true)));
    let root_wire = tree.to_value();
    let Value::Map(root_node) = &root_wire else {
        panic!("the root wire node is a map");
    };
    let Value::Map(root_args) = root_node.get(plan_keys::ARGS).expect("the root args") else {
        panic!("the root args are a map");
    };
    assert!(
        root_args.get(plan_keys::BATCHED).is_none(),
        "a declined operator emits no {} key",
        plan_keys::BATCHED
    );
}

/// LABS-645: the batch filter's zone skips are a PROFILE metric — a
/// conjunct the chunk's derived min/max zone provably decided for no
/// value skips that chunk's evaluation (the skip IS the correct empty
/// survivor set), the count rides the scan's metric set, the wire emits
/// `ZoneSkips` with it only when nonzero, and every operator that
/// skipped nothing reports `0` with no key.
#[test]
fn profile_counts_the_batch_filters_zone_skips() {
    let mut conn = connection();
    // One typed Int column over two chunk batches' worth of rows:
    // `[0, 511]`, `[512, 1023]`, `[1024, 1199]`. The bound `> 1195` is
    // above the first two chunks' maxima — two provable skips — and the
    // third chunk walks and keeps 1196..=1199.
    conn.query("UNWIND range(0, 1199) AS i CREATE (:Bulk {n: i})")
        .unwrap();
    let result = conn
        .query("PROFILE MATCH (b:Bulk) WHERE b.n > 1195 RETURN count(b.n) AS kept")
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Int(4)]],
        "the zone-skipped filter answers the full filter's count"
    );
    let tree = plan(&result);
    let scan = common::find_node(tree, "NodeByLabelScan").expect("the scan operator");
    let metrics = scan.metrics.expect("the scan's metrics");
    assert!(metrics.batched, "the scan ran the batch chain: {metrics:?}");
    assert_eq!(metrics.zone_skips, 2, "two chunks skipped: {metrics:?}");
    // Every operator that skipped nothing reports zero — the count's own
    // node included.
    let root = tree.metrics.expect("the root's metrics");
    assert_eq!(root.zone_skips, 0, "the projection never skips: {root:?}");
    // The wire emits the count with the metric set, only when nonzero.
    let wire = scan.to_value();
    let Value::Map(node) = &wire else {
        panic!("the wire node is a map");
    };
    let Value::Map(args) = node.get(plan_keys::ARGS).expect("the args map") else {
        panic!("the args are a map");
    };
    assert_eq!(args.get(plan_keys::ZONE_SKIPS), Some(&Value::Int(2)));
    let root_wire = tree.to_value();
    let Value::Map(root_node) = &root_wire else {
        panic!("the root wire node is a map");
    };
    let Value::Map(root_args) = root_node.get(plan_keys::ARGS).expect("the root args") else {
        panic!("the root args are a map");
    };
    assert!(
        root_args.get(plan_keys::ZONE_SKIPS).is_none(),
        "an operator that skipped nothing emits no {} key",
        plan_keys::ZONE_SKIPS
    );
}

/// The early predicate evaluation is PROFILE-visible end to end: a
/// selective mid-pattern predicate on the MATCH WHERE (evaluated the
/// moment the mid node binds, pre-hop at the anchor of the quantified
/// tail) expands strictly fewer hops than the same predicate as a WITH
/// WHERE post-filter (evaluated once per complete path) — and both
/// spellings answer the same rows.
#[test]
fn profile_shows_the_early_predicate_pruning_the_walk() {
    let mut conn = connection();
    // A hub `s` with a fixed hop to two mid nodes, then a quantified tail
    // to `t`: the failing mid node carries the denser tail.
    let mut create = String::from(
        "CREATE (s:Geo {id:'s'}), (m1:Geo {id:'m1', keep: false}), \
         (m2:Geo {id:'m2', keep: true}), (t:Geo {id:'t'}), \
         (u1:Geo {id:'u1'}), (u2:Geo {id:'u2'}), ",
    );
    create.push_str(
        "(s)-[:ROAD]->(m1), (s)-[:ROAD]->(m2), \
         (m1)-[:LINK]->(u1), (u1)-[:LINK]->(t), \
         (m1)-[:LINK]->(u2), (u2)-[:LINK]->(t), (m2)-[:LINK]->(t)",
    );
    conn.query(&create).unwrap();
    let body = |filter: &str| {
        format!(
            "PROFILE MATCH (s:Geo {{id:'s'}})-[:ROAD]->(m:Geo)-[:LINK*1..2]->(t:Geo) \
             {filter} RETURN s.id AS s, m.id AS m, t.id AS t ORDER BY s, m, t"
        )
    };
    let early = conn.query(&body("WHERE m.keep = true")).unwrap();
    let postfilter = conn
        .query(&body("WITH s, m, t WHERE m.keep = true"))
        .unwrap();
    assert_eq!(early.rows, postfilter.rows, "the same rows");
    let early_hops = tree_hops(plan(&early));
    let post_hops = tree_hops(plan(&postfilter));
    assert!(early_hops > 0, "the quantified walk counts its hops");
    assert!(
        early_hops < post_hops,
        "early {early_hops} hops vs post-filter {post_hops}"
    );
}

/// The LIMIT pull-in is PROFILE-visible end to end: a bounded LIMIT over
/// a quantified pattern halts the work once the cap is full. The capped
/// shape is frontier-served (ADR-0478: the BFS halts at the filling
/// level, its run counters ride the PROFILE record, and its rows are a
/// contract-free prefix — a valid subset of the full enumeration, not
/// the DFS's own first rows, Decision 4), while the uncapped shape the
/// comparison enumerates stays on the DFS — so the capped run expands
/// zero hops and the full one expands the whole enumeration.
#[test]
fn profile_shows_the_limit_pull_in_halting_the_walk() {
    let mut conn = connection();
    let mut create = String::from("CREATE ");
    for i in 0..6 {
        create.push_str(&format!("(n{i}:Geo {{id:'n{i}'}}), "));
    }
    for i in 0..6 {
        let j = (i + 1) % 6;
        create.push_str(&format!("(n{i})-[:LINK]->(n{j}), (n{j})-[:LINK]->(n{i}), "));
    }
    create.truncate(create.len() - 2);
    conn.query(&create).unwrap();
    let limited = conn
        .query(
            "PROFILE MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
                 RETURN x.id AS x, y.id AS y LIMIT 3",
        )
        .unwrap();
    let full = conn
        .query(
            "MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
                 RETURN x.id AS x, y.id AS y",
        )
        .unwrap();
    assert_eq!(limited.rows.len(), 3);
    // The capped prefix's rows are valid rows — each one a row the full
    // enumeration also produced (a multiset-containment check: the
    // frontier's emit order is its own, ADR-0478 Decision 4).
    let mut remaining = full.rows.clone();
    for row in &limited.rows {
        let Some(position) = remaining.iter().position(|other| other == row) else {
            panic!("the capped prefix holds a row the full enumeration never produced: {row:?}");
        };
        remaining.remove(position);
    }
    assert!(
        tree_frontier_levels(plan(&limited)) > 0,
        "the frontier served the capped shape, its levels on the record"
    );
    let limited_hops = tree_hops(plan(&limited));
    let (full_hops, full_frontier_levels) = {
        let full_profiled = conn
            .query(
                "PROFILE MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
                     RETURN x.id AS x, y.id AS y",
            )
            .unwrap();
        let tree = plan(&full_profiled);
        (tree_hops(tree), tree_frontier_levels(tree))
    };
    assert_eq!(
        full_frontier_levels, 0,
        "the uncapped shape is full enumeration — the DFS keeps it"
    );
    assert!(full_hops > 3, "the unbounded walk enumerates: {full_hops}");
    assert_eq!(
        limited_hops, 0,
        "the frontier's BFS replaces the DFS's hop expansions (its edge \
         visits ride the neighbor-walk kind)"
    );
}

/// ADR-0463: PROFILE reports the expression allocation bytes a clause
/// charged separately from the row-materialization `memory_bytes`. A
/// `range(1, 100)` in the projection charges a value buffer, so the root
/// carries a nonzero expression figure while its row bytes stay the
/// returned list's logical size; the wire form carries the same fact under
/// `ExpressionBuiltBytes`.
#[test]
fn profile_reports_expression_allocation_bytes_beside_memory_bytes() {
    let mut conn = connection();
    let result = conn.query("PROFILE RETURN range(1, 100) AS r").unwrap();
    let tree = plan(&result);
    let root = tree.metrics.expect("the projection's metrics");
    assert!(
        root.expression_built_bytes > 0,
        "the range buffer was charged: {root:?}"
    );
    assert!(
        root.memory_bytes > 0,
        "the returned row still carries its row-materialization bytes: {root:?}"
    );
    let Value::Map(map) = tree.to_value() else {
        panic!("to_value is a map");
    };
    let Value::Map(args) = map.get("args").expect("args") else {
        panic!("args is a map");
    };
    let Value::Int(expression_built_bytes) = args
        .get("ExpressionBuiltBytes")
        .expect("ExpressionBuiltBytes")
    else {
        panic!("ExpressionBuiltBytes is an integer");
    };
    assert_eq!(
        *expression_built_bytes as u64, root.expression_built_bytes,
        "{args:?}"
    );
}
