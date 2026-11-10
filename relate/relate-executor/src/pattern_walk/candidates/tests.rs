//! LABS-803's candidate-selection pins: the funnel shapes that read the
//! ascending id-list algebra — the served folds and exactness
//! compositions, multi-label patterns, label+filter combinations, and
//! the served-superset narrowing's binary-search membership against the
//! label answer. The algebra's own two-pointer merges live one tier
//! down beside their code (`overlay/idlists.rs`), with their direct
//! tests there.

use std::collections::BTreeMap;

use relate_core::{NodeId, Result, Value};
use relate_ir::{Expr, LabelExpr, NodePattern};
use relate_names::SmallName;

use crate::graph::Graph;
use crate::overlay::{intersect_ids, union_ids, ExecGraph};
use crate::row::Row;

use super::seek::ServedIds;
use super::{node_candidates, node_candidates_capped, Capped};

/// One node with `labels` and the name/age pair the fixtures filter on —
/// the fixture shape every test here reads. Callers batch their fixture
/// nodes inside one `begin_write`/`commit_write` pair.
fn labeled_node(g: &Graph, labels: &[&str], name: &str, age: i64) -> NodeId {
    g.create_node(
        labels.iter().map(|l| l.to_string()).collect(),
        BTreeMap::from([
            ("name".into(), Value::String(name.into())),
            ("age".into(), Value::Int(age)),
        ]),
    )
    .unwrap()
}

/// The pattern every funnel test reads: `labels` as a static conjunction
/// (the planner's `add_labels` shape, what every plain
/// `MATCH (n:A:B {…})` carries) plus an optional literal property map.
fn pattern(variable: &str, labels: &[&str], props: Option<&[(&str, Value)]>) -> NodePattern {
    NodePattern {
        variable: Some(SmallName::from_name(variable)),
        labels: labels
            .iter()
            .map(|l| (SmallName::from_name(l), Default::default()))
            .collect(),
        label_expr: Some(LabelExpr::And(
            labels
                .iter()
                .map(|l| LabelExpr::Label((*l).into()))
                .collect(),
        )),
        properties: props.map(|entries| {
            Expr::Map(
                entries
                    .iter()
                    .map(|(k, v)| ((*k).into(), Expr::RuntimeValue(v.clone())))
                    .collect(),
            )
        }),
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    }
}

/// The candidates the funnel answers for one pattern over `g`: the
/// unbound variable's seek strategy and scan tail, no residual
/// conjuncts — the shape every label-only and label+property MATCH
/// takes first.
fn candidates(g: &Graph, np: &NodePattern) -> Vec<NodeId> {
    node_candidates(
        &ExecGraph::read(g),
        np,
        &Row::new(),
        &BTreeMap::new(),
        None,
        &[],
    )
    .unwrap()
}

/// The both-label population's Alice property map — the narrowing
/// fixture's one filter.
fn alice_props() -> Result<BTreeMap<String, Value>> {
    Ok(BTreeMap::from([(
        "name".into(),
        Value::String("Alice".into()),
    )]))
}

/// The Boolean composite keeps its exactness tag only when both arms are
/// exact — one superset arm keeps the composite a superset — and folds
/// the ids through the merge the op selects.
#[test]
fn folded_composes_exactness_and_ids() {
    assert!(matches!(
        ServedIds::Exact(vec![1, 3]).folded(ServedIds::Exact(vec![1, 2]), union_ids),
        ServedIds::Exact(_)
    ));
    assert!(matches!(
        ServedIds::Exact(vec![1, 3]).folded(ServedIds::Superset(vec![3, 4]), intersect_ids),
        ServedIds::Superset(_)
    ));
    assert_eq!(
        ServedIds::Exact(vec![1, 3])
            .folded(ServedIds::Exact(vec![3, 4]), union_ids)
            .into_ids(),
        vec![1, 3, 4]
    );
    assert_eq!(
        ServedIds::Superset(vec![1, 3])
            .folded(ServedIds::Superset(vec![3, 4]), intersect_ids)
            .into_ids(),
        vec![3]
    );
}

/// The served-superset narrowing probes the label/property answer by
/// binary search on its ascending list — every member of the superset
/// the answer does not carry is dropped (LABS-803: the membership test
/// iterates the label id list, no hash set).
#[test]
fn narrowed_probes_the_label_answer_by_binary_search() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let bob = labeled_node(&g, &["Person", "Employee"], "Bob", 40);
    let alice = labeled_node(&g, &["Person", "Employee"], "Alice", 30);
    let _carol = labeled_node(&g, &["Person"], "Carol", 50);
    g.commit_write();
    let labels: Vec<String> = vec!["Person".into(), "Employee".into()];

    // The whole both-label population, in the ascending order a serve
    // answers: narrowing by Alice's name keeps only her id out of the
    // served superset.
    let narrowed =
        ServedIds::Superset(vec![bob, alice]).narrowed(&ExecGraph::read(&g), &labels, alice_props);
    assert_eq!(narrowed.unwrap(), vec![alice]);

    // A superset member the label answer does not carry is dropped; an
    // empty superset narrows to nothing.
    g.begin_write().unwrap();
    let stranger = labeled_node(&g, &["Outsider"], "Dan", 60);
    g.commit_write();
    let narrowed = ServedIds::Superset(vec![alice, stranger]).narrowed(
        &ExecGraph::read(&g),
        &labels,
        alice_props,
    );
    assert_eq!(narrowed.unwrap(), vec![alice]);
    let narrowed =
        ServedIds::Superset(Vec::new()).narrowed(&ExecGraph::read(&g), &labels, alice_props);
    assert!(narrowed.unwrap().is_empty());

    // An exact serve answers as served — the property map is never even
    // evaluated (the lazy contract the narrowing gate keeps).
    let unchanged = ServedIds::Exact(vec![bob]).narrowed(
        &ExecGraph::read(&g),
        &labels,
        || -> Result<BTreeMap<String, Value>> {
            panic!("an exact serve never evaluates the property map")
        },
    );
    assert_eq!(unchanged.unwrap(), vec![bob]);
}

/// A multi-label pattern's candidates are exactly the nodes carrying
/// every label — the single-label and unlabeled decoys stay out.
#[test]
fn multi_label_patterns_intersect_their_labels() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let both = labeled_node(&g, &["Person", "Employee"], "Bob", 40);
    let triple = labeled_node(&g, &["Person", "Employee", "Manager"], "Dilbert", 45);
    let _person_only = labeled_node(&g, &["Person"], "Alice", 30);
    let _employee_only = labeled_node(&g, &["Employee"], "Carol", 50);
    let _plain = labeled_node(&g, &[], "Dan", 60);
    g.commit_write();

    let ids = candidates(&g, &pattern("p", &["Person", "Employee"], None));
    let mut expected = vec![both, triple];
    expected.sort_unstable();
    assert_eq!(ids, expected);
}

/// A label+filter combination narrows the multi-label population to the
/// property map's exact matches.
#[test]
fn label_and_property_filters_narrow_together() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let bob = labeled_node(&g, &["Person", "Employee"], "Bob", 40);
    let _other_bob = labeled_node(&g, &["Person"], "Bob", 40);
    let _dilbert = labeled_node(&g, &["Person", "Employee"], "Dilbert", 45);
    g.commit_write();

    let ids = candidates(
        &g,
        &pattern(
            "p",
            &["Person", "Employee"],
            Some(&[("name", Value::String("Bob".into()))]),
        ),
    );
    assert_eq!(ids, vec![bob]);
}

/// `n` nodes labeled `Person` sharing one property value, in one write.
fn same_name(g: &Graph, n: usize) -> Vec<NodeId> {
    g.begin_write().unwrap();
    let ids = (0..n)
        .map(|i| labeled_node(g, &["Person"], "Bob", i as i64))
        .collect();
    g.commit_write();
    ids
}

/// The capped funnel answers the complete list up to its cap and `Over`
/// from one match past it.
#[test]
fn a_capped_funnel_is_complete_at_the_cap_and_over_past_it() {
    let g = Graph::default();
    let np = pattern(
        "p",
        &["Person"],
        Some(&[("name", Value::String("Bob".into()))]),
    );
    let capped = |g: &Graph, cap| {
        node_candidates_capped(
            &ExecGraph::read(g),
            &np,
            &Row::new(),
            &BTreeMap::new(),
            None,
            &[],
            cap,
        )
        .unwrap()
    };
    let mut ids = same_name(&g, 4);
    ids.sort_unstable();
    assert!(matches!(capped(&g, 4), Capped::Within(got) if got == ids));
    assert!(matches!(capped(&g, 3), Capped::Over));
    assert!(matches!(capped(&g, 0), Capped::Over));
    assert_eq!(candidates(&g, &np), ids);
}

/// The scan stops at the first match past the cap: the node reads the
/// funnel counts stay at `cap + 1` where the uncapped funnel reads every
/// match.
#[test]
fn a_capped_funnel_stops_scanning_past_the_cap() {
    let g = Graph::default();
    same_name(&g, 200);
    let np = pattern(
        "p",
        &["Person"],
        Some(&[("name", Value::String("Bob".into()))]),
    );
    let _ = crate::ctx::take_node_reads();
    let capped = node_candidates_capped(
        &ExecGraph::read(&g),
        &np,
        &Row::new(),
        &BTreeMap::new(),
        None,
        &[],
        16,
    )
    .unwrap();
    assert!(matches!(capped, Capped::Over));
    assert_eq!(crate::ctx::take_node_reads(), 17);
    assert_eq!(candidates(&g, &np).len(), 200);
    assert_eq!(crate::ctx::take_node_reads(), 200);
}
