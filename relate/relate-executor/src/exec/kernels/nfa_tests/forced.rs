//! The forced rung's gap classes (ADR-0519's `RELATE_PATTERN_ENGINE=nfa`):
//! interior admission compiles to per-state guards (class K) and the
//! quantified group's own WHERE to a transition predicate (class F) — the
//! same shapes the auto rung declines in the parent suite, its decline
//! tests untouched. A child of [`super`] so the compile helpers stay one
//! spelling.

use relate_ir::{NodePattern, PatternElement};

use super::super::nfa::{compile, FrontierNfa};
use super::{declines, graph_with_types};
use crate::exec::test_support::{
    named_node, one_hop_rel, path_pattern, patterns, quantified, var_length_rel,
};
use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;

/// Compile `cypher`'s first pattern on the forced rung — the same store
/// the auto tests use, the rung the only difference.
fn forced_compiled(cypher: &str, names: &[&str]) -> (Graph, FrontierNfa) {
    let graph = graph_with_types(names);
    let pattern = &patterns(cypher)[0];
    let nfa = compile(&graph, pattern, PatternEngine::Nfa)
        .unwrap_or_else(|| panic!("{cypher} should compile forced"));
    (graph, nfa)
}

/// Compile `cypher`'s first pattern on the forced rung and assert the
/// decline.
fn forced_declines(cypher: &str, names: &[&str]) {
    let graph = graph_with_types(names);
    let pattern = &patterns(cypher)[0];
    assert!(
        compile(&graph, pattern, PatternEngine::Nfa).is_none(),
        "{cypher} should decline even forced"
    );
}

/// One state's guard labels, in attach order — the layout pin's
/// assertion vocabulary.
fn guard_labels(nfa: &FrontierNfa, state: u32) -> Vec<String> {
    nfa.state_guards[state as usize]
        .iter()
        .flat_map(|guard| guard.label_names().map(|label| label.to_string()))
        .collect()
}

/// Class K, the guarded-round-start form: the body's first node's label
/// is the `open` state's guard, entered by epsilon from the entry, and
/// the round-closing hop still lands the entry (no close split — the
/// boundary carries only the start pattern), so the epsilon re-check
/// admits each next round's start. Two states, one hop per round.
#[test]
fn forced_guarded_round_start_compiles_as_the_open_state_guard() {
    let (_, nfa) = forced_compiled(
        "MATCH p = (x)((m:Start)-[:knows]-(n)){1,}(y) RETURN p LIMIT 100",
        &["knows"],
    );
    assert_eq!((nfa.states, nfa.start), (2, 0));
    assert_eq!((nfa.min_hops, nfa.max_hops), (1, None));
    assert_eq!(nfa.epsilons, vec![(0, 1)]);
    assert_eq!(nfa.transitions.len(), 1);
    assert_eq!((nfa.transitions[0].from, nfa.transitions[0].to), (1, 0));
    assert_eq!(guard_labels(&nfa, 0), Vec::<String>::new());
    assert_eq!(guard_labels(&nfa, 1), vec!["Start".to_string()]);
    assert!(nfa.predicates.iter().all(Option::is_none));
}

/// Class K, the guarded-round-end form: the body's last node's label
/// forces the boundary split — an `open` state (no guard; the body's
/// first node is plain) and a `close` state carrying the label, the
/// last hop's target, stitched back to the open state by epsilon so the
/// next round re-enters through the same positions.
#[test]
fn forced_guarded_round_end_compiles_as_the_split_close_state() {
    let (_, nfa) = forced_compiled(
        "MATCH p = (x)-[:ctx]-(k)((m)-[:knows]-(n:End)){1,}(y) RETURN p LIMIT 100",
        &["ctx", "knows"],
    );
    assert_eq!((nfa.states, nfa.start), (4, 0));
    // One leading context hop plus one hop per round.
    assert_eq!((nfa.min_hops, nfa.max_hops), (2, None));
    assert_eq!(nfa.epsilons, vec![(1, 2), (3, 2)]);
    assert_eq!(nfa.transitions.len(), 2);
    assert_eq!((nfa.transitions[0].from, nfa.transitions[0].to), (0, 1));
    assert_eq!((nfa.transitions[1].from, nfa.transitions[1].to), (2, 3));
    // The interior context node is plain (no guard); the close state
    // carries the round-end label.
    assert_eq!(guard_labels(&nfa, 1), Vec::<String>::new());
    assert_eq!(guard_labels(&nfa, 3), vec!["End".to_string()]);
}

/// Class F: the group WHERE rides the body's round-closing transition,
/// each referenced variable mapped to the state its position occupies —
/// the round start to the open state, the round end to the close state,
/// an unreferenced mid position mapped nowhere.
#[test]
fn forced_group_where_compiles_as_the_closing_transition_guard() {
    let (_, nfa) = forced_compiled(
        "MATCH p = (x)((m)-[:knows]->(w)-[:likes]->(n) WHERE m.k < n.k){1,}(y) \
         RETURN p LIMIT 100",
        &["knows", "likes"],
    );
    assert_eq!((nfa.states, nfa.start), (4, 0));
    assert_eq!((nfa.min_hops, nfa.max_hops), (2, None));
    assert_eq!(nfa.epsilons, vec![(0, 1), (2, 1)]);
    assert_eq!(
        (nfa.transitions[0].from, nfa.transitions[0].to),
        (1, 3),
        "the first body hop departs the open state to the mid"
    );
    assert_eq!(
        (nfa.transitions[1].from, nfa.transitions[1].to),
        (3, 2),
        "the closing hop lands the close state"
    );
    assert!(nfa.predicates[0].is_none());
    let Some(predicate) = &nfa.predicates[1] else {
        panic!("the group WHERE rides the closing transition");
    };
    assert_eq!(
        predicate.vars,
        vec![("m".to_string(), 1), ("n".to_string(), 2)],
        "the round start maps to the open state, the round end to the close"
    );
}

/// The fraud f03-f06 query texts (the fraud-qpp workload's own shapes,
/// `testing/relate-query-latency/workloads/fraud-qpp/queries/`):
/// every one compiles under the forced rung — the interior labels and
/// property maps as per-state guards, the group WHERE as the closing
/// transition's predicate — and every one declines under `auto`, the
/// decline the existing out-of-vocabulary pins already spell.
#[test]
fn fraud_gap_queries_compile_forced_and_decline_auto() {
    let f03 = "MATCH p = SIMPLE (a:Account)<-[:FROM]-(first:Transaction) \
               ((ti:Transaction)-[:TO]->(ai:Account)<-[:FROM]-(tj:Transaction) \
                 WHERE ti.date < tj.date \
                   AND ti.amount >= tj.amount >= 0.80 * ti.amount){1,6} \
               (last:Transaction)-[:TO]->(a) \
               RETURN a.id AS account, \
                      [n IN nodes(p) WHERE n:Transaction | n.amount] AS amounts";
    let f04 = "MATCH p = SIMPLE (a:Account)<-[:FROM]-(first:Transaction) \
               ((ti:Transaction)-[:TO]->(ai:Account)<-[:FROM]-(tj:Transaction) \
                 WHERE ti.date < tj.date \
                   AND ti.amount >= tj.amount >= 0.80 * ti.amount){1,} \
               (last:Transaction)-[:TO]->(a) \
               RETURN a.id AS account, \
                      [n IN nodes(p) WHERE n:Transaction | n.amount] AS amounts";
    let f05 = "MATCH p = SIMPLE (a:Account {id: 'needle-0'})\
               <-[:FROM]-(first:Transaction {test: true}) \
               ((ti:Transaction {test: true})-[:TO]->(ai:Account {test: true})\
               <-[:FROM]-(tj:Transaction {test: true}) \
                 WHERE ti.date < tj.date){1,} \
               (last:Transaction {test: true})-[:TO]->(a) \
               RETURN [n IN nodes(p) WHERE n:Account | n.id] AS ring";
    let f06 = "MATCH p = SIMPLE (a:Account {a_id: 1})<-[:FROM]-(first:Transaction) \
               ((ti:Transaction)-[:TO]->(ai:Account)<-[:FROM]-(tj:Transaction) \
                 WHERE ti.date < tj.date \
                   AND ti.amount >= tj.amount >= 0.80 * ti.amount){1,6} \
               (last:Transaction)-[:TO]->(a) \
               RETURN [n IN nodes(p) WHERE n:Account | n.a_id] AS ring, \
                      [n IN nodes(p) WHERE n:Transaction | n.amount] AS amounts";
    for cypher in [f03, f04, f05, f06] {
        declines(cypher, &["FROM", "TO"]);
        forced_compiled(cypher, &["FROM", "TO"]);
    }
    let (_, nfa) = forced_compiled(f03, &["FROM", "TO"]);
    // The f03 layout: the account anchor (0), the anchored transaction
    // (1), the split round boundary — open/ti (2) and close/tj (3) — the
    // mid account (4), the post-group `last` on its own exit state (5,
    // entered by epsilon from the close), and the closing account (6).
    assert_eq!(nfa.states, 7);
    assert_eq!((nfa.min_hops, nfa.max_hops), (4, Some(14)));
    assert_eq!(nfa.epsilons, vec![(1, 2), (3, 2), (3, 5)]);
    assert_eq!(nfa.transitions.len(), 4);
    // Guards: first (1), ti (2), the round end tj (3), the mid account
    // (4), last (5); the boundary nodes' admission is the caller's.
    assert_eq!(
        (guard_labels(&nfa, 0), guard_labels(&nfa, 6)),
        (Vec::<String>::new(), Vec::<String>::new()),
        "the boundary nodes' admission is the caller's"
    );
    assert_eq!(guard_labels(&nfa, 1), vec!["Transaction".to_string()]);
    assert_eq!(guard_labels(&nfa, 2), vec!["Transaction".to_string()]);
    assert_eq!(
        guard_labels(&nfa, 3),
        vec!["Transaction".to_string()],
        "the round end carries only the round-end pattern"
    );
    assert_eq!(guard_labels(&nfa, 4), vec!["Account".to_string()]);
    assert_eq!(guard_labels(&nfa, 5), vec!["Transaction".to_string()]);
    // The predicate rides the round-closing FROM hop (transition 2 of
    // FROM, TO, FROM, TO), ti and tj its referenced variables.
    let Some(predicate) = &nfa.predicates[2] else {
        panic!("the fraud WHERE rides the round-closing hop");
    };
    assert_eq!(
        predicate.vars,
        vec![("ti".to_string(), 2), ("tj".to_string(), 3)]
    );
    let others = [0, 1, 3]
        .iter()
        .filter(|transition| nfa.predicates[**transition].is_some())
        .count();
    assert_eq!(others, 0, "no other transition carries a predicate");
}

/// The forced rung's own declines, one shape each: the guard vocabulary
/// is the walker's per-node checks, not the per-path ones.
#[test]
fn forced_guard_vocabulary_declines() {
    // An inline element WHERE on an interior node — per-path semantics.
    forced_declines(
        "MATCH p = (x)-[:ctx]-(m:Person WHERE m.k > 1)-[:knows*1..]-(y) RETURN p LIMIT 100",
        &["ctx", "knows"],
    );
    // A parameterized property-map value — the guard must evaluate
    // without the statement's parameter set.
    forced_declines(
        "MATCH p = (x)((m)-[:knows]-(n {k: $bound})){1,}(y) RETURN p LIMIT 100",
        &["knows"],
    );
    // A zero-round group whose close state would carry a guard: the
    // zero-round pass-through would apply the round-end pattern to a
    // node no round visited.
    forced_declines(
        "MATCH p = (x)((m)-[:knows]-(n:End)){0,2}(y) RETURN p LIMIT 100",
        &["knows"],
    );
    // A predicate referencing a name the body does not bind.
    forced_declines(
        "MATCH p = (x)((m)-[:knows]-(n) WHERE z.k > 1){1,}(y) RETURN p LIMIT 100",
        &["knows"],
    );
}

/// A guarded boundary and a predicate over an unrolling body decline —
/// the var-length inner rel (parser-unreachable inside a group, so the
/// hand-built IR the nested-quantifier pins use) has no guard positions
/// to stitch copies onto. One IR fixture, both facts.
#[test]
fn forced_guards_decline_over_an_unrolling_body() {
    let graph = graph_with_types(&["knows"]);
    let guarded_start = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(labeled_node("Start")),
                PatternElement::Relationship(var_length_rel(&["knows"], 1, Some(2))),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            1,
            None,
        )),
        PatternElement::Node(named_node("y")),
    ]);
    assert!(
        compile(&graph, &guarded_start, PatternEngine::Nfa).is_none(),
        "a guarded round start over a var-length body declines"
    );
    let mut predicated = guarded_start;
    let PatternElement::Quantified(qpp) = &mut predicated.elements[1] else {
        panic!("the fixture's spine");
    };
    qpp.elements[0] = PatternElement::Node(NodePattern::anonymous());
    let prop = |name: &str| {
        Box::new(relate_ir::Expr::Property {
            key_id: Default::default(),
            expr: Box::new(relate_ir::Expr::Variable(name.into())),
            key: "k".into(),
        })
    };
    qpp.where_clause = Some(relate_ir::Expr::Binary {
        op: relate_ir::BinaryOp::Lt,
        left: prop("a"),
        right: prop("b"),
    });
    assert!(
        compile(&graph, &predicated, PatternEngine::Nfa).is_none(),
        "a group predicate over a var-length body declines"
    );
}

/// A labeled node pattern — the IR-constructor vocabulary's labeled
/// form, the hand-built guard fixture.
fn labeled_node(label: &str) -> NodePattern {
    NodePattern {
        labels: vec![(label.into(), Default::default())],
        ..NodePattern::anonymous()
    }
}

/// A dynamic label expression declines even forced — the guard must be
/// checkable without an evaluation context (the re-arrival search's
/// checks carry no row or parameters). Hand-built IR: the dynamic
/// spelling is a runtime label, not a static conjunction.
#[test]
fn forced_dynamic_label_expression_declines() {
    use relate_ir::Expr;
    let graph = graph_with_types(&["knows"]);
    let mut node = NodePattern::anonymous();
    node.label_expr = Some(relate_ir::LabelExpr::Dynamic {
        expr: Box::new(Expr::String("Person".into())),
        quantifier: relate_ir::DynamicLabelQuantifier::Default,
    });
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(node),
                PatternElement::Relationship(one_hop_rel(&["knows"])),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            1,
            None,
        )),
        PatternElement::Node(named_node("y")),
    ]);
    assert!(
        compile(&graph, &pattern, PatternEngine::Nfa).is_none(),
        "a dynamic label expression declines even forced"
    );
}

/// A rolled-back speculative emission leaves no fact behind. A group
/// whose body has a variable hop length takes the unroll path: the
/// cycle probe is emitted, rolled back, and the body unrolled instead.
/// The probe's interior guard must vanish with the probe — its state
/// counter was rewound, so a surviving guard would scatter onto a state
/// a later emission reuses, or index past the kept state count at
/// `finish` (the builder-restore bug this pins).
#[test]
fn rolled_back_probe_leaves_no_stale_guard() {
    let guarded = NodePattern {
        labels: vec![("User".into(), Default::default())],
        ..named_node("u1")
    };
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(NodePattern::anonymous()),
                PatternElement::Relationship(var_length_rel(&["knows"], 1, Some(2))),
                PatternElement::Node(guarded),
                PatternElement::Relationship(one_hop_rel(&["knows"])),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            1,
            Some(2),
        )),
        PatternElement::Node(named_node("y")),
    ]);
    let graph = graph_with_types(&["knows"]);
    let nfa = compile(&graph, &pattern, PatternEngine::Nfa)
        .expect("the forced rung unrolls a variable-length guarded body");
    // Every guard lands on a state the kept emission allocated.
    for (state, guards) in nfa.state_guards.iter().enumerate() {
        assert!(
            guards.is_empty() || (state as u32) < nfa.states,
            "guard scattered onto state {state} of {}",
            nfa.states
        );
    }
    // One guard per kept copy ({1,2} = one mandatory + one optional
    // round): a surviving probe guard would make this 3.
    let guards: usize = nfa.state_guards.iter().map(Vec::len).sum();
    assert_eq!(
        guards, 2,
        "the rolled-back probe's guard must not survive the unroll"
    );
}

/// The post-group node's guard sits on its own exit state, past the
/// round boundary: the close state every round ends at carries no guard
/// (so an intermediate round's end is never checked against the
/// post-group pattern), and the exit is entered only by epsilon from the
/// close — checked once, when the group is finished. Both spine forms:
/// the fixed-length cycle, and the unrolled variable-length body.
#[test]
fn forced_guarded_post_group_node_takes_its_own_exit_state() {
    // The cycle form: `(s)` at 0, the self-loop round on 0 (no boundary
    // split — the body is plain), `(c:V)` on the exit state 1, `(d)` at 2.
    let (_, nfa) = forced_compiled(
        "MATCH (s)((a)-[:R]->(b)){2}(c:V)-[:S]->(d) RETURN d",
        &["R", "S"],
    );
    assert_eq!(nfa.states, 3);
    assert_eq!((nfa.min_hops, nfa.max_hops), (3, Some(3)));
    assert_eq!((nfa.transitions[0].from, nfa.transitions[0].to), (0, 0));
    assert_eq!(nfa.epsilons, vec![(0, 1)]);
    assert_eq!(guard_labels(&nfa, 0), Vec::<String>::new());
    assert_eq!(guard_labels(&nfa, 1), vec!["V".to_string()]);
    assert!(
        nfa.transitions.iter().all(|t| t.to != 1),
        "the exit state is reached by the close's epsilon only"
    );

    // The unroll form: a variable-length round (hand-built IR — the
    // nested var-length rel), the post-group guard on a state no rel
    // lands on, entered by epsilon from the last copy's end.
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("s")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(NodePattern::anonymous()),
                PatternElement::Relationship(var_length_rel(&["R"], 1, Some(2))),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            2,
            Some(2),
        )),
        PatternElement::Node(labeled_node("V")),
        PatternElement::Relationship(one_hop_rel(&["S"])),
        PatternElement::Node(named_node("d")),
    ]);
    let graph = graph_with_types(&["R", "S"]);
    let nfa = compile(&graph, &pattern, PatternEngine::Nfa)
        .expect("the forced rung unrolls a variable-length body");
    let guarded: Vec<u32> = (0..nfa.states)
        .filter(|state| !nfa.state_guards[*state as usize].is_empty())
        .collect();
    assert_eq!(guarded.len(), 1, "one guard: the post-group node's");
    let exit = guarded[0];
    assert_eq!(guard_labels(&nfa, exit), vec!["V".to_string()]);
    assert!(
        nfa.transitions.iter().all(|t| t.to != exit),
        "no round's hop lands on the exit state"
    );
    assert_eq!(
        nfa.epsilons.iter().filter(|(_, to)| *to == exit).count(),
        1,
        "the exit state is entered by one epsilon from the group's end"
    );
}

/// An interior node whose only admission fact is a non-wildcard label
/// expression over an empty label list — `Not` or `Or` — carries that
/// fact as its guard: it must never compile as a plain (unguarded) node.
/// The wildcard matches every node and stays plain.
#[test]
fn forced_label_expression_without_labels_compiles_as_a_guard() {
    use relate_ir::LabelExpr;
    let graph = graph_with_types(&["ctx", "knows"]);
    let with_expr = |expr: LabelExpr| {
        let mut node = NodePattern::anonymous();
        node.label_expr = Some(expr);
        path_pattern(vec![
            PatternElement::Node(named_node("x")),
            PatternElement::Relationship(one_hop_rel(&["ctx"])),
            PatternElement::Node(node),
            PatternElement::Quantified(quantified(
                vec![
                    PatternElement::Node(NodePattern::anonymous()),
                    PatternElement::Relationship(one_hop_rel(&["knows"])),
                    PatternElement::Node(NodePattern::anonymous()),
                ],
                1,
                None,
            )),
            PatternElement::Node(named_node("y")),
        ])
    };
    let label = |name: &str| LabelExpr::Label(name.into());
    for expr in [
        LabelExpr::Not(Box::new(label("Bad"))),
        LabelExpr::Or(vec![label("A"), label("B")]),
    ] {
        let nfa = compile(&graph, &with_expr(expr.clone()), PatternEngine::Nfa)
            .expect("a static label expression compiles forced");
        // The context node sits at state 1, the first rel's target.
        assert_eq!(
            nfa.state_guards[1].len(),
            1,
            "{expr:?} must compile to the context node's guard, not to a plain node"
        );
        assert_eq!(nfa.state_guards[1][0].label_expr, Some(expr));
    }
    let nfa = compile(&graph, &with_expr(LabelExpr::Wildcard), PatternEngine::Nfa)
        .expect("the wildcard compiles forced");
    assert!(
        nfa.state_guards.iter().all(Vec::is_empty),
        "the wildcard admits every node — no guard"
    );
}

/// A group WHERE referencing a name two body positions bind declines:
/// the name's last binding is no one state's arrival.
#[test]
fn forced_group_where_over_a_twice_bound_name_declines() {
    let graph = graph_with_types(&["knows"]);
    let prop = |name: &str| {
        Box::new(relate_ir::Expr::Property {
            key_id: Default::default(),
            expr: Box::new(relate_ir::Expr::Variable(name.into())),
            key: "k".into(),
        })
    };
    let mut group = quantified(
        vec![
            PatternElement::Node(named_node("a")),
            PatternElement::Relationship(one_hop_rel(&["knows"])),
            PatternElement::Node(named_node("b")),
            PatternElement::Relationship(one_hop_rel(&["knows"])),
            PatternElement::Node(named_node("a")),
        ],
        1,
        None,
    );
    group.where_clause = Some(relate_ir::Expr::Binary {
        op: relate_ir::BinaryOp::Lt,
        left: prop("a"),
        right: prop("b"),
    });
    let mut pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(group),
        PatternElement::Node(named_node("y")),
    ]);
    assert!(
        compile(&graph, &pattern, PatternEngine::Nfa).is_none(),
        "a twice-bound referenced name declines"
    );
    // The same body with distinct names compiles — the decline is the
    // twice-bound name's, not the shape's.
    let PatternElement::Quantified(group) = &mut pattern.elements[1] else {
        panic!("the fixture's spine");
    };
    group.elements[4] = PatternElement::Node(named_node("c"));
    assert!(compile(&graph, &pattern, PatternEngine::Nfa).is_some());
}

/// An empty quantified body (hand-built IR — the parser never produces
/// one) declines under both engines: the forced rung's boundary-guard
/// read takes the body's last index without underflowing, and the
/// compile's contract for an out-of-vocabulary shape is `None`.
#[test]
fn empty_quantified_body_declines_under_both_engines() {
    let graph = graph_with_types(&["knows"]);
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(Vec::new(), 1, None)),
        PatternElement::Node(named_node("y")),
    ]);
    for engine in [PatternEngine::Auto, PatternEngine::Nfa] {
        assert!(
            compile(&graph, &pattern, engine).is_none(),
            "an empty quantified body declines under {engine:?}"
        );
    }
}
