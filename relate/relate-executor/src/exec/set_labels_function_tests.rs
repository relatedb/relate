//! The `SET labels(n) = / += / -=` operators — the dynamic label-set
//! write surface. `=` makes the node's label list exactly the operand's
//! names (an omitted name leaves, an empty list strips every label),
//! `+=` adds the names not already carried after the ones kept, and
//! `-=` drops the names the operand names; a no-op spelling leaves both
//! the list and the graph-diff label counters untouched. The label list
//! reads back in write order — the pattern's carried names first, the
//! written ones after them.
//!
//! The operand resolves through the one dynamic-label rule every
//! dynamic label atom shares (`STRING` or `LIST<STRING>`): a constant
//! operand is checked at admission — a zero-row `MATCH` still fails the
//! statement, rendered as a syntax error — while a runtime value is
//! checked per row, rendered as a type error; the rule's detail text is
//! the one authored spelling either way.
//!
//! Every operator test pins the label list the node carries after the
//! write together with the two graph-diff label counters, and the MERGE
//! tests pin the composition with `ON CREATE` / `ON MATCH`, whose arms
//! are ordinary SET item lists — only the taken arm applies.

use super::test_support::{
    column, error_of, exec_autocommit, exec_write, exec_write_uncommitted, list_cell, write_scratch,
};
use crate::graph::Graph;
use relate_core::{DynamicAccessError, DynamicNameToken, Error, Value};

/// The one pin of a `labels(n)` write: `Some(setup)` runs first
/// (committed) when the statement writes against a pre-existing graph,
/// then `cypher`'s single `ls` column — the label list the node carries
/// after the write — with the two graph-diff label counters.
fn assert_label_write(
    setup: Option<&str>,
    cypher: &str,
    expected: &[&str],
    added: i64,
    removed: i64,
) {
    let g = Graph::default();
    if let Some(setup) = setup {
        exec_autocommit(&g, setup).unwrap();
    }
    let result = exec_write_uncommitted(&g, cypher);
    let ls = column(&result, "ls", list_cell);
    let [ls] = ls.as_slice() else {
        panic!("one `ls` row, got {:?}", result.rows);
    };
    assert_eq!(*ls, label_list(expected), "{cypher}");
    assert_eq!(result.stats.labels_added, added, "{cypher}");
    assert_eq!(result.stats.labels_removed, removed, "{cypher}");
}

/// The one rejection pin: `cypher` fails with `detail` — the expected
/// rendering built by [`wrong_label_type`], whichever channel raises it.
fn assert_label_rejection(cypher: &str, detail: &str) {
    let g = Graph::default();
    let result = exec_write(&g, &write_scratch(&g), cypher);
    assert_eq!(error_of(result), detail, "{cypher}");
}

/// The expected label list as one result cell — the value model's own
/// construction, spelled beside the pins that compare against it (the
/// catalog family's `string_list` renders its SHOW/procedure rows; this
/// suite's oracle is not that policy).
fn label_list(names: &[&str]) -> Value {
    Value::List(
        names
            .iter()
            .map(|n| Value::String((*n).into()))
            .collect::<Vec<_>>()
            .into(),
    )
}

/// The dynamic-label rule's wrong-type error, built by the rule's own
/// constructor — the detail is the rule's one authored spelling, never
/// re-stated here.
fn wrong_label_error(type_name: &str) -> Error {
    Error::from(DynamicAccessError::wrong_type(
        DynamicNameToken::Label,
        type_name,
    ))
}

/// The executor channel's rendering: the typed error exactly as the
/// per-row rule raises it — `Error::from` is the same conversion the
/// executor's label evaluation applies.
fn per_row_rejection(type_name: &str) -> String {
    wrong_label_error(type_name).to_string()
}

/// The admission channel's rendering: the same error re-classed into
/// the compile-time syntax family — an `Error::syntax` whose detail
/// carries the kind token, exactly the rendering the front's validation
/// boundary emits for a constant operand's rejection
/// ([`Error::into_syntax`] is the model's own spelling of that
/// re-classing) — so no channel prefix or class token is spelled in
/// this suite.
fn admission_rejection(type_name: &str) -> String {
    wrong_label_error(type_name).into_syntax().to_string()
}

// `=` — the label list becomes exactly the operand's names.

#[test]
fn overwrite_makes_the_label_list_exactly_the_operands_names() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) = ['label1', 'label2'] RETURN labels(n) AS ls",
        &["label1", "label2"],
        2,
        1,
    );
}

#[test]
fn overwrite_with_an_empty_list_strips_every_label() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) = [] RETURN labels(n) AS ls",
        &[],
        0,
        1,
    );
}

#[test]
fn overwrite_on_an_unlabeled_node_sets_exactly_the_list() {
    assert_label_write(
        None,
        "CREATE (n) SET labels(n) = ['label1', 'label2'] RETURN labels(n) AS ls",
        &["label1", "label2"],
        2,
        0,
    );
}

#[test]
fn overwrite_from_a_bound_list_sets_the_bound_names() {
    assert_label_write(
        Some("CREATE (n)"),
        "WITH 'Foo' AS label MATCH (n) SET labels(n) = [label] RETURN labels(n) AS ls",
        &["Foo"],
        1,
        0,
    );
}

// `+=` — the carried names stay, the missing ones arrive.

#[test]
fn extend_with_an_empty_list_is_a_no_op() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) += [] RETURN labels(n) AS ls",
        &["label0"],
        0,
        0,
    );
}

#[test]
fn extend_on_an_unlabeled_node_sets_exactly_the_list() {
    assert_label_write(
        None,
        "CREATE (n) SET labels(n) += ['label1', 'label2'] RETURN labels(n) AS ls",
        &["label1", "label2"],
        2,
        0,
    );
}

#[test]
fn extend_keeps_carried_names_and_adds_only_the_missing_ones() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) WITH n, ['label1', 'label2'] AS ls SET labels(n) += ls \
         RETURN labels(n) AS ls",
        &["label0", "label1", "label2"],
        2,
        0,
    );
}

#[test]
fn extend_of_a_concatenated_expression_adds_the_missing_names() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) += ['label0'] + ['label1'] + ['label2'] \
         RETURN labels(n) AS ls",
        &["label0", "label1", "label2"],
        2,
        0,
    );
}

// `-=` — the named names leave; the rest stay.

#[test]
fn remove_drops_the_named_names() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) -= ['label0'] RETURN labels(n) AS ls",
        &[],
        0,
        1,
    );
}

#[test]
fn remove_with_an_empty_list_is_a_no_op() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) -= [] RETURN labels(n) AS ls",
        &["label0"],
        0,
        0,
    );
}

#[test]
fn remove_of_a_name_not_carried_is_silently_ignored() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) -= ['NotThere'] RETURN labels(n) AS ls",
        &["label0"],
        0,
        0,
    );
}

#[test]
fn remove_duplicates_in_the_operand_behave_as_one() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) -= ['label0', 'label0'] RETURN labels(n) AS ls",
        &[],
        0,
        1,
    );
}

#[test]
fn remove_on_an_unlabeled_node_is_a_no_op() {
    assert_label_write(
        None,
        "CREATE (n) SET labels(n) -= ['Anything'] RETURN labels(n) AS ls",
        &[],
        0,
        0,
    );
}

// The shared value rule: `STRING` or `LIST<STRING>`.

#[test]
fn a_scalar_string_operand_names_one_label() {
    assert_label_write(
        Some("CREATE (n:label0)"),
        "MATCH (n:label0) SET labels(n) = 'Foo' RETURN labels(n) AS ls",
        &["Foo"],
        1,
        1,
    );
}

#[test]
fn constant_bad_operands_are_rejected_before_execution() {
    // A zero-row MATCH still fails: the operand is a constant, checked
    // at admission — the write never runs, so no per-row rule can fire.
    for (item, type_name) in [
        ("SET labels(n) = null", "NULL"),
        ("SET labels(n) += null", "NULL"),
        ("SET labels(n) = [1]", "INTEGER"),
        ("SET labels(n) -= [null]", "NULL"),
    ] {
        let query = format!("MATCH (n:NoSuchLabel) {item} RETURN labels(n) AS ls");
        assert_label_rejection(&query, &admission_rejection(type_name));
    }
}

#[test]
fn runtime_bad_operands_are_rejected_per_row() {
    // WITH-bound values are not constants: admission checks only the
    // type class and leaves the value to the per-row rule — a bound
    // null, and a bound list's bad element, fail at execution on a
    // write that actually ran.
    for (query, type_name) in [
        (
            "CREATE (n:label0) WITH n, null AS ns SET labels(n) += ns RETURN labels(n) AS ls",
            "NULL",
        ),
        (
            "CREATE (n:label0) WITH n, [1] AS ls SET labels(n) = ls RETURN labels(n) AS ls",
            "INTEGER",
        ),
        (
            "CREATE (n:label0) WITH n, [null] AS rs SET labels(n) -= rs RETURN labels(n) AS ls",
            "NULL",
        ),
    ] {
        assert_label_rejection(query, &per_row_rejection(type_name));
    }
}

// MERGE composition: the taken arm's SET items apply, the other arm's
// do not.

#[test]
fn merge_on_a_matching_node_takes_the_on_match_arm() {
    assert_label_write(
        Some("CREATE (:Person {name: 'Gem'})"),
        "WITH 'NewLabel' AS label, 'OldLabel' AS label2 \
         MERGE (gem:Person {name: 'Gem'}) \
         ON CREATE SET labels(gem) = [label] \
         ON MATCH SET labels(gem) += [label2] \
         RETURN labels(gem) AS ls",
        &["Person", "OldLabel"],
        1,
        0,
    );
}

#[test]
fn merge_on_a_missing_node_takes_the_on_create_arm() {
    assert_label_write(
        None,
        "WITH 'Fresh' AS label \
         MERGE (gem:Person {name: 'Gem'}) \
         ON CREATE SET labels(gem) += [label] \
         ON MATCH SET labels(gem) += ['Never'] \
         RETURN labels(gem) AS ls",
        &["Person", "Fresh"],
        2,
        0,
    );
}
