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

/// The rollback's classification, one row per op kind: a create is the entity
/// itself; a property or label op is undone by the same drop when it names one
/// of the statement's created ids — the only target a CREATE-only statement's
/// property defaults can reach — and is refused when it names anything else.
#[test]
fn a_property_or_label_op_is_invertible_only_on_a_created_entity() {
    let mut created = std::collections::HashSet::new();
    created.insert(crate::graph::EntityId::Node(7));
    created.insert(crate::graph::EntityId::Rel(9));
    let invertible = |op: &RedoOp| super::scratch::invertible_by_dropping_creates(op, &created);

    assert!(invertible(&RedoOp::CreateNode {
        id: 7,
        labels: Arc::new(vec!["N".into()]),
        properties: Arc::new(relate_core::PropertyMap::default()),
    }));
    assert!(invertible(&RedoOp::SetNodeProp {
        id: 7,
        key: "x".into(),
        value: Value::Int(1),
    }));
    assert!(invertible(&RedoOp::RemoveNodeProp {
        id: 7,
        key: "x".into(),
    }));
    assert!(invertible(&RedoOp::AddLabels {
        id: 7,
        labels: vec!["N".into()],
    }));
    assert!(invertible(&RedoOp::SetRelProp {
        id: 9,
        key: "x".into(),
        value: Value::Int(1),
    }));
    // A foreign entity, or a delete, is not this mark's to undo.
    assert!(!invertible(&RedoOp::SetNodeProp {
        id: 8,
        key: "x".into(),
        value: Value::Int(1),
    }));
    assert!(!invertible(&RedoOp::DeleteNode { id: 7 }));
    assert!(!invertible(&RedoOp::DeleteRel { id: 9 }));
}

/// ADR-0642: a redo the mark cannot invert is refused rather than
/// half-applied — the statement's entries stay put and the caller fails the
/// batch, so a rollback that could not finish never leaves partial writes to
/// be committed with the batch.
#[test]
fn a_rollback_that_cannot_invert_its_redo_is_refused_not_half_applied() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "CREATE (:Person {name: 'Ada'})").unwrap();
    let earlier = *scratch
        .borrow()
        .batch
        .nodes
        .keys()
        .next()
        .expect("the first statement's node is in the batch");

    let mark = scratch.borrow().mark_statement();
    exec_write(&g, &scratch, "CREATE (:Person {name: 'Grace'})").unwrap();
    assert_eq!(scratch.borrow().batch.nodes.len(), 2);

    // A property op on the *earlier* statement's node: reachable only through
    // a redo the batch's eligibility rule forbids, and not undoable by
    // dropping this statement's creates.
    scratch.borrow_mut().push_redo(RedoOp::SetNodeProp {
        id: earlier,
        key: "x".into(),
        value: Value::Int(1),
    });
    assert!(
        scratch.borrow_mut().rollback_statement(mark).is_err(),
        "a foreign-id property op is not invertible by dropping the statement's creates"
    );
    assert_eq!(
        scratch.borrow().batch.nodes.len(),
        2,
        "a refused rollback leaves the batch as it was"
    );
}

/// ADR-0642: the pipelined write batch's per-statement rollback is a
/// [`StatementMark`], not a batch image. It must undo exactly what the
/// statement created — rows, adjacency and live-label accounting included —
/// and leave the batch's earlier statements untouched, or the batch would
/// pay a per-statement batch copy for a guarantee it already has.
#[test]
fn a_statement_mark_rolls_back_only_the_marked_statement() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    exec_write(&g, &scratch, "CREATE (:Person {name: 'Ada'})").unwrap();

    let mark = scratch.borrow().mark_statement();
    // Two relationships on one endpoint, so the rollback cannot read its
    // adjacency entries off the end of the list.
    exec_write(
        &g,
        &scratch,
        "CREATE (a:Person {name: 'Grace'})-[:KNOWS]->(b:Person {name: 'Alan'}), \
         (a)-[:KNOWS]->(c:Person {name: 'Ada L'})",
    )
    .unwrap();
    {
        let s = scratch.borrow();
        assert_eq!(s.batch.nodes.len(), 4, "the statement's creates are in");
        assert_eq!(s.batch.rels.len(), 2);
        assert_eq!(s.batch.created_nodes.len(), 4);
        assert_eq!(s.batch.created_rels.len(), 2);
        assert_eq!(s.batch.out_adj.len(), 1);
        assert_eq!(s.batch.out_adj.values().next().unwrap().len(), 2);
        assert_eq!(s.batch.in_adj.len(), 2);
        assert_eq!(s.live_label_delta("Person"), 4);
    }

    scratch
        .borrow_mut()
        .rollback_statement(mark)
        .expect("the marked statement's redo is invertible");
    let s = scratch.borrow();
    assert_eq!(
        s.batch.nodes.len(),
        1,
        "the earlier statement's row survives"
    );
    assert!(s.batch.rels.is_empty());
    assert_eq!(
        s.batch.created_nodes.len(),
        1,
        "the batch-spanning set keeps the earlier statement's create"
    );
    assert!(s.batch.created_rels.is_empty());
    assert!(s.batch.out_adj.is_empty(), "the rel left no adjacency");
    assert!(s.batch.in_adj.is_empty());
    assert_eq!(
        s.live_label_delta("Person"),
        1,
        "the failed statement's labels are off the accounting"
    );
    assert!(
        s.redo_since(0).unwrap().len() == 1,
        "the statement's own redo is dropped, the earlier one stays"
    );
}
