//! The matcher's own pins, as the kernels' sibling convention: the
//! compiled-contract proofs the recognizer's suite pins, one file
//! beside the module it tests.

use crate::row::Row;
use relate_front::parse_and_lower;
use relate_ir::{Clause, Expr, MatchMode, PathPattern};
use relate_names::SmallName;

use super::matcher::*;
use super::program::{NodeTarget, Prog};

/// The planned `MATCH` of `cypher`: its patterns (the planner orients
/// and orders them the way the executor sees them) and its flattened
/// `WHERE` conjuncts — the two inputs `compile` reads.
fn planned(cypher: &str) -> (Vec<PathPattern>, Vec<Expr>) {
    let query = parse_and_lower(cypher).unwrap();
    let plan = relate_planner::plan(query, &mut |_| false).unwrap();
    match &plan.parts[0][0] {
        Clause::Match {
            patterns,
            where_clause: Some(w),
            ..
        } => (
            patterns.clone(),
            crate::exec::pushdown::flatten_and_owned(w),
        ),
        Clause::Match { patterns, .. } => (patterns.clone(), Vec::new()),
        other => panic!("not a MATCH: {other:?}"),
    }
}

fn family_of(cypher: &str) -> Option<&'static str> {
    let (patterns, conjuncts) = planned(cypher);
    match compile(
        &patterns,
        MatchMode::DifferentRelationships,
        &Row::new(),
        &conjuncts,
    ) {
        Some(Contract::Chain(_)) => Some("chain"),
        Some(Contract::HubStar(contract)) => {
            if contract.shared_terminal {
                Some("hub-star (shared terminal)")
            } else {
                Some("hub-star")
            }
        }
        None => None,
    }
}

fn matched(cypher: &str, want: &str) {
    let got = family_of(cypher).unwrap_or_else(|| panic!("{cypher} should match a family"));
    assert_eq!(got, want, "{cypher}");
}

fn declined(cypher: &str) {
    assert!(family_of(cypher).is_none(), "{cypher} should decline");
}

// Acceptance 1: the matched plan shapes, one per family arm.
#[test]
fn matched_plan_shapes() {
    // (a) The unrolled chain: 2, 3, and 4 typed hops, predicates at
    // the ends and the middle.
    matched(
        "MATCH (a:Person)-[:knows]->(b:Person)-[:studyAt]->(o:Organisation) RETURN a",
        "chain",
    );
    matched(
        "MATCH (a)-[:x]->(b)-[:y]->(c)-[:z]->(d) WHERE b.k > 1 RETURN a",
        "chain",
    );
    // The planner folds `WHERE o.name = 'MIT'` into o's property map;
    // a single-entry map is exactly an equality program.
    matched(
        "MATCH (a:Person)-[:knows]->(b)-[:studyAt]->(o:Organisation) \
         WHERE o.name = 'MIT' RETURN a",
        "chain",
    );
    // (b) The open hub-star (q19/q01/q26): two one-hop comma patterns
    // sharing a hub, endpoint predicates.
    matched(
        "MATCH (c:Comment)-[:hasTag]->(t:Tag), (c)-[:locatedIn]->(l:Place) \
         WHERE t.name CONTAINS 'x' RETURN l",
        "hub-star",
    );
    // (b) The bounded cycle (q30): a shared terminal.
    matched(
        "MATCH (c:Comment)-[:hasCreator]->(p:Person), \
         (c)-[:replyOf]->(post:Post)-[:postHasCreator]->(p) RETURN c",
        "hub-star (shared terminal)",
    );
    // A 2-hop side against a 1-hop side, sharing the terminal.
    matched(
        "MATCH (c)-[:a]->(t:Tag), (c)-[:b]->(mid:Post)-[:d]->(t) RETURN c",
        "hub-star (shared terminal)",
    );
    // The hub at the last end of one pattern (mixed orientation).
    matched(
        "MATCH (c:Comment)-[:hasTag]->(t:Tag), (l:Place)-[:locatedIn]->(c) RETURN c",
        "hub-star",
    );
    // A fully anonymous chain is the family (stellar B11): no element
    // names anything, so the breaker's projection is empty — one empty
    // row per complete binding. Both spellings; the labeled one is the
    // shape the cost gate engages (a bare start is unmeasurable).
    matched("MATCH ()-[:x]->()-[:y]->() RETURN 1", "chain");
    matched("MATCH (:X)-[:x]->(:Y)-[:y]->(:Z) RETURN 1", "chain");
}

// Acceptance 1: the unmatched plan shapes — every structural proof's
// decline, each keeping the per-row walker.
#[test]
fn unmatched_plan_shapes() {
    // Scan segment only: the batch chain's family, not a traversal.
    declined("MATCH (c:Comment) WHERE c.k > 1 RETURN c");
    // One hop: below the chain's 2-hop floor.
    declined("MATCH (a)-[:x]->(b) RETURN a");
    // Five hops: past the unroll bound.
    declined("MATCH (a)-[:x]->(b)-[:y]->(c)-[:z]->(d)-[:w]->(e)-[:v]->(f) RETURN a");
    // Var-length and quantified hops keep the recursive walker.
    declined("MATCH (a)-[:x*2]->(b)-[:y]->(c) RETURN a");
    declined("MATCH (a)((n)-[:x]->(m)){1,2}(b) RETURN a");
    // A named path keeps the walker's path binding.
    declined("MATCH p = (a)-[:x]->(b)-[:y]->(c) RETURN p");
    // A two-entry property map is two predicates on one variable.
    declined("MATCH (a:Person {ID: 7, k: 1})-[:x]->(b)-[:y]->(c) RETURN a");
    // A rel property map keeps `rel_edge_ok`.
    declined("MATCH (a)-[:x {k: 1}]->(b)-[:y]->(c) RETURN a");
    // A repeated node variable is the walker's reconciliation.
    declined("MATCH (a)-[:x]->(a)-[:y]->(c) RETURN a");
    // A repeated rel variable likewise.
    declined("MATCH (a)-[r:x]->(b)-[r:y]->(c) RETURN a");
    // A dynamic label expression keeps the walker's label evaluation.
    declined("MATCH (a:Person|Place)-[:x]->(b)-[:y]->(c) RETURN a");
    // Predicates outside the closed vocabulary: a function over the
    // row, two predicates on one variable, a predicate on a variable
    // the patterns do not bind.
    declined("MATCH (a)-[:x]->(b)-[:y]->(c) WHERE size(b.k) > 1 RETURN a");
    declined("MATCH (a)-[:x]->(b)-[:y]->(c) WHERE b.k > 1 AND b.j < 2 RETURN a");
    // Hub-star declines: three patterns; no shared variable; a 3-hop
    // side; a rel variable shared across the pair. (A 2-hop side with
    // a mid node IS the family — the matched-shape suite covers it.)
    declined("MATCH (c)-[:a]->(t), (c)-[:b]->(l), (c)-[:d]->(m) RETURN c");
    declined("MATCH (c)-[:a]->(t), (d)-[:b]->(l) RETURN c");
    declined("MATCH (a:Person)-[:x]->(b)-[:y]->(c)-[:z]->(d), (a)-[:w]->(e) RETURN a");
    declined("MATCH (c)-[r:a]->(t), (c)-[r:b]->(l) RETURN c");
    // A one-node pattern is not a hub-star side (stellar B10): zero
    // hops, no side to expand — the ceiling alone admitted it, and the
    // cost gate's terminal estimate reads a side's hops. Both
    // orderings, and the both-sides-one-node shape.
    declined("MATCH (a), (a)-[:x]->(b) RETURN a");
    declined("MATCH (a)-[:x]->(b), (a) RETURN a");
    declined("MATCH (a), (a) RETURN a");
    // A fully anonymous pair shares no hub variable (stellar B11's
    // pair arm): the hub-star is defined by one.
    declined("MATCH ()-[:a]->(), ()-[:b]->() RETURN 1");
}

// The row and mode gates: a carrying row and REPEATABLE ELEMENTS
// decline even a perfect family shape.
#[test]
fn row_and_mode_gates_decline() {
    let (patterns, conjuncts) = planned("MATCH (a)-[:x]->(b)-[:y]->(c) RETURN a");
    let carrying = Row::from_iter(vec![("outer".to_string(), relate_core::Value::Null)]);
    assert!(compile(
        &patterns,
        MatchMode::DifferentRelationships,
        &carrying,
        &conjuncts
    )
    .is_none());
    assert!(compile(
        &patterns,
        MatchMode::RepeatableElements,
        &Row::new(),
        &conjuncts
    )
    .is_none());
    assert!(compile(
        &patterns,
        MatchMode::DifferentRelationships,
        &Row::new(),
        &conjuncts
    )
    .is_some());
}

// The compiled chain walks the planner-oriented end: hop targets carry
// the written node patterns, seeds the nodes before them.
#[test]
fn chain_hops_carry_seed_and_target_patterns() {
    let (patterns, conjuncts) = planned(
        "MATCH (a:Person)-[:knows]->(b)-[:studyAt]->(o:Organisation) \
         WHERE o.name = 'MIT' RETURN a",
    );
    let Some(Contract::Chain(chain)) = compile(
        &patterns,
        MatchMode::DifferentRelationships,
        &Row::new(),
        &conjuncts,
    ) else {
        panic!("the chain shape compiles");
    };
    assert_eq!(chain.hops.len(), 2);
    // The planner may orient either end first (the selective end won
    // here); the kernel walks what it planned, so the assertions are
    // orientation-agnostic: the one compiled predicate — the folded
    // `o.name = 'MIT'` — sits on the Organisation node wherever the
    // planner put it, and the unpredicated nodes carry no program.
    let is_org = |target: &NodeTarget<'_>| {
        target
            .pattern
            .label_names()
            .any(|label| *label == SmallName::from_name("Organisation"))
    };
    let org = if is_org(&chain.start) {
        &chain.start
    } else {
        chain
            .hops
            .iter()
            .map(|hop| &hop.target)
            .find(|target| is_org(target))
            .expect("the Organisation is one of the chain's nodes")
    };
    assert!(
        matches!(org.prog, Prog::Cmp { .. }),
        "the folded equality compiled onto the Organisation"
    );
    let others: Vec<_> = std::iter::once(&chain.start)
        .chain(chain.hops.iter().map(|hop| &hop.target))
        .filter(|target| !is_org(target))
        .collect();
    assert_eq!(others.len(), 2);
    assert!(others
        .iter()
        .all(|target| matches!(target.prog, Prog::Bare)));
}

// The compiled hub-star orients both sides hub → terminal and applies
// the hub variable's own conjunct once, in side 0's slot.
#[test]
fn hub_star_sides_are_hub_oriented() {
    let (patterns, conjuncts) = planned(
        "MATCH (c:Comment)-[:hasCreator]->(p:Person), \
         (c)-[:replyOf]->(post:Post)-[:postHasCreator]->(p) RETURN c",
    );
    let Some(Contract::HubStar(star)) = compile(
        &patterns,
        MatchMode::DifferentRelationships,
        &Row::new(),
        &conjuncts,
    ) else {
        panic!("the q30 shape compiles");
    };
    assert!(star.shared_terminal);
    // Both hub slots name the same variable, and both carry the hub's
    // compiled program — the conjunct re-checked idempotently, like
    // the labels. This query's hub carries no conjunct, so both slots
    // are Bare; the conjunct-carrying shape is pinned by
    // `hub_conjunct_attaches_to_both_slots`.
    assert_eq!(
        star.hub[0].pattern.variable.as_ref(),
        star.hub[1].pattern.variable.as_ref()
    );
    assert!(matches!(star.hub[0].prog, Prog::Bare));
    assert!(matches!(star.hub[1].prog, Prog::Bare));
    // Side lengths: 1 and 2 hops.
    let lengths: Vec<usize> = star.sides.iter().map(|side| side.len()).collect();
    assert_eq!(lengths, vec![1, 2]);
    // Every side's first hop leaves the hub's node pattern, and both
    // sides arrive at the same terminal variable.
    for (side, hub) in star.sides.iter().zip(&star.hub) {
        assert_eq!(side[0].seed.variable, hub.pattern.variable);
    }
    // Both sides bind a terminal, and it is the same one: an `Option`
    // comparison, so an absent terminal on either side fails the
    // assertion instead of matching another absence.
    let terminals: Vec<Option<&SmallName>> = star
        .sides
        .iter()
        .map(|side| side.last().unwrap().target.pattern.variable.as_ref())
        .collect();
    assert_eq!(terminals[0], terminals[1], "the shared terminal is shared");
}

// The hub variable's own conjunct attaches to BOTH hub slots — the
// idempotent re-check the field doc states (a `Bare` slot 1 would be
// the "runs in slot 0 only" behavior the code does not have).
#[test]
fn hub_conjunct_attaches_to_both_slots() {
    let (patterns, conjuncts) = planned(
        "MATCH (c:Comment)-[:hasTag]->(t:Tag), (c)-[:locatedIn]->(l:Place) \
         WHERE c.k > 1 RETURN c",
    );
    let Some(Contract::HubStar(star)) = compile(
        &patterns,
        MatchMode::DifferentRelationships,
        &Row::new(),
        &conjuncts,
    ) else {
        panic!("the hub-star shape with a hub conjunct compiles");
    };
    for (slot, target) in star.hub.iter().enumerate() {
        assert!(
            matches!(target.prog, Prog::Cmp { .. }),
            "slot {slot} carries the hub's conjunct: {:?}",
            target.prog
        );
    }
}
