//! ADR-0687's staleness decisions, end to end through the public API: an
//! entry carries its token footprint and the counts it was planned against, an
//! unrelated commit leaves it hot, cardinality drift re-stamps it or marks it
//! dirty, and the epoch, an absent name or a degraded plan grade re-plans it.

use super::*;

/// Inject one grade reading on the front entry (ADR-0687 decision 6): the
/// seam's address is the entry's template and its plan identity, so both are
/// read from the entry itself — a test never fabricates either half.
fn grade_front(db: &Database, reads: f64) {
    let mut cache = crate::plan_cache::lock(&db.inner);
    let template = cache.front_template().expect("one entry");
    let plan = cache.front_plan().expect("one entry");
    cache.record_grade(&template, &plan, reads);
}

/// ADR-0687: the compiled entry carries the plan's token footprint — the
/// label it reads, with the row count it was planned against — drift
/// leaves those plan-time numbers as the baseline, and a re-plan (here
/// forced by a schema change) refreshes them against the committed state.
#[test]
fn a_cached_entry_carries_and_refreshes_its_plans_footprint() {
    let (db, mut c) = users();
    c.query("MATCH (n:User {id: 1}) RETURN n.name AS name")
        .unwrap();
    let footprint = crate::plan_cache::lock(&db.inner)
        .front_footprint()
        .expect("one entry");
    assert_eq!(footprint.labels.len(), 1, "{footprint:?}");
    assert_eq!(
        footprint.labels[0].1,
        Some(3),
        "three Users at plan time: {footprint:?}"
    );
    // The pattern's `id` and the projection's `name` are both read keys.
    assert_eq!(footprint.prop_keys.len(), 2, "{footprint:?}");

    // Doubling the label's cardinality is drift at the ADR-0687 threshold:
    // the same template with another literal still serves, so the baseline
    // stays the plan-time count.
    for id in 4..=6 {
        c.query(&format!("CREATE (:User {{id: {id}, name: 'x'}})"))
            .unwrap();
    }
    c.query("MATCH (n:User {id: 2}) RETURN n.name AS name")
        .unwrap();
    {
        let entries = crate::plan_cache::lock(&db.inner);
        assert_eq!(
            entries.front_footprint().expect("one entry").labels[0].1,
            Some(3),
            "drift never re-prices the baseline"
        );
    }
    // A schema change re-plans through the epoch and the fresh footprint
    // records the live count.
    c.query("CREATE INDEX user_name FOR (n:User) ON (n.name)")
        .unwrap();
    c.query("MATCH (n:User {id: 2}) RETURN n.name AS name")
        .unwrap();
    let refreshed = crate::plan_cache::lock(&db.inner)
        .front_footprint()
        .expect("the same entry");
    assert_eq!(refreshed.labels.len(), 1, "{refreshed:?}");
    assert_eq!(
        refreshed.labels[0].1,
        Some(6),
        "the re-plan priced six Users: {refreshed:?}"
    );
}

/// ADR-0687: a commit whose tokens all fall outside the entry's footprint
/// leaves the plan hot — the next hit is a plain template hit, no re-plan,
/// and the entry stays clean.
#[test]
fn an_unrelated_commit_keeps_the_template_hit() {
    let (db, mut c) = users();
    let read = |c: &mut Connection, id: i64| {
        let statement = format!("MATCH (n:User {{id: {id}}}) RETURN n.name AS name");
        c.query(&statement).unwrap().plan_cache()
    };
    assert_eq!(read(&mut c, 1), crate::PlanCacheOutcome::Miss);
    assert_eq!(
        read(&mut c, 1),
        crate::PlanCacheOutcome::HitRawFastPath,
        "the raw fast path is unchanged"
    );
    // A `:Tag` write touches no User token: the User read's footprint is
    // clean, so the next same-template statement is a template hit.
    c.query("CREATE (:Tag {t: 1})").unwrap();
    assert_eq!(read(&mut c, 3), crate::PlanCacheOutcome::HitTemplate);
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.front_dirty(), Some(false), "no footprint touch");
    assert_eq!(
        entries.front_footprint().expect("one entry").labels[0].1,
        Some(3),
        "the plan's counts were never re-priced"
    );
}

/// ADR-0687 decision 5: a footprint commit whose drift stays inside the
/// threshold re-stamps the entry at the current generation and serves it
/// clean — no dirty mark — with the plan-time counts still the baseline.
#[test]
fn a_footprint_commit_inside_the_threshold_restamps_and_serves_clean() {
    let (db, mut c) = users();
    let read = |c: &mut Connection, id: i64| {
        let statement = format!("MATCH (n:User {{id: {id}}}) RETURN n.name AS name");
        c.query(&statement).unwrap().plan_cache()
    };
    assert_eq!(read(&mut c, 1), crate::PlanCacheOutcome::Miss);
    // 3 -> 4 Users is 4/3 drift, inside the 2x threshold.
    c.query("CREATE (:User {id: 4, name: 'dee'})").unwrap();
    assert_eq!(
        read(&mut c, 2),
        crate::PlanCacheOutcome::HitTemplate,
        "the cached plan still serves"
    );
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.front_dirty(), Some(false), "inside the threshold");
    let current = db.inner.stats_generation.current();
    assert_eq!(
        entries.front_generation(),
        Some(current),
        "the entry is re-stamped at the current generation"
    );
    assert_eq!(
        entries.front_footprint().expect("one entry").labels[0].1,
        Some(3),
        "the baseline stays the plan-time count, so drift accumulates"
    );
}

/// ADR-0687 decisions 5-6: drift at the 2x threshold marks the entry dirty
/// and it still serves — re-planning a degraded dirty plan is the plan
/// grade's job, never this admission's.
#[test]
fn drift_past_the_threshold_marks_dirty_and_still_serves() {
    let (db, mut c) = users();
    let read = |c: &mut Connection, id: i64| {
        let statement = format!("MATCH (n:User {{id: {id}}}) RETURN n.name AS name");
        c.query(&statement).unwrap().plan_cache()
    };
    assert_eq!(read(&mut c, 1), crate::PlanCacheOutcome::Miss);
    // 3 -> 6 Users is exactly 2x: past the threshold.
    for id in 4..=6 {
        c.query(&format!("CREATE (:User {{id: {id}, name: 'x'}})"))
            .unwrap();
    }
    let outcome = read(&mut c, 2);
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(
        outcome,
        crate::PlanCacheOutcome::HitTemplate,
        "the cached plan still serves"
    );
    assert_eq!(entries.front_dirty(), Some(true), "the entry is dirty");
    assert_eq!(
        entries.front_footprint().expect("one entry").labels[0].1,
        Some(3),
        "the plan was not rebuilt: the baseline is still the plan-time count"
    );
}

/// ADR-0687 decision 6, the synthetic arm: the plan grade is injected at
/// the cache seam (`record_grade`) so the admission decision is pinned
/// independently of how many store reads a fixture plan performs — a dirty
/// entry whose average reaches twice its baseline re-plans, a congruent
/// one keeps serving, and a *clean* entry serves whatever its grade says
/// (the grade gates a dirty plan only).
#[test]
fn a_degraded_grade_replans_a_dirty_entry_and_a_congruent_one_serves() {
    let (db, mut c) = users();
    let read = |c: &mut Connection, id: i64| {
        let statement = format!("MATCH (n:User {{id: {id}}}) RETURN n.name AS name");
        c.query(&statement).unwrap().plan_cache()
    };
    assert_eq!(read(&mut c, 1), crate::PlanCacheOutcome::Miss);
    let template = crate::plan_cache::lock(&db.inner)
        .front_template()
        .expect("one entry");
    grade_front(&db, 4.0);
    grade_front(&db, 4.0);
    crate::plan_cache::lock(&db.inner).mark_dirty(&template, db.inner.stats_generation.current());
    assert_eq!(
        read(&mut c, 2),
        crate::PlanCacheOutcome::HitTemplate,
        "a congruent grade keeps serving the dirty plan"
    );
    {
        let kept = crate::plan_cache::lock(&db.inner);
        assert_eq!(kept.front_dirty(), Some(true), "and leaves it dirty");
        assert_eq!(
            kept.front_grade().expect("graded").baseline(),
            Some(4.0),
            "the injected baseline stands"
        );
    }
    // One reading far above the baseline — 4 + (40 - 4) / 8 = 8.5, past
    // twice the 4.0 baseline — is the degrade the next hit re-plans on.
    grade_front(&db, 40.0);
    assert!(
        crate::plan_cache::lock(&db.inner)
            .front_grade()
            .expect("graded")
            .degraded(),
        "the injected EWMA is past the factor"
    );
    assert_eq!(
        read(&mut c, 3),
        crate::PlanCacheOutcome::Replan,
        "the degraded grade re-plans the dirty plan on that hit"
    );
    {
        let replanned = crate::plan_cache::lock(&db.inner);
        assert_eq!(
            replanned.front_dirty(),
            Some(false),
            "a re-plan clears the dirty mark"
        );
        assert!(
            replanned
                .front_grade()
                .expect("graded")
                .baseline()
                .is_some_and(|baseline| baseline != 40.0),
            "the fresh plan starts over: its own first execution records the \
             baseline, never the re-planned plan's injected one"
        );
    }
    // The same degraded reading on a *clean* entry changes nothing: the
    // grade gates a dirty plan only.
    grade_front(&db, 400.0);
    assert!(crate::plan_cache::lock(&db.inner)
        .front_grade()
        .expect("graded")
        .degraded());
    assert_eq!(
        read(&mut c, 1),
        crate::PlanCacheOutcome::HitTemplate,
        "a clean plan serves whatever its measured work says"
    );
}

/// ADR-0687 decision 6, the real-execution arm: no injection — the grade
/// is the measured store reads one execution performed, so a label that
/// outgrows its plan lifts the average by itself. The first hit records the
/// baseline,
/// the drift hit marks the entry dirty and grades the far larger read
/// count, and the next hit re-plans on the degraded grade.
#[test]
fn a_plan_that_outgrows_its_grade_replans_after_drift() {
    let (db, mut c) = users();
    // A whole-label scan: its reads are one per node examined, so the
    // measured work grows with the label (a point match's list is the one
    // row it answered, whatever the label holds).
    let statement = "MATCH (n:User) RETURN n.name AS name";
    let read = |c: &mut Connection| c.query(statement).unwrap().plan_cache();
    assert_eq!(read(&mut c), crate::PlanCacheOutcome::Miss);
    assert_eq!(
        read(&mut c),
        crate::PlanCacheOutcome::HitRawFastPath,
        "the first hit is the entry's first graded execution"
    );
    let baseline = crate::plan_cache::lock(&db.inner)
        .front_grade()
        .expect("graded")
        .baseline()
        .expect("the first hit records the baseline");
    assert!(
        baseline > 0.0,
        "the scan reads the label's three nodes: {baseline}"
    );
    // 3 -> 90 Users is 30x the plan-time count — drift far past the 2x
    // threshold, and the scan's reads grow with it.
    for id in 4..=90 {
        c.query(&format!("CREATE (:User {{id: {id}, name: 'x'}})"))
            .unwrap();
    }
    assert_eq!(
        read(&mut c),
        crate::PlanCacheOutcome::HitRawFastPath,
        "the drift hit still serves"
    );
    let graded = crate::plan_cache::lock(&db.inner)
        .front_grade()
        .expect("graded");
    assert_eq!(
        graded.baseline(),
        Some(baseline),
        "drift never re-baselines the grade"
    );
    assert!(
        graded.ewma().expect("graded") > baseline,
        "the drift hit measured the bigger scan: {graded:?}"
    );
    assert!(
        graded.degraded(),
        "30x the rows lifts the average past twice the baseline: {graded:?}"
    );
    assert_eq!(
        crate::plan_cache::lock(&db.inner).front_dirty(),
        Some(true),
        "the drift hit marked the entry dirty"
    );
    assert_eq!(
        read(&mut c),
        crate::PlanCacheOutcome::Replan,
        "the next hit re-plans on the measured degradation"
    );
    assert_eq!(
        crate::plan_cache::lock(&db.inner).front_dirty(),
        Some(false),
        "and the re-plan clears the dirty mark"
    );
}

/// ADR-0687 decision 4: schema DDL moves the full-invalidation epoch, so
/// an entry whose footprint tokens all stand still still re-plans.
#[test]
fn a_schema_change_replans_through_the_epoch() {
    let (db, mut c) = users();
    let read = |c: &mut Connection, id: i64| {
        let statement = format!("MATCH (n:User {{id: {id}}}) RETURN n.name AS name");
        c.query(&statement).unwrap().plan_cache()
    };
    assert_eq!(read(&mut c, 1), crate::PlanCacheOutcome::Miss);
    let epoch = db.shared().lock_token_changes().epoch();
    c.query("CREATE INDEX user_name FOR (n:User) ON (n.name)")
        .unwrap();
    assert_eq!(
        db.shared().lock_token_changes().epoch(),
        epoch + 1,
        "DDL advances the epoch"
    );
    // The label's cardinality did not move, and the footprint has no
    // token to stamp: only the epoch can re-plan this entry.
    assert_eq!(read(&mut c, 2), crate::PlanCacheOutcome::Replan);
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.front_epoch(), Some(epoch + 1));
    assert_eq!(entries.front_dirty(), Some(false));
}

/// ADR-0687 decision 4: an inserted entry carries the epoch it was planned
/// at, under the same guard as the generation.
#[test]
fn an_inserted_entry_carries_the_plan_epoch() {
    let (db, mut c) = users();
    // the fixture's own index DDL moved the epoch; the entry carries it
    let epoch = db.shared().lock_token_changes().epoch();
    c.query("MATCH (n:User {id: 1}) RETURN n.name AS name")
        .unwrap();
    let entries = crate::plan_cache::lock(&db.inner);
    assert_eq!(entries.front_epoch(), Some(epoch));
    assert_eq!(entries.front_dirty(), Some(false));
}

/// ADR-0687 decision 3: a name resolved as absent has no id to stamp, so
/// an entry carrying one keeps ADR-0503's any-commit check — the commit
/// that interns the name re-plans instead of serving a plan built before
/// the name existed.
#[test]
fn a_commit_interns_an_absent_name_and_replans() {
    let (_db, mut c) = users();
    let read = "MATCH (n:Ghost) RETURN count(n) AS c";
    assert_eq!(
        rows(&c.query(read).unwrap()),
        vec![vec![Value::Int(0)]],
        "no Ghost at plan time"
    );
    assert_eq!(
        c.query(read).unwrap().plan_cache(),
        crate::PlanCacheOutcome::HitRawFastPath
    );
    c.query("CREATE (:Ghost)").unwrap();
    let after = c.query(read).unwrap();
    assert_eq!(
        after.plan_cache(),
        crate::PlanCacheOutcome::Replan,
        "the entry keeps the any-commit check"
    );
    assert_eq!(
        rows(&after),
        vec![vec![Value::Int(1)]],
        "the re-planned read sees the interned label"
    );
}

/// Every write commit advances the stats generation, and the next read
/// follows the new state through the footprint admission: within the
/// drift threshold the entry is re-stamped and serves, at or past it the
/// entry is marked dirty and still serves — neither re-plans, so the
/// rows reflect the write against the cached plan.
#[test]
fn write_commits_advance_the_generation_and_reads_follow() {
    let (db, mut c) = users();
    let read = "MATCH (n:User) RETURN count(n) AS c";
    let before = c.query(read).unwrap();
    assert_eq!(rows(&before), vec![vec![Value::Int(3)]]);
    let stamped = db.inner.stats_generation.current();
    for i in 0..3 {
        c.query("CREATE (:User {id: 4, name: 'x'})").unwrap();
        let after = c.query(read).unwrap();
        assert_eq!(rows(&after), vec![vec![Value::Int(4 + i)]]);
        assert_eq!(
            after.plan_cache(),
            crate::PlanCacheOutcome::HitRawFastPath,
            "the cached plan serves through the footprint admission, no replan"
        );
        // One commit = one advance.
        assert_eq!(
            db.inner.stats_generation.current(),
            stamped + (i as u64 + 1)
        );
    }
}

/// A schema change is a commit like any other: the generation advances and
/// the reads stay correct through the replan.
#[test]
fn schema_changes_advance_the_generation() {
    let (db, mut c) = users();
    let read = "MATCH (n:User {id: 1}) RETURN n.name AS name";
    assert_eq!(
        rows(&c.query(read).unwrap()),
        vec![vec![Value::String("ada".into())]]
    );
    let stamped = db.inner.stats_generation.current();
    c.query("CREATE INDEX user_name FOR (n:User) ON (n.name)")
        .unwrap();
    assert_eq!(db.inner.stats_generation.current(), stamped + 1);
    // The cached shape replans against the index and stays correct.
    for _ in 0..2 {
        assert_eq!(
            rows(&c.query(read).unwrap()),
            vec![vec![Value::String("ada".into())]]
        );
    }
}

/// Run `statement` as a cache hit inside an explicit transaction while the
/// pool is filled to zero free after its admission: the paused statement's
/// own reservation is already counted, so nothing remains for any later
/// growth — the gauge ADR-0687 decision 6's tail failure needs. The
/// transaction matters: its arm runs the materialized write-path entry, so
/// the result keeps its rows and the statement's epilogue — not the
/// consumer — is what charges them. The pause hook lands between admission
/// and execution (the transaction arm's test seam).
fn run_in_txn_with_filled_pool(
    db: &Database,
    statement: &str,
) -> crate::Result<crate::QueryResult> {
    use std::sync::{Arc, Barrier};

    let pause = Arc::new(crate::ReadPause {
        entered: Barrier::new(2),
        release: Barrier::new(2),
    });
    *db.inner.read_pause.lock().unwrap() = Some(Arc::clone(&pause));
    let mut conn = db.connect().unwrap();
    conn.begin().unwrap();
    let statement = statement.to_owned();
    let run = std::thread::spawn(move || conn.query(&statement));
    pause.entered.wait();
    let budget = &db.inner.resolved.budget;
    // Fill the admission ceiling and the tail alike: the statement is
    // admitted, everything else is held here until the run finished.
    let ceiling_fill = budget.occupy_leaving(0).expect("the ceiling has room");
    let capacity = budget.snapshot().expect("the budget is on").capacity;
    let reserved = budget.snapshot().expect("the budget is on").reserved;
    let tail_fill = match budget.try_hold_grow(capacity - reserved) {
        crate::token_budget::HoldAttempt::Held(hold) => hold,
        _ => panic!("the tail fills"),
    };
    pause.release.wait();
    let result = run.join().expect("the statement thread cannot panic");
    drop(tail_fill);
    drop(ceiling_fill);
    result
}

/// ADR-0687 decision 6 and the grading seam's own contract: a statement that
/// dispatches successfully but fails in the epilogue — here the retained
/// rows' workspace charge — is not the plan's work, so its reads never reach
/// the entry's grade. The pool is filled to zero free after admission: the
/// dispatch fits the admitted reservation (the same shape with a short
/// literal answers under the identical fill), and only the 5 KiB row's
/// retention needs growth.
#[test]
fn a_statement_failed_in_the_epilogue_does_not_grade_its_entry() {
    use relate_core::tokens::CHARGE_QUANTUM_BYTES;

    let db = Database::open_with(
        ":memory:",
        crate::OpenOptions {
            concurrent_logical_workspace_limit: Some(8 * CHARGE_QUANTUM_BYTES),
            ..crate::OpenOptions::default()
        },
    )
    .unwrap();
    let mut c = db.connect().unwrap();
    // One template, several literals: the first run inserts the entry, the
    // second proves it a hit, and the graded entry is pinned below.
    let statement = |literal: &str| format!("RETURN '{literal}' AS s");
    c.query(&statement("x")).unwrap();
    c.query(&statement("y")).unwrap();
    grade_front(&db, 1000.0);
    let graded = |db: &Database| {
        crate::plan_cache::lock(&db.inner)
            .front_grade()
            .and_then(|grade| grade.ewma())
    };
    assert_eq!(graded(&db), Some(1000.0), "the injected grade stands");

    let failed = run_in_txn_with_filled_pool(&db, &statement(&"z".repeat(5_000)));
    assert!(
        matches!(
            failed,
            Err(crate::Error::ResourceLimit {
                name: "concurrent-logical-workspace-limit",
                ..
            })
        ),
        "the retained rows' charge is refused: {failed:?}"
    );
    assert_eq!(
        graded(&db),
        Some(1000.0),
        "a statement the user sees fail does not grade its entry"
    );

    // Control: the identical fill does not break the dispatch — a short
    // literal of the same template, whose rows fit the admitted
    // reservation, answers, so the failure above is the retention's.
    let answered = run_in_txn_with_filled_pool(&db, &statement("w")).unwrap();
    assert_eq!(rows(&answered), vec![vec![Value::String("w".into())]]);
}
