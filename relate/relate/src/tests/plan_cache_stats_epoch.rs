//! The plan cache and the per-key statistics epoch (ADR-0503, ADR-0696,
//! ADR-0700): a cached plan whose planning consulted the per-key value
//! statistics replans once those statistics change, and a plan that never
//! consulted them does not — composed with ADR-0687's footprint rules,
//! which decide the commit half of freshness.
//!
//! No production planner rung consults the statistics yet, so these tests
//! drive the consult through the store's test seam
//! (`Shared::test_plan_consult`): every plan-time adapter of the store
//! then consults `key_stats` for the configured `(label, property)`, the
//! way a planner rung will.

use crate::{Connection, Database, PlanCacheOutcome};

const COUNT_C1: &str = "MATCH (n:City {country: 'C1'}) RETURN count(n) AS c";
const COUNT_C2: &str = "MATCH (n:City {country: 'C2'}) RETURN count(n) AS c";
/// A statement of a different template: it shares the store, not the plan.
const PEOPLE: &str = "MATCH (p:Person {id: 1}) RETURN p.id AS id";

/// 60 cities over six countries, and a person.
fn store() -> (Database, Connection) {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("UNWIND range(0, 59) AS i CREATE (:City {country: 'C' + toString(i % 6)})")
        .unwrap();
    c.query("CREATE (:Person {id: 1})").unwrap();
    (db, c)
}

/// Make the store's planning consult the statistics of `City.country`, or
/// stop doing so.
fn consult_city_country(db: &Database, on: bool) {
    *db.inner.test_plan_consult.lock().unwrap() =
        on.then(|| ("City".to_string(), "country".to_string()));
}

/// The store's per-key statistics epoch, read under the publication guard
/// the graph cell requires.
fn epoch(db: &Database) -> u64 {
    let _read = db.inner.read_guard().unwrap();
    let graph: &relate_executor::Graph = &db.inner.graph;
    graph.key_stats_epoch()
}

fn outcome(c: &mut Connection, statement: &str) -> PlanCacheOutcome {
    c.query(statement).unwrap().plan_cache()
}

fn is_hit(outcome: PlanCacheOutcome) -> bool {
    matches!(
        outcome,
        PlanCacheOutcome::HitRawFastPath | PlanCacheOutcome::HitTemplate
    )
}

/// A plan cached while the label's statistics were unmeasured replans once
/// the statement-completion pump has published them, then hits.
#[test]
fn a_plan_priced_while_unmeasured_replans_once_the_statistics_exist() {
    let (db, mut c) = store();
    consult_city_country(&db, true);
    // The miss plans while `City` is unbuilt: the consult answers `None`
    // and requests the build; the statement's own completion serves it.
    assert_eq!(outcome(&mut c, COUNT_C1), PlanCacheOutcome::Miss);
    assert_eq!(
        outcome(&mut c, COUNT_C1),
        PlanCacheOutcome::Replan,
        "the statistics published after the plan was priced"
    );
    assert!(
        is_hit(outcome(&mut c, COUNT_C1)),
        "the replan read the statistics: nothing moved since, so it hits"
    );
    // The same template with another literal shares the entry.
    assert!(is_hit(outcome(&mut c, COUNT_C2)));
}

/// The two causes compose (ADR-0687 decision 5, ADR-0700 decisions 3-4):
/// a within-threshold footprint commit re-stamps the entry and serves a
/// clean hit without disturbing its statistics stamp, and because the
/// statistics epoch is global, another label's publication still re-plans
/// a plan that read the statistics.
#[test]
fn a_restamp_keeps_the_statistics_stamp_and_a_foreign_publication_replans() {
    let (db, mut c) = store();
    consult_city_country(&db, true);
    // Miss (unmeasured), Replan (the completion published City), hit.
    assert_eq!(outcome(&mut c, COUNT_C1), PlanCacheOutcome::Miss);
    assert_eq!(outcome(&mut c, COUNT_C1), PlanCacheOutcome::Replan);
    assert!(is_hit(outcome(&mut c, COUNT_C1)));
    // A commit on the City footprint with small drift (60 -> 61 rows) is a
    // re-stamp, not a re-plan: the plan still serves, clean.
    c.query("CREATE (:City {country: 'C1'})").unwrap();
    assert!(
        is_hit(outcome(&mut c, COUNT_C1)),
        "drift inside the threshold re-stamps and serves"
    );
    {
        let entries = crate::plan_cache::lock(&db.inner);
        assert_eq!(entries.front_dirty(), Some(false), "inside the threshold");
        assert_eq!(
            entries.front_generation(),
            Some(db.inner.stats_generation.current()),
            "the entry was re-stamped at the current generation"
        );
    }
    // Point the planning consult at another, unbuilt label: its statement
    // misses, and its completion publishes `Person` — no commit, so only
    // the statistics epoch moves.
    *db.inner.test_plan_consult.lock().unwrap() = Some(("Person".to_string(), "id".to_string()));
    let before = epoch(&db);
    assert_eq!(outcome(&mut c, PEOPLE), PlanCacheOutcome::Miss);
    assert!(epoch(&db) > before, "Person's statistics were published");
    assert_eq!(
        outcome(&mut c, COUNT_C1),
        PlanCacheOutcome::Replan,
        "the re-stamp kept the plan's statistics stamp: a foreign publication still re-plans it"
    );
    assert!(is_hit(outcome(&mut c, COUNT_C1)), "the replan re-stamped");
}

/// A plan that never consulted the statistics is not replanned when some
/// label's statistics publish.
#[test]
fn a_plan_that_never_consulted_the_statistics_is_not_replanned() {
    let (db, mut c) = store();
    // Planned with the seam off: no consult, so no epoch stamp.
    assert_eq!(outcome(&mut c, PEOPLE), PlanCacheOutcome::Miss);
    consult_city_country(&db, true);
    let before = epoch(&db);
    // Another statement consults, and its completion publishes the label.
    assert_eq!(outcome(&mut c, COUNT_C1), PlanCacheOutcome::Miss);
    assert!(epoch(&db) > before, "the statistics were published");
    assert!(
        is_hit(outcome(&mut c, PEOPLE)),
        "an unrelated publication is no reason to replan"
    );
    // The statement that did consult replans.
    assert_eq!(outcome(&mut c, COUNT_C1), PlanCacheOutcome::Replan);
}
