//! Batch commit over the overlay: `commit_batch` hands the batch's redo
//! ops to the sink and rebases the scratch, a failed batch restores the
//! pre-batch scratch, and a later part of the same statement reads the
//! properties its own batches committed (ADR-0210 × ADR-0541: a key the
//! plan resolved as never interned is interned by the batch commits
//! after it, so the plan's `Absent` token goes stale mid-statement).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::ExecGraph;
use crate::exec::test_support::{
    column, exec_autocommit, exec_write, exec_write_batched, int_cell, write_scratch, ApplySink,
};
use crate::graph::Graph;
use crate::redo::RedoOp;
use relate_core::Value;
use relate_names::{Token, TokenId};

/// ADR-0210: `ExecGraph::commit_batch` hands the batch's redo ops to the
/// sink, rebases the scratch onto the new commit timestamp, and preserves
/// the accumulated statement stats; a call with no new ops is a no-op
/// (the sink is not called again).
#[test]
fn commit_batch_applies_ops_and_rebases_scratch() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "CREATE (:Person {name: 'Ada'})").unwrap();
    assert_eq!(g.live_node_count(), 0, "uncommitted batch stays in scratch");

    let view = ExecGraph::write(&g, &scratch);
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    view.commit_batch(&sink).unwrap();
    assert_eq!(sink.calls.load(Ordering::Relaxed), 1);
    assert_eq!(g.live_node_count(), 1, "batch 1 is committed to the base");
    assert_eq!(
        view.stats().nodes_created,
        1,
        "statement stats survive the rebase"
    );
    assert_eq!(
        scratch.borrow().snapshot_ts(),
        g.commit_ts(),
        "scratch rebased onto the batch commit"
    );

    // The fresh scratch works for the next batch, and an empty batch does
    // not reach the sink. (`exec_write` runs a full `execute`, which resets
    // statement stats — the cross-batch stats carry-over is asserted above,
    // inside one execute.)
    exec_write(&g, &scratch, "CREATE (:Person {name: 'Grace'})").unwrap();
    assert_eq!(g.live_node_count(), 1);
    view.commit_batch(&sink).unwrap();
    assert_eq!(g.live_node_count(), 2);
    view.commit_batch(&sink).unwrap();
    assert_eq!(
        sink.calls.load(Ordering::Relaxed),
        2,
        "an empty batch is not committed"
    );
}

#[test]
fn failed_batch_restores_the_pre_batch_scratch() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "CREATE (n:Person {name: 'Ada'})").unwrap();
    let (ada, props) = {
        let s = scratch.borrow();
        assert_eq!(s.redo_ops().len(), 1);
        let RedoOp::CreateNode { id, properties, .. } = &s.redo_ops()[0] else {
            panic!("first op is the create");
        };
        let node = s
            .batch
            .nodes
            .get(id)
            .expect("create installs the scratch node");
        assert!(
            Arc::ptr_eq(&node.properties, properties),
            "the scratch node and its redo op share the property map"
        );
        assert_eq!(
            s.applied_ops(),
            1,
            "the statement's own applied count is its create"
        );
        (*id, Arc::clone(properties))
    };
    exec_write(
        &g,
        &scratch,
        "UNWIND [1] AS i \
         CALL { WITH i CREATE (:Gone {name: 'no'}) RETURN i / 0 AS bad } \
         IN TRANSACTIONS OF 1 ROWS ON ERROR CONTINUE \
         RETURN bad",
    )
    .unwrap();
    let s = scratch.borrow();
    assert_eq!(s.redo_ops().len(), 1, "the failed batch's create is gone");
    assert_eq!(
        s.applied_ops(),
        0,
        "the second statement's entry reset zeroed the applied count and the \
         rollback image restored that pre-batch zero, so the failed batch's \
         create is not counted"
    );
    assert!(s.batch.nodes.contains_key(&ada));
    assert!(
        s.batch.nodes.keys().all(|id| *id == ada),
        "only the pre-batch node remains"
    );
    let RedoOp::CreateNode { properties, .. } = &s.redo_ops()[0] else {
        panic!("the original create is still the only redo op");
    };
    assert!(Arc::ptr_eq(properties, &props));
}

/// ADR-0210 × ADR-0541 (LABS-1584 gap 4): the plan resolves the property
/// key `i` as `Absent` — the store has not interned it at admission — the
/// statement's own `IN TRANSACTIONS` batches then commit writes that
/// intern it, and the NEXT part's `z.i` reads must return the committed
/// values, agreeing with `properties(z)` on every row, instead of the
/// nulls the stale `Absent` token answered.
#[test]
fn next_part_reads_properties_committed_by_earlier_batches() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    let result = exec_write_batched(
        &g,
        &scratch,
        "UNWIND [1, 2, 3] AS i \
         CALL (i) { CREATE (z:Z {i: i}) } IN TRANSACTIONS OF 1 ROW \
         RETURN count(*) AS _ \
         NEXT \
         MATCH (z:Z) RETURN z.i AS zi, properties(z) AS props ORDER BY zi",
        &sink,
    )
    .unwrap();
    assert_eq!(column(&result, "zi", int_cell), vec![1, 2, 3]);
    for (zi, props) in result.rows.iter().map(|row| (&row[0], &row[1])) {
        let Value::Map(map) = props else {
            panic!("properties(z) is not a map: {props:?}");
        };
        assert_eq!(
            map.get("i"),
            Some(zi),
            "z.i and properties(z) disagree on the row: {zi:?} vs {props:?}"
        );
    }
    assert_eq!(
        sink.calls.load(Ordering::Relaxed),
        3,
        "one batch commits per row"
    );
}

/// The relationship arm of the same staleness: the NEXT part's `r.w`
/// reads a key the batches interned after the plan resolved it. The type
/// `LINK` is interned before the statement (a seed rel), so the walk's
/// type filter is plan-correct and only the property read is at stake.
#[test]
fn next_part_reads_relationship_properties_committed_by_earlier_batches() {
    let g = Graph::default();
    exec_autocommit(&g, "CREATE (:A)-[:LINK]->(:B)").unwrap();
    let scratch = write_scratch(&g);
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    let result = exec_write_batched(
        &g,
        &scratch,
        "UNWIND [1, 2, 3] AS i \
         CALL (i) { CREATE (:C {n: i})-[:LINK {w: i}]->(:C {n: i}) } \
         IN TRANSACTIONS OF 1 ROW \
         RETURN count(*) AS _ \
         NEXT \
         MATCH (:C)-[r:LINK]->(:C) RETURN r.w AS w ORDER BY w",
        &sink,
    )
    .unwrap();
    assert_eq!(column(&result, "w", int_cell), vec![1, 2, 3]);
}

/// The type arm of the same staleness: `WIRE` is interned by the
/// statement's own batches, so the plan resolved it as never-interned
/// (`Absent`) — the walk's type filter must re-resolve that stale token
/// by name against the live interner, or the NEXT part's `MATCH`
/// enumerates zero hops (the plan's `Absent` is a plan-time fact the
/// batches made stale, exactly like the property-key twin).
#[test]
fn next_part_matches_a_rel_type_interned_by_the_statement_own_batches() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    let result = exec_write_batched(
        &g,
        &scratch,
        "UNWIND [1, 2, 3] AS i \
         CALL (i) { CREATE (:C {n: i})-[:WIRE {w: i}]->(:C {n: i}) } \
         IN TRANSACTIONS OF 1 ROW \
         RETURN count(*) AS _ \
         NEXT \
         MATCH (:C)-[r:WIRE]->(:C) RETURN r.w AS w ORDER BY w",
        &sink,
    )
    .unwrap();
    assert_eq!(column(&result, "w", int_cell), vec![1, 2, 3]);
}

/// The `ON ERROR BREAK` variant of the gap report: the batches before the
/// failing one stay committed, and the NEXT part reads their properties —
/// the failed batch's scratch restore keeps the rebased snapshot, so the
/// stale-`Absent` re-check still applies.
#[test]
fn next_part_after_on_error_break_reads_the_committed_batches() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    let result = exec_write_batched(
        &g,
        &scratch,
        "UNWIND [1, 2, 3] AS i \
         CALL (i) { \
             WHEN i = 3 THEN { CREATE (:X {e: 1/0}) } \
             ELSE { CREATE (z:Z {i: i}) } \
         } IN TRANSACTIONS OF 1 ROW ON ERROR BREAK \
         RETURN count(*) AS _ \
         NEXT \
         MATCH (z:Z) RETURN z.i AS zi ORDER BY zi",
        &sink,
    )
    .unwrap();
    assert_eq!(
        column(&result, "zi", int_cell),
        vec![1, 2],
        "the two committed batches' nodes read their properties; the failed \
         third batch left nothing to read"
    );
}

/// The rebase gate behind the fix: a plan's `Absent` token answers `None`
/// while the scratch sits at the view's admission pin (nothing the view
/// can read was committed after the plan), and resolves by name once a
/// batch commit rebased the scratch past that pin.
#[test]
fn absent_key_token_resolves_only_once_the_scratch_rebased() {
    let g = Graph::default();
    // Intern `w` with a committed write, so the by-name lookup can find
    // an id once the gate opens (the plan-time `Absent` verdict under
    // test is constructed against it, pinning the gate's behavior).
    exec_autocommit(&g, "CREATE (:Z {w: 1})").unwrap();
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    assert_eq!(
        view.prop_key_id_resolved("w", Token::Absent),
        None,
        "unrebased: the plan's Absent verdict cannot have gone stale"
    );
    exec_write(&g, &scratch, "CREATE (:Z {w: 2})").unwrap();
    let sink = ApplySink {
        graph: &g,
        calls: AtomicUsize::new(0),
    };
    view.commit_batch(&sink).unwrap();
    let interned = g.prop_key_id("w").expect("the seed interned w");
    assert_eq!(
        view.prop_key_id_resolved("w", Token::Absent),
        Some(interned),
        "rebased: Absent re-resolves by name against the live interner"
    );
    assert_eq!(
        view.prop_key_id_resolved("w", Token::Id(TokenId(interned))),
        Some(interned),
        "an Id token answers the plan's id either way"
    );
    assert_eq!(
        view.prop_key_id_resolved("w", Token::Unresolved),
        Some(interned),
        "an Unresolved token looks the name up either way"
    );
}
