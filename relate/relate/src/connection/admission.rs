//! Statement admission (ADR-0248, ADR-0259; the plan-cache stages and
//! the `Admission` bundle they produce, ADR-0503): the one admission
//! sequence every statement path enters — health and failed-transaction
//! gates, the plan-cache lookup stages, the parse-and-plan tail, the
//! admission gates — extracted from `dispatch.rs` so that module keeps
//! the reservation → dispatch epilogue its own doc describes and this
//! one owns everything before it. The cache itself lives in `cache.rs`,
//! the EXPLAIN/PROFILE render in `render.rs`.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use web_time::Instant;

use relate_front::parse_and_lower_statement_with_policy_and_params;
use relate_front::validate::StatementPolicy;
use relate_parser::PreambleMode;
use relate_planner::{
    is_database_admin_command, plan_contains_dynamic_operands, plan_has_bound_float_integer_site,
    LogicalPlan, QueryType,
};

use super::cache::{CacheHit, CacheUse, InsertSeed, Lookup, PlanMemory};
use super::Connection;
use crate::gate::ResultStamp;
use crate::plan_bytes::plan_bytes;
use crate::plan_cache::{HitPayload, MAX_CACHED_STATEMENT_BYTES};
use crate::{Error, Result, Value};

/// What [`Connection::admit_statement`] resolves: the parsed preamble
/// mode plus the gated, classified logical plan — one bundle so
/// `run_query` and `explain_plan` share the single admission sequence —
/// the always-on PARSE / PLAN stage durations of the admission
/// (LABS-832), the first two arms of the phase split, and the statement's
/// clock: `started` (ADR-0221, one measurement around the whole
/// statement) and the commit-timing policy's opt-in flag, both read at
/// the top of the one admission sequence so every statement path — the
/// worker body and the ADR-0504 fast path — stamps identical timing.
/// The third arm — EXECUTE — is measured by the statement's dispatch (or
/// the EXPLAIN render), and the complete split is constructed only at the
/// stamping tail, where the result header takes it.
pub(super) struct Admission {
    /// ADR-0221: one measurement around the whole statement body —
    /// parse, plan, gates, dispatch — stamped onto every result.
    pub(super) started: Instant,
    /// The commit-timing policy's opt-in flag (the write path derives
    /// its parse instant from `started` under this flag; LABS-832's
    /// always-on split lives in the `parse`/`plan` durations instead).
    /// Carrying the flag rather than a second instant holds the
    /// invariant `t_parse == started` by construction.
    pub(super) timing_on: bool,
    /// The statement preamble. `EXPLAIN` and `PROFILE` are mutually
    /// exclusive — the parser rejects a second one — so the admission
    /// carries one mode. Either non-execute mode renders instead of
    /// inserting a cache entry, and a hit is always [`PreambleMode::Execute`].
    pub(super) preamble: PreambleMode,
    /// The plan-derived administration fact, resolved once at admission
    /// (ADR-0116 §2): the EXPLAIN arm, the PROFILE epilogue, the dispatch
    /// target selection and the notification view all read this one value
    /// instead of re-deriving it from the plan.
    pub(super) admin: bool,
    /// The plan, shared (ADR-0503): a cache hit hands out the entry's
    /// plan as a pointer clone — an `Arc`, so the fast path's
    /// by-value `Admission` moves a pointer, and every `&LogicalPlan`
    /// consumer derefs through it.
    pub(super) logical: Arc<LogicalPlan>,
    pub(super) stamp: ResultStamp,
    /// The admission's measured timing facts (LABS-832 + LABS-1120) as the
    /// one carrier — the PARSE / PLAN durations, the fine sub-split and
    /// the plan-cache outcome — never loose same-typed fields beside it,
    /// so the facts cannot drift between shapes on the way to the
    /// stamping tail.
    pub(super) timing: crate::phase_timing::AdmissionTiming,
    /// ADR-0503: the hit path's already-merged effective parameter map —
    /// the caller's own plus the extracted `$__AUTO_*__` values, merged
    /// once where the gates ran under it, so every consumer after
    /// admission sees the values exactly as a hand-written parameterized
    /// statement would and the caller merges nothing. `None` on every
    /// other path: the caller's undisturbed map IS the effective one.
    pub(super) effective: Option<BTreeMap<String, Value>>,
    /// Where this plan's retained bytes are charged. A hit is already on
    /// the cache ledger. An original compile is charged on the statement.
    /// A cacheable miss is charged on the cache ledger after the statement
    /// reservation, when the ceiling has room.
    pub(super) memory: PlanMemory,
}

impl Connection {
    /// The one statement-admission sequence (ADR-0259): the health gate,
    /// the failed-transaction gate, parse + front-end lowering, then the
    /// plan gates and classification — and the statement's clock, armed
    /// at the top so `started` covers every stage below (ADR-0221).
    /// `run_query` and `explain_plan`'s worker body both enter here — one
    /// canonical entry, so no statement path can skip a gate. Errors
    /// surface through the caller's usual epilogue (the transaction fails
    /// there).
    pub(super) fn admit_statement(
        &mut self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        cache: CacheUse,
    ) -> Result<Admission> {
        // The clock, armed before the first gate so the ADR-0221 total
        // covers admission itself: `started` for every result, and the
        // commit-timing flag when the store's policy is on
        // (the opt-in window the write path consumes; LABS-832's
        // always-on split is the `parse`/`plan` durations below).
        let started = Instant::now();
        // The commit-timing window opens with the statement; the flag is
        // the policy opt-in, and the write path derives the instant from
        // `started` under it — no second clock reading exists.
        let timing_on = self.inner.timing_on.load(Ordering::Relaxed);
        self.inner.check_healthy()?;
        if self.txn.as_ref().is_some_and(|t| t.failed.get()) {
            return Err(Error::semantic("transaction failed; rollback required"));
        }
        // ADR-0371: the store's RELATE_PREVIEW-resolved set threads
        // into the parse -- a gated preview construct (CIP-278 COMBINE)
        // fails closed with the authored disabled-feature error here,
        // before anything of it plans or executes. ADR-0378: the set has one
        // owner, the store-level `StoreServices` bundle, so admission reads
        // it from the graph under a read guard rather than keeping a second
        // copy on `Shared`.
        let preview = {
            let _read = self.inner.read_guard()?;
            self.inner.graph.preview()
        };
        // LABS-832: the PARSE phase — the cache lookup stages (ADR-0503),
        // then on everything but a hit the front-end parse and lowering,
        // measured always-on (the admission's own `timing_on` flag above
        // serves the separate commit-timing policy).
        let parse_started = Instant::now();
        // ADR-0503: the lookup decides only the uncached tail's seed —
        // every variant named, so a variant added later fails to compile
        // here instead of silently falling into a catch-all. A hit stages
        // its admission directly in [`Self::admit_hit`] and returns it; a
        // miss seeds the insert; a skip enters the tail with no seed. One
        // tail call serves the last two alike.
        let preamble = AdmitPreamble { started, timing_on };
        let tail = match self.cache_lookup(cypher, params, cache, preview) {
            Lookup::Hit(hit) => {
                return self.admit_hit(cypher, params, hit, preview, parse_started, preamble)
            }
            Lookup::Miss(seed) => UncachedTail::SeededMiss(seed),
            Lookup::Skip => UncachedTail::Bypassed,
        };
        self.admit_uncached(cypher, params, preview, tail, parse_started, preamble)
    }

    /// The cache-hit admission path (ADR-0503 stage 3): the entry's
    /// handles ride out of the lookup, so an eviction after it cannot
    /// fail the hit (the Arcs keep the plan valid; freshness is the
    /// generation check under the guard), and the caller receives the
    /// already-merged effective map (it merges nothing). The lookup was
    /// the whole PARSE window, so the fine sub-split stamps the true
    /// zero and the outcome explains it; the literal-shape
    /// re-verification below runs inside the PLAN window — armed before
    /// it, closed after — so its cost stays measured in the coarse
    /// `plan` duration, never hidden from the envelope.
    fn admit_hit(
        &self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        hit: HitPayload,
        preview: relate_core::PreviewFeatures,
        parse_started: Instant,
        preamble: AdmitPreamble,
    ) -> Result<Admission> {
        let parse = parse_started.elapsed();
        // LABS-832: the PLAN phase — the generation check, a possible
        // replan from the carried AST, the literal-shape re-verification,
        // and the admission gates.
        let plan_started = Instant::now();
        let CacheHit {
            plan,
            stamp,
            effective,
            outcome,
        } = self.admit_from_cache(cypher, hit.handles, hit.stage, &hit.values, params)?;
        // A shared plan was compiled for one literal value. A later
        // statement on the same template can be a float SKIP, a
        // non-boolean operand, or a float slice bound. Those fail
        // on the statement as written; the cached plan is left in
        // place and this run takes the original compile.
        let policy = StatementPolicy {
            safe_mode: self.inner.resolved.safe_mode,
            max_traversal_depth: self.inner.resolved.max_traversal_depth,
        };
        let literal_rejected =
            parse_and_lower_statement_with_policy_and_params(cypher, preview, policy, params)
                .is_err()
                || plan_has_bound_float_integer_site(&plan, &effective);
        if literal_rejected {
            // LABS-1120: the hit was rejected for literal-shape
            // safety — the statement runs an original compile, so
            // no cached plan served it: the outcome reports the
            // miss that the run was. (The rejection itself never
            // reaches a result: a statement whose literals fail
            // the shape rules fails its own original compile too,
            // and an admission failure carries no result header.)
            return self.admit_uncached(
                cypher,
                params,
                preview,
                UncachedTail::RejectedHit,
                parse_started,
                preamble,
            );
        }
        Ok(stage_admission(
            preamble,
            PreambleMode::Execute,
            plan,
            stamp,
            crate::phase_timing::AdmissionTiming {
                parse,
                plan: plan_started.elapsed(),
                // LABS-1120: the hit path's PARSE window was the lookup
                // alone, so the fine sub-split is the true zero and the
                // outcome explains it — the literal-shape re-verification
                // above rides the coarse PLAN window instead.
                stages: relate_front::ParseStages::ZERO,
                plan_cache: outcome,
            },
            Some(effective),
            PlanMemory::Cached,
        ))
    }

    /// The uncached admission tail (ADR-0259's parse-and-plan stages, with
    /// ADR-0503's parameterized compile): a seed-bearing miss compiles the
    /// rewritten statement once and, when that plan is eligible, that is
    /// the plan that runs. The cache insert itself waits until the
    /// statement has reserved workspace. Anything that does not
    /// participate — or whose rewritten compile fails — compiles the
    /// original text once.
    fn admit_uncached(
        &self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        preview: relate_core::PreviewFeatures,
        tail: UncachedTail,
        parse_started: Instant,
        preamble: AdmitPreamble,
    ) -> Result<Admission> {
        let cache = tail.outcome();
        let seed = tail.seed();
        // LABS-610: the traversal-depth admission limit resolved at open
        // rides the parse so a hop bound above it is a typed syntax error
        // here, before the walker runs. LABS-680 (ADR-0477): the resolved
        // safe-mode flag rides the same policy carrier, so the shape rules
        // refuse an agent-typical statement at the lowering boundary.
        // LABS-709: the bound parameters ride it too, so a parameter-typed
        // dynamic label/property key of the wrong type fails here (Neo4j
        // treats parameters as compile-time constants) rather than hiding
        // behind a zero-row MATCH.
        // The policy is derived once and shared by both parses — the
        // admission's own and the cache insert's re-parse of the
        // rewritten text — so the two cannot drift (found in review).
        let policy = StatementPolicy {
            safe_mode: self.inner.resolved.safe_mode,
            max_traversal_depth: self.inner.resolved.max_traversal_depth,
        };
        if let Some(seed) = seed {
            if let Some(compiled) =
                self.compile_parameterized(cypher, preview, policy, params, seed, parse_started)?
            {
                let admission = stage_admission(
                    preamble,
                    compiled.preamble,
                    Arc::clone(&compiled.logical),
                    compiled.stamp,
                    crate::phase_timing::AdmissionTiming {
                        parse: compiled.parse,
                        plan: compiled.plan,
                        stages: compiled.stages,
                        plan_cache: cache,
                    },
                    Some(compiled.effective),
                    PlanMemory::Cacheable(compiled.pending),
                );
                if cache_insert_eligible(&admission, cypher) {
                    return Ok(admission);
                }
                // The parameterized plan is not what this statement runs.
                // Drop it and compile the original text below. `preamble`
                // is `Copy`, so the original tail still has its clock.
                drop(admission);
            }
        }
        let parsed =
            parse_and_lower_statement_with_policy_and_params(cypher, preview, policy, params)?;
        let parse = parse_started.elapsed();
        let mode = parsed.preamble;
        // LABS-832: the PLAN phase — binding, logical planning and the
        // admission classification, measured always-on.
        let plan_started = Instant::now();
        let (logical, stamp) = self.plan_gate_classify(parsed.query, params)?;
        let plan = plan_started.elapsed();
        let logical = Arc::new(logical);
        let bytes = plan_bytes(&logical);
        Ok(stage_admission(
            preamble,
            mode,
            logical,
            stamp,
            crate::phase_timing::AdmissionTiming {
                parse,
                plan,
                stages: parsed.stages,
                plan_cache: cache,
            },
            None,
            PlanMemory::Statement(bytes),
        ))
    }
}

/// The clock facts `admit_statement` arms before its first gate, bundled
/// so they thread to the uncached tail and the one construction as one
/// named carrier — the connection module's convention for stage
/// hand-offs (`DispatchCtx`, `TxnRun`) — instead of loose same-typed
/// positionals whose swap would compile (found in review).
#[derive(Clone, Copy)]
struct AdmitPreamble {
    started: Instant,
    timing_on: bool,
}

/// The cache lookup's resolution for the uncached tail (LABS-1120),
/// modelled as exactly the reachable states — never a nullable pair, in
/// which a seed beside a non-`Miss` outcome would compile but cannot
/// occur: only a seed-bearing miss carries a seed; a hit rejected for
/// literal-shape safety and a bypass both compile the original text with
/// no insert.
enum UncachedTail {
    /// A seed-bearing miss (ADR-0503 stage 2): the tail compiles the
    /// rewritten statement, and when eligible that plan runs and inserts.
    SeededMiss(InsertSeed),
    /// A hit rejected for literal-shape safety: the original text
    /// compiles, nothing inserts — no cached plan served, so the outcome
    /// is `miss`.
    RejectedHit,
    /// The cache was not consulted (the plan-tree bypass, a caller
    /// parameter map carrying the reserved prefix, a non-cacheable
    /// statement): the original text compiles, nothing inserts.
    Bypassed,
}

impl UncachedTail {
    /// The insert seed, present only on the seeded-miss arm.
    fn seed(self) -> Option<InsertSeed> {
        match self {
            Self::SeededMiss(seed) => Some(seed),
            Self::RejectedHit | Self::Bypassed => None,
        }
    }

    /// The plan-cache outcome the tail stamps.
    fn outcome(&self) -> crate::PlanCacheOutcome {
        match self {
            Self::SeededMiss(_) | Self::RejectedHit => crate::PlanCacheOutcome::Miss,
            Self::Bypassed => crate::PlanCacheOutcome::Bypassed,
        }
    }
}

/// The one `Admission` construction (ADR-0503): both admission paths —
/// the cache hit and the uncached tail — end here, so the bundle's
/// invariants (the admin fact derived once from the plan, ADR-0116 §2)
/// have one spelling and the struct cannot drift between paths.
fn stage_admission(
    preamble: AdmitPreamble,
    mode: PreambleMode,
    logical: Arc<LogicalPlan>,
    stamp: ResultStamp,
    timing: crate::phase_timing::AdmissionTiming,
    effective: Option<BTreeMap<String, Value>>,
    memory: PlanMemory,
) -> Admission {
    Admission {
        started: preamble.started,
        timing_on: preamble.timing_on,
        preamble: mode,
        admin: is_database_admin_command(&logical),
        logical,
        stamp,
        timing,
        effective,
        memory,
    }
}

/// ADR-0503's insert eligibility, read from the finished admission — the
/// one predicate that owns every exclusion, so "what gets cached?" has
/// one home: no `EXPLAIN`/`PROFILE` preamble, the statement text within
/// [`MAX_CACHED_STATEMENT_BYTES`] (the memory bound), a read-classified
/// statement (`r`, and not write-routed — an ontology write procedure can
/// classify `r`). The session's conservative CALL { … }/FOREACH
/// exclusions (research note §7) ride the classification itself: the
/// planner folds a `CALL { … }` subquery as `Access::WRITE` and a
/// `FOREACH` as a write clause, so neither can classify `r` — the
/// exclusion is enforced where it is owned, never re-encoded here.
/// Writes never insert and self-invalidate every reader through the
/// generation bump. One more exclusion, found in review: a plan carrying
/// a **dynamic name operand** (dynamic label or property key) never
/// inserts — the front validates such operands against bound *values* at
/// lowering time (LABS-709, with a content half an empty or null-byte
/// name beyond its type half), and a hit skips lowering, so caching it
/// would let a later statement's values escape that admission check.
/// Such statements stay uncached, never silently unvalidated.
pub(super) fn cache_insert_eligible(admission: &Admission, cypher: &str) -> bool {
    admission.preamble == PreambleMode::Execute
        && cypher.len() <= MAX_CACHED_STATEMENT_BYTES
        && admission.stamp.qtype == QueryType::Read
        && !admission.stamp.write_route
        && !plan_contains_dynamic_operands(&admission.logical)
}
