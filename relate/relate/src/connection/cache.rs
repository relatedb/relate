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
    /// The cache-hit inline route's probe (ADR-0663): stage 1 of the
    /// lookup only — the raw string, a map probe that cannot recurse
    /// with statement nesting. Stage 2's normalizer is part of the
    /// front: it walks the statement to its nesting depth (ADR-0257's
    /// cap the bound), the recursion the query worker's 64 MiB stack
    /// exists for — the probe never runs it on the caller's thread. A
    /// probe miss is the ordinary cold path: the worker's full admission
    /// re-runs every stage, including the normalizer, on its own stack.
    /// Probe-only: `admit_statement` is never called with it (the probe
    /// drops its cold tail instead of running it).
    RawProbe,
}

/// What the plan-cache lookup stages (ADR-0503) resolved to, before any
/// publication guard: stage 1 (the raw string) and stage 2 (the
/// auto-parameterized template) run without the guard — only the
/// footprint and epoch check needs it, and only a hit pays for it.
#[derive(Debug)]
pub(super) enum Lookup {
    /// A live entry, as the one hit payload ([`HitPayload`]: the entry's
    /// shared handles — template, lowered AST, compiled plan, token
    /// footprint and its stamps — and this statement's extracted literal
    /// values, all pointer clones of the entry's own storage, so stage 3
    /// never re-finds the entry and an eviction after the lookup cannot
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
/// merges nothing, clones nothing — the hit's plan-cache outcome
/// (LABS-1120: which stage served, and whether admission re-planned), and
/// the entry's template, the key the execution's plan grade is recorded
/// against (ADR-0687 decision 6).
pub(super) struct CacheHit {
    pub(super) plan: Arc<LogicalPlan>,
    pub(super) stamp: ResultStamp,
    pub(super) effective: BTreeMap<String, Value>,
    pub(super) outcome: crate::PlanCacheOutcome,
    pub(super) template: Arc<str>,
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
    /// Boxed: the pending bundle carries a whole `EntryHandles`, and this
    /// enum rides inside every `Admission` — keeping the one rare variant
    /// behind a pointer leaves the bundle whole without pricing every
    /// admission for it.
    Cacheable(Box<PendingCache>),
}

/// The parameterized compile a cacheable miss executes and, when the
/// pool has room, retains. It carries the entry's own handles — the
/// template, AST, plan, footprint, stamps and statistics stamp as one
/// bundle, built once in [`Connection::compile_parameterized`] — so the
/// publish is a move, never a field-by-field transcription of a parallel
/// declaration: a new `EntryHandles` field rides through by construction.
pub(super) struct PendingCache {
    pub(super) handles: EntryHandles,
    pub(super) text: String,
    pub(super) values: Vec<(String, Value)>,
    pub(super) bytes: u64,
}

/// What ADR-0687's admission check decided for one cache hit — the
/// taxonomy the epoch, absent-name and footprint rules resolve to before
/// any work runs.
enum HitAdmission {
    /// No footprint token changed since the plan: serve it as carried.
    Serve,
    /// A footprint number changed with drift inside
    /// [`crate::plan_footprint::DRIFT_FACTOR`]: serve the plan and
    /// re-stamp the entry at the current generation — clean.
    Restamp,
    /// Wholesale change (the epoch moved), the per-key statistics the plan
    /// read changing (ADR-0700), a commit since an absent-name resolution,
    /// or a dirty entry whose measured grade reached its factor (ADR-0687
    /// decision 6): rebuild the plan from the carried AST.
    Replan,
    /// A footprint number changed with drift at or past the factor:
    /// serve the plan and mark the entry dirty — the plan grade decides a
    /// later re-plan (ADR-0687 decisions 5-6).
    MarkDirty,
}

impl Connection {
    /// The plan-cache lookup stages (ADR-0503): stage 1 — the raw string
    /// against each entry's last raw hit, no normalizer pass; stage 2 —
    /// the auto-parameterized template, the real key, from the
    /// lexer-level normalizer. Both run without the publication guard —
    /// only the footprint and epoch check of stage 3 needs it, and only a
    /// hit pays for it — and each lock section is a short data operation:
    /// the normalizer's tokenize runs between them, never under a lock.
    pub(super) fn cache_lookup(
        &self,
        cypher: &str,
        params: &BTreeMap<String, Value>,
        cache: CacheUse,
        preview: relate_core::PreviewFeatures,
    ) -> Lookup {
        // A caller parameter map carrying the reserved prefix disables the
        // cache for the statement entirely — no stage, not even the raw
        // lookup: the caller can never collide with the engine's own
        // placeholder names, and a raw hit would otherwise serve a cached,
        // auto-parameterized plan beside a caller map the design excludes.
        if relate_parser::params_carry_reserved_prefix(params) {
            return Lookup::Skip;
        }
        // The consulting modes, every variant named — a fourth
        // `CacheUse` fails this match instead of silently falling into
        // a lookup stage (the module's own exhaustiveness convention).
        // The probe's arm (ADR-0663) is stage 1 only, never the
        // normalizing stage 2; a miss is `Skip`: for the probe that is
        // simply "cold" (its caller drops the tail), and the probe is
        // never the inserting path.
        match cache {
            // The plan-tree path: no lookup, no insert. The match owns
            // the bypass answer — one home, ahead of the guard above
            // only in position, never in effect (both return `Skip`).
            CacheUse::Bypass => Lookup::Skip,
            CacheUse::RawProbe => self.raw_lookup(cypher).map_or(Lookup::Skip, Lookup::Hit),
            CacheUse::Query => {
                // Stage 1: the raw string.
                if let Some(raw) = self.raw_lookup(cypher) {
                    return Lookup::Hit(raw);
                }
                self.lookup_template(cypher, preview)
            }
        }
    }

    /// Stage 1 of the cache lookup: the raw string against each entry's
    /// last raw text — the one spelling of the raw lookup, shared by the
    /// query path and the probe's raw-only resolution (ADR-0663).
    fn raw_lookup(&self, cypher: &str) -> Option<crate::plan_cache::HitPayload> {
        crate::plan_cache::lock(&self.inner).lookup_raw(cypher)
    }

    /// Stage 2 of the cache lookup: the auto-parameterized template.
    fn lookup_template(&self, cypher: &str, preview: relate_core::PreviewFeatures) -> Lookup {
        // Stage 2: the template, normalized without any lock held. The
        // participation rule (the reserved-prefix caller check above) is
        // decided here and only here — the front's normalize is a pure
        // mapping. Both declines are one guard-chain shape: a statement
        // the normalizer refuses as non-cacheable, and one whose payload
        // literal fails the value-depth bound (ADR-0462) — the uncached
        // tail compiles the original text and reports the error the
        // uncached run reports, never a silently over-deep value riding
        // the payload (REL8-1120).
        let relate_parser::NormalizeOutcome::Normalized(normalized) =
            relate_front::normalize_statement(cypher, preview)
        else {
            return Lookup::Skip;
        };
        let Some(values) = crate::plan_cache::engine_values(normalized.values) else {
            return Lookup::Skip;
        };
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

    /// Stage 3 of the cache lookup (ADR-0503, ADR-0687, ADR-0700), under
    /// the publication read guard: admit the plan the lookup carried — the
    /// full-invalidation epoch must match and, when the plan read the
    /// per-key statistics, the statistics epoch must match too; a
    /// footprint number changed since the plan is judged by its
    /// cardinality drift (inside the threshold the entry is re-stamped and
    /// serves clean, at or past it the entry is marked dirty and serves),
    /// and a dirty entry re-plans once its measured plan grade degraded —
    /// while an epoch mismatch, a statistics publication past the entry's
    /// stamp, or an absent-name commit re-plans from the carried AST
    /// (Neo4j's replan-on-stats-drift equivalent) — and run the admission
    /// gates on the resulting plan, per statement, exactly as a fresh one.
    /// The entry's handles were carried out of the lookup, so no re-find
    /// happens here and no eviction can fail the hit: the `Arc`s keep the
    /// plan valid past eviction, and freshness is the footprint, epoch,
    /// statistics and grade checks, not the entry's presence. Every
    /// plan-cache lock section is a short data operation — the re-plan
    /// runs between them, never under the lock (another connection's
    /// lookup must never queue behind a re-plan).
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
        let stamps = self.stamps_under_guard();
        let current = stamps.generation;
        // The per-key statistics epoch (ADR-0700), read under the same
        // publication read guard the stamps were taken under.
        let key_stats_epoch_now = self.inner.graph.key_stats_epoch();
        let stats_fresh = handles.key_stats_fresh(key_stats_epoch_now);
        let EntryHandles {
            template,
            query,
            mut plan,
            footprint,
            stamps: entry_stamps,
            dirty,
            grade,
            // The statistics stamp is decided by `stats_fresh` above, on
            // the whole bundle, before it is taken apart.
            ..
        } = handles;
        // ADR-0687 decision 1: the epoch and the footprint's newest stamp,
        // one coherent pair under the guard the plan was stamped under. The
        // epoch is the one `stamps_under_guard` already read; only the
        // footprint's newest stamp needs the table itself.
        let epoch_now = stamps.epoch;
        let newest_change = {
            let table = self.inner.lock_token_changes();
            footprint.newest_change(|token| table.last_changed(token))
        };
        // The composed admission check (ADR-0687, ADR-0700), the two
        // unconditional re-plan causes first: the full-invalidation epoch
        // moved (wholesale change), or the per-key statistics the plan read
        // changed (ADR-0700 decision 3 — a plan that read none is never
        // compared). Then an absent-name entry keeps ADR-0503's any-commit
        // rule (decision 3); a dirty entry re-plans once its measured grade
        // degraded (decision 6); and a footprint number changed since the
        // plan is judged by cardinality drift (decision 5). The dirty mark
        // and the grade are the lookup's snapshot of the entry (the
        // carried handles), so the check needs no second lock: a mark
        // another connection lands between the lookup and here is decided
        // on the next hit, which is the same one-hit window the stamps
        // already have.
        let admission = if entry_stamps.epoch != epoch_now || !stats_fresh {
            HitAdmission::Replan
        } else if footprint.absent {
            if entry_stamps.generation == current {
                HitAdmission::Serve
            } else {
                HitAdmission::Replan
            }
        } else if dirty && grade.degraded() {
            HitAdmission::Replan
        } else if newest_change > entry_stamps.generation {
            let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, &effective);
            if footprint.drift_past_threshold(&stats) {
                HitAdmission::MarkDirty
            } else {
                HitAdmission::Restamp
            }
        } else {
            HitAdmission::Serve
        };
        // LABS-1120: the outcome starts as the lookup's hit flavor (the
        // payload's own typed value) and is overwritten only by a re-plan
        // below — the two facts that made this hit what it is, derived
        // once, here.
        let mut outcome = stage;
        match admission {
            HitAdmission::Serve => {}
            HitAdmission::Restamp => {
                // The plan still serves and is clean: re-stamp the entry
                // at the current generation. The recorded numbers stay the
                // plan-time ones, so gradual drift accumulates.
                crate::plan_cache::lock(&self.inner).restamp(&template, current);
            }
            HitAdmission::MarkDirty => {
                // The drift reached the threshold: the plan still serves,
                // and the dirty mark is what the plan grade re-plans on
                // (ADR-0687 decisions 5-6).
                crate::plan_cache::lock(&self.inner).mark_dirty(&template, current);
            }
            HitAdmission::Replan => {
                // Rebuild from the carried AST against fresh stats; the
                // entry refreshes its plan, footprint, generation, epoch
                // and statistics stamp only if it is still present —
                // outside every lock. The plan and the footprint are read
                // under the same guard, so the counts are the ones this
                // re-plan was priced with, and the statistics stamp rides
                // out of that same planning call (a re-plan that read no
                // statistics records `None`, ADR-0700 decision 2).
                let (replanned, key_stats_epoch) =
                    self.plan_stamped_under_guard((*query).clone(), &effective)?;
                let replanned = Arc::new(replanned);
                let footprint = Arc::new(self.footprint_under_guard(&replanned, &effective));
                crate::plan_cache::lock(&self.inner).update_plan(
                    &template,
                    Arc::clone(&replanned),
                    footprint,
                    // The stamps read before the re-plan, so a commit or a
                    // publication landing while it was built moves past
                    // the entry and re-plans it once more.
                    stamps,
                    key_stats_epoch,
                );
                plan = replanned;
                outcome = crate::PlanCacheOutcome::Replan;
            }
        }
        // Remember this text and its values: the next identical text
        // resolves through stage 1 without the normalizer pass. The
        // helper is idempotent — a stage-1 hit's entry already carries
        // exactly this text and these values, so its re-record is a
        // compare-and-skip, never an allocation.
        crate::plan_cache::lock(&self.inner).note_raw(&template, cypher, values);
        let stamp = self.gates_classify_under_guard(&plan, &effective)?;
        // The effective map rides out with the plan: the caller merges
        // nothing, clones nothing — the one derivation happened here. The
        // template rides out too: the execution that follows records its
        // plan grade against this entry (ADR-0687 decision 6).
        Ok(CacheHit {
            plan,
            stamp,
            effective,
            outcome,
            template,
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
        // ADR-0687: the generation and the epoch are read together under
        // one guard, so the entry records one coherent pair. ADR-0700: the
        // statistics stamp comes out of the same planning call, read under
        // that guard before the plan is built.
        let stamps = self.stamps_under_guard();
        let Ok((plan, key_stats_epoch)) =
            self.plan_stamped_under_guard(parsed.query.clone(), &effective)
        else {
            return Ok(None);
        };
        let Ok(stamp) = self.gates_classify_under_guard(&plan, &effective) else {
            return Ok(None);
        };
        // ADR-0687: the footprint is captured under the same guard the
        // plan was compiled under, so its counts are the plan's own.
        let footprint = Arc::new(self.footprint_under_guard(&plan, &effective));
        let plan_dur = plan_started.elapsed();
        // The entry's handles, built once here: the template converted, a
        // fresh plan's dirty/grade defaults set (a re-plan or an insert
        // starts the grade over — ADR-0687 decision 6: the first graded
        // execution records the baseline).
        let handles = EntryHandles {
            template: template.into(),
            query: Arc::new(parsed.query),
            plan: Arc::new(plan),
            footprint,
            stamps,
            key_stats_epoch,
            dirty: false,
            grade: crate::plan_grade::PlanGrade::default(),
        };
        let bytes = entry_bytes(
            &handles.template,
            &handles.query,
            &handles.plan,
            cypher,
            &values,
            &handles.footprint,
        );
        Ok(Some(ParameterizedCompile {
            preamble: parsed.preamble,
            logical: Arc::clone(&handles.plan),
            stamp,
            parse,
            plan: plan_dur,
            stages: parsed.stages,
            effective,
            pending: PendingCache {
                handles,
                text: cypher.to_owned(),
                values,
                bytes,
            },
        }))
    }

    /// The plan's two freshness stamps, read together under the caller's
    /// publication guard (ADR-0687 decision 4): the stats generation and the
    /// full-invalidation epoch as one value, so no call site can pair a
    /// generation with another read's epoch.
    fn stamps_under_guard(&self) -> crate::plan_cache::PlanStamps {
        crate::plan_cache::PlanStamps {
            generation: self.inner.stats_generation.current(),
            epoch: self.inner.lock_token_changes().epoch(),
        }
    }

    /// Publish a parameterized plan onto the cache. `hold` is the cache
    /// ledger's reservation; `None` when admission is off. The pending
    /// insert carries the entry's own handles, so the publish is a move.
    pub(super) fn publish_cache_entry(&self, pending: PendingCache, hold: Option<PlanHold>) {
        let PendingCache {
            handles,
            text,
            values,
            bytes: _,
        } = pending;
        let mut cache = crate::plan_cache::lock(&self.inner);
        cache.insert(crate::plan_cache::PlanCacheEntry {
            handles,
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
