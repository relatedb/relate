//! The frontier-NFA compiler's unit suite: parity with the research
//! prototype's two constructor families (expected shapes pinned from
//! the retired `graph/qpp.rs` — ADR-0476 quarantined it, ADR-0478
//! replaced it with this kernel; its `Pattern` fields were private to
//! that module, so the literals were read from its constructors and
//! stay pinned here as the independent record of its shapes), the
//! concatenation
//! context, per-transition direction tags, nested-quantifier compilation
//! (hand-built IR: the parser's `NestedQuantification` rule rejects the
//! spelling, and the IR type system is the compiler's contract), and
//! every decline the vocabulary defines.
//!
//! The pattern-lowering and IR-constructor helpers live in the shared
//! test module (`exec/test_support.rs`): every frontier suite reuses
//! them — the one canonical spelling of "lower this Cypher to patterns"
//! and "build this IR shape".

use std::collections::BTreeMap;

use relate_ir::{Direction, NodePattern, PathPattern, PatternElement, RelPattern};

use super::nfa::{compile, FrontierNfa, STATE_CEILING};
use crate::exec::test_support::{
    named_node, one_hop_rel, path_pattern, patterns, quantified, var_length_rel,
};
use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;

/// A graph with every rel type in `names` interned (one live rel per
/// name) — the compiler interns through the store, so the test store
/// must carry the types the query names.
fn graph_with_types(names: &[&str]) -> Graph {
    let graph = Graph::default();
    graph.begin_write().unwrap();
    let a = graph
        .create_node(vec!["N".into()], BTreeMap::new())
        .unwrap();
    let b = graph
        .create_node(vec!["N".into()], BTreeMap::new())
        .unwrap();
    for name in names {
        graph
            .create_rel(a, b, (*name).into(), BTreeMap::new())
            .unwrap();
    }
    graph.commit_write();
    graph
}

/// Compile `cypher`'s first pattern against a store interning
/// `names`, with the store kept for the expected interned ids.
fn compiled(cypher: &str, names: &[&str]) -> (Graph, FrontierNfa) {
    let graph = graph_with_types(names);
    let pattern = &patterns(cypher)[0];
    let nfa = compile(&graph, pattern, PatternEngine::Auto)
        .unwrap_or_else(|| panic!("{cypher} should compile"));
    (graph, nfa)
}

/// Compile `cypher`'s first pattern and assert the decline.
fn declines(cypher: &str, names: &[&str]) {
    let graph = graph_with_types(names);
    let pattern = &patterns(cypher)[0];
    assert!(
        compile(&graph, pattern, PatternEngine::Auto).is_none(),
        "{cypher} should decline"
    );
}

/// q9 `knows+` (the coverage table's bare flat class) and the flat
/// family's bounds variants: the hops family — one state, one
/// accepting self-loop over the interned union, bounds `{min, max}`.
/// Expected shapes pinned from the prototype's `Pattern::hops`
/// (the retired `graph/qpp.rs`, ADR-0476/0478): `transitions =
/// [{from: 0, to: 0, types, accepts: true}], start 0, states 1,
/// min_hops = min, max_hops = max`. The prototype's `Pattern`
/// fields were private to its module (no accessors; the research
/// harness drove it through `reach`), so the literals were read from
/// its constructors and stay pinned here.
#[test]
fn hops_family_parity() {
    let (graph, nfa) = compiled(
        "MATCH p = (x:Person)-[:knows*1..]-(y) WHERE x.ID = 1 RETURN p LIMIT 100",
        &["knows"],
    );
    assert_eq!((nfa.states, nfa.start), (1, 0));
    assert_eq!((nfa.min_hops, nfa.max_hops), (1, None));
    assert_eq!(nfa.accepting, vec![true]);
    assert!(nfa.epsilons.is_empty());
    assert_eq!(nfa.transitions.len(), 1);
    let t = &nfa.transitions[0];
    assert_eq!((t.from, t.to), (0, 0));
    assert_eq!(t.types, vec![graph.rel_type_id("knows").unwrap()]);
    assert_eq!(t.direction, Direction::Both);
    assert!(t.accepts);

    // q12 `knows*`: the zero-length arm — bounds {0, unbounded}.
    let (_, nfa) = compiled(
        "MATCH p = (x:Person)-[:knows*0..]-(y) RETURN p LIMIT 100",
        &["knows"],
    );
    assert_eq!((nfa.min_hops, nfa.max_hops), (0, None));

    // A bounded flat quantifier: bounds {2, 5} on the same one-state
    // loop — `Pattern::hops(types, 2, Some(5))`.
    let (_, nfa) = compiled(
        "MATCH p = (x:Person)-[:knows*2..5]-(y) RETURN p LIMIT 100",
        &["knows"],
    );
    assert_eq!(nfa.states, 1);
    assert_eq!((nfa.min_hops, nfa.max_hops), (2, Some(5)));

    // q47 `(hasCreator|isLocatedIn)+`: the per-hop union rides one
    // transition's type list — never an alternation arm.
    let (graph, nfa) = compiled(
        "MATCH p = (x:Comment)-[:commentHasCreator|postHasCreator|commentIsLocatedIn|\
         organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn*1..]-(y) \
         WHERE x.ID = 1 RETURN p LIMIT 100",
        &[
            "commentHasCreator",
            "postHasCreator",
            "commentIsLocatedIn",
            "organisationIsLocatedIn",
            "personIsLocatedIn",
            "postIsLocatedIn",
        ],
    );
    assert_eq!(nfa.states, 1);
    assert_eq!(nfa.transitions.len(), 1);
    assert_eq!(
        nfa.transitions[0].types,
        [
            "commentHasCreator",
            "postHasCreator",
            "commentIsLocatedIn",
            "organisationIsLocatedIn",
            "personIsLocatedIn",
            "postIsLocatedIn"
        ]
        .iter()
        .map(|name| graph.rel_type_id(name).unwrap())
        .collect::<Vec<_>>()
    );
}

/// q37 `(likes.hasCreator)+` (the coverage table's two-step group):
/// the rounds family — pinned from the prototype's `Pattern::rounds`:
/// `states = steps.len()`, start 0, step *i* the transition
/// `i -> (i+1 == len ? 0 : i+1)` with `accepts = (to == 0)`, hop
/// bounds `rounds x steps.len()`.
#[test]
fn rounds_family_parity() {
    let (graph, nfa) = compiled(
        "MATCH p = (x:Person)((a)-[:likeComment|likePost]-(b)\
         -[:commentHasCreator|postHasCreator]-(c)){1,}(y) WHERE x.ID = 1 RETURN p LIMIT 100",
        &[
            "likeComment",
            "likePost",
            "commentHasCreator",
            "postHasCreator",
        ],
    );
    assert_eq!((nfa.states, nfa.start), (2, 0));
    assert_eq!((nfa.min_hops, nfa.max_hops), (2, None));
    assert_eq!(nfa.accepting, vec![true, false]);
    assert!(nfa.epsilons.is_empty());
    assert_eq!(nfa.transitions.len(), 2);
    let (likes, has_creator) = (
        [
            graph.rel_type_id("likeComment").unwrap(),
            graph.rel_type_id("likePost").unwrap(),
        ],
        [
            graph.rel_type_id("commentHasCreator").unwrap(),
            graph.rel_type_id("postHasCreator").unwrap(),
        ],
    );
    assert_eq!((nfa.transitions[0].from, nfa.transitions[0].to), (0, 1));
    assert_eq!(nfa.transitions[0].types, likes);
    assert!(!nfa.transitions[0].accepts);
    assert_eq!((nfa.transitions[1].from, nfa.transitions[1].to), (1, 0));
    assert_eq!(nfa.transitions[1].types, has_creator);
    assert!(nfa.transitions[1].accepts);

    // A bounded rounds quantifier: the same two-state cycle, bounds
    // `rounds x len` — `Pattern::rounds(steps, 2, Some(4))`.
    let (_, nfa) = compiled(
        "MATCH p = (x:Person)((a)-[:likeComment]-(b)-[:postHasCreator]-(c)){2,4}(y) \
         RETURN p LIMIT 100",
        &["likeComment", "postHasCreator"],
    );
    assert_eq!(nfa.states, 2);
    assert_eq!((nfa.min_hops, nfa.max_hops), (4, Some(8)));
    assert!(nfa.epsilons.is_empty());
}

/// The concatenation context (the scan+segment classes): plain
/// sibling rels around the quantified element compile to transitions
/// in the same chain, the bounds offset by the fixed context hops.
/// The prototype has no constructor for these — the expected shapes
/// are the design's Thompson concatenation of its two families.
#[test]
fn concatenation_context_compiles() {
    // q67 `(knows+).studyAt` — the trailing context hop: the flat
    // cycle at state 0, the trailing hop 0 -> 1, accepting state 1,
    // bounds {1 round x 1 hop + 1 context, unbounded}.
    let (_, nfa) = compiled(
        "MATCH p = (x:Person)-[:knows*1..]-()-[:studyAt]-(y) WHERE x.ID = 1 \
         RETURN p LIMIT 100",
        &["knows", "studyAt"],
    );
    assert_eq!(nfa.states, 2);
    assert_eq!(nfa.accepting, vec![false, true]);
    assert_eq!((nfa.min_hops, nfa.max_hops), (2, None));
    assert_eq!(nfa.transitions.len(), 2);
    assert_eq!((nfa.transitions[0].from, nfa.transitions[0].to), (0, 0));
    assert!(!nfa.transitions[0].accepts);
    assert_eq!((nfa.transitions[1].from, nfa.transitions[1].to), (0, 1));
    assert!(nfa.transitions[1].accepts);

    // q86 `knows.(knows+)` — the leading context hop: 0 -> 1 over
    // knows, then the accepting self-loop at 1, bounds {1 + 1}.
    let (_, nfa) = compiled(
        "MATCH p = (x:Person)-[:knows]-()-[:knows*1..]-(y) WHERE x.ID = 1 \
         RETURN p LIMIT 100",
        &["knows"],
    );
    assert_eq!(nfa.states, 2);
    assert_eq!(nfa.accepting, vec![false, true]);
    assert_eq!((nfa.min_hops, nfa.max_hops), (2, None));
    assert_eq!(nfa.transitions.len(), 2);
    assert_eq!((nfa.transitions[0].from, nfa.transitions[0].to), (0, 1));
    assert!(nfa.transitions[0].accepts);
    assert_eq!((nfa.transitions[1].from, nfa.transitions[1].to), (1, 1));
    assert!(nfa.transitions[1].accepts);
}

/// Direction rides per transition, never globally: an outgoing
/// var-length hop, an incoming one, and a mixed chain each keep
/// their own tags.
#[test]
fn direction_tags_per_transition() {
    let (_, nfa) = compiled(
        "MATCH p = (x:Person)-[:knows*1..]->(y) RETURN p LIMIT 100",
        &["knows"],
    );
    assert_eq!(nfa.transitions[0].direction, Direction::Outgoing);

    let (_, nfa) = compiled(
        "MATCH p = (x:Person)<-[:knows*1..]-(y) RETURN p LIMIT 100",
        &["knows"],
    );
    assert_eq!(nfa.transitions[0].direction, Direction::Incoming);

    // q67's trailing hop directed against the undirected cycle.
    let (_, nfa) = compiled(
        "MATCH p = (x:Person)-[:knows*1..]-()-[:studyAt]->(y) RETURN p LIMIT 100",
        &["knows", "studyAt"],
    );
    assert_eq!(nfa.transitions[0].direction, Direction::Both);
    assert_eq!(nfa.transitions[1].direction, Direction::Outgoing);
}

fn compiled_ir(pattern: &PathPattern, names: &[&str]) -> FrontierNfa {
    let graph = graph_with_types(names);
    compile(&graph, pattern, PatternEngine::Auto)
        .unwrap_or_else(|| panic!("{pattern:?} should compile"))
}

fn declines_ir(pattern: &PathPattern, names: &[&str]) {
    let graph = graph_with_types(names);
    assert!(
        compile(&graph, pattern, PatternEngine::Auto).is_none(),
        "{pattern:?} should decline"
    );
}

/// A nested exact quantifier (`*2..2`) makes the round fixed-length,
/// so the outer unbounded group takes the cycle form — exactly the
/// rounds family over the unrolled inner hops, the prototype's
/// `Pattern::rounds` over two knows steps.
#[test]
fn nested_exact_quantifier_is_the_rounds_family() {
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(NodePattern::anonymous()),
                PatternElement::Relationship(var_length_rel(&["knows"], 2, Some(2))),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            1,
            None,
        )),
        PatternElement::Node(named_node("y")),
    ]);
    let nfa = compiled_ir(&pattern, &["knows"]);
    assert_eq!((nfa.states, nfa.start), (2, 0));
    assert_eq!((nfa.min_hops, nfa.max_hops), (2, None));
    assert_eq!(nfa.accepting, vec![true, false]);
    assert!(nfa.epsilons.is_empty());
    assert_eq!(nfa.transitions.len(), 2);
    assert_eq!((nfa.transitions[0].from, nfa.transitions[0].to), (0, 1));
    assert_eq!((nfa.transitions[1].from, nfa.transitions[1].to), (1, 0));
    assert!(nfa.transitions[1].accepts && !nfa.transitions[0].accepts);
}

/// A quantified group containing an inner bounded quantifier with an
/// optional tail (`*2..3` inside `{2,4}`): the inner unrolls
/// structurally (2 mandatory hops + 1 epsilon-skipped optional), the
/// variable round length makes the outer unroll structurally too (2
/// mandatory rounds + 2 optional) — 13 states, every hop one knows
/// transition, bounds {2 rounds x 2, 4 x 3}. Both nested spellings —
/// the var-length rel the front end produces and the nested
/// `Quantified` element — normalize to the same NFA.
#[test]
fn nested_bounded_quantifier_unrolls() {
    let as_var_length = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(NodePattern::anonymous()),
                PatternElement::Relationship(var_length_rel(&["knows"], 2, Some(3))),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            2,
            Some(4),
        )),
        PatternElement::Node(named_node("y")),
    ]);
    let as_nested_group = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(quantified(
            vec![
                PatternElement::Node(NodePattern::anonymous()),
                PatternElement::Quantified(quantified(
                    vec![
                        PatternElement::Node(NodePattern::anonymous()),
                        PatternElement::Relationship(one_hop_rel(&["knows"])),
                        PatternElement::Node(NodePattern::anonymous()),
                    ],
                    2,
                    Some(3),
                )),
                PatternElement::Node(NodePattern::anonymous()),
            ],
            2,
            Some(4),
        )),
        PatternElement::Node(named_node("y")),
    ]);
    let a = compiled_ir(&as_var_length, &["knows"]);
    let b = compiled_ir(&as_nested_group, &["knows"]);
    assert!(a.states <= STATE_CEILING);
    assert_eq!(a.states, 13);
    assert_eq!((a.min_hops, a.max_hops), (4, Some(12)));
    // 4 outer copies x 3 inner transitions (2 mandatory + 1 optional).
    assert_eq!(a.transitions.len(), 12);
    // 2 outer optional skips + 4 inner optional skips.
    assert_eq!(a.epsilons.len(), 6);
    assert!(a.transitions.iter().all(|t| t.types.len() == 1));
    // The final state accepts, and the states an epsilon skip can
    // complete from accept too.
    assert!(a.accepting[12]);
    assert!(a.accepting.iter().filter(|accept| **accept).count() >= 2);
    // The two spellings normalize to the same NFA: same transitions
    // (from, to, types, accepts), same epsilon edges, same bounds.
    let shape = |nfa: &FrontierNfa| {
        nfa.transitions
            .iter()
            .map(|t| (t.from, t.to, t.types.clone(), t.accepts))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(a.epsilons, b.epsilons);
    assert_eq!((b.states, b.min_hops, b.max_hops), (13, 4, Some(12)));
}

/// The unsound nested compositions decline: a nested unbounded loop
/// (the global interval pins totals, not each instance's count), and
/// an unbounded outer over variable-length rounds (one short round
/// compensated by a long one would pass the interval).
#[test]
fn declines_unsound_nested_quantifiers() {
    let outer = |inner: RelPattern, min: u32, max: Option<u32>| {
        path_pattern(vec![
            PatternElement::Node(named_node("x")),
            PatternElement::Quantified(quantified(
                vec![
                    PatternElement::Node(NodePattern::anonymous()),
                    PatternElement::Relationship(inner),
                    PatternElement::Node(NodePattern::anonymous()),
                ],
                min,
                max,
            )),
            PatternElement::Node(named_node("y")),
        ])
    };
    // Nested unbounded, under a bounded or an unbounded outer.
    declines_ir(
        &outer(var_length_rel(&["knows"], 1, None), 2, Some(4)),
        &["knows"],
    );
    declines_ir(
        &outer(var_length_rel(&["knows"], 0, None), 1, None),
        &["knows"],
    );
    // Unbounded outer over variable rounds.
    declines_ir(
        &outer(var_length_rel(&["knows"], 2, Some(3)), 1, None),
        &["knows"],
    );
}

/// The state ceiling is a decline, not the prototype's assert: a
/// 64-hop group round compiles exactly 64 states, a 65-hop one is
/// above the 64-state bitmask ceiling.
#[test]
fn declines_above_the_state_ceiling() {
    let group = |hops: usize| {
        let mut body = String::from("((a)");
        for _ in 0..hops {
            body.push_str("-[:knows]-()");
        }
        format!("MATCH p = (x){body}){{1,}}(y) RETURN p LIMIT 100")
    };
    let (_, nfa) = compiled(&group(64), &["knows"]);
    assert_eq!(nfa.states, STATE_CEILING);
    assert_eq!((nfa.min_hops, nfa.max_hops), (64, None));
    declines(&group(65), &["knows"]);
}

/// A predicate-bearing inner rel declines — the NFA has no surface
/// that could check it, so accepting the shape would silently drop
/// the predicate.
#[test]
fn declines_predicate_bearing_inner_rel() {
    declines(
        "MATCH p = (x)((a)-[:knows {since: 2010}]-(b)){1,}(y) RETURN p LIMIT 100",
        &["knows"],
    );
    declines(
        "MATCH p = (x)((a)-[r:knows WHERE r.since < 2010]-(b)){1,}(y) RETURN p LIMIT 100",
        &["knows"],
    );
}

/// A path selector (the shortest machinery's contract) declines.
#[test]
fn declines_selector() {
    declines(
        "MATCH p = ANY SHORTEST (x:Person)-[:knows*1..]-(y) RETURN p LIMIT 100",
        &["knows"],
    );
}

/// A subpath variable on the quantified group declines. The front now
/// rejects a named quantified path pattern (42N34), so this shape reaches the
/// compiler only from hand-built IR — pinned here like the nested-quantifier
/// shapes above.
#[test]
fn declines_subpath_variable() {
    let graph = graph_with_types(&["knows"]);
    let mut qpp = quantified(
        vec![
            PatternElement::Node(named_node("a")),
            PatternElement::Relationship(one_hop_rel(&["knows"])),
            PatternElement::Node(named_node("b")),
        ],
        1,
        None,
    );
    qpp.subpath_variable = Some("r".into());
    let pattern = path_pattern(vec![
        PatternElement::Node(named_node("x")),
        PatternElement::Quantified(qpp),
        PatternElement::Node(named_node("y")),
    ]);
    assert!(
        compile(&graph, &pattern, PatternEngine::Auto).is_none(),
        "a named quantified group should decline"
    );
}

/// An empty type list (the any-type `[]` hop) compiles: the transition
/// carries no interned id, and `arms` resolves that empty list to
/// `RelTypes::Any` — the store's own "every type matches". The union the
/// old decline worried about never has to be enumerated (LABS-1434).
#[test]
fn compiles_empty_type_list_as_the_any_type_set() {
    let (graph, nfa) = compiled(
        "MATCH p = (x:Person)-[*1..]-(y) RETURN p LIMIT 100",
        &["knows"],
    );
    // The type list is the pattern's, never the store's: `knows` is
    // interned below, and the untyped hop still names nothing, so no
    // transition carries an id. A regression that resolved the untyped
    // hop against the store's vocabulary would show here as a non-empty
    // list rather than as a wrong answer.
    assert!(!nfa.transitions.is_empty(), "the unrolled body emits hops");
    for t in &nfa.transitions {
        assert!(
            t.types.is_empty(),
            "the untyped hop carries no interned id, got {:?}",
            t.types
        );
    }
    // The assertion above is about the hop, not about an empty store.
    assert!(graph.rel_type_id("knows").is_some());
}

/// The wider out-of-vocabulary declines, one shape each.
#[test]
fn declines_out_of_vocabulary_shapes() {
    // A second quantified element: a second cycle the one hop
    // interval cannot pin.
    declines(
        "MATCH p = (x:Person)-[:knows*1..]-(m)-[:studyAt*1..]-(y) RETURN p LIMIT 100",
        &["knows", "studyAt"],
    );
    // The group's own WHERE clause (per-iteration semantics).
    declines(
        "MATCH p = (x)((a)-[:knows]-(b) WHERE a.k > 1){1,}(y) RETURN p LIMIT 100",
        &["knows"],
    );
    // An admission-bearing inner node — no node-admission surface.
    declines(
        "MATCH p = (x)((a:Person)-[:knows]-(b)){1,}(y) RETURN p LIMIT 100",
        &["knows"],
    );
    // An admission-bearing interior context node.
    declines(
        "MATCH p = (x:Person)-[:knows]-(m:Person)-[:studyAt*1..]-(y) RETURN p LIMIT 100",
        &["knows", "studyAt"],
    );
    // A type name the store never interned.
    declines(
        "MATCH p = (x:Person)-[:knows*1..]-(y) RETURN p LIMIT 100",
        &["studyAt"],
    );
    // A pattern-level WHERE — per-path semantics the NFA cannot carry (the
    // predicate reads a group-local variable; reading the enclosing path
    // variable would be the front's 42I21).
    declines(
        "MATCH p = ((x:Person)-[:knows*1..]-(y) WHERE x.ID > 1) RETURN p LIMIT 100",
        &["knows"],
    );
}

// The forced rung's gap classes: their own suite, beside this one.
mod forced;
