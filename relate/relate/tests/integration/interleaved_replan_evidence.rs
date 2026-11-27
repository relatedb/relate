//! ADR-0687 evidence: an interleaved write+read workload keeps a hot
//! template's plan cached across the commits it does not touch, and
//! re-plans only once a footprint commit reaches the drift threshold or
//! schema DDL moves the full-invalidation epoch. Every assertion reads the
//! per-statement plan-cache outcome off the public result header — the same
//! fact every surface renders as `planCache` — so the evidence is the
//! surface's own signal, not a test-only accessor.
//!
//! The plan grade's arm is a real execution, not an injected number: a
//! whole-label scan's store reads grow with the label, so a footprint that
//! has drifted past the threshold is graded with the work the drift caused
//! and re-plans on the next hit. The wall-clock p50 the hit path pays is
//! recorded by `interleaved_replan_measure.rs`, with its numbers in
//! `docs/testing/plan-cache-interleaved-evidence.md`.

use crate::common;

use common::{connection, run};
use relate::{Connection, PlanCacheOutcome, Value};

/// The three `:User` rows every point-read arm plans against.
fn seed(conn: &mut Connection) {
    run(
        conn,
        "CREATE (:User {id: 1, name: 'ada'}), (:User {id: 2, name: 'bob'}), \
         (:User {id: 3, name: 'cy'})",
    );
}

/// The point-read template with `id` bound by a literal: the same template
/// at any id, so each call after the first is a stage-2 template hit. The
/// returned name is the statement's own literal's answer — a hit that bound
/// the wrong value would answer the wrong row.
fn read_user(conn: &mut Connection, id: i64) -> (PlanCacheOutcome, Value) {
    let statement = format!("MATCH (n:User {{id: {id}}}) RETURN n.name AS name");
    let result = run(conn, &statement);
    let outcome = result.plan_cache();
    assert_eq!(result.rows.len(), 1, "{statement}: one row");
    let name = result.rows.into_iter().next().expect("one row").remove(0);
    (outcome, name)
}

/// The interleaved arm: an unrelated `:Tag` commit leaves the `:User`
/// footprint clean, so the next same-template read is a plain template hit;
/// the point read's own drift reaches only the dirty mark (its answered list
/// is one row, so its grade never degrades), and schema DDL is the one
/// wholesale trigger that re-plans it.
#[test]
fn an_interleaved_workload_hits_until_drift_or_schema_change() {
    let mut conn = connection();
    seed(&mut conn);
    // `id` is indexed so the point read prices from the index, not from the
    // lazily built per-key statistics (ADR-0696): a read of those replans
    // once as they publish (ADR-0700), which is not the rule this arm pins.
    run(&mut conn, "CREATE INDEX user_id FOR (n:User) ON (n.id)");

    // Cold: the first statement compiles, plans and inserts.
    let (outcome, name) = read_user(&mut conn, 1);
    assert_eq!(
        outcome,
        PlanCacheOutcome::Miss,
        "the first statement is cold"
    );
    assert_eq!(name, Value::String("ada".into()));

    // The same template at a different literal is a stage-2 hit.
    let (outcome, name) = read_user(&mut conn, 2);
    assert_eq!(outcome, PlanCacheOutcome::HitTemplate);
    assert_eq!(name, Value::String("bob".into()));

    // Interleaved unrelated writes: each `:Tag` commit touches no token the
    // User read resolved, so every following read stays a hit.
    for (tag, id, expected) in [(1i64, 3i64, "cy"), (2, 1, "ada"), (3, 2, "bob")] {
        run(&mut conn, &format!("CREATE (:Tag {{t: {tag}}})"));
        let (outcome, name) = read_user(&mut conn, id);
        assert_eq!(
            outcome,
            PlanCacheOutcome::HitTemplate,
            "an unrelated commit ({tag}) must not re-plan the User read"
        );
        assert_eq!(
            name,
            Value::String(expected.into()),
            "the hit binds its own literal"
        );
    }

    // A User commit inside the 2x drift threshold re-stamps the entry and
    // serves clean: 3 -> 4 rows is 4/3. The literal differs from the
    // previous read's raw text, so this is the stage-2 template path.
    run(&mut conn, "CREATE (:User {id: 4, name: 'dee'})");
    let (outcome, name) = read_user(&mut conn, 1);
    assert_eq!(
        outcome,
        PlanCacheOutcome::HitTemplate,
        "inside the threshold"
    );
    assert_eq!(name, Value::String("ada".into()));

    // 3 -> 6 rows is exactly the 2x threshold: the plan is marked dirty and
    // still serves. A point read answers one row at any label size, so its
    // measured grade does not degrade and this admission never re-plans it.
    run(
        &mut conn,
        "CREATE (:User {id: 5, name: 'x'}), (:User {id: 6, name: 'x'})",
    );
    let (outcome, name) = read_user(&mut conn, 3);
    assert_eq!(
        outcome,
        PlanCacheOutcome::HitTemplate,
        "past the drift threshold the plan is dirty, not re-planned"
    );
    assert_eq!(name, Value::String("cy".into()));

    // Schema DDL moves the full-invalidation epoch: the next hit re-plans
    // whatever the footprint says, and serves that statement correctly.
    run(&mut conn, "CREATE INDEX user_name FOR (n:User) ON (n.name)");
    let (outcome, name) = read_user(&mut conn, 4);
    assert_eq!(outcome, PlanCacheOutcome::Replan, "DDL advances the epoch");
    assert_eq!(name, Value::String("dee".into()));

    // The re-plan started a clean entry at the new epoch: the workload is
    // back to hits, unrelated commits included.
    let (outcome, name) = read_user(&mut conn, 1);
    assert_eq!(
        outcome,
        PlanCacheOutcome::HitTemplate,
        "the re-plan is clean"
    );
    assert_eq!(name, Value::String("ada".into()));
    run(&mut conn, "CREATE (:Tag {t: 4})");
    let (outcome, name) = read_user(&mut conn, 2);
    assert_eq!(outcome, PlanCacheOutcome::HitTemplate);
    assert_eq!(name, Value::String("bob".into()));
}

/// The plan-grade arm, from a real execution: a whole-label scan's store
/// reads grow with the label, so a footprint commit that multiplies the
/// label marks the plan dirty *and* grades it with the larger work; the next
/// hit re-plans on the degraded grade instead of serving the stale shape.
#[test]
fn a_scan_that_outgrows_its_grade_replans_after_footprint_drift() {
    let mut conn = connection();
    seed(&mut conn);

    let scan = |conn: &mut Connection| {
        let result = run(conn, "MATCH (n:User) RETURN n.name AS name");
        (result.plan_cache(), result.rows.len())
    };

    assert_eq!(scan(&mut conn).0, PlanCacheOutcome::Miss);
    assert_eq!(
        scan(&mut conn).0,
        PlanCacheOutcome::HitRawFastPath,
        "the exact repeat is the entry's first graded execution"
    );

    // 3 -> 90 Users is 30x the plan-time count: far past the 2x threshold,
    // and the scan reads every added node, so the drift hit is graded with
    // the work that drift caused.
    for id in 4..=90 {
        run(
            &mut conn,
            &format!("CREATE (:User {{id: {id}, name: 'x'}})"),
        );
    }
    let (outcome, rows) = scan(&mut conn);
    assert_eq!(
        outcome,
        PlanCacheOutcome::HitRawFastPath,
        "the drift hit still serves — the grade decides the re-plan"
    );
    assert_eq!(rows, 90);

    let (outcome, rows) = scan(&mut conn);
    assert_eq!(
        outcome,
        PlanCacheOutcome::Replan,
        "the degraded grade re-plans the dirty plan on this hit"
    );
    assert_eq!(rows, 90, "the re-planned scan sees every row");
}
