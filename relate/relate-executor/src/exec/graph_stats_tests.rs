//! The live-graph stats adapter's planner-facing facts (LABS-643): the
//! sampled `CONTAINS` estimate a store without a TEXT index measurably
//! reports, the comma-pattern order it induces (the GBL SF0.1 q19 shape,
//! pinned without the 16k-row label — the sampler's exact small-label
//! counts carry the same decision the 16k extrapolation makes there), and
//! the floor that keeps the estimate from undercutting an index-served
//! seed (the GBL q2 shape). ADR-0474's compound adds the scan-floor side:
//! the estimate carries the label bucket's rows, and the anchor decisions
//! the floor drives (q8 flips to the rival scan, q19 keeps the seed).
//! ADR-0486 adds the range window: the posting-length count a RANGE
//! index answers for a comparison predicate, the ladder level it induces
//! on a live graph, and the orientation it flips.

use std::collections::BTreeMap;

use relate_core::Value;
use relate_front::parse_and_lower;
use relate_ir::{BinaryOp, Clause, Expr};
use relate_names::{NamedToken, SmallName, Token, TokenResolver};
use relate_planner::{
    plan_with_stats, seed_on_rel_start, walk_cost, ContainsCount, Estimate, PatternStats,
};

use super::graph_stats::GraphStats;
use super::test_support::{exec_autocommit, exec_cypher, range_start_graph};
use crate::graph::Graph;

/// A name beside the unresolved token: the by-name stats form.
fn unresolved(name: &str) -> NamedToken {
    (SmallName::from_name(name), Token::Unresolved)
}

/// The GBL q19 shape at toy scale: 40 Tags of which 2 carry the needle,
/// 30 Places, 200 Comments each tagging one tag and located in one place.
/// No TEXT index exists anywhere (the GBL upstream profile's RANGE-only
/// stance), so `CONTAINS` is measurable only through the LABS-643 sampler.
fn q19_graph() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut tags = Vec::new();
    for i in 0..40 {
        let name = if i < 2 { "Copernicus" } else { "Kepler" };
        tags.push(
            g.create_node(
                vec!["Tag".into()],
                BTreeMap::from([("name".into(), Value::String(name.into()))]),
            )
            .unwrap(),
        );
    }
    let mut places = Vec::new();
    for i in 0..30 {
        places.push(
            g.create_node(
                vec!["Place".into()],
                BTreeMap::from([("name".into(), Value::String(format!("Place{i}").into()))]),
            )
            .unwrap(),
        );
    }
    for i in 0..200 {
        let comment = g
            .create_node(vec!["Comment".into()], BTreeMap::new())
            .unwrap();
        g.create_rel(
            comment,
            tags[i % tags.len()],
            "commentHasTag".into(),
            BTreeMap::new(),
        )
        .unwrap();
        g.create_rel(
            comment,
            places[i % places.len()],
            "commentIsLocatedIn".into(),
            BTreeMap::new(),
        )
        .unwrap();
    }
    g.commit_write();
    g
}

#[test]
fn sampled_contains_seeds_q19_on_the_tag_side_without_a_text_index() {
    let g = q19_graph();
    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    // The measured facts the order rides: the CONTAINS count without any
    // TEXT index (2 survivors, not the 40 whole-label rows the pre-
    // LABS-643 ladder fell back to), against Place's label rows. The
    // sampled form carries the label bucket's rows as its scan floor —
    // the compound's input (ADR-0474).
    assert_eq!(
        stats.contains_rows(
            &unresolved("Tag"),
            &unresolved("name"),
            &Expr::String("Copernicus".into())
        ),
        Some(ContainsCount::SampleScan { rows: 2, scan: 40 })
    );
    assert_eq!(stats.label_rows(&unresolved("Place")), Some(30));

    let query = parse_and_lower(
        "MATCH (c:Comment)-[:commentHasTag]->(t:Tag), \
         (c)-[:commentIsLocatedIn]->(l:Place) \
         WHERE t.name CONTAINS 'Copernicus' \
         RETURN l.name, COUNT(c.ID) AS comment_count \
         ORDER BY comment_count DESC LIMIT 1",
    )
    .unwrap();
    let plan = plan_with_stats(query, &mut |_| false, &stats).unwrap();
    let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
        panic!("expected a leading MATCH");
    };
    // The Tag pattern seeds (its 2 CONTAINS survivors beat Place's 30
    // label rows), and the second pattern then grows from the bound c.
    assert!(
        patterns[0].nodes[0]
            .label_names()
            .any(|label| label == "Tag"),
        "expected the Tag pattern first, got {:?}",
        patterns[0].nodes[0].labels
    );
    assert_eq!(
        patterns[1].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("c".to_string())
    );
}

/// The floor's protective side (LABS-643, the GBL q2 shape): a sampled
/// `CONTAINS` estimate is floored at 1% of its label, so it never
/// undercuts an index-served seed — anchoring the walk on the
/// contains-filtered Post label pays a whole-label content scan, and the
/// RANGE-equality Person seed must keep the start. Sized so the floored
/// estimate (Post/100 = 30) loses the orientation comparison to the
/// equality-scaled walk (2 survivors x 3,000 posts / 300 persons = 20),
/// while the raw survivor count (3) would win it and flip the seed to
/// Post.
#[test]
fn sampled_contains_floor_never_undercuts_an_equality_seeded_start() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut persons = Vec::new();
    for i in 0..300 {
        let first = if i < 2 { "Lei" } else { "Ada" };
        persons.push(
            g.create_node(
                vec!["Person".into()],
                BTreeMap::from([("firstName".into(), Value::String(first.into()))]),
            )
            .unwrap(),
        );
    }
    for i in 0..3_000 {
        let content = if i % 1_000 == 0 {
            "xxZuluxx"
        } else {
            "lorem ipsum"
        };
        let post = g
            .create_node(
                vec!["Post".into()],
                BTreeMap::from([("content".into(), Value::String(content.into()))]),
            )
            .unwrap();
        g.create_rel(
            post,
            persons[i % persons.len()],
            "postHasCreator".into(),
            BTreeMap::new(),
        )
        .unwrap();
    }
    g.commit_write();
    exec_autocommit(&g, "CREATE INDEX FOR (p:Person) ON (p.firstName)").unwrap();

    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    // The measured facts: the RANGE-equality seed (2 Leis) against the
    // floored sampled estimate (30 = 3,000/100, not the 3 raw survivors),
    // whose scan floor (3,000) is the compound's input (ADR-0474).
    assert_eq!(
        stats.equality_rows(
            &unresolved("Person"),
            &unresolved("firstName"),
            &Expr::String("Lei".into())
        ),
        Some(2)
    );
    assert_eq!(
        stats.contains_rows(
            &unresolved("Post"),
            &unresolved("content"),
            &Expr::String("Zulu".into())
        ),
        Some(ContainsCount::SampleScan {
            rows: 30,
            scan: 3_000
        })
    );

    let query = parse_and_lower(
        "MATCH (p:Person)<-[:postHasCreator]-(post:Post) \
         WHERE p.firstName = 'Lei' AND post.content CONTAINS 'Zulu' \
         RETURN post.ID",
    )
    .unwrap();
    let plan = plan_with_stats(query, &mut |_| false, &stats).unwrap();
    let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
        panic!("expected a leading MATCH");
    };
    // The equality-served Person keeps the written start; the Post side
    // stays the walked target.
    assert_eq!(
        patterns[0].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("p".to_string())
    );
    assert_eq!(
        patterns[0].nodes[1]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("post".to_string())
    );
}

/// ADR-0486's range level on the live stats: the RANGE index's
/// posting-length window prices a comparison-predicated side below its
/// whole-label rows, so the pattern orients to start there — the same
/// `GraphStats` the executor's start selection reads, so plan order and
/// runtime start agree on the anchor. The window is exact and seekable:
/// no scan floor, unlike the sampled CONTAINS level below it.
#[test]
fn the_range_window_seeds_the_pattern_through_the_live_stats() {
    let g = range_start_graph("x", &(0..100).map(Value::Int).collect::<Vec<_>>());
    exec_autocommit(&g, "CREATE INDEX FOR (q:Q) ON (q.x)").unwrap();
    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    // The measured facts the level rides: the window (4) against the
    // whole-label rows it replaces (100), and the decline a pair with no
    // index keeps.
    assert_eq!(
        stats.range_rows(
            &unresolved("Q"),
            &unresolved("x"),
            BinaryOp::Lt,
            &Expr::Int(4)
        ),
        Some(4)
    );
    assert_eq!(stats.label_rows(&unresolved("Q")), Some(100));
    assert_eq!(
        stats.range_rows(
            &unresolved("Q"),
            &unresolved("nokey"),
            BinaryOp::Lt,
            &Expr::Int(4)
        ),
        None,
        "a pair no RANGE index serves stays unpriced"
    );

    let query = parse_and_lower(
        "MATCH (p:P)-[:LINKS]->(q:Q) WHERE q.x < 4 \
         RETURN id(q) AS q, id(p) AS p",
    )
    .unwrap();
    let plan = plan_with_stats(query, &mut |_| false, &stats).unwrap();
    let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
        panic!("expected a leading MATCH");
    };
    // The Q side's degree-scaled window (4) beats the P side's
    // degree-scaled label scan: the pattern orients to start at Q.
    assert_eq!(
        patterns[0].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("q".to_string())
    );
    assert_eq!(
        patterns[0].nodes[1]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("p".to_string())
    );
}

/// ADR-0493's crossover on the live stats and the live funnel: a
/// majority window (600 of 1,200 — at the measured `age >= 18` shape's
/// selectivity) declines the range level, so the pattern orients to the
/// rival label scan side and the funnel serves the scan with the
/// residual filter — the answers stay exact through the new path. A
/// minority window (100 of 1,200, under both the fraction and the
/// floor) still seeds from the index.
#[test]
fn the_crossover_flips_the_majority_window_to_the_scan_side() {
    let g = range_start_graph("x", &(0..1_200).map(Value::Int).collect::<Vec<_>>());
    exec_autocommit(&g, "CREATE INDEX FOR (q:Q) ON (q.x)").unwrap();
    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);

    // The majority window's level declines (the element prices at its
    // whole-label rows — pinned in the planner's order tests), so the
    // rival side wins the anchor: the pattern starts at P (20 rows)
    // instead of the gated Q window.
    let query = parse_and_lower(
        "MATCH (p:P)-[:LINKS]->(q:Q) WHERE q.x < 600 \
         RETURN id(q) AS q, id(p) AS p",
    )
    .unwrap();
    let plan = plan_with_stats(query, &mut |_| false, &stats).unwrap();
    let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
        panic!("expected a leading MATCH");
    };
    assert_eq!(
        patterns[0].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("p".to_string())
    );
    assert_eq!(
        patterns[0].nodes[1]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("q".to_string())
    );

    // The funnel's scan path answers exactly (the residual re-applies
    // the predicate): 600 of the 1,200 Q nodes match, one row each.
    let r = exec_cypher(&g, "MATCH (q:Q) WHERE q.x < 600 RETURN count(q) AS n");
    assert_eq!(r.rows[0][0], Value::Int(600));
    // The minority window (100 of 1,200) still seeks and answers.
    let r = exec_cypher(&g, "MATCH (q:Q) WHERE q.x < 100 RETURN count(q) AS n");
    assert_eq!(r.rows[0][0], Value::Int(100));
}

/// The compound's anchor flip (ADR-0474, the GBL q8 shape): the sampled
/// `CONTAINS` seed sits on the LARGER label, and its scan floor ties the
/// rival's degree-scaled walk — both orientations land on the 100
/// `hasModerator` rels — so the tie-break's seed comparison decides, and
/// the floored seed (its whole 100-row label) loses to Person's 10 rows.
/// The raw selectivity (2 title survivors) stole the anchor instead, and
/// the walk paid the 100-row title scan — the exact trade the 13,750-row
/// GBL label makes at SF0.1, pinned here without it.
#[test]
fn sampled_contains_scan_floor_flips_q8_to_the_person_side() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let mut persons = Vec::new();
    for (i, birthday) in ["1980-05-05", "1995-12-31"].into_iter().enumerate() {
        persons.push(
            g.create_node(
                vec!["Person".into()],
                BTreeMap::from([
                    ("id".into(), Value::Int(i as i64)),
                    ("birthday".into(), Value::String(birthday.into())),
                ]),
            )
            .unwrap(),
        );
        // Four more persons per birthday pair: 10 persons, all moderating.
        for j in 0..4 {
            persons.push(
                g.create_node(
                    vec!["Person".into()],
                    BTreeMap::from([
                        ("id".into(), Value::Int((2 + i * 4 + j) as i64)),
                        (
                            "birthday".into(),
                            Value::String(
                                if (i + j) % 2 == 0 {
                                    "1980-05-05"
                                } else {
                                    "1995-12-31"
                                }
                                .into(),
                            ),
                        ),
                    ]),
                )
                .unwrap(),
            );
        }
    }
    // 100 forums, one moderator each, two titles carrying the needle.
    for i in 0..100 {
        let title = if i < 2 {
            format!("forum {i} about Emilio Fernandez posts")
        } else {
            format!("forum {i}")
        };
        let forum = g
            .create_node(
                vec!["Forum".into()],
                BTreeMap::from([("title".into(), Value::String(title.into()))]),
            )
            .unwrap();
        g.create_rel(
            forum,
            persons[i % persons.len()],
            "hasModerator".into(),
            BTreeMap::new(),
        )
        .unwrap();
    }
    g.commit_write();

    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    // The measured facts the compound rides: the sampled seed's output
    // (2) against its scan floor (the 100-row Forum bucket), and the
    // Person label the tie-break compares.
    assert_eq!(
        stats.contains_rows(
            &unresolved("Forum"),
            &unresolved("title"),
            &Expr::String("Emilio Fernandez".into())
        ),
        Some(ContainsCount::SampleScan { rows: 2, scan: 100 })
    );
    assert_eq!(stats.label_rows(&unresolved("Person")), Some(10));

    let query = parse_and_lower(
        "MATCH (p:Person)<-[:hasModerator]-(f:Forum) \
         WHERE date(p.birthday) > date(\"1990-01-01\") \
           AND f.title CONTAINS \"Emilio Fernandez\" \
         RETURN DISTINCT p.id",
    )
    .unwrap();
    let plan = plan_with_stats(query, &mut |_| false, &stats).unwrap();
    let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
        panic!("expected a leading MATCH");
    };
    // Person owns the start: its 10-row scan wins the seed tie-break
    // against the floored 100-row Forum seed, where the raw 2-survivor
    // selectivity anchored the walk on the Forum side.
    assert_eq!(
        patterns[0].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("p".to_string())
    );
    assert_eq!(
        patterns[0].nodes[1]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("f".to_string())
    );
}

/// LABS-671's insurance: the executor's compound first-hop estimate IS
/// the planner's `direction_cost` arithmetic — both sites read the one
/// `walk_cost` model, and this test pins the tie at the executor side
/// through the same dispatch production runs, against the same measured
/// sampled inputs, so a future private re-spelling of either site's
/// compound fails here instead of silently diverging the anchor
/// decisions (the q8 flip, the q19 keep). The floor invariant is
/// asserted too: the walk cost never drops below the whole-label scan,
/// whatever the degree scaling does to the output.
#[test]
fn the_compound_first_hop_is_the_planners_sample_scan_arithmetic() {
    let g = q19_graph();
    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    let query = parse_and_lower(
        "MATCH (t:Tag)<-[:commentHasTag]-(c:Comment) \
         WHERE t.name CONTAINS 'Copernicus' \
         RETURN c",
    )
    .unwrap();
    let Clause::Match {
        patterns,
        where_clause,
        ..
    } = &query.parts[0].clauses[0]
    else {
        panic!("expected a leading MATCH");
    };
    let path = &patterns[0];
    let np = &path.nodes[0];
    let rel = &path.rels[0];
    let conjuncts: Vec<&Expr> = where_clause.iter().collect();
    let bound: relate_planner::Bound = &|_: &str| false;

    let got = stats.estimate(np, bound, conjuncts.iter().copied(), Some(rel), false);
    let Some(ContainsCount::SampleScan { rows, scan }) = stats.contains_rows(
        &unresolved("Tag"),
        &unresolved("name"),
        &Expr::String("Copernicus".into()),
    ) else {
        panic!("expected a sampled CONTAINS estimate");
    };
    assert_eq!((rows, scan), (2, 40));
    let want = walk_cost(
        Estimate::SampleScan {
            rows: u128::from(rows),
            scan: u128::from(scan),
        },
        std::iter::once((np, rel, seed_on_rel_start(rel, false))),
        &stats,
    );
    assert_eq!(u128::from(got), want);
    // the floor invariant: the whole-label scan is the walk cost's floor
    assert!(got >= scan);
}

/// ADR-0541: one method per probe. An `Id` token reads the store by id and
/// an `Absent` one answers its zero without touching the interner, while an
/// `Unresolved` token falls back to the by-name lookup.
#[test]
fn token_forms_probe_the_store_by_id_or_not_at_all() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    let b = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    g.create_rel(a, b, "KNOWS".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    let resolved = |name: &str| (SmallName::from_name(name), g.resolve(name));
    let absent = |name: &str| (SmallName::from_name(name), Token::Absent);
    let (person, knows) = (resolved("Person"), resolved("KNOWS"));

    let _ = crate::store::take_get_id_calls();
    assert_eq!(stats.label_rows(&person), Some(2));
    assert_eq!(stats.rel_type_rows(&knows), Some(1));
    assert_eq!(stats.label_rows(&absent("Ghost")), Some(0));
    assert_eq!(stats.rel_type_rows(&absent("HAUNTS")), Some(0));
    assert_eq!(stats.degree_tail(&absent("Ghost"), &knows), None);
    assert_eq!(
        stats.contains_rows(
            &absent("Ghost"),
            &unresolved("name"),
            &Expr::String("x".into())
        ),
        Some(ContainsCount::SampleScan { rows: 0, scan: 0 })
    );
    assert_eq!(
        crate::store::take_get_id_calls(),
        0,
        "id/absent probed by name"
    );

    assert_eq!(stats.label_rows(&unresolved("Person")), Some(2));
    assert!(crate::store::take_get_id_calls() > 0);
}
