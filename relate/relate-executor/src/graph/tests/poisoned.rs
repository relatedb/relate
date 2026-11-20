//! Persisted graph-type bytes that never ran the body validator (ADR-0388).

use super::*;

// ---------------------------------------------------------------------
// ADR-0388 review (LABS-386): persisted graph-type bytes that never ran
// the body validator
// ---------------------------------------------------------------------

/// The hand-crafted persisted bytes of a poisoned graph type: a
/// `Constructor` default whose name the CIP-262 table never admitted --
/// the bytes an enabled follower of the gated trio could store, and this
/// engine then deserializes at WAL replay or checkpoint verification.
const POISONED_SPEC_BYTES: &str = r#"{
    "node_types": [{
        "alias": null,
        "identifying_label": "L",
        "implied_labels": [],
        "properties": [{
            "name": "p",
            "value_type": {"Predefined": {"kind": "String", "not_null": false}},
            "default_value": {"Constructor": {"name": "toString", "args": []}},
            "inline_constraint": null
        }]
    }]
}"#;

/// The poisoned spec decoded once for the family's five siblings.
fn poisoned_spec() -> crate::graph_type::GraphTypeSpec {
    serde_json::from_str(POISONED_SPEC_BYTES).expect("hand-crafted bytes decode")
}

/// The one assertion every poisoned-spec sibling makes: the driven path
/// fails with the typed storage-corruption error, never a panic.
fn assert_poisoned_storage_error<T: std::fmt::Debug, E: std::fmt::Display>(
    result: std::result::Result<T, E>,
) {
    let error = result.expect_err("a poisoned persisted default fails typed, not panic");
    assert!(
        error
            .to_string()
            .contains("persisted graph type carries an invalid default value"),
        "expected the typed storage-corruption error, got: {error}"
    );
}

#[test]
fn replay_of_a_poisoned_persisted_default_fails_typed_not_panic() {
    // ADR-0388 review (LABS-386): a gated construct stored by an enabled
    // follower, deserialized from persisted bytes, never ran
    // `validate_graph_type_body`. Committed replay -- WAL redo preflight
    // then apply, the `Database::open` catch-up path -- must surface it
    // as the typed storage-corruption error, never a panic.
    let graph = Graph::default();
    assert_poisoned_storage_error(graph.apply_redo(
        vec![crate::redo::RedoOp::SetCurrentGraphType {
            body: poisoned_spec(),
        }],
        1,
    ));
}

#[test]
fn visualize_of_a_poisoned_persisted_default_fails_typed_not_panic() {
    // The AS GRAPH sibling of the poisoned-spec family (the rust
    // review's High): the same hand-crafted bytes driving the SHOW and
    // checkpoint paths, through the SHOW ... AS GRAPH renderer -- typed
    // storage error, never the visualization panic it used to be.
    let spec: crate::graph_type::GraphTypeSpec =
        serde_json::from_str(POISONED_SPEC_BYTES).expect("hand-crafted persisted bytes decode");
    let body = spec.to_body();
    assert_poisoned_storage_error(crate::graph_type_visualize::render_graph_type_as_graph(
        &body,
        &[],
        relate_parser::AnyNodeMode::Single,
    ));
}

#[test]
fn json_restore_of_a_poisoned_persisted_default_fails_typed_not_panic() {
    // The JSON-snapshot restore sibling (the round-7 finding): the third
    // persisted-spec entry point runs the graph-type validator with the
    // store's preview set before install -- typed family error, never a
    // silently-installed poisoned body.
    let graph = Graph::default();
    // The restore entry takes the body-text form ({ <body> }), the same
    // poisoned default as the serde bytes' `toString` constructor.
    assert_poisoned_storage_error(graph.restore_graph_type_spec(Some(
        "{ (:L => {p :: STRING DEFAULT ON COMMIT toString()})}",
    )));
}

#[test]
fn checkpoint_verify_of_a_poisoned_persisted_default_fails_typed_not_panic() {
    // The checkpoint-verification sibling of the replay test: the same
    // hand-crafted persisted bytes as a checkpoint theater image, restored
    // through the same verify path `Database::open` runs -- typed error,
    // never a panic.
    let image: super::theater::CheckpointTheater = serde_json::from_str(&format!(
        r#"{{"databases": {{}}, "index_details": {{}}, "index_options": {{}}, "graph_types": {{}}, "current_graph_type": {POISONED_SPEC_BYTES}, "derived_constraints": {{}}}}"#
    ))
    .expect("hand-crafted checkpoint bytes decode");
    let graph = Graph::default();
    assert_poisoned_storage_error(graph.restore_checkpoint_theater(image));
}

#[test]
fn checkpoint_writer_of_a_poisoned_persisted_default_fails_typed_not_panic() {
    // The checkpoint-writer sibling of the replay test: the snapshot
    // codec's spec string over a poisoned theater model (the bytes an
    // enabled follower could commit before this engine's replay-time
    // reject) is the same typed storage-corruption error, never a
    // panic.
    let graph = Graph::default();
    graph.theater.lock().unwrap().current_graph_type = poisoned_spec();
    assert_poisoned_storage_error(graph.graph_type_spec());
}

#[test]
fn show_graph_type_of_a_poisoned_persisted_default_fails_typed_not_panic() {
    // The SHOW GRAPH TYPE sibling: the stored-spec render path maps the
    // same poisoned bytes to the same typed error, never a panic.
    let graph = Graph::default();
    graph
        .theater
        .lock()
        .unwrap()
        .graph_types
        .insert("gt".to_string(), poisoned_spec());
    let view = crate::overlay::ExecGraph::read(&graph);
    let clause = relate_parser::ShowGraphTypeClause {
        name: SmallName::from_name("gt"),
        yields: Default::default(),
    };
    assert_poisoned_storage_error(crate::exec::catalog_show::show_graph_type_rows(
        &view, &clause,
    ));
}

#[test]
fn show_current_graph_type_of_a_poisoned_persisted_default_fails_typed_not_panic() {
    // The SHOW CURRENT GRAPH TYPE sibling: the stored-spec render path
    // maps the same poisoned bytes to the same typed error, never a
    // panic.
    let graph = Graph::default();
    graph.theater.lock().unwrap().current_graph_type = poisoned_spec();
    let view = crate::overlay::ExecGraph::read(&graph);
    let clause = relate_parser::ShowCurrentGraphTypeClause {
        yields: Default::default(),
        stages: Vec::new(),
        where_expr: None,
        as_graph: None,
        variant: None,
    };
    assert_poisoned_storage_error(crate::exec::catalog_show::show_current_graph_type_rows(
        &view, &clause,
    ));
}
