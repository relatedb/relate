//! The plan-text rendering tests (ADR-0359): the boundary's one rendering
//! of an `EXPLAIN`/`PROFILE` plan — `plan_text_lines` / `plan_header_lines`
//! over a real engine plan tree (built through the canonical producer
//! `PlanNode::to_value` + `value_to_cypher`, never hand-copied wire forms).
//! These pin the text-layout facts the engine tests used to own before the
//! engine's own renderer was deleted: the header echo (preamble stripped,
//! `CYPHER 25` preserved), the boxed operator table, the estimate column's
//! gate, the PROFILE ratio column with its `!` marker and unavailable
//! spelling, the never-executed marker, and the warnings column and footer.

use super::*;
use relate::PlanWarning;

/// A fresh in-memory connection — the one fixture entry these tests need.
fn conn() -> relate::Connection {
    relate::Database::open(":memory:")
        .unwrap()
        .connect()
        .unwrap()
}

/// The plan text lines of `statement` — the canonical producer pipeline the
/// CLI, Bolt and the HTTP Query API all run: the engine's own wire form,
/// then the formatter's one layout.
fn text_of(conn: &mut relate::Connection, statement: &str) -> Vec<String> {
    let result = conn.query(statement).unwrap();
    let tree = result.plan_root().expect("EXPLAIN/PROFILE stamps the plan");
    let echo = tree.statement_echo();
    plan_text_lines(&relate::value_to_cypher(&tree.to_value()), echo)
}

/// The header (ADR-0359): `echo` is the ENGINE-stamped statement echo —
/// the parse classified the preamble and the engine stripped it once at
/// the producer, so this boundary only collapses whitespace and heads the
/// line. The strip itself is pinned end-to-end in the engine's
/// `statement_echo` test (the tree carries the echo).
#[test]
fn header_heads_the_engine_echo_and_collapses_whitespace() {
    assert_eq!(
        plan_header_lines("MATCH (n) RETURN n"),
        ["Query: MATCH (n) RETURN n", "Plan:"]
    );
    assert_eq!(
        plan_header_lines("MATCH (n)   RETURN\n  n"),
        ["Query: MATCH (n) RETURN n", "Plan:"]
    );
    assert_eq!(
        plan_header_lines("CYPHER 25 RETURN 1 AS n"),
        ["Query: CYPHER 25 RETURN 1 AS n", "Plan:"]
    );
}

/// The boxed operator table names every operator with its subject — the
/// scan carries its label, the root is the projection (ADR-0344 phase 3).
#[test]
fn operator_table_names_each_operator_with_its_subject() {
    let mut conn = conn();
    let lines = text_of(&mut conn, "EXPLAIN MATCH (n:Person) RETURN n.name");
    assert_eq!(lines[0], "Query: MATCH (n:Person) RETURN n.name");
    assert_eq!(lines[1], "Plan:");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("NodeByLabelScan") && l.contains("(n:Person)")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("ProduceResults")),
        "{lines:?}"
    );
    // The next-chain stage link names its stage.
    let chained = text_of(
        &mut conn,
        "EXPLAIN RETURN 1 AS x NEXT MATCH (n) RETURN x, n",
    );
    assert!(
        chained
            .iter()
            .any(|l| l.contains("Next") && l.contains("stage 2")),
        "{chained:?}"
    );
}

/// ADR-0486: an index-backed start renders as the seek operator in the
/// boxed table — `NodeIndexSeekByRange` for the comparison window,
/// `NodeIndexSeek` for the anchored-equality point read — the seek line
/// in the `Details` cell, where the fused narration showed
/// `NodeByLabelScan` and the pattern text.
#[test]
fn index_backed_starts_render_the_seek_operators() {
    let mut conn = conn();
    conn.query("CREATE RANGE INDEX user_age FOR (u:User) ON (u.age)")
        .unwrap();
    conn.query("UNWIND range(1, 10) AS i CREATE (:User {age: i})")
        .unwrap();
    let window = text_of(&mut conn, "EXPLAIN MATCH (n:User) WHERE n.age > 5 RETURN n");
    assert!(
        window.iter().any(|l| l.contains("NodeIndexSeekByRange")
            && l.contains("RANGE INDEX n:User(age) WHERE age > 5")),
        "the window start's row: {window:?}"
    );
    assert!(
        !window.iter().any(|l| l.contains("NodeByLabelScan")),
        "the seek replaces the fused scan row: {window:?}"
    );
    let point = text_of(
        &mut conn,
        "EXPLAIN MATCH (n:User {age: 5}) RETURN n.age AS age",
    );
    assert!(
        point
            .iter()
            .any(|l| l.contains("NodeIndexSeek")
                && l.contains("RANGE INDEX n:User(age) WHERE age = 5")),
        "the point read's row: {point:?}"
    );
}

/// The estimate column's gate (ADR-0218): the column appears when any node
/// carries an estimate and stays off when none does — never a fabricated
/// number.
#[test]
fn estimate_column_appears_only_when_a_node_carries_an_estimate() {
    let mut conn = conn();
    conn.query("CREATE (:Person {name: 'Alice'}), (:Person {name: 'Bob'})")
        .unwrap();
    let estimated = text_of(&mut conn, "EXPLAIN MATCH (n:Person) RETURN n.name");
    assert!(
        estimated.iter().any(|l| l.contains("Estimated Rows")),
        "the scan's estimate fills the column: {estimated:?}"
    );
    let unestimated = text_of(
        &mut conn,
        "EXPLAIN MATCH (:Person)-[:KNOWS*1..2]->(:Person) RETURN 1",
    );
    assert!(
        !unestimated.iter().any(|l| l.contains("Estimated Rows")),
        "labeled endpoints narrow past the type count: {unestimated:?}"
    );
}

/// The typed warnings (typed node warnings) render beside the affected
/// operator in the `Warnings` column and in the footer line, which names
/// the affected operator's id.
#[test]
fn typed_warnings_render_beside_the_operator_and_in_the_footer() {
    let mut conn = conn();
    let disjoint = text_of(
        &mut conn,
        "EXPLAIN MATCH (a:Person), (b:City) RETURN a.name, b.name",
    );
    assert!(
        disjoint
            .iter()
            .any(|l| l.contains("Expand") && l.contains("the MATCH patterns share no join anchor")),
        "the CartesianProduct warning renders beside the operator: {disjoint:?}"
    );
    assert!(
        disjoint.iter().any(|l| l.starts_with("Warnings: Id ")
            && l.contains("the MATCH patterns share no join anchor")),
        "the footer names the affected operator's id: {disjoint:?}"
    );

    conn.query("CREATE INDEX person_name FOR (n:Person) ON (n.name)")
        .unwrap();
    let labelless = text_of(&mut conn, "EXPLAIN MATCH (n) RETURN n");
    assert!(
        labelless.iter().any(|l| l.contains("AllNodesScan")
            && l.contains(
                "the scan matches every node; consider the property index (:Person(name))"
            )),
        "the AllNodesScan warning names the existing property index: {labelless:?}"
    );
    assert!(
        labelless
            .iter()
            .any(|l| l.starts_with("Warnings: Id ") && l.contains(":Person(name)")),
        "the footer names the suggested index: {labelless:?}"
    );
}

/// The PROFILE metric columns (ADR-0344 phase 2 + metric fields): the
/// ratio column appears on a profiled tree with estimates, a 10x
/// overshoot carries the `!` marker, and an estimate-less operator renders
/// the shared unavailable spelling — never a fabricated ratio.
#[test]
fn profile_ratio_column_flags_a_ten_x_overshoot() {
    let mut conn = conn();
    conn.query("UNWIND range(1, 25) AS i CREATE (:Person {flag: i = 1})")
        .unwrap();
    let skewed = text_of(&mut conn, "PROFILE MATCH (n:Person) WHERE n.flag RETURN n");
    assert!(
        skewed.iter().any(|l| l.contains("Est/Actual")),
        "the ratio column appears on a profiled tree with estimates: {skewed:?}"
    );
    assert!(
        skewed
            .iter()
            .any(|l| l.contains("NodeByLabelScan") && l.contains("25.0x !")),
        "the 25-row estimate against one actual row: {skewed:?}"
    );
    assert!(
        skewed
            .iter()
            .any(|l| l.contains("estimated rows 10x or more above the actual rows")),
        "the typed EstimationMiss warning renders beside the operator: {skewed:?}"
    );
    // The projection: no estimate of its own — the shared unavailable
    // spelling, never a fabricated ratio.
    assert!(
        skewed
            .iter()
            .any(|l| l.contains("ProduceResults") && l.contains(" | - ")),
        "{skewed:?}"
    );
    // A matching estimate: every person, no filter — the bare ratio
    // (1.0x), no warning marker.
    let matched = text_of(&mut conn, "PROFILE MATCH (n:Person) RETURN n");
    assert!(
        matched
            .iter()
            .any(|l| l.contains("NodeByLabelScan") && l.contains("1.0x")),
        "{matched:?}"
    );
    assert!(
        !matched.iter().any(|l| l.contains("1.0x !")),
        "a matching estimate carries no warning: {matched:?}"
    );
}

/// The cumulative `Total (ms)` column (cumulative vs self): it renders on
/// PROFILE trees beside the operator's own `Time (ms)`, and a leaf
/// operator's total equals its own time (no descendants) while the root's
/// covers the whole pipeline — never a fabricated or missing cell on a
/// metrics-carrying node.
#[test]
fn profile_total_column_sums_the_subtree() {
    let mut conn = conn();
    conn.query("UNWIND range(1, 4) AS i CREATE (:Person {name: 'p' + i})")
        .unwrap();
    let text = text_of(&mut conn, "PROFILE MATCH (n:Person) RETURN n.name");
    // The header row names the columns; read the Time and Total cells of
    // an operator row by the header's own indexes, never by position guess.
    let header = text
        .iter()
        .find(|l| l.contains("Total (ms)"))
        .expect("the cumulative column appears on a profiled tree");
    let columns = boxed_cells(header);
    let time_ix = columns
        .iter()
        .position(|c| *c == "Time (ms)")
        .expect("the self-time column renders");
    let total_ix = columns
        .iter()
        .position(|c| *c == "Total (ms)")
        .expect("the cumulative column renders");
    let row_ms = |line: &str| -> Vec<Option<f64>> {
        boxed_cells(line)
            .into_iter()
            .map(|cell| {
                cell.split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<f64>().ok())
            })
            .collect()
    };
    // A leaf (the scan) has no descendants: its total equals its own time.
    let leaf = text
        .iter()
        .find(|l| l.contains("NodeByLabelScan"))
        .expect("the scan row renders");
    let leaf_cells = row_ms(leaf);
    let own = leaf_cells[time_ix].expect("the scan's own time renders");
    let total = leaf_cells[total_ix].expect("the scan's total renders");
    assert_eq!(own, total, "a leaf's total equals its own time: {leaf:?}");
    // The root (ProduceResults) heads the whole pipeline: its total covers
    // its own time plus every descendant's.
    let root = text
        .iter()
        .find(|l| l.contains("ProduceResults"))
        .expect("the projection row renders");
    let root_cells = row_ms(root);
    let own = root_cells[time_ix].expect("the root's own time renders");
    let total = root_cells[total_ix].expect("the root's total renders");
    assert!(
        total >= own,
        "the root's total covers its own time: own={own} total={total} ({root:?})"
    );
}

/// The never-executed marker: an operator whose input clause produced no
/// rows did no work — its metric cells render the shared marker, never
/// zeros, while the tree's root (the projection that produced the answer)
/// renders its actual zeros.
#[test]
fn profile_renders_the_never_executed_marker_on_a_starved_operator() {
    let mut conn = conn();
    conn.query("CREATE (:Person {name: 'Alice'})").unwrap();
    let starved = text_of(
        &mut conn,
        "PROFILE MATCH (n:Person) WHERE n.name = 'Nobody' MATCH (m:Person) RETURN m.name",
    );
    assert!(
        starved
            .iter()
            .any(|l| l.contains("NodeByLabelScan") && l.contains("(never executed)")),
        "the starved operator's cells carry the marker: {starved:?}"
    );
    let root_row = starved
        .iter()
        .find(|l| l.contains("ProduceResults"))
        .expect("the root row");
    assert!(
        !root_row.contains("(never executed)"),
        "the root always executes: {root_row}"
    );
}

/// The wire form's typed-warning serialization is pinned on the engine
/// side (`PlanWarning`); the boundary renders exactly those texts through
/// the typed warning's own message — the footer line composition.
#[test]
fn warnings_footer_line_names_each_affected_id() {
    let mut conn = conn();
    conn.query("UNWIND range(1, 25) AS i CREATE (:Person {flag: i = 1})")
        .unwrap();
    let result = conn
        .query("PROFILE MATCH (n:Person) WHERE n.flag RETURN n")
        .unwrap();
    let tree = result.plan_root().expect("PROFILE stamps the plan");
    assert_eq!(
        tree.children[0].warnings,
        [PlanWarning::EstimationMiss],
        "the engine tree carries the typed warning the text renders"
    );
    let lines = plan_text_lines(
        &relate::value_to_cypher(&tree.to_value()),
        "PROFILE MATCH (n:Person) WHERE n.flag RETURN n",
    );
    assert!(
        lines.iter().any(|l| l.starts_with("Warnings: Id ")
            && l.contains("estimated rows 10x or more above the actual rows")),
        "the footer names the affected operator's id: {lines:?}"
    );
}

#[test]
fn profile_renders_the_read_kinds_and_the_walks_start() {
    // LABS-369: the rendered plan carries the read-kind breakdown beside
    // the Db Hits total, and the walk's chosen start on the operator's
    // details — the facts an opaque `Expand` used to hide. Pinned over a
    // real engine plan (the canonical producer), like every claim here.
    let mut conn = conn();
    conn.query(
        "CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})\
         -[:KNOWS]->(c:Person {name: 'Carol'})",
    )
    .unwrap();
    let lines = text_of(
        &mut conn,
        "PROFILE MATCH (b:Person) WITH b \
         MATCH (a)-[k1:KNOWS]->(b)-[k2:KNOWS]->(c) RETURN a.name AS n",
    );
    let text = lines.join("\n");
    // ADR-0397: the kinds render as their own columns — the walk followed
    // rels and enumerated neighbors, so both columns appear beside the
    // node reads.
    assert!(
        text.contains("Nodes") && text.contains("Rels") && text.contains("Degree"),
        "the read-kind columns render: {text}"
    );
    // A zero renders empty (absence), a nonzero count stands alone in its
    // column — the vertical scan for outliers ADR-0397 is about. The
    // expand's row carries counts in all three; the projection's row
    // carries empties.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Expand") && l.contains(" | ")),
        "the kind columns render: {text}"
    );
    // The bound pivot names itself: element #1, backward, bound-pivot.
    assert!(
        text.contains("[start #1, backward, bound-pivot]"),
        "the walk's start renders on the details: {text}"
    );
}

#[test]
fn the_executed_column_appears_only_when_an_operator_never_ran() {
    // LABS-343: the run state as its own column — one mark per operator,
    // gated to appear only when some operator never ran (an all-executed
    // plan gains no column), instead of reading the marker out of every
    // metric cell.
    let mut conn = conn();
    // On the empty store the scan yields no row, so the expansion over it
    // never executes: the column renders, marking the runner and the
    // never-run.
    let starved = text_of(
        &mut conn,
        "PROFILE MATCH (p:Person) WITH p, count(*) AS x \
         MATCH (q:Person)-[k:KNOWS]->(f:Person) RETURN q.name AS n",
    )
    .join("\n");
    assert!(
        starved.contains(crate::plan_table::PlanColumn::Executed.name()),
        "the column renders: {starved}"
    );
    // Anchor each mark to its own operator row — a whole-blob `contains`
    // would pass on the `x` inside the `Query:` echo's `count(*) AS x`.
    assert!(
        starved.lines().any(|line| line.contains("Expand")
            && line.contains(&format!("| {} ", crate::plan_table::NOT_EXECUTED_MARK))),
        "the starved operator carries the never-ran mark: {starved}"
    );
    assert!(
        starved.lines().any(|line| line.contains("ProduceResults")
            && line.contains(&format!("| {} ", crate::plan_table::EXECUTED_MARK))),
        "the root carries the ran mark: {starved}"
    );
    // With data every operator runs: no column.
    conn.query("CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})")
        .unwrap();
    let ran = text_of(&mut conn, "PROFILE MATCH (p:Person) RETURN p.name AS n").join("\n");
    assert!(
        !ran.contains(crate::plan_table::PlanColumn::Executed.name()),
        "an all-executed plan gains no column: {ran}"
    );
}

/// ADR-0468 (LABS-359): a scan-shaped `MATCH` whose interior ran the
/// batch scan/filter/project chain marks it on the details cell, beside
/// the walk's start — the PROFILE-visible fact the chain engaged, riding
/// the scan's own row.
#[test]
fn the_batch_chain_marks_the_scan_it_ran() {
    let mut conn = conn();
    conn.query("CREATE (:Comment {length: 2000}), (:Comment {length: 10})")
        .unwrap();
    let text = text_of(
        &mut conn,
        "PROFILE MATCH (c:Comment) WHERE c.length > 1000 RETURN count(c.id) AS n",
    )
    .join("\n");
    assert!(
        text.lines()
            .any(|l| l.contains("NodeByLabelScan") && l.contains("[batched]")),
        "the mark rides the scan's own row: {text}"
    );
    // The projection the chain never touches carries no mark.
    assert!(
        !text
            .lines()
            .any(|l| l.contains("ProduceResults") && l.contains("[batched]")),
        "a declined operator carries no mark: {text}"
    );
}

/// LABS-645: the batch filter's zone-skip count rides the details cell
/// beside the chain's own mark — the measured fact that a chunk's
/// derived min/max provably replaced a conjunct's row walk with the
/// correct empty survivor set.
#[test]
fn the_zone_skip_count_marks_the_scan_that_skipped() {
    let mut conn = conn();
    conn.query("CREATE (:Comment {length: 10}), (:Comment {length: 20})")
        .unwrap();
    let text = text_of(
        &mut conn,
        "PROFILE MATCH (c:Comment) WHERE c.length > 1000 RETURN count(c.id) AS n",
    )
    .join("\n");
    assert!(
        text.lines()
            .any(|l| l.contains("NodeByLabelScan") && l.contains("[zone-skips 1]")),
        "the skip count rides the scan's own row: {text}"
    );
    // The projection the chain never touches carries no count.
    assert!(
        !text
            .lines()
            .any(|l| l.contains("ProduceResults") && l.contains("[zone-skips")),
        "an operator that skipped nothing carries no count: {text}"
    );
}

/// ADR-0478: a clause the frontier-NFA QPP kernel served marks itself on
/// the details cell with its BFS run counters — the PROFILE-visible fact
/// the family engaged, riding the walk's own row; ADR-0516 adds the NFA
/// rung's own facts (the re-arrival budget fraction, the memo's hits and
/// misses, the admission asks by verdict) as their own mark on the same
/// row; a clause it declined (the uncapped full enumeration) carries
/// neither mark.
#[test]
fn the_frontier_kernel_marks_the_clause_it_ran() {
    let mut conn = conn();
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
    let text = text_of(
        &mut conn,
        "PROFILE MATCH (x:Geo {id:'n0'})-[:LINK*]-(y:Geo) \
             RETURN count(DISTINCT y.id) AS c",
    )
    .join("\n");
    assert!(
        text.lines().any(|l| l.contains("[frontier edges=")
            && l.contains("levels=")
            && l.contains("endpoints=")
            && l.contains("cells=")),
        "the run counters ride the served clause's own row: {text}"
    );
    assert!(
        text.lines().any(|l| l.contains("[frontier-rule budget=")
            && l.contains("memo-hits=")
            && l.contains("memo-misses=")
            && l.contains("representative-proofs=")
            && l.contains("search-proven=")
            && l.contains("search-disproven=")
            && l.contains("declined-exhausted=")),
        "the NFA rung's own facts ride the same row: {text}"
    );
    assert!(
        !text.contains("[frontier-declined"),
        "a served clause carries no decline mark: {text}"
    );
    assert!(
        text.lines().any(|l| l.contains("[kernel frontier-nfa]")),
        "the frontier rung names itself on the served row (LABS-1117): {text}"
    );
    // The full enumeration the family declined carries no run counters,
    // and names the gate that declined it (LABS-1437).
    let plain = text_of(
        &mut conn,
        "PROFILE MATCH (x:Geo {id:'n0'})-[:LINK*1..3]-(y:Geo) \
             RETURN x.id AS x, y.id AS y",
    )
    .join("\n");
    assert!(
        !plain.contains("[frontier edges="),
        "a declined clause carries no run counters: {plain}"
    );
    assert!(
        plain
            .lines()
            .any(|l| l.contains("[frontier-declined full-enumeration]")),
        "the plan-time decline names its gate on the walker's row: {plain}"
    );
}

/// LABS-1117: the Details cell names the traversal rung that served the
/// `MATCH` — the chain with its hop count, the frontier-NFA rung, or the
/// per-row walker — so the text table answers "which path ran this
/// clause". A non-`MATCH` operator carries no mark.
#[test]
fn the_kernel_rung_marks_the_clause_it_served() {
    let mut conn = conn();
    conn.query(relate_test_support::plan_rung::THREE_HOP_CORRIDOR)
        .unwrap();
    let chain = text_of(
        &mut conn,
        &format!(
            "PROFILE {}",
            relate_test_support::plan_rung::THREE_HOP_MATCH
        ),
    )
    .join("\n");
    assert!(
        chain.lines().any(|l| l.contains("[kernel chain hops=3]")),
        "the chain rung and its hop count ride the served row: {chain}"
    );

    // The walker is a named rung too: an untyped rel leaves the kernels'
    // closed vocabulary, so the ladder records the per-row walker.
    let walked = text_of(
        &mut conn,
        "PROFILE MATCH (a:Alpha)-->(b:Beta) RETURN a.k AS ak",
    )
    .join("\n");
    assert!(
        walked.lines().any(|l| l.contains("[kernel walker]")),
        "the walker rides its own row: {walked}"
    );
    assert!(
        !walked
            .lines()
            .any(|l| l.contains("ProduceResults") && l.contains("[kernel")),
        "a non-MATCH operator carries no rung: {walked}"
    );
}
