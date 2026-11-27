//! The catalogue's invariants and the fixture-backed cross-checks: every
//! group binds what its shape says and answers one integer with a
//! PROFILEd plan, and the shortest-path spellings agree on one pair.

use relate::Value;
use relate_test_support::pokec::{open, PokecShape};

use super::groups::{Binds, GROUPS, PAIR_OFFSET};
use super::{plan_evidence, profile_query, run};

/// The smallest store both ends of every pair group exist in: users
/// `1..=START + PAIR_OFFSET` need ids past [`PAIR_OFFSET`].
const SHAPE: PokecShape = PokecShape {
    nodes: 5_200,
    rels: 26_000,
    seed: 0x706f_6b65_6301,
};

#[test]
fn only_pair_groups_bind_the_second_end() {
    for g in &GROUPS {
        let step = g.first_run();
        assert_eq!(
            step.params.contains_key("to"),
            matches!(g.binds, Binds::Pair(_)),
            "{}",
            g.name
        );
        assert_eq!(
            step.params.contains_key("id"),
            matches!(g.binds, Binds::Start(_) | Binds::Pair(_)),
            "{}",
            g.name
        );
    }
    let pair = GROUPS
        .iter()
        .find(|g| g.name == "shortest_pair")
        .expect("the pair group");
    let offset = i64::try_from(PAIR_OFFSET).expect("PAIR_OFFSET is small");
    let third = pair
        .runs()
        .into_iter()
        .find(|step| step.start == Some(3))
        .expect("the pair group runs start id 3");
    assert_eq!(third.params.get("to"), Some(&Value::Int(3 + offset)));
}

#[test]
fn every_group_answers_one_integer_and_profiles_a_plan() {
    let db = open(SHAPE).expect("open the fixture");
    let mut conn = db.connect().expect("connect");
    for g in &GROUPS {
        run(&mut conn, g, g.query, &g.first_run()).unwrap_or_else(|e| panic!("{}: {e}", g.name));
        let evidence = plan_evidence(&profile_query(&mut conn, g).expect("PROFILE"));
        assert!(
            evidence.contains("kernel="),
            "{}: plan evidence names a kernel slot: {evidence}",
            g.name
        );
    }
}

/// The shortest-path groups agree where they ask one question: the
/// selector, the function, the postfix quantifier and the expression
/// form on one pair find the same number of paths (one, or none when
/// the pair is unconnected).
#[test]
fn the_pair_spellings_agree() {
    let db = open(SHAPE).expect("open the fixture");
    let mut conn = db.connect().expect("connect");
    let by_name = |name: &str| GROUPS.iter().find(|g| g.name == name).expect(name);
    let answers: Vec<i64> = ["shortest_pair", "shortest_path_fn", "shortest_postfix"]
        .iter()
        .map(|n| {
            let g = by_name(n);
            run(&mut conn, g, g.query, &g.first_run()).expect(n)
        })
        .collect();
    assert!(answers.iter().all(|a| *a == answers[0]), "{answers:?}");
    let expr = {
        let g = by_name("shortest_expr");
        run(&mut conn, g, g.query, &g.first_run()).expect("expr")
    };
    assert_eq!(expr, answers[0], "the expression form finds the same pair");
}
