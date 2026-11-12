//! LABS-803's candidate-selection pins: the funnel shapes that read the
//! ascending id-list algebra — the served folds and exactness
//! compositions, multi-label patterns, label+filter combinations, and
//! the served-superset narrowing's binary-search membership against the
//! label answer — plus LABS-1083's label-disjunction union arm. The
//! algebra's own two-pointer merges live one tier
//! down beside their code (`overlay/idlists.rs`), with their direct
//! tests there.

use std::collections::BTreeMap;

use relate_core::{NodeId, Result, Value};
use relate_ir::{BinaryOp, Expr, LabelExpr, NodePattern};
use relate_names::SmallName;

use crate::graph::{test_support, Graph};
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
    node_candidates(&ExecGraph::read(g), np, &Row::new(), &BTreeMap::new(), &[]).unwrap()
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
        &[],
        16,
    )
    .unwrap();
    assert!(matches!(capped, Capped::Over));
    assert_eq!(crate::ctx::take_node_reads(), 17);
    assert_eq!(candidates(&g, &np).len(), 200);
    assert_eq!(crate::ctx::take_node_reads(), 200);
}

/// The fold's plain-Or label shape: `Or([Model, Source])` over no
/// conjunction labels, `np.labels` empty — the shape
/// `MATCH (p) WHERE p:Model OR p:Source` carries after the rewrite.
fn or_pattern(variable: &str, disjuncts: &[&str]) -> NodePattern {
    NodePattern {
        variable: Some(SmallName::from_name(variable)),
        labels: Vec::new(),
        label_expr: Some(LabelExpr::Or(
            disjuncts
                .iter()
                .map(|l| LabelExpr::Label((*l).into()))
                .collect(),
        )),
        properties: None,
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    }
}

/// The fold's nested shape: the pattern's own conjunction `base` carried
/// beside the folded `Or` — `And([C, Or([A, B])])` with `np.labels` the
/// base, the shape `MATCH (p:C) WHERE p:A OR p:B` carries after the
/// rewrite.
fn and_nested_pattern(variable: &str, base: &[&str], disjuncts: &[&str]) -> NodePattern {
    let mut arms: Vec<LabelExpr> = base.iter().map(|l| LabelExpr::Label((*l).into())).collect();
    arms.push(LabelExpr::Or(
        disjuncts
            .iter()
            .map(|l| LabelExpr::Label((*l).into()))
            .collect(),
    ));
    NodePattern {
        variable: Some(SmallName::from_name(variable)),
        labels: base
            .iter()
            .map(|l| (SmallName::from_name(l), Default::default()))
            .collect(),
        label_expr: Some(LabelExpr::And(arms)),
        properties: None,
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    }
}

/// The equality conjunct `var.key = value` — the residual predicate the
/// funnel's seek loop keys on (the rewrite leaves a row-bound predicate
/// here; a literal one lands in the pattern's property map instead).
fn eq_conjunct(var: &str, key: &str, value: i64) -> Expr {
    Expr::Binary {
        op: BinaryOp::Eq,
        left: Box::new(Expr::Property {
            key_id: Default::default(),
            expr: Box::new(Expr::Variable(SmallName::from_name(var))),
            key: SmallName::from_name(key),
        }),
        right: Box::new(Expr::Int(value)),
    }
}

/// The funnel's answer for one pattern with residual conjuncts — the
/// seek strategy a row-bound predicate drives.
fn candidates_with(g: &Graph, np: &NodePattern, conjuncts: &[Expr]) -> Vec<NodeId> {
    node_candidates(
        &ExecGraph::read(g),
        np,
        &Row::new(),
        &BTreeMap::new(),
        conjuncts,
    )
    .unwrap()
}

/// LABS-1083: a static label disjunction with a keyed equality predicate
/// serves as the union of the per-label keyed postings — every disjunct
/// label's index consulted, the ids deduplicated so a node carrying BOTH
/// labels yields ONE candidate (the `Or` predicate yields each node
/// once), and the single-label decoys and other-valued rows stay out.
#[test]
fn label_disjunction_seeks_as_the_deduplicated_union_of_per_label_postings() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let model = labeled_node(&g, &["Model"], "m", 7);
    let source = labeled_node(&g, &["Source"], "s", 7);
    let both = labeled_node(&g, &["Model", "Source"], "b", 7);
    let _other_model = labeled_node(&g, &["Model"], "m2", 8);
    let _other_label = labeled_node(&g, &["Other"], "o", 7);
    g.commit_write();
    test_support::range_index(&g, "Model", "age");
    test_support::range_index(&g, "Source", "age");

    let ids = candidates_with(
        &g,
        &or_pattern("p", &["Model", "Source"]),
        &[eq_conjunct("p", "age", 7)],
    );
    let mut expected = vec![model, source, both];
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "each disjunct's match once, the both-labels node exactly once"
    );
}

/// LABS-1083's nested composition: `And([C, Or([A, B])])` — every arm is
/// the shared conjunction plus one disjunct, each a static conjunction
/// the keyed arm serves; the union keeps the nodes carrying `C` and
/// either disjunct, once each, and drops the `C`-less and disjunct-less
/// decoys the per-arm re-check filters.
#[test]
fn nested_disjunction_seeks_per_arm_conjunctions() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let ca = labeled_node(&g, &["C", "A"], "ca", 7);
    let cb = labeled_node(&g, &["C", "B"], "cb", 7);
    let _ab = labeled_node(&g, &["A", "B"], "ab", 7);
    let _c_only = labeled_node(&g, &["C"], "c", 7);
    let _other_value = labeled_node(&g, &["C", "A"], "ca2", 8);
    g.commit_write();
    test_support::range_index(&g, "A", "age");
    test_support::range_index(&g, "B", "age");

    let ids = candidates_with(
        &g,
        &and_nested_pattern("p", &["C"], &["A", "B"]),
        &[eq_conjunct("p", "age", 7)],
    );
    let mut expected = vec![ca, cb];
    expected.sort_unstable();
    assert_eq!(ids, expected);
}

/// A disjunct label with no index on the sought pair declines the whole
/// union (the OR-union pass's every-arm-served rule): the funnel falls
/// to the scan tail, whose answer is every node id — the label
/// expression and the predicate are re-applied per candidate by the
/// walk, so the decline keeps the answer, only the seek is lost.
#[test]
fn an_unindexed_disjunct_arm_declines_to_the_scan_tail() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let model = labeled_node(&g, &["Model"], "m", 7);
    let source = labeled_node(&g, &["Source"], "s", 7);
    let _other_label = labeled_node(&g, &["Other"], "o", 7);
    g.commit_write();
    // Only :Model(age) carries an index: the :Source arm cannot serve.
    test_support::range_index(&g, "Model", "age");

    let ids = candidates_with(
        &g,
        &or_pattern("p", &["Model", "Source"]),
        &[eq_conjunct("p", "age", 7)],
    );
    // The scan tail's answer: every node id, the per-candidate check
    // filters from here.
    let mut expected = ExecGraph::read(&g).all_node_ids();
    expected.sort_unstable();
    assert_eq!(ids, expected);
    assert!(ids.contains(&model) && ids.contains(&source));
}

/// LABS-1083 review (the folded-map repro shape): a seek-const equality
/// folds into the pattern's own property map, so the residual carries no
/// `Seek` — the scan tail must serve the map's equalities per arm, the
/// per-arm analogue of the single-label path's one `lookup_nodes` call:
/// each arm's `(label, property)` index consulted with the map's value,
/// the answers deduplicated (the both-labels node once), the other-valued
/// and other-label decoys out. Without the per-arm indexes the same arms
/// answer through their label scans inside the lookup — the single-label
/// path's own decline — with the same ids.
#[test]
fn the_folded_map_equality_serves_per_arm() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let model = labeled_node(&g, &["Model"], "m", 7);
    let source = labeled_node(&g, &["Source"], "s", 7);
    let both = labeled_node(&g, &["Model", "Source"], "b", 7);
    let _other_model = labeled_node(&g, &["Model"], "m2", 8);
    let _other_label = labeled_node(&g, &["Other"], "o", 7);
    g.commit_write();
    test_support::range_index(&g, "Model", "age");
    test_support::range_index(&g, "Source", "age");

    let mut folded = or_pattern("p", &["Model", "Source"]);
    folded.properties = Some(Expr::Map(vec![(
        "age".to_string(),
        Expr::RuntimeValue(Value::Int(7)),
    )]));
    let ids = candidates(&g, &folded);
    let mut expected = vec![model, source, both];
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "each arm's keyed match once, the both-labels node exactly once"
    );

    // The same shape over a store without the indexes: each arm's lookup
    // falls to that arm's label scan inside `lookup_nodes`, the
    // single-label path's decline — the same answer.
    let bare = Graph::default();
    bare.begin_write().unwrap();
    let model = labeled_node(&bare, &["Model"], "m", 7);
    let source = labeled_node(&bare, &["Source"], "s", 7);
    let both = labeled_node(&bare, &["Model", "Source"], "b", 7);
    let _other_model = labeled_node(&bare, &["Model"], "m2", 8);
    let _other_label = labeled_node(&bare, &["Other"], "o", 7);
    bare.commit_write();
    let ids = candidates(&bare, &folded);
    let mut expected = vec![model, source, both];
    expected.sort_unstable();
    assert_eq!(ids, expected, "the per-arm label scans answer the same ids");
}

/// LABS-1083 review (the unserved-union fallback): `And([C, Or([A, B])])`
/// with no residual predicate and no property map — the shape
/// `MATCH (p:C) WHERE p:A OR p:B` carries after the fold — scans the
/// conjunction base (the pre-fold `:C` label scan), never every node id:
/// the candidates are the `:C` population, the `Or` arms left to the
/// per-candidate check, and the `C`-less decoys stay out.
#[test]
fn an_unserved_disjunction_scans_the_conjunction_base() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let ca = labeled_node(&g, &["C", "A"], "ca", 7);
    let cb = labeled_node(&g, &["C", "B"], "cb", 7);
    let c_only = labeled_node(&g, &["C"], "c", 7);
    let _ab = labeled_node(&g, &["A", "B"], "ab", 7);
    let _other = labeled_node(&g, &["Other"], "o", 7);
    g.commit_write();

    let ids = candidates(&g, &and_nested_pattern("p", &["C"], &["A", "B"]));
    let mut expected = vec![ca, cb, c_only];
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "the :C label scan's population, not the store's every node id"
    );
}

/// The bare-`Or` shape without equalities keeps the all-nodes scan its
/// pre-fold form took: no conjunction base to scan, no map to key — the
/// per-candidate check filters the arms from the full population.
#[test]
fn a_bare_disjunction_without_equalities_keeps_the_all_nodes_scan() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let _model = labeled_node(&g, &["Model"], "m", 7);
    let _source = labeled_node(&g, &["Source"], "s", 7);
    let _other = labeled_node(&g, &["Other"], "o", 7);
    g.commit_write();

    let ids = candidates(&g, &or_pattern("p", &["Model", "Source"]));
    let mut expected = ExecGraph::read(&g).all_node_ids();
    expected.sort_unstable();
    assert_eq!(ids, expected);
}

/// The pair-bound probe is bounded for every scan-served endpoint shape
/// (ADR-0567): over a graph far larger than the bound, each shape's funnel
/// reads stay at `cap + 1` per scanned arm, and the counter the PROFILE
/// Db Hits column reads equals the reads performed. The index-served shape
/// streams its posting and stops at the bound too.
#[test]
fn a_capped_probe_reads_cap_plus_one_for_every_scan_shape() {
    use crate::exec::test_support::exec_autocommit;
    const CAP: usize = 16;
    let g = Graph::default();
    exec_autocommit(
        &g,
        "UNWIND range(0, 999) AS i CREATE (:Person {name: 'Bob'}), (:Robot {name: 'Bob'}), \
         (:Other {name: 'Bob'})",
    )
    .unwrap();
    let probe = |np: &NodePattern| {
        let _ = crate::ctx::take_node_reads();
        let got = node_candidates_capped(
            &ExecGraph::read(&g),
            np,
            &Row::new(),
            &BTreeMap::new(),
            &[],
            CAP,
        )
        .unwrap();
        assert!(matches!(got, Capped::Over));
        crate::ctx::take_node_reads()
    };
    let name = [("name", Value::String("Bob".into()))];
    // A label and a map, no index.
    assert_eq!(probe(&pattern("p", &["Person"], Some(&name))), 17);
    // A map alone: the all-nodes scan, stopped at the bound.
    let mut bare = pattern("p", &[], Some(&name));
    bare.label_expr = None;
    assert_eq!(probe(&bare), 17);
    // A label and a map under a declared index: the posting is streamed and
    // stops at the bound, not read in full (1000 postings).
    exec_autocommit(&g, "CREATE INDEX person_name FOR (n:Person) ON (n.name)").unwrap();
    assert_eq!(probe(&pattern("p", &["Person"], Some(&name))), 17);
    // A label disjunction: one capped scan per arm.
    let mut either = pattern("p", &[], Some(&name));
    either.label_expr = Some(LabelExpr::Or(vec![
        LabelExpr::Label("Person".into()),
        LabelExpr::Label("Robot".into()),
    ]));
    assert_eq!(probe(&either), 34);
}

/// A negated label has no keyed scan: the funnel's all-nodes tail is a
/// superset, which the route must not read as a match count.
#[test]
fn only_exact_scan_shapes_answer_a_capped_probe() {
    let name = [("name", Value::String("Bob".into()))];
    assert!(super::scan_answers_exactly(&pattern(
        "p",
        &["Person"],
        Some(&name)
    )));
    let mut either = pattern("p", &[], Some(&name));
    either.label_expr = Some(LabelExpr::Or(vec![
        LabelExpr::Label("A".into()),
        LabelExpr::Label("B".into()),
    ]));
    assert!(super::scan_answers_exactly(&either));
    let mut not = pattern("p", &[], Some(&name));
    not.label_expr = Some(LabelExpr::Not(Box::new(LabelExpr::Label("A".into()))));
    assert!(!super::scan_answers_exactly(&not));
}
