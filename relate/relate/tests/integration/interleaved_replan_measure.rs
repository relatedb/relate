//! ADR-0687 evidence harness: the wall-clock p50 a hot template pays after
//! an unrelated commit — the measurement whose numbers live in
//! `docs/testing/plan-cache-interleaved-evidence.md`.
//!
//! The file is deliberately free of this branch's admission types so the
//! same harness compiles and runs on the pre-change base as well as here;
//! it reports the outcome through [`PlanCacheOutcome::as_str`], the public
//! wire spelling both trees share.
//!
//! Run it explicitly, once per checkout, on a quiet machine:
//!
//! ```text
//! CARGO_TARGET_DIR=$PWD/target cargo test --profile corpus -p relate \
//!   --test interleaved_replan_measure -- --ignored --nocapture
//! ```
//!
//! It seeds a small `:User` label, warms the point-read template, then times
//! one read per round of interleaved unrelated `:Tag` commits. Only the read
//! is inside the clock; the commit is outside every sample.

use crate::common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use common::{connection, run};

/// Reads timed per round.
const READS: usize = 1000;
/// Measured rounds this harness reports; the report's run count.
const ROUNDS: usize = 3;
/// Users the point read scans — small enough that a re-plan, not the label
/// scan, dominates a cold-admission read.
const USERS: i64 = 10;

#[test]
#[ignore = "measurement harness: run explicitly with `--profile corpus -- --ignored --nocapture`"]
fn hot_template_after_an_unrelated_commit_p50() {
    for round in 0..ROUNDS {
        let mut conn = connection();
        // The read resolves `:User` and its `id` key; the `:Tag` label and
        // `t` key the writes intern are tokens the read never resolves.
        run(
            &mut conn,
            &format!("UNWIND range(1, {USERS}) AS i CREATE (:User {{id: i}})"),
        );
        // Warm the template to a hit before the clock starts.
        for id in 1..=USERS {
            run(
                &mut conn,
                &format!("MATCH (n:User {{id: {id}}}) RETURN n.id AS id"),
            );
        }

        let mut wall = Vec::with_capacity(READS);
        let mut plan_ms = Vec::with_capacity(READS);
        let mut outcomes: BTreeMap<&'static str, usize> = BTreeMap::new();
        for i in 0..READS {
            run(&mut conn, &format!("CREATE (:Tag {{t: {i}}})"));
            // Cycle the literal so every read is a stage-2 template hit, not
            // the raw-string fast path of an identical text.
            let id = (i as i64 % USERS) + 1;
            let statement = format!("MATCH (n:User {{id: {id}}}) RETURN n.id AS id");
            let started = Instant::now();
            let result = conn.query(&statement).expect("the timed read");
            wall.push(started.elapsed());
            plan_ms.push(result.plan_ms());
            *outcomes.entry(result.plan_cache().as_str()).or_default() += 1;
        }

        println!(
            "PCI round={round} reads={READS} p50_ns={} plan_p50_ms={:.6} outcomes={outcomes:?}",
            p50_duration(&mut wall).as_nanos(),
            p50_f64(&mut plan_ms),
        );
    }
}

/// The 50th percentile of a sample set, by nearest rank.
fn p50_duration(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[rank(samples.len())]
}

/// [`p50_duration`] for the `f64` timing accessors.
fn p50_f64(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[rank(samples.len())]
}

/// The nearest-rank index of the 50th percentile.
fn rank(len: usize) -> usize {
    ((len as f64 - 1.0) * 0.5).round() as usize
}
