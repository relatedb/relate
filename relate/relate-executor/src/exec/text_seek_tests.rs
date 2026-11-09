//! The TEXT-index seek tests of the candidates funnel (moved from
//! graph/tests — they pin exec behavior): the OR-union decline policy,
//! the ENDS WITH ∩ CONTAINS intersection over a pushdown Residual, and
//! the several-served-OR-conjuncts intersection (LABS-803's sorted-list
//! fold) over a pushdown Residual.

use std::collections::BTreeMap;

use super::test_support::exec_autocommit;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::Row;
use relate_core::{NodeId, Value};

/// One :File node carrying the content/name/path triple the seek tests
/// probe — the one fixture both tests' rows come from.
fn file_node(g: &Graph, content: &str, name: &str, path: &str) -> NodeId {
    g.create_node(
        vec!["File".into()],
        BTreeMap::from([
            ("content".into(), Value::String(content.into())),
            ("name".into(), Value::String(name.into())),
            ("path".into(), Value::String(path.into())),
        ]),
    )
    .unwrap()
}

/// The TEXT index on one fixture property (the format!-built Cypher the
/// sibling seek tests already use).
fn file_text_index(g: &Graph, prop: &str) {
    exec_autocommit(
        g,
        &format!("CREATE TEXT INDEX file_{prop} FOR (f:File) ON (f.{prop})"),
    )
    .unwrap();
}

/// The candidates of one conjunct over the fixture graph: parse, lower,
/// flatten, and ask the funnel — sorted, so the assertions compare sets.
fn file_candidates(g: &Graph, conjunct: &str) -> Vec<NodeId> {
    let parsed = relate_parser::parser::parse_expression_str(conjunct).unwrap();
    let expr = relate_front::lower_expr(&parsed).unwrap();
    let conjuncts = crate::exec::pushdown::flatten_and_owned(&expr);
    let mut ids = crate::pattern_walk::node_candidates(
        &ExecGraph::read(g),
        &file_pattern(),
        &Row::new(),
        &BTreeMap::new(),
        None,
        &conjuncts,
    )
    .unwrap();
    ids.sort_unstable();
    ids
}

/// The :File pattern the seek tests probe.
fn file_pattern() -> relate_ir::NodePattern {
    use relate_ir::{LabelExpr, NodePattern};
    NodePattern {
        variable: Some("f".into()),
        labels: vec![("File".into(), Default::default())],
        label_expr: Some(LabelExpr::Label("File".into())),
        properties: None,
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    }
}

/// The OR-union fixture: one file per arm holding the needle, one holding
/// it in both, one holding it nowhere — plus a decoy :Person the scan
/// must not pick up. Returns (graph, content, name, path, both, none).
fn walrecord_files() -> (Graph, NodeId, NodeId, NodeId, NodeId, NodeId) {
    let g = Graph::default();
    g.begin_write().unwrap();
    let content = file_node(&g, "has WalRecord", "a", "a.txt");
    let name = file_node(&g, "plain", "WalRecord notes", "b.txt");
    let path = file_node(&g, "plain", "b", "dir/WalRecord");
    let both = file_node(&g, "WalRecord", "WalRecord", "both.txt");
    let none = file_node(&g, "plain", "c", "c.txt");
    let _person = g
        .create_node(
            vec!["Person".into()],
            BTreeMap::from([("name".into(), Value::String("WalRecord".into()))]),
        )
        .unwrap();
    g.commit_write();
    (g, content, name, path, both, none)
}

/// The ENDS WITH ∩ CONTAINS fixture: the files whose needle hits sit in
/// different suffix/membership combinations. Returns
/// (graph, both, path, suffix_inside).
fn suffix_files() -> (Graph, NodeId, NodeId, NodeId) {
    let g = Graph::default();
    g.begin_write().unwrap();
    let both = file_node(&g, "has WalRecord", "lib.rs", "src/lib.rs");
    let path = file_node(&g, "plain", "notes.txt", "dir/WalRecord");
    let _content_only = file_node(&g, "has WalRecord", "notes.txt", "notes.txt");
    let suffix_inside = file_node(&g, "has WalRecord", "lib.rs.bak", "bak");
    let _rs_only = file_node(&g, "plain", "other.rs", "other.rs");
    let _none = file_node(&g, "plain", "c", "c.txt");
    g.commit_write();
    (g, both, path, suffix_inside)
}

/// A missing arm index declines the whole OR union to the :File scan —
/// the union is a candidate set only when every arm is served.
#[test]
fn or_union_candidates_skip_the_non_matching_file() {
    let (g, content, name, path, both, none) = walrecord_files();
    // content and name only — the path index stays missing for the
    // decline case below.
    file_text_index(&g, "content");
    file_text_index(&g, "name");
    const UNION: &str =
        "f.content CONTAINS 'WalRecord' OR f.name CONTAINS 'WalRecord' OR f.path CONTAINS 'WalRecord'";

    let scanned = file_candidates(&g, UNION);
    let mut every_file = vec![content, name, path, both, none];
    every_file.sort_unstable();
    assert_eq!(
        scanned, every_file,
        "a missing path index declines the union and scans :File"
    );

    file_text_index(&g, "path");
    let sought = file_candidates(&g, UNION);
    let mut matches = vec![content, name, path, both];
    matches.sort_unstable();
    assert_eq!(sought, matches);
}

/// ENDS WITH is served by the text index as a contains-superset; the two
/// conjuncts of one arm intersect their candidate sets.
#[test]
fn ends_with_and_intersects_the_text_index() {
    let (g, both, path, suffix_inside) = suffix_files();
    file_text_index(&g, "content");
    file_text_index(&g, "name");
    file_text_index(&g, "path");

    let sought = file_candidates(
        &g,
        "(f.content CONTAINS 'WalRecord' AND f.name ENDS WITH '.rs') OR f.path CONTAINS 'WalRecord'",
    );
    let mut expected = vec![both, path, suffix_inside];
    expected.sort_unstable();
    assert_eq!(sought, expected);
}

/// Several served OR conjuncts are AND factors, so their id lists are
/// intersected — the accumulator fold across two OR conjuncts, each a
/// union of two served arms (LABS-803: the fold merges the ascending
/// id lists directly, no per-statement hash set).
#[test]
fn several_served_or_conjuncts_intersect_their_unions() {
    let (g, content, _name, _path, both, _none) = walrecord_files();
    file_text_index(&g, "content");
    file_text_index(&g, "name");
    file_text_index(&g, "path");

    // Conjunct 1's union is {content, name, both}; conjunct 2's is
    // {content, path, both} ('Record' hits the two WalRecord contents
    // and the WalRecord path). Their intersection: {content, both}.
    let sought = file_candidates(
        &g,
        "(f.content CONTAINS 'WalRecord' OR f.name CONTAINS 'WalRecord') \
         AND (f.content CONTAINS 'Record' OR f.path CONTAINS 'Record')",
    );
    let mut expected = vec![content, both];
    expected.sort_unstable();
    assert_eq!(sought, expected);
}
