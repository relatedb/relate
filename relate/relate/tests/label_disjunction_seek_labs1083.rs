//! LABS-1083: the OR-label disjunction's keyed union. The ticket repro —
//! `MATCH (m:Model {id: n.id}) MATCH (p) WHERE (p:Model OR p:Source) AND
//! p.id = d` — planned a per-row scan because the executor's keyed seek
//! required a static label conjunction; the planner fold (the same
//! ticket's other half) moves the disjunction into the pattern node's
//! label expression, and this file pins the executor half: the union
//! serves it from the per-label indexes (deduplicated by node, a
//! both-labels node yielding one row), the plan names the keyed lookups
//! as seeks instead of scans, and the answers match the scan path's.

mod common;

use common::{connection, find_node, find_nodes, has_notification, plan};

/// The repro's row seeding — the fixture both stores share: one
/// `:Model`/`:Source`/`:Column` row per id, and the dedup fixture's one
/// node carrying BOTH disjunct labels on the shared id-1. The keyed
/// store adds its two range indexes on top; the scan-path comparison
/// store is the seeding alone, so the two can never drift apart.
fn seed_rows(conn: &mut relate::Connection) {
    conn.query(
        "UNWIND range(1, 3) AS i \
         CREATE (:Model {id: i}), (:Source {id: i}), (:Column {id: i})",
    )
    .unwrap();
    conn.query("CREATE (:Model:Source {id: 1})").unwrap();
}

/// The repro's store: the `:Model(id)`/`:Source(id)` range indexes the
/// keyed union consults over the shared row seeding.
fn store() -> relate::Connection {
    let mut conn = connection();
    conn.query("CREATE INDEX model_id FOR (n:Model) ON (n.id)")
        .unwrap();
    conn.query("CREATE INDEX source_id FOR (n:Source) ON (n.id)")
        .unwrap();
    seed_rows(&mut conn);
    conn
}

/// The ticket repro's statement: the second MATCH keys `:Model(id)` per
/// row, the third serves the label disjunction as the keyed union.
const REPRO: &str = "MATCH (n:Column) MATCH (m:Model {id: n.id}) \
                     MATCH (p) WHERE (p:Model OR p:Source) AND p.id = n.id \
                     RETURN p.id AS id";

/// EXPLAIN names the keyed lookups as seeks, never the per-row scan the
/// fused narration overstated: the `:Column` label scan stays a
/// `NodeByLabelScan` (it is one), while the per-row `:Model(id)` lookup
/// and the disjunction union both narrate `NodeByLabelSeek` — the
/// repro's 291.7M-dbHits per-row `AllNodesScan`/`NodeByLabelScan` shape
/// is gone from the plan.
#[test]
fn explain_names_the_keyed_union_not_a_scan() {
    let mut conn = store();
    let explained = conn.query(&format!("EXPLAIN {REPRO}")).unwrap();
    let tree = plan(&explained);
    assert!(
        find_node(tree, "AllNodesScan").is_none(),
        "no all-nodes scan anywhere in the plan: {tree:?}"
    );
    // The one true scan: the plain :Column label scan.
    assert_eq!(
        find_nodes(tree, "NodeByLabelScan").len(),
        1,
        "only the :Column scan stays a scan: {tree:?}"
    );
    let seeks = find_nodes(tree, "NodeByLabelSeek");
    assert_eq!(
        seeks.len(),
        2,
        "the per-row :Model(id) lookup and the disjunction union: {tree:?}"
    );
    assert!(
        seeks
            .iter()
            .any(|seek| seek.details.contains("m:Model(id)")),
        "the per-row keyed lookup names its index: {tree:?}"
    );
    assert!(
        seeks
            .iter()
            .any(|seek| seek.details.contains("p:Model(id), p:Source(id)")),
        "the union names each arm's index: {tree:?}"
    );
}

/// The union's answer is the scan path's answer, node for node: the same
/// disjunction statement over a store without the indexes (the funnel's
/// scan tail plus the per-candidate check) returns the same rows, and
/// the both-labels node appears ONCE per row — the union deduplicates by
/// node, so a node carrying both disjunct labels is not yielded once per
/// arm.
#[test]
fn the_union_answers_like_the_scan_path_and_dedups_both_label_nodes() {
    // The disjunction without the repro's `m` lookup (whose own :Model
    // match doubles the id-1 rows through no fault of the union): per
    // :Column row, the Model, the Source, and — on id 1 only — the
    // both-labels node exactly once: 3 + 2 + 2 = 7 rows.
    const DISJUNCTION: &str = "MATCH (n:Column) MATCH (p) \
                               WHERE (p:Model OR p:Source) AND p.id = n.id \
                               RETURN p.id AS id ORDER BY id";
    let expected: Vec<i64> = [1i64, 1, 1, 2, 2, 3, 3].to_vec();

    let mut keyed = store();
    assert_eq!(
        ids(&keyed.query(DISJUNCTION).unwrap()),
        expected,
        "the keyed union's rows"
    );

    // The same store shape without the indexes: every candidate comes
    // from the scan tail, filtered per candidate — the shared seeding,
    // never a re-spelled copy of the keyed store's rows.
    let mut scanning = connection();
    seed_rows(&mut scanning);
    assert_eq!(
        ids(&scanning.query(DISJUNCTION).unwrap()),
        expected,
        "the scan path's rows"
    );

    // The dedup, counted: the id-1 row matches exactly three nodes —
    // the Model, the Source, and the both-labels node once — never four
    // (once per arm) or six.
    let counted = keyed
        .query(
            "MATCH (n:Column {id: 1}) MATCH (p) \
             WHERE (p:Model OR p:Source) AND p.id = n.id \
             RETURN count(p) AS c",
        )
        .unwrap();
    assert_eq!(counted.rows[0][0].as_int().unwrap(), 3);
}

/// The single `p.id` column of a result's rows, in order.
fn ids(result: &relate::QueryResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| row[0].as_int().unwrap())
        .collect()
}

/// LABS-1083 review (the folded-map repro shape): the equality the
/// statement spells — `(p:Model OR p:Source) AND p.id = 1` — folds into
/// the pattern's own property map, so no residual `Seek` ever reaches the
/// union arm; the funnel serves the map's equality per arm (each arm's
/// `:Label(id)` index consulted with the folded value), so the narrated
/// `NodeByLabelSeek` is what runs: PROFILE shows the operator reading the
/// union's three candidates — id 1's Model, Source, and both-labels node
/// once each — never the store's ten nodes the pre-fix all-nodes scan
/// read while the plan claimed the seek.
#[test]
fn the_folded_map_equality_serves_per_arm_as_narrated() {
    let mut conn = store();
    // EXPLAIN: the narration names each arm's index and scans nothing.
    let explained = conn
        .query(
            "EXPLAIN MATCH (p) WHERE (p:Model OR p:Source) AND p.id = 1 \
             RETURN p.id AS id",
        )
        .unwrap();
    let tree = plan(&explained);
    assert!(
        find_node(tree, "AllNodesScan").is_none(),
        "no all-nodes scan anywhere in the plan: {tree:?}"
    );
    let seeks = find_nodes(tree, "NodeByLabelSeek");
    assert_eq!(seeks.len(), 1, "the one MATCH narrates one seek: {tree:?}");
    assert!(
        seeks[0].details.contains("p:Model(id), p:Source(id)"),
        "the union names each arm's index: {tree:?}"
    );
    // The answer, node for node: the Model, the Source, and the
    // both-labels node — once each, the union deduplicated.
    let answered = conn
        .query(
            "MATCH (p) WHERE (p:Model OR p:Source) AND p.id = 1 \
             RETURN p.id AS id ORDER BY id",
        )
        .unwrap();
    assert_eq!(ids(&answered), vec![1, 1, 1]);
    // PROFILE: what ran matches what was narrated — the per-arm union's
    // own candidates, not the store's whole population.
    let profiled = conn
        .query(
            "PROFILE MATCH (p) WHERE (p:Model OR p:Source) AND p.id = 1 \
             RETURN p.id AS id",
        )
        .unwrap();
    let tree = plan(&profiled);
    let seek = find_node(tree, "NodeByLabelSeek").expect("the narrated seek");
    let metrics = seek.metrics.expect("PROFILE stamps the seek's metrics");
    // Index postings read, per arm (ADR-0567): the both-labels node is
    // posted under both arms, so four postings are read for the union's
    // three distinct candidates (it was 3 when only returned candidates
    // counted) — still never the store's ten nodes.
    assert_eq!(
        metrics.db_hits, 4,
        "the arms' four postings, never the store's ten nodes: {metrics:?}"
    );
}

/// LABS-1083 review (the unserved-union fallback): `MATCH (p:C) WHERE
/// p:A OR p:B` — no index serves anything — folds to
/// `And([C, Or([A, B])])`, and the scan tail scans the conjunction base
/// `:C` (the pre-fold label scan), not every node: PROFILE shows the
/// operator reading the `:C` population only, the `Or` arms filtering per
/// candidate, and the narration stays the fused `NodeByLabelScan` — no
/// seek claimed, none run.
#[test]
fn the_unserved_and_disjunction_scans_the_conjunction_base() {
    let mut conn = connection();
    conn.query(
        "CREATE (:C:A {id: 1}), (:C:B {id: 2}), (:C {id: 3}), \
         (:A {id: 4}), (:B {id: 5})",
    )
    .unwrap();
    let profiled = conn
        .query("PROFILE MATCH (p:C) WHERE p:A OR p:B RETURN p.id AS id ORDER BY id")
        .unwrap();
    assert_eq!(ids(&profiled), vec![1, 2], "the C-labeled A-or-B rows");
    let tree = plan(&profiled);
    assert!(
        find_node(tree, "NodeByLabelSeek").is_none(),
        "no equality to key on, no seek narrated: {tree:?}"
    );
    let scan = find_node(tree, "NodeByLabelScan").expect("the fused label scan narration");
    let metrics = scan.metrics.expect("PROFILE stamps the scan's metrics");
    assert_eq!(
        metrics.db_hits, 3,
        "the :C population, never the store's five nodes: {metrics:?}"
    );
}

/// LABS-1083 (review): the bare `Or` with no equality to key on scans
/// every node — the plan names `AllNodesScan` and warns it, exactly the
/// shape's pre-fold narration (a `NodeByLabelScan` claim would lie: no
/// label constrains the scan, and no per-arm lookup runs without an
/// equality to key it on).
#[test]
fn the_bare_disjunction_without_equalities_narrates_the_all_nodes_scan() {
    let mut conn = connection();
    conn.query("CREATE INDEX column_id FOR (n:Column) ON (n.id)")
        .unwrap();
    conn.query("CREATE (:Model {id: 1}), (:Source {id: 1})")
        .unwrap();
    let explained = conn
        .query("EXPLAIN MATCH (p) WHERE p:Model OR p:Source RETURN p")
        .unwrap();
    let tree = plan(&explained);
    assert!(
        find_node(tree, "AllNodesScan").is_some(),
        "the every-node scan is named what it is: {tree:?}"
    );
    assert!(
        find_node(tree, "NodeByLabelScan").is_none(),
        "no label constrains this scan: {tree:?}"
    );
    // The typed kind, through the canonical helper — a description
    // substring would pass for any kind carrying the sentence.
    assert!(
        has_notification(
            &explained,
            relate_model::wire::NotificationKind::AllNodesScan
        ),
        "the AllNodesScan warning fires again: {:?}",
        explained.notifications()
    );
}
