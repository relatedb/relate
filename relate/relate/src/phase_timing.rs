//! Always-on per-phase statement timing (Neo4j timing alignment,
//! LABS-832): the PARSE / PLAN / EXECUTE split of the ADR-0221
//! wall-clock `elapsed`, extended by the fine sub-split and the
//! plan-cache outcome (LABS-1120).
//!
//! The phases partition the same measured statement body
//! [`crate::ResultHeader::elapsed`] spans: PARSE (the front-end parse and
//! lowering), PLAN (binding, logical planning, admission classification),
//! EXECUTE (the dispatch — or, for `EXPLAIN`, the plan render). The stages
//! between them (the post-EXPLAIN gates, token reservation) stay in the
//! total alone, so the three durations are disjoint sub-windows and their
//! sum never exceeds `elapsed`. The fine sub-split (LABS-1120) partitions
//! PARSE (`lex_parse` / `rewrite` / `lower`, stamped by the front) and
//! EXECUTE (`result_generation`, the result build) the same way — every
//! fine window is a sub-window of its coarse phase.
//!
//! The struct carries no `merge` arm: no production site merges phases —
//! `elapsed` and `stats` are per-statement at every multi-statement site
//! in the tree, and the one `QueryStats::merge` call is the per-batch
//! merge inside a single ADR-0209 batched bulk write, one statement whose
//! phases are stamped once. A merged-result surface re-adds the arm in
//! its own change, together with its production caller.

use std::time::Duration;
use web_time::Instant;

use relate_front::ParseStages;

use crate::PlanCacheOutcome;

/// A statement's per-phase timings (LABS-832): how the ADR-0221
/// wall-clock total splits across the pipeline's three stages,
/// measured always-on — no PROFILE opt-in, no feature flag — plus the
/// fine sub-split of PARSE and EXECUTE (LABS-1120), measured always-on
/// the same way: the measured floor on point reads puts the whole fine
/// split's four extra clock stamps at ~130 ns, ~1% of a 12 µs engine
/// point read (the LABS-1120 measurement), so no gating.
///
/// Home: this crate (the facade), because the parse/plan/execute
/// boundary is the facade's statement pipeline —
/// `Connection::admit_statement` and `Connection::dispatch` in
/// `connection/dispatch.rs`, the same sites that measure the
/// ADR-0221 `elapsed`. The executor receives a finished logical
/// plan, so no lower crate can host the split. `pub(crate)`
/// permanently: the public exposure (ADR-0491, LABS-1120) is the
/// accessor family `QueryResult::parse_ms` / `plan_ms` / `execute_ms`
/// and the fine-split accessors beside them, over the header's copy —
/// one public path, never a `pub` type beside them. The fields are
/// private to this module: outside it, the split is only read (through
/// the phase accessors below) or stamped (through
/// [`TimingStamp::stamped`]), never constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhaseTimings {
    /// The PARSE stage: the front-end parse and lowering of the statement
    /// text (`parse_and_lower_statement_with_policy_and_params`) — on a
    /// plan-cache hit, the lookup stages instead (ADR-0503), which is why
    /// the fine sub-windows below are zero there: nothing was parsed, and
    /// the result's plan-cache outcome says so.
    parse: Duration,
    /// The PLAN stage: binding and logical planning with the admission
    /// classification (`plan_gate_classify`).
    plan: Duration,
    /// The EXECUTE stage: the dispatch — a transaction's overlay run, an
    /// auto-commit write, or a snapshot read. For `EXPLAIN` this is the
    /// plan render: the wrapped statement never executes, so the render
    /// is that path's execute-equivalent stage.
    execute: Duration,
    /// The PARSE stage's fine sub-split (LABS-1120): `lex_parse` /
    /// `rewrite` / `lower` — sub-windows of `parse`, zero on a cache hit
    /// (nothing was parsed). The two boundaries the front does not have
    /// stay fused inside these windows, each for an architectural reason
    /// recorded on [`ParseStages`]: lexer-vs-parser (one tokenizing
    /// constructor) and validation-vs-lowering (the battery runs inside
    /// the lowering walk).
    parse_stages: ParseStages,
    /// The EXECUTE stage's result-generation sub-window (LABS-1120): the
    /// buffered executor result's conversion into the `QueryResult` (the
    /// column-type stamp, the header, the staged-failure columns) — a
    /// sub-window of `execute`. Zero on the `EXPLAIN` path, whose
    /// execute-equivalent stage (the render) IS its result generation —
    /// there is no second window to carve there.
    result_generation: Duration,
}

impl PhaseTimings {
    /// The one construction — structurally: the fields are private to this
    /// module, so no code outside it can build a split. Called only by
    /// [`TimingStamp::stamped`], at the statement's stamping tail: PARSE
    /// and PLAN as admission measured them, EXECUTE as the statement's
    /// third stage (the dispatch, or the EXPLAIN render) measured it, and
    /// the fine sub-windows as the stages that own them measured them. A
    /// split is never built half-measured, so no stage ever carries a
    /// zero placeholder for work still ahead of it.
    fn complete(
        parse: Duration,
        plan: Duration,
        execute: Duration,
        stages: ParseStages,
        result_generation: Duration,
    ) -> Self {
        Self {
            parse,
            plan,
            execute,
            parse_stages: stages,
            result_generation,
        }
    }

    /// The PARSE stage's measured duration.
    pub(crate) fn parse(&self) -> Duration {
        self.parse
    }

    /// The PLAN stage's measured duration.
    pub(crate) fn plan(&self) -> Duration {
        self.plan
    }

    /// The EXECUTE stage's measured duration.
    pub(crate) fn execute(&self) -> Duration {
        self.execute
    }

    /// The PARSE stage's fine sub-split (LABS-1120).
    pub(crate) fn parse_stages(&self) -> ParseStages {
        self.parse_stages
    }

    /// The EXECUTE stage's result-generation sub-window (LABS-1120).
    pub(crate) fn result_generation(&self) -> Duration {
        self.result_generation
    }
}

/// The admission-measured timing facts (LABS-832 + LABS-1120) — the PARSE
/// and PLAN durations, the PARSE stage's fine sub-split, and the
/// statement's plan-cache outcome — as ONE carrier: it is what
/// [`crate::connection::admission::Admission`] holds (no loose
/// same-typed fields beside it), what `DispatchCtx` forwards, and what
/// [`TimingStamp::stamped`] consumes, so the admission's timing facts
/// have exactly one shape between the admission and the stamping tail
/// (the carrier convention of `AdmitPreamble`) and a fact cannot be
/// stamped without the facts it partitions.
#[derive(Clone, Copy)]
pub(crate) struct AdmissionTiming {
    /// The PARSE stage's measured duration, as admission measured it.
    pub(crate) parse: Duration,
    /// The PLAN stage's measured duration, as admission measured it.
    pub(crate) plan: Duration,
    /// The PARSE stage's fine sub-split (LABS-1120).
    pub(crate) stages: ParseStages,
    /// The statement's plan-cache outcome (LABS-1120).
    pub(crate) plan_cache: PlanCacheOutcome,
}

/// The dispatch stage's two measured windows (LABS-1120): EXECUTE, closed
/// at the dispatch boundary (the raw executor result in hand), and its
/// result-generation sub-window, closed after the result build — one
/// bundle because the two are adjacent same-typed durations at the
/// stamping tail, where a transposed pair would compile and silently
/// misreport (found in review).
#[derive(Clone, Copy)]
pub(crate) struct DispatchWindows {
    /// The EXECUTE stage's measured duration, closed at the dispatch
    /// boundary.
    pub(crate) execute: Duration,
    /// The EXECUTE stage's result-generation sub-window (LABS-1120);
    /// `Duration::ZERO` on the `EXPLAIN` path, whose render is itself its
    /// result generation.
    pub(crate) result_generation: Duration,
}

/// The statement's measured timing as one stamp: the ADR-0221 wall-clock
/// total (`elapsed`) together with its always-on PARSE / PLAN / EXECUTE
/// split (`phases`, LABS-832) and the statement's plan-cache outcome
/// (LABS-1120). Bundled like `ResultStamp` — so a result-metadata
/// construction site cannot stamp the total without its split, and the
/// header constructions stay under `clippy::too_many_arguments`.
pub(crate) struct TimingStamp {
    /// The ADR-0221 wall-clock total — what [`crate::ResultHeader::elapsed`]
    /// announces.
    pub(crate) elapsed: Duration,
    /// The total's PARSE / PLAN / EXECUTE split with its fine sub-split.
    pub(crate) phases: PhaseTimings,
    /// The statement's plan-cache outcome (LABS-1120) — stamped with the
    /// timing it explains (a hit's near-zero parse window), beside it.
    pub(crate) plan_cache: PlanCacheOutcome,
}

impl TimingStamp {
    /// The one spelling of the LABS-832 stamping policy (ADR-0491,
    /// extended by LABS-1120): the total is measured from the statement's
    /// start instant (`started.elapsed()`), PARSE and PLAN (with the fine
    /// sub-split and the plan-cache outcome) are the admission's measured
    /// facts, and EXECUTE is the duration the dispatch stage closed at its
    /// own boundary — with `result_generation` the window the result build
    /// closed after it — so the total's window structurally contains the
    /// disjoint sub-windows and their sum never exceeds `elapsed`. Called
    /// at both stamping tails (the dispatch tail and the lazy closure
    /// inside the EXPLAIN stage), so the policy lives here once instead of
    /// being re-spelled per tail.
    pub(crate) fn stamped(
        started: Instant,
        windows: DispatchWindows,
        admission: AdmissionTiming,
    ) -> Self {
        Self {
            elapsed: started.elapsed(),
            phases: PhaseTimings::complete(
                admission.parse,
                admission.plan,
                windows.execute,
                admission.stages,
                windows.result_generation,
            ),
            plan_cache: admission.plan_cache,
        }
    }
}

/// The sample statements every phase-timing test runs — one per
/// phase-carrying path (a plain read, a property read, an aggregate, an
/// `EXPLAIN` whose render is the EXECUTE stage, a `PROFILE` with its
/// epilogue) over the one-node fixture each test sets up. Consulted by
/// this module's Duration-level test and `result`'s accessor-level test,
/// so the corpus has one spelling.
#[cfg(test)]
pub(crate) const SAMPLE_STATEMENTS: [&str; 5] = [
    "RETURN 1 AS one",
    "MATCH (n:Person) WHERE n.name = 'a' RETURN n.name",
    "MATCH (n) RETURN count(n) AS people",
    "EXPLAIN MATCH (n:Person) RETURN n",
    "PROFILE MATCH (n:Person) RETURN n.name",
];

#[cfg(test)]
mod tests {
    use super::SAMPLE_STATEMENTS;
    use crate::Database;

    /// Invariant: the phases partition the measured window — PARSE, PLAN
    /// and EXECUTE are disjoint sub-windows of the statement body the
    /// ADR-0221 `elapsed` spans (the between-stages work stays in the
    /// total alone), so their sum never exceeds `elapsed`. Pinned over
    /// sample statements covering every phase-carrying path: a plain
    /// read, a write, an aggregate, an `EXPLAIN` (render as the EXECUTE
    /// stage) and a `PROFILE` (normal dispatch plus the epilogue).
    #[test]
    fn phase_sum_never_exceeds_elapsed_for_sample_statements() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query("CREATE (:Person {name: 'a'})").unwrap();
        for statement in SAMPLE_STATEMENTS {
            let result = conn.query(statement).unwrap();
            let phases = result.header.phases;
            let sum = phases.parse + phases.plan + phases.execute;
            assert!(
                sum <= result.header.elapsed,
                "{statement}: parse {phases:?} sum {sum:?} exceeds elapsed {:?}",
                result.header.elapsed
            );
        }
    }
}
