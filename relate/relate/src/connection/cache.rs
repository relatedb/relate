//! The plan cache's admission stages (ADR-0503), extracted from
//! `dispatch.rs` to keep that file under the repository's file-size
//! budget: the lookup stages, the under-guard hit admission, and the
//! insert. The admission *sequence* — where these stages sit among the
//! health gate, the parse and the plan gates, with the
//! insert-eligibility predicate beside its caller — lives in
//! `Connection::admit_statement` (`admission.rs`); this module owns what
//! each stage does.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;

use relate_front::parse_and_lower_statement_with_policy_and_params;
use relate_front::validate::StatementPolicy;
use relate_ir::Query;
use relate_parser::PreambleMode;
use relate_planner::LogicalPlan;

use super::Connection;
use crate::gate::ResultStamp;
use crate::plan_bytes::entry_bytes;
use crate::plan_cache::{EntryHandles, HitPayload};
use crate::token_budget::PlanHold;
use crate::{Result, Value};

/// Whether this admission consults the plan cache (ADR-0503): the query
/// paths look up and insert; `explain_plan` bypasses both, so the plan
/// tree keeps spelling the statement as written (its seek lines' own
/// contract). `EXPLAIN`/`PROFILE`-prefixed statements need no bypass —
/// their templates carry the prefix, so they can never hit an entry, and
/// eligibility never inserts one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheUse {
    /// The normal query path: lookup, and insert when eligible.
    Query,
    /// The plan-tree path: no lookup, no insert.
    Bypass,
}

/// What the plan-cache lookup stages (ADR-0503) resolved to, before any
/// publication guard: stage 1 (the raw string) and stage 2 (the
/// auto-parameterized template) run without the guard — only the
/// generation check needs it, and only a hit pays for it.
#[derive(Debug)]
pub(super) enum Lookup {
    /// A live entry, as the one hit payload ([`HitPayload`]: the entry's
    /// shared handles — template, lowered AST, compiled plan, stats
    /// generation — and this statement's extracted literal values, all
    /// pointer clones of the entry's own storage, so stage 3 never
    /// re-finds the entry and an eviction after the lookup cannot
    /// invalidate the hit; the values are the entry's own shared slice
    /// on a raw hit, the normalizer's fresh vector on a template hit).
    Hit(HitPayload),
    /// No entry yet, but the statement normalized cleanly: the insert
    /// seed.
    Miss(InsertSeed),
    /// No cache participation — a bypass, a reserved-prefix caller, or a
    /// non-cacheable statement: no lookup hit, and no insert later.
    Skip,
}

/// The insert seed a seed-bearing miss hands the uncached tail
/// (ADR-0503): the lookup's template triple — everything
/// [`Connection::insert_cache_entry`] needs beyond the admission's own
/// facts. A bypass or a skip carries no seed (`None`): no insert.
#[derive(Debug)]
pub(super) struct InsertSeed {
    pub(super) template: String,
    pub(super) rewritten: String,
    pub(super) values: Vec<(String, Value)>,
}

/// What a cache hit resolved to (stage 3's outcome): the shared plan,
/// its stamp, the already-built effective parameter map — the caller
/// merges nothing, clones nothing — and the hit's plan-cache outcome
/// (LABS-1120): which stage served, and whether the generation check
/// re-planned.
pub(super) struct CacheHit {
    pub(super) plan: Arc<LogicalPlan>,
    pub(super) stamp: ResultStamp,
    pub(super) effective: BTreeMap<String, Value>,
    pub(super) outcome: crate::PlanCacheOutcome,
}

/// How the plan about to run is charged against the workspace pool.
pub(super) enum PlanMemory {
    /// A cache hit. The cache ledger already holds the plan.
    Cached,
    /// An original compile. `bytes` is reserved on the statement, under
    /// the admission ceiling, before execution.
    Statement(u64),
    /// A parameterized plan. After the statement reservation succeeds,
    /// the cache ledger takes `pending.bytes` when the ceiling has room.
    Cacheable(PendingCache),
}

/// The parameterized compile a cacheable miss executes and, when the
/// pool has room, retains.
pub(super) struct PendingCache {
    pub(super) template: String,
    pub(super) query: Arc<Query>,
    pub(super) plan: Arc<LogicalPlan>,
    pub(super) generation: u64,
    pub(super) text: String,
    pub(super) values: Vec<(String, Value)>,
    pub(super) bytes: u64,
}

impl Connection {
    /// The plan-cache lookup stages (ADR-0503): stage 1 — the raw string
    /// against each entry's last raw hit, no normalizer pass; stage 2 —
    /// the auto-parameterized template, the real key, from the
    /// lexer-level normalizer. Both run without the publication guard —
    /// only the generation check of stage 3 needs it, and only a hit pays
    /// for it — and each lock section is a short data operation: the
    /// normalizer's tokenize runs between them, never under a lock.
    pub(super) fn cache_lookup(
        &self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        cache: CacheUse,
        preview: relate_core::PreviewFeatures,
    ) -> Lookup {
        if cache == CacheUse::Bypass {
            return Lookup::Skip;
        }
        // A caller parameter map carrying the reserved prefix disables the
        // cache for the statement entirely — no stage, not even the raw
        // lookup: the caller can never collide with the engine's own
        // placeholder names, and a raw hit would otherwise serve a cached,
        // auto-parameterized plan beside a caller map the design excludes.
        if relate_parser::params_carry_reserved_prefix(params) {
            return Lookup::Skip;
        }
        // Stage 1: the raw string.
        if let Some(raw) = crate::plan_cache::lock(&self.inner).lookup_raw(cypher) {
            return Lookup::Hit(raw);
        }
        // Stage 2: the template, normalized without any lock held. The
        // participation rule (the reserved-prefix caller check above) is
        // decided here and only here — the front's normalize is a pure
        // mapping.
        match relate_front::normalize_statement(cypher, preview) {
            relate_parser::NormalizeOutcome::NotCacheable => Lookup::Skip,
            relate_parser::NormalizeOutcome::Normalized(normalized) => {
                let values = crate::plan_cache::engine_values(normalized.values);
                match crate::plan_cache::lock(&self.inner).find_template(&normalized.template) {
                    Some(handles) => Lookup::Hit(HitPayload {
                        handles,
                        values: Arc::new(values),
                        stage: crate::PlanCacheOutcome::HitTemplate,
                    }),
                    None => Lookup::Miss(InsertSeed {
                        template: normalized.template,
                        rewritten: normalized.rewritten,
                        values,
                    }),
                }
            }
        }
    }

    /// Stage 3 of the cache lookup (ADR-0503), under the publication read
    /// guard: check the stats generation the lookup carried — re-planning
    /// from the carried AST on mismatch, Neo4j's replan-on-stats-drift
    /// equivalent — and run the admission gates on the resulting plan,
    /// per statement, exactly as a fresh one. The entry's handles were
    /// carried out of the lookup, so no re-find happens here and no
    /// eviction can fail the hit: the `Arc`s keep the plan valid past
    /// eviction, and freshness is the generation check, not the entry's
    /// presence. Every plan-cache lock section is a short data operation
    /// — the replan runs between them, never under the lock (another
    /// connection's lookup must never queue behind a replan).
    pub(super) fn admit_from_cache(
        &self,
        cypher: &str,
        handles: EntryHandles,
        stage: crate::PlanCacheOutcome,
        values: &[(String, Value)],
        params: &BTreeMap<String, Value>,
    ) -> Result<CacheHit> {
        let _read = self.inner.read_guard()?;
        // The effective parameters: the caller's map plus this statement's
        // extracted literal values — the gates below (and every consumer
        // after admission) see the values exactly as a hand-written
        // parameterized statement would.
        let effective = effective_params(params, values);
        let current = self.inner.stats_generation.current();
        let EntryHandles {
            template,
            query,
            mut plan,
            generation,
        } = handles;
        // LABS-1120: the outcome starts as the lookup's hit flavor (the
        // payload's own typed value) and is overwritten only by the
        // generation check below — the two facts that made this hit what
        // it is, derived once, here.
        let mut outcome = stage;
        if generation != current {
            // The store committed since this plan was compiled: the
            // carried AST re-plans against fresh stats, and the entry
            // updates itself only if it is still present — outside every
            // lock.
            let replanned = Arc::new(self.plan_under_guard((*query).clone(), &effective)?);
            crate::plan_cache::lock(&self.inner).update_plan(
                &template,
                Arc::clone(&replanned),
                current,
            );
            plan = replanned;
            outcome = crate::PlanCacheOutcome::ReplanGenerationMismatch;
        }
        // Remember this text and its values: the next identical text
        // resolves through stage 1 without the normalizer pass. The
        // helper is idempotent — a stage-1 hit's entry already carries
        // exactly this text and these values, so its re-record is a
        // compare-and-skip, never an allocation.
        crate::plan_cache::lock(&self.inner).note_raw(&template, cypher, values);
        let stamp = self.gates_classify_under_guard(&plan, &effective)?;
        // The effective map rides out with the plan: the caller merges
        // nothing, clones nothing — the one derivation happened here.
        Ok(CacheHit {
            plan,
            stamp,
            effective,
            outcome,
        })
    }

    /// Compile the rewritten statement once (ADR-0503). `None` when that
    /// compile fails: the caller plans the original text instead, and the
    /// statement stays uncached. The generation is read under the same
    /// publication guard as the plan.
    pub(super) fn compile_parameterized(
        &self,
        cypher: &str,
        preview: relate_core::PreviewFeatures,
        policy: StatementPolicy,
        params: &BTreeMap<String, Value>,
        seed: InsertSeed,
        parse_started: Instant,
    ) -> Result<Option<ParameterizedCompile>> {
        let InsertSeed {
            template,
            rewritten,
            values,
        } = seed;
        let effective = effective_params(params, &values);
        let Ok(parsed) = parse_and_lower_statement_with_policy_and_params(
            &rewritten, preview, policy, &effective,
        ) else {
            return Ok(None);
        };
        let parse = parse_started.elapsed();
        let plan_started = Instant::now();
        let _read = self.inner.read_guard()?;
        let generation = self.inner.stats_generation.current();
        let Ok(plan) = self.plan_under_guard(parsed.query.clone(), &effective) else {
            return Ok(None);
        };
        let Ok(stamp) = self.gates_classify_under_guard(&plan, &effective) else {
            return Ok(None);
        };
        let plan_dur = plan_started.elapsed();
        let query = Arc::new(parsed.query);
        let plan = Arc::new(plan);
        let bytes = entry_bytes(&template, &query, &plan, cypher, &values);
        Ok(Some(ParameterizedCompile {
            preamble: parsed.preamble,
            logical: Arc::clone(&plan),
            stamp,
            parse,
            plan: plan_dur,
            stages: parsed.stages,
            effective,
            pending: PendingCache {
                template,
                query,
                plan,
                generation,
                text: cypher.to_owned(),
                values,
                bytes,
            },
        }))
    }

    /// Publish a parameterized plan onto the cache. `hold` is the cache
    /// ledger's reservation; `None` when admission is off.
    pub(super) fn publish_cache_entry(&self, pending: PendingCache, hold: Option<PlanHold>) {
        let PendingCache {
            template,
            query,
            plan,
            generation,
            text,
            values,
            bytes: _,
        } = pending;
        let mut cache = crate::plan_cache::lock(&self.inner);
        cache.insert(crate::plan_cache::PlanCacheEntry {
            handles: EntryHandles {
                template: template.into(),
                query,
                plan,
                generation,
            },
            raw: crate::plan_cache::RawStatement {
                text: text.into(),
                values: Arc::new(values),
            },
            hold,
        });
    }
}

/// One successful parameterized compile, before eligibility decides
/// whether it is the plan that runs.
pub(super) struct ParameterizedCompile {
    pub(super) preamble: PreambleMode,
    pub(super) logical: Arc<LogicalPlan>,
    pub(super) stamp: ResultStamp,
    pub(super) parse: Duration,
    pub(super) plan: Duration,
    /// The rewritten statement's PARSE fine sub-split (LABS-1120) — the
    /// compile's own parse, riding like the coarse durations do.
    pub(super) stages: relate_front::ParseStages,
    pub(super) effective: BTreeMap<String, Value>,
    pub(super) pending: PendingCache,
}

/// The statement's effective parameters (ADR-0503): the caller's map plus
/// the extracted `$__AUTO_*__` values — the one derivation shared by the
/// insert (which plans the rewritten text under it) and every hit (whose
/// gates, replan and dispatch must reconstruct exactly that set).
fn effective_params(
    params: &BTreeMap<String, Value>,
    values: &[(String, Value)],
) -> BTreeMap<String, Value> {
    let mut effective = params.clone();
    effective.extend(values.iter().cloned());
    effective
}
