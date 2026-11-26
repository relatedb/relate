//! The plan-cache unit tests (ADR-0503, ADR-0687, ADR-0700): the cache's
//! keying, LRU, stamps and grade seams, one module beside the type they
//! exercise (the parent declares it with `#[path]` to keep `plan_cache.rs`
//! inside the repository's file-size budget).

use super::*;

/// LABS-1120: the outcome vocabulary pinned against the enum's own
/// spelling — the wire table `as_str` owns, in full and exactly once
/// per variant, kebab-case throughout. The wire envelope carries this
/// spelling as a bare string across the crate boundary, so this pin
/// (not a hand-maintained list in a doc comment) is what a rename or
/// an added variant must pass through.
#[test]
fn outcome_spellings_are_the_complete_kebab_vocabulary() {
    // The one declaration drives both the spelling table and this
    // iteration (a new variant must be added to the macro to compile),
    // so the two lists cannot fall out of step.
    for (outcome, spelling) in PlanCacheOutcome::SPELLINGS {
        assert_eq!(outcome.as_str(), *spelling);
        assert!(
            !spelling.contains(|c: char| c.is_uppercase() || c == '_'),
            "the wire spelling is kebab-case: {spelling}"
        );
    }
}

/// ADR-0687 decision 6: the grade is a per-plan fact, so a reading is
/// dropped when a re-plan landed between the lookup and the cache write —
/// the fresh entry starts over and the superseded plan's measurement is
/// never folded into it.
#[test]
fn a_grade_for_a_superseded_plan_is_dropped() {
    let mut cache = PlanCache::default();
    cache.insert(entry("t", 0, "RETURN 1 AS one"));
    let served = cache.find_template("t").expect("present").plan;
    // Another connection's re-plan lands between the lookup and the write.
    let fresh = Arc::new(
        relate_planner::plan(
            relate_front::parse_and_lower("RETURN 1 AS one").unwrap(),
            &mut |_| false,
        )
        .unwrap(),
    );
    cache.update_plan(
        "t",
        Arc::clone(&fresh),
        Arc::new(PlanFootprint::default()),
        PlanStamps {
            generation: 5,
            epoch: 0,
        },
        // The stand-in re-plan consulted no statistics (ADR-0700).
        None,
    );
    cache.record_grade("t", &served, 9.0);
    assert_eq!(
        cache.front_grade(),
        Some(PlanGrade::default()),
        "the superseded plan's reading is dropped"
    );
    cache.record_grade("t", &fresh, 9.0);
    assert_eq!(
        cache.front_grade().and_then(|grade| grade.baseline()),
        Some(9.0),
        "the plan that ran grades"
    );
}

/// One entry for the cache-shape tests: a stand-in lowered AST and
/// plan (a fixed `RETURN 1 AS one` — the tests exercise keying, LRU
/// and generation, never the plan's content) and a stand-in raw
/// memory (the given text, no extracted values). The stand-ins are
/// the fixture's contract, not a real admission's output.
fn entry(template: &str, generation: u64, raw: &str) -> PlanCacheEntry {
    let query = relate_front::parse_and_lower("RETURN 1 AS one").unwrap();
    let plan = relate_planner::plan(query.clone(), &mut |_| false).unwrap();
    PlanCacheEntry {
        handles: EntryHandles {
            template: template.into(),
            query: Arc::new(query),
            plan: Arc::new(plan),
            footprint: Arc::new(PlanFootprint::default()),
            stamps: PlanStamps {
                generation,
                epoch: 0,
            },
            key_stats_epoch: None,
            dirty: false,
            grade: PlanGrade::default(),
        },
        raw: RawStatement {
            text: raw.into(),
            values: Arc::new(Vec::new()),
        },
        hold: None,
    }
}

/// The raw fast path: an entry remembers its last raw text, and an
/// exact repeat resolves to that entry's template without any
/// normalizer input.
#[test]
fn raw_lookup_finds_the_last_raw_text() {
    let mut cache = PlanCache::default();
    cache.insert(entry("RETURN $__AUTO_INT_0__ AS one", 0, "RETURN 1 AS one"));
    assert!(cache.lookup_raw("RETURN 1 AS one").is_some());
    assert_eq!(
        &*cache
            .lookup_raw("RETURN 1 AS one")
            .unwrap()
            .handles
            .template,
        "RETURN $__AUTO_INT_0__ AS one"
    );
    assert!(cache.lookup_raw("RETURN 2 AS one").is_none());
}

/// The template is the real key: present/absent, and the entry
/// handles it carries for the stage-3 check.
#[test]
fn template_lookup_hands_the_entry_handles() {
    let mut cache = PlanCache::default();
    assert!(cache.find_template("t").is_none());
    cache.insert(entry("t", 7, "RETURN 1 AS one"));
    let handles = cache.find_template("t").expect("present");
    assert_eq!(handles.stamps.generation, 7);
    // The shared plan is the one the entry was built with.
    assert!(Arc::strong_count(&handles.plan) >= 1);
    assert!(cache.find_template("other").is_none());
    // A re-plan updates the plan, the footprint, the generation, the
    // epoch and the statistics stamp in place, and clears the dirty
    // mark.
    let query = (*handles.query).clone();
    assert_eq!(query.parts.len(), 1);
    let fresh = Arc::new(relate_planner::plan(query, &mut |_| false).unwrap());
    let footprint = Arc::new(PlanFootprint {
        labels: vec![(relate_names::TokenId(1), Some(3))],
        rel_types: Vec::new(),
        prop_keys: Vec::new(),
        seeks: Vec::new(),
        absent: false,
    });
    cache.mark_dirty("t", 7);
    assert_eq!(cache.front_dirty(), Some(true));
    cache.update_plan(
        "t",
        Arc::clone(&fresh),
        Arc::clone(&footprint),
        PlanStamps {
            generation: 8,
            epoch: 2,
        },
        Some(3),
    );
    let replanned = cache.find_template("t").expect("present");
    assert_eq!(replanned.stamps.generation, 8);
    assert_eq!(replanned.stamps.epoch, 2);
    assert_eq!(replanned.key_stats_epoch, Some(3));
    assert!(Arc::ptr_eq(&replanned.plan, &fresh));
    assert!(Arc::ptr_eq(&replanned.footprint, &footprint));
    assert_eq!(cache.front_dirty(), Some(false), "a re-plan is clean");
}

/// ADR-0687 decision 5: a footprint commit inside the drift threshold
/// re-stamps the entry at the current generation and leaves it clean;
/// the plan itself is untouched, and an evicted template is a no-op.
#[test]
fn restamp_moves_only_the_generation() {
    let mut cache = PlanCache::default();
    cache.insert(entry("t", 3, "RETURN 1 AS one"));
    let plan = cache.find_template("t").unwrap().plan;
    cache.restamp("t", 9);
    let kept = cache.find_template("t").expect("present");
    assert_eq!(kept.stamps.generation, 9);
    assert!(Arc::ptr_eq(&kept.plan, &plan), "the plan is untouched");
    assert_eq!(cache.front_dirty(), Some(false), "inside the threshold");
    // A re-stamp leaves an existing dirty mark standing.
    cache.mark_dirty("t", 9);
    cache.restamp("t", 10);
    assert_eq!(cache.front_dirty(), Some(true));
    // An evicted template has nothing to re-stamp.
    cache.restamp("gone", 11);
    cache.mark_dirty("gone", 11);
    assert_eq!(cache.len(), 1);
}

/// ADR-0687 decisions 5-6: a footprint commit at or past the drift
/// threshold marks the entry dirty and re-stamps it — the plan still
/// serves, and the plan grade decides the later re-plan.
#[test]
fn mark_dirty_sets_the_flag_and_restamps() {
    let mut cache = PlanCache::default();
    cache.insert(entry("t", 3, "RETURN 1 AS one"));
    let plan = cache.find_template("t").unwrap().plan;
    cache.mark_dirty("t", 9);
    let kept = cache.find_template("t").expect("present");
    assert_eq!(kept.stamps.generation, 9);
    assert!(Arc::ptr_eq(&kept.plan, &plan), "the plan is untouched");
    assert_eq!(cache.front_dirty(), Some(true));
    // A later insert of the same template is a fresh entry: clean.
    cache.insert(entry("t", 10, "RETURN 2 AS one"));
    assert_eq!(cache.front_dirty(), Some(false));
}

/// ADR-0687: a lookup hit hands the footprint out as a pointer clone
/// of the entry's own storage — the template hit and the raw fast
/// path alike, which stays a lookup with no copy, no re-derivation.
#[test]
fn hits_hand_the_footprint_out_as_a_pointer_clone() {
    use relate_names::TokenId;

    let footprint = Arc::new(PlanFootprint {
        labels: vec![(TokenId(3), Some(12))],
        rel_types: vec![(TokenId(4), Some(1))],
        prop_keys: vec![TokenId(5)],
        seeks: Vec::new(),
        absent: false,
    });
    let mut cache = PlanCache::default();
    let mut fixture = entry("t", 0, "RETURN 1 AS one");
    fixture.handles.footprint = Arc::clone(&footprint);
    cache.insert(fixture);

    let template = cache.find_template("t").expect("present");
    assert!(Arc::ptr_eq(&template.footprint, &footprint));
    let raw = cache.lookup_raw("RETURN 1 AS one").expect("the raw hit");
    assert!(Arc::ptr_eq(&raw.handles.footprint, &footprint));
    // The raw fast path itself is unchanged: still stage 1, the same
    // entry, its own handles.
    assert_eq!(raw.stage, PlanCacheOutcome::HitRawFastPath);
    assert_eq!(&*raw.handles.template, "t");
}

/// The statistics epoch is compared only for a plan that read the
/// per-key statistics (ADR-0700 decision 3); a plan that never read
/// them ignores it entirely. The commit half of freshness is ADR-0687's
/// footprint check, covered by the footprint suites.
#[test]
fn the_key_stats_epoch_is_compared_only_for_a_plan_that_read_the_statistics() {
    let mut cache = PlanCache::default();
    cache.insert(entry("t", 7, "RETURN 1 AS one"));
    let unstamped = cache.find_template("t").expect("present");
    assert!(
        unstamped.key_stats_fresh(0) && unstamped.key_stats_fresh(99),
        "no stamp: any epoch"
    );
    let stamped = EntryHandles {
        key_stats_epoch: Some(5),
        ..unstamped
    };
    assert!(stamped.key_stats_fresh(5));
    assert!(
        !stamped.key_stats_fresh(6),
        "the epoch moved past the stamp"
    );
}

/// At most `PLAN_CACHE_CAPACITY` entries: the least recently used
/// falls off the back.
#[test]
fn capacity_evicts_the_least_recently_used() {
    let mut cache = PlanCache::default();
    for i in 0..PLAN_CACHE_CAPACITY {
        cache.insert(entry(&format!("t{i}"), 0, "RETURN 1 AS one"));
    }
    assert_eq!(cache.len(), PLAN_CACHE_CAPACITY);
    // Touch the oldest so the next insert evicts the second-oldest
    // instead.
    assert!(cache.find_template("t0").is_some());
    cache.insert(entry("fresh", 0, "RETURN 1 AS one"));
    assert_eq!(cache.len(), PLAN_CACHE_CAPACITY);
    assert!(
        cache.find_template("t0").is_some(),
        "the touched entry survives"
    );
    assert!(cache.find_template("fresh").is_some());
    assert!(
        cache.find_template("t1").is_none(),
        "the untouched LRU evicts"
    );
}

/// Inserting the same template replaces the old entry (an insert
/// after an eviction race, or a re-insert of a dropped shape).
#[test]
fn insert_replaces_the_same_template() {
    let mut cache = PlanCache::default();
    cache.insert(entry("t", 1, "RETURN 1 AS one"));
    cache.insert(entry("t", 2, "RETURN 2 AS one"));
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.find_template("t").unwrap().stamps.generation, 2);
    assert!(cache.lookup_raw("RETURN 2 AS one").is_some());
    assert!(cache.lookup_raw("RETURN 1 AS one").is_none());
}

/// `note_raw` keeps the fast path's memory per template.
#[test]
fn note_raw_remembers_the_latest_text_and_values() {
    let mut cache = PlanCache::default();
    cache.insert(entry("t", 0, "RETURN 1 AS one"));
    cache.note_raw(
        "t",
        "RETURN 41 AS one",
        &[("__AUTO_INT_0__".into(), Value::Int(41))],
    );
    let hit = cache
        .lookup_raw("RETURN 41 AS one")
        .expect("the noted text hits");
    assert_eq!(
        hit.values.as_ref(),
        &[("__AUTO_INT_0__".into(), Value::Int(41))]
    );
}

/// The normalizer's values convert to the engine's parameter values,
/// lists included.
#[test]
fn engine_values_convert_lists() {
    let values = engine_values(vec![
        ("a".into(), NormalizedValue::Int(1)),
        (
            "b".into(),
            NormalizedValue::List(vec![
                NormalizedValue::Str("x".into()),
                NormalizedValue::Float(2.5),
            ]),
        ),
    ])
    .expect("in-bound literals convert");
    assert_eq!(values[0].1, Value::Int(1));
    assert_eq!(
        values[1].1,
        Value::List(Arc::new(
            vec![Value::String("x".into()), Value::Float(2.5),]
        ))
    );
}

/// REL8-1120: a literal list nested past the value-depth bound
/// (ADR-0462) refuses conversion — the payload boundary is where the
/// hit path catches the one value-dependent literal class the plan
/// itself cannot, so an over-deep literal never rides a cached plan.
#[test]
fn engine_values_refuse_over_depth_list() {
    let nested = |depth: u32| -> NormalizedValue {
        let mut value = NormalizedValue::Int(0);
        for _ in 0..depth {
            value = NormalizedValue::List(vec![value]);
        }
        value
    };
    // A scalar is depth 0 and each list wrap adds one (ADR-0462), so
    // `bound` wraps sit exactly at the cap and one more refuses.
    let bound = relate_core::MAX_VALUE_DEPTH;
    let in_bound = engine_values(vec![("a".into(), nested(bound))]);
    assert!(in_bound.is_some(), "a list at the bound converts");
    let over_bound = engine_values(vec![("a".into(), nested(bound + 1))]);
    assert!(over_bound.is_none(), "a list past the bound refuses");
}

/// ADR-0541: tokens are resolved only in the compiled plan. The cached
/// statement tree stays `Unresolved`; the plan carries the store's ids.
#[test]
fn statement_tree_holds_no_resolved_tokens() {
    use relate_names::{Token, TokenId};
    struct Interned;
    impl relate_names::TokenResolver for Interned {
        fn resolve(&self, name: &str) -> Token {
            if name == "Person" || name == "KNOWS" || name == "age" {
                Token::Id(TokenId(7))
            } else {
                Token::Absent
            }
        }
    }
    let query =
        relate_front::parse_and_lower("MATCH (n:Person)-[:KNOWS]->(m) WHERE n.age > 1 RETURN m")
            .unwrap();
    let resolved = relate_planner::plan_with_tokens(
        query.clone(),
        &mut |_| false,
        &relate_planner::NoPatternStats,
        &Interned,
    )
    .unwrap();

    let relate_ir::Clause::Match { patterns, .. } = &query.parts[0].clauses[0] else {
        panic!("expected MATCH first");
    };
    assert!(patterns[0]
        .nodes
        .iter()
        .all(|n| n.labels.iter().all(|(_, t)| *t == Token::Unresolved)));
    assert!(patterns[0]
        .rels
        .iter()
        .all(|r| r.types.iter().all(|(_, t)| *t == Token::Unresolved)));

    let relate_ir::Clause::Match { patterns, .. } = &resolved.parts[0][0] else {
        panic!("expected MATCH first");
    };
    assert_eq!(patterns[0].nodes[0].labels[0].1, Token::Id(TokenId(7)));
    assert_eq!(patterns[0].rels[0].types[0].1, Token::Id(TokenId(7)));
}
