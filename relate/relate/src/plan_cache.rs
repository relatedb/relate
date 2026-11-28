//! The per-store plan cache (ADR-0503): at most [`PLAN_CACHE_CAPACITY`]
//! template-keyed entries, most-recent-first, shared by every connection
//! of one store through `Shared`.
//!
//! Each entry is one auto-parameterized template: the lowered AST of the
//! rewritten statement (literals as `$__AUTO_*__` parameters), the plan
//! compiled from it, the plan's **token footprint** — the store tokens it
//! reads and the counts it was priced at (ADR-0687) — the **stats
//! generation** and **full-invalidation epoch** it was planned at, the
//! **per-key statistics epoch** whose statistics its planning read
//! (ADR-0700, `None` when it read none), and its **plan grade**, the
//! measured store reads per execution ([`crate::plan_grade`]).
//! The entry is reused while no commit touched a footprint token, the
//! epoch stands, and the statistics it read stand (ADR-0687, ADR-0700;
//! the check runs under the publication read guard, the same guard that
//! stamped it): a token changed with cardinality drift inside
//! `DRIFT_FACTOR` re-stamps the entry at the current generation and serves
//! clean, drift at or past the factor marks it dirty and it keeps serving
//! until its grade says the plan degraded — a dirty entry whose average
//! reaches `GRADE_FACTOR` times its baseline re-plans on that hit — while
//! an epoch mismatch, a statistics publication past the entry's
//! statistics stamp, or an absent-name resolution re-plans from the cached
//! AST with fresh stats. A re-plan refreshes the entry's plan, footprint,
//! generation, epoch and statistics stamp together and starts its grade
//! over.
//!
//! The cache keys the plan, never the metadata: namespace, query type,
//! write route, notifications and the compile-time gates all still derive
//! per statement, from the cached or the fresh plan.
//!
//! The raw-string fast path rides the entry: each entry remembers the
//! last raw statement text that hit it plus that text's extracted values,
//! so an exact retry resolves without the normalizer pass.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, MutexGuard};

use relate_ir::Query;
use relate_parser::NormalizedValue;
use relate_planner::LogicalPlan;

use crate::plan_bytes::entry_bytes;
use crate::plan_footprint::PlanFootprint;
use crate::plan_grade::PlanGrade;
use crate::shared::Shared;
use crate::token_budget::PlanHold;
use crate::Value;

/// The store's stats generation (ADR-0503): the monotonic counter the
/// per-token change table stamps and the cache records a plan at.
///
/// Every applied commit — data or schema, the follower's batch apply, and
/// a graph replacement (a follow reload installs a different data state)
/// — advances the generation under the publication write boundary. An
/// entry carries the generation its plan was compiled at, and admission
/// reads the current one under the publication read guard (the same guard
/// that reads the planner's live cardinalities and the change table).
/// ADR-0687 replaced the any-commit check with the footprint check: the
/// generation itself only orders a token's last change against a plan.
///
/// `commit_ts` is deliberately NOT this counter: a follower's
/// checkpoint+reload replays fewer records than the prior graph instance
/// had commits, so equal timestamps can describe different data states.
/// This counter moves only forward. The type lives in this crate (not
/// `relate-core`) because every advance site and every reader is a
/// facade path — the lowest general-purpose crate carries no plan-cache
/// concept it cannot observe.
#[derive(Debug, Default)]
pub(crate) struct StatsGeneration {
    current: AtomicU64,
}

impl StatsGeneration {
    /// The generation a plan compiled now must carry: the value every
    /// commit since has moved past.
    pub(crate) fn current(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    /// Open the next generation: called at each applied commit and at
    /// graph replacement, under the publication write boundary, and at
    /// the follower's batch apply, under its *read* apply guard (an
    /// applying follower shares that guard with readers — the advance
    /// lands after the batch, before the next poll, the same mid-batch
    /// window the live planner has always had). The publication read
    /// guard pairs a stamp with its check: a reader holding one never
    /// observes a mid-mutation pair on the writer paths.
    pub(crate) fn advance(&self) {
        self.current.fetch_add(1, Ordering::Release);
    }
}

/// A plan's two freshness stamps, read and carried as one value (ADR-0687):
/// the stats generation it was compiled at and the full-invalidation epoch.
/// One value so no call site can pair a generation with another read's
/// epoch — the module's convention for same-typed facts whose swap would
/// compile (`AdmitClocks`, `StagedPlan`, `DispatchCtx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlanStamps {
    /// The stats generation the plan was compiled at.
    pub(crate) generation: u64,
    /// The full-invalidation epoch it was compiled at (ADR-0687 decision 4).
    pub(crate) epoch: u64,
}

/// How many templates a store keeps — "last-N ≤ 16, not single-element":
/// agents interleave a couple of shapes (introspection + data reads), and
/// exact-retry hits on the raw string are the rare case.
pub(crate) const PLAN_CACHE_CAPACITY: usize = 16;

/// The largest statement text the cache remembers or builds from — its
/// memory bound (ADR-0503): the count cap alone would let a few maximal
/// statements pin disproportionate derived memory (a lowered `Query`, a
/// `LogicalPlan`, and a remembered raw text per entry) outside every
/// budget, and this path runs before any of them. Real repeated shapes —
/// the workload the cache exists for — are orders of magnitude below it;
/// a statement past it simply stays uncached. The no-size-threshold
/// constraint governs the *lookup*, which this never touches: every
/// lookup still normalizes. Both the insert and the raw memory's
/// re-record enforce it.
pub(crate) const MAX_CACHED_STATEMENT_BYTES: usize = 64 * 1024;

/// One cached template.
pub(crate) struct PlanCacheEntry {
    /// The entry's shared handles (ADR-0503) — its template (the real
    /// key; the raw string is only the fast path), the lowered AST the
    /// re-plan rebuilds from (the replan clones the AST itself, once, on
    /// the re-plan path only), the plan compiled from that AST, the
    /// plan's token footprint, the stamps it was compiled at and its plan
    /// grade (ADR-0687) — stored as one bundle so a lookup hit hands them
    /// out as one clone, the `Arc`s pointer clones, never per-field copies.
    pub(crate) handles: EntryHandles,
    /// The last raw statement text within the 64 KiB insert bound that
    /// resolved to this entry, with that text's extracted values — the
    /// raw fast path's memory. Always present: the insert that creates
    /// an entry records its statement's text and values, and every later
    /// hit re-records them. Only the latest such raw text is kept (the
    /// fast path is the bonus, the template is the key); a larger hit
    /// text is served by the template and deliberately not remembered
    /// (`note_raw`'s size policy).
    pub(crate) raw: RawStatement,
    /// Workspace-pool reservation for this entry. `None` when admission
    /// is off. Dropping the entry — count eviction, template replacement,
    /// or [`PlanCache::clear`] — releases it.
    pub(crate) hold: Option<PlanHold>,
}

/// The raw-string fast path's memory: one exact statement text, the
/// values its literals extracted, and each extracted literal's authored
/// spelling (ADR-0718 — the naming path's substitution input, recorded
/// once so a raw hit never normalizes). The same text always extracts
/// the same values and spellings, so the triple stays valid for as long
/// as the entry does.
pub(crate) struct RawStatement {
    pub(crate) text: Box<str>,
    /// Shared so a raw hit hands the values out as a pointer clone: the
    /// hit path's only copy is the one it must make anyway, into the
    /// statement's effective parameter map.
    pub(crate) values: Arc<Vec<(String, Value)>>,
    /// The `(placeholder, authored spelling)` pairs, parallel to
    /// `values` — shared for the same reason.
    pub(crate) spellings: Arc<relate_parser::AuthoredSpellings>,
}

/// The entry's shared handles a lookup hit carries (ADR-0503): its
/// template, lowered AST, compiled plan, the plan's token footprint, and
/// the per-plan state that plan was left in — the stamps it was compiled
/// at, the per-key statistics epoch it was priced at (ADR-0700), its
/// dirty mark and its plan grade (ADR-0687). The `Arc`s are
/// pointer clones of the entry's own storage; the numbers are its current
/// snapshot, which is what the admission check decides on. Stage 3 never
/// re-finds the entry: an eviction after the lookup cannot invalidate the
/// plan (the `Arc`s keep it alive, and freshness is decided by the
/// footprint, epoch and grade checks under the publication guard, not by
/// the entry's presence).
#[derive(Clone, Debug)]
pub(crate) struct EntryHandles {
    pub(crate) template: Arc<str>,
    pub(crate) query: Arc<Query>,
    pub(crate) plan: Arc<LogicalPlan>,
    /// The tokens the plan reads and the counts it was planned against
    /// (ADR-0687) — the fact a later commit is checked against.
    pub(crate) footprint: Arc<PlanFootprint>,
    /// The generation and epoch this plan was compiled at (ADR-0687), read
    /// under one guard and carried as one value: a footprint token stamped
    /// after the generation makes the entry re-check drift, and an epoch
    /// mismatch re-plans whatever the footprint says.
    pub(crate) stamps: PlanStamps,
    /// The per-key statistics epoch the plan was priced at (ADR-0700
    /// decision 2), `None` when planning never consulted the statistics —
    /// such an entry is never compared, so another label's statistics
    /// publishing is no reason to replan it. A separate fact from
    /// [`PlanStamps`]: the statistics move without a commit.
    pub(crate) key_stats_epoch: Option<u64>,
    /// Whether a footprint number changed with drift at or past
    /// [`crate::plan_footprint::DRIFT_FACTOR`] since this plan was
    /// compiled (ADR-0687 decisions 5-6): the entry still serves, and its
    /// plan grade decides a later re-plan. A re-plan or an insert clears
    /// it; a within-threshold re-stamp leaves it as it stands.
    pub(crate) dirty: bool,
    /// The measured store reads over this plan's graded executions
    /// (ADR-0687 decision 6), against the baseline its first one recorded:
    /// the gate a dirty entry re-plans on. A re-plan clears it — the new
    /// plan starts over — while a re-stamp or a dirty mark leaves it.
    pub(crate) grade: PlanGrade,
}

impl EntryHandles {
    /// Whether the per-key statistics this plan was priced from still
    /// stand (ADR-0700 decision 3): none published, aged out or cleared
    /// since the stamp. A plan that never consulted them carries `None`
    /// and is never compared, so another label's statistics changing never
    /// re-plans it. The current epoch is read under the publication read
    /// guard, the same guard the stamp was taken under. ADR-0687's
    /// footprint check owns the commit half of freshness; this is the one
    /// statistics half.
    pub(crate) fn key_stats_fresh(&self, key_stats_epoch: u64) -> bool {
        self.key_stats_epoch.is_none_or(|e| e == key_stats_epoch)
    }
}

impl PlanCacheEntry {
    /// Pointer clones of this entry's handles — the one construction
    /// `lookup_raw` and `find_template` share.
    fn handles(&self) -> EntryHandles {
        self.handles.clone()
    }
}

/// The one hit payload (ADR-0503): the entry's shared handles (stage
/// 3's inputs) and this statement's extracted literal values — pointer
/// clones of the entry's own storage, so a raw hit allocates nothing.
/// Carried directly by the cache lookup's hit variant; a template hit
/// builds the same shape with the entry's handles and the normalizer's
/// fresh values. `stage` names which lookup stage produced it (LABS-1120)
/// — the plan-cache outcome's hit flavor, read by the admission path.
#[derive(Debug)]
pub(crate) struct HitPayload {
    pub(crate) handles: EntryHandles,
    pub(crate) values: Arc<Vec<(String, Value)>>,
    /// The statement's authored spellings (ADR-0718): stage 1's come from
    /// the entry's recorded triple, stage 2's from the normalization that
    /// just ran — either way, one pointer clone beside the values.
    pub(crate) spellings: Arc<relate_parser::AuthoredSpellings>,
    /// Which lookup stage produced the hit (LABS-1120), typed as the
    /// outcome it becomes when admission serves it as carried — the hit
    /// flavor itself, never a second vocabulary it converts into.
    pub(crate) stage: PlanCacheOutcome,
}

/// The statement's plan-cache outcome (LABS-1120): which ADR-0503 stage
/// served the plan — one fact the admission path already knows, surfaced
/// beside the phase timings so "is the plan cache active for this
/// workload?" is answered from the result, never from the source. The
/// wire spelling ([`Self::as_str`], kebab-case) is the one every surface
/// renders; the enum is the engine's single vocabulary. ADR-0687 renamed
/// the re-plan variant: it owns the timing story (no parse, a fresh
/// plan), not the cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanCacheOutcome {
    /// Stage 2 served the cached plan: the auto-parameterized template
    /// matched and no footprint token changed since the plan.
    HitTemplate,
    /// Stage 1 served the cached plan: the raw statement text matched an
    /// entry's last raw hit, no normalizer pass.
    HitRawFastPath,
    /// A hit whose entry re-planned before serving (ADR-0687, ADR-0700):
    /// the full-invalidation epoch moved (schema DDL or graph
    /// replacement), the per-key value statistics the plan read changed
    /// (ADR-0700 — only a plan that consulted them is compared), an
    /// absent-name resolution saw a commit, or a dirty entry's plan grade
    /// degraded. The carried AST rebuilt the plan under fresh stats — no
    /// parse, a fresh plan. Drift alone never re-plans here: it re-stamps
    /// the entry or marks it dirty and serves.
    Replan,
    /// No entry matched — or a hit whose cached plan was rejected for
    /// literal-shape safety, so the statement ran an original compile:
    /// either way, no cached plan served the statement (a cacheable miss
    /// may insert the parameterized plan for later statements).
    Miss,
    /// The cache was not consulted: the plan-tree path's bypass, a caller
    /// parameter map carrying the reserved prefix, or a statement the
    /// normalizer refuses as non-cacheable.
    Bypassed,
}

/// The one variant→wire-spelling declaration (LABS-1120): the macro expands
/// both [`PlanCacheOutcome::as_str`]'s exhaustive match and the test's
/// iteration from one list, so a new variant must be spelled here to compile
/// and cannot be spelled for the match while the test's list forgets it.
macro_rules! plan_cache_outcomes {
    ($($variant:ident => $spelling:literal),+ $(,)?) => {
        impl PlanCacheOutcome {
            /// The outcome's wire spelling (LABS-1120), rendered by every
            /// surface that carries the signal — the one table, never
            /// re-spelled per surface.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $spelling),+
                }
            }

            /// Every variant beside its wire spelling — the one list the
            /// vocabulary test iterates, produced by the same declaration as
            /// [`Self::as_str`], so the two cannot diverge.
            #[cfg(test)]
            pub(crate) const SPELLINGS: &'static [(PlanCacheOutcome, &'static str)] =
                &[$( (PlanCacheOutcome::$variant, $spelling) ),+];
        }
    };
}

plan_cache_outcomes! {
    HitTemplate => "hit-template",
    HitRawFastPath => "hit-raw-fastpath",
    Replan => "replan",
    Miss => "miss",
    Bypassed => "bypassed",
}

impl PlanCacheOutcome {
    /// Whether this outcome served a cached artifact (REL8-1120): the
    /// template hit, the raw-string fast path, and the re-plan alike —
    /// the three flavors that skipped the front's value-dependent
    /// validation, and so the retry eligibility of the statement that
    /// reported it. Exhaustive by construction: a future variant must
    /// classify here to compile.
    pub(crate) fn served_from_cache(self) -> bool {
        match self {
            Self::HitTemplate | Self::HitRawFastPath | Self::Replan => true,
            Self::Miss | Self::Bypassed => false,
        }
    }
}

/// The per-store cache: entries most-recent-first, at most
/// [`PLAN_CACHE_CAPACITY`].
#[derive(Default)]
pub(crate) struct PlanCache {
    entries: Vec<PlanCacheEntry>,
}

impl PlanCache {
    /// The entry position a template occupies, or `None` — the one
    /// spelling of the template-key lookup every method below consults.
    fn position_of(&self, template: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.handles.template.as_ref() == template)
    }

    /// Move the entry at `index` to the front — the one LRU touch.
    fn touch_at(&mut self, index: usize) {
        let entry = self.entries.remove(index);
        self.entries.insert(0, entry);
    }

    /// The raw-string fast path: the entry whose last raw text is exactly
    /// `cypher`, moved to the front. No normalizer pass runs.
    pub(crate) fn lookup_raw(&mut self, cypher: &str) -> Option<HitPayload> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.raw.text.as_ref() == cypher)?;
        let handles = self.entries[index].handles();
        let values = Arc::clone(&self.entries[index].raw.values);
        let spellings = Arc::clone(&self.entries[index].raw.spellings);
        let hit = HitPayload {
            handles,
            values,
            spellings,
            stage: PlanCacheOutcome::HitRawFastPath,
        };
        self.touch_at(index);
        Some(hit)
    }

    /// The template's entry handles, moved to the front: a stage-2 hit
    /// hands the entry's own shared template (with its plan, AST,
    /// footprint and stamps) out as pointer clones — the freshly
    /// normalized template string is dropped, never copied into the hit.
    pub(crate) fn find_template(&mut self, template: &str) -> Option<EntryHandles> {
        let index = self.position_of(template)?;
        let handles = self.entries[index].handles();
        self.touch_at(index);
        Some(handles)
    }

    /// Record the re-plan admission produced (ADR-0687, ADR-0700): the
    /// fresh plan, the token footprint and counts it was planned against,
    /// the generation and epoch it was planned at — one [`PlanStamps`]
    /// value, so a re-plan cannot land a plan with mismatched stamps — and
    /// the per-key statistics epoch it read (`None` when the re-plan
    /// consulted no statistics — ADR-0700 decision 2). The entry is clean
    /// again and its grade starts over — a re-plan is a fresh plan, graded
    /// from its own first execution. A no-op when the template was evicted
    /// in between (the next miss re-inserts it).
    pub(crate) fn update_plan(
        &mut self,
        template: &str,
        plan: Arc<LogicalPlan>,
        footprint: Arc<PlanFootprint>,
        stamps: PlanStamps,
        key_stats_epoch: Option<u64>,
    ) {
        if let Some(index) = self.position_of(template) {
            let entry = &mut self.entries[index];
            let logical = entry_bytes(
                &entry.handles.template,
                &entry.handles.query,
                &plan,
                &entry.raw.text,
                &entry.raw.values,
                &entry.raw.spellings,
                &footprint,
            );
            // A re-plan must land even when the pool cannot grow the hold:
            // the entry would otherwise keep serving a plan for the
            // previous stamps. The previous reservation stays.
            if let Some(hold) = entry.hold.as_mut() {
                hold.grow_best_effort(logical);
            }
            entry.handles.plan = plan;
            entry.handles.footprint = footprint;
            entry.handles.stamps = stamps;
            entry.handles.key_stats_epoch = key_stats_epoch;
            entry.handles.dirty = false;
            entry.handles.grade = PlanGrade::default();
        }
    }

    /// Re-stamp a footprint-touched entry whose drift stayed inside the
    /// threshold (ADR-0687 decision 5): it serves at the current
    /// generation, and its dirty mark stands as it was. A no-op when the
    /// template was evicted in between.
    pub(crate) fn restamp(&mut self, template: &str, generation: u64) {
        if let Some(index) = self.position_of(template) {
            self.entries[index].handles.stamps.generation = generation;
        }
    }

    /// Mark a footprint-touched entry whose drift reached the threshold
    /// (ADR-0687 decisions 5-6), re-stamping it at the current generation:
    /// the plan keeps serving, and the plan grade decides the later
    /// re-plan. A no-op when the template was evicted in between.
    pub(crate) fn mark_dirty(&mut self, template: &str, generation: u64) {
        if let Some(index) = self.position_of(template) {
            let entry = &mut self.entries[index];
            entry.handles.stamps.generation = generation;
            entry.handles.dirty = true;
        }
    }

    /// Fold one graded execution's reading — the store reads it performed —
    /// into the plan's entry (ADR-0687 decision 6): the hit path's one cache
    /// write after the driver returned, so no cache lock is held across an
    /// execution. The write is keyed on the plan the run served (`plan`, the
    /// entry's own `Arc` identity), never the template alone: a no-op when
    /// the template was evicted in between, and a *dropped* reading when a
    /// re-plan landed — the fresh entry starts over, and the superseded
    /// plan's measurement is not its work.
    pub(crate) fn record_grade(&mut self, template: &str, plan: &Arc<LogicalPlan>, reads: f64) {
        let Some(index) = self.position_of(template) else {
            return;
        };
        let entry = &mut self.entries[index];
        if Arc::ptr_eq(&entry.handles.plan, plan) {
            entry.handles.grade.record(reads);
        }
    }

    /// Remember this statement text and its extracted values on the
    /// template's entry — the raw fast path's memory. Idempotent: a
    /// stage-1 hit found the entry *by* exactly this text, so its
    /// re-record compares equal and skips with no allocation — the values
    /// a raw hit carries are, by construction, the ones the entry already
    /// remembers. Ownership is taken only in the record arm, where the
    /// slice is cloned into the entry.
    pub(crate) fn note_raw(
        &mut self,
        template: &str,
        text: &str,
        values: &[(String, Value)],
        spellings: &relate_parser::AuthoredSpellings,
    ) {
        // The same memory bound the insert enforces: a template hit can
        // carry a text far larger than the one the entry was built from
        // (whitespace and comments normalize away), and the raw memory
        // must not become the amplification channel around the insert
        // bound. An oversized text is simply not remembered — the
        // template still serves it.
        if text.len() > MAX_CACHED_STATEMENT_BYTES {
            return;
        }
        if let Some(index) = self.position_of(template) {
            let entry = &mut self.entries[index];
            if entry.raw.text.as_ref() == text
                && entry.raw.values.as_slice() == values
                && entry.raw.spellings.as_slice() == spellings
            {
                return;
            }
            let logical = entry_bytes(
                &entry.handles.template,
                &entry.handles.query,
                &entry.handles.plan,
                text,
                values,
                spellings,
                &entry.handles.footprint,
            );
            // A larger raw text is remembered only when the cache hold can
            // grow to it. The template still serves the statement.
            if let Some(hold) = entry.hold.as_mut() {
                if !hold.grow_or_skip(logical) {
                    return;
                }
            }
            entry.raw = RawStatement {
                text: text.into(),
                values: Arc::new(values.to_vec()),
                spellings: Arc::new(spellings.to_vec()),
            };
        }
    }

    /// Insert an entry: replace the same template's entry if present,
    /// otherwise push the front and evict the least-recently-used beyond
    /// [`PLAN_CACHE_CAPACITY`].
    pub(crate) fn insert(&mut self, entry: PlanCacheEntry) {
        self.entries
            .retain(|e| e.handles.template.as_ref() != entry.handles.template.as_ref());
        self.entries.insert(0, entry);
        while self.entries.len() > PLAN_CACHE_CAPACITY {
            self.entries.pop();
        }
    }

    /// How many templates are cached (tests and diagnostics).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The most recently used entry's token footprint (tests): the plan's
    /// resolved tokens and the counts it was planned against.
    #[cfg(test)]
    pub(crate) fn front_footprint(&self) -> Option<Arc<PlanFootprint>> {
        self.entries
            .first()
            .map(|entry| Arc::clone(&entry.handles.footprint))
    }

    /// The most recently used entry's dirty mark (tests): whether a
    /// footprint commit at or past the drift threshold marked the plan
    /// (ADR-0687).
    #[cfg(test)]
    pub(crate) fn front_dirty(&self) -> Option<bool> {
        self.entries.first().map(|entry| entry.handles.dirty)
    }

    /// The most recently used entry's plan epoch (tests): the
    /// full-invalidation epoch its plan was compiled at (ADR-0687).
    #[cfg(test)]
    pub(crate) fn front_epoch(&self) -> Option<u64> {
        self.entries.first().map(|entry| entry.handles.stamps.epoch)
    }

    /// The most recently used entry's generation stamp (tests): the plan
    /// generation, moved by a within-threshold re-stamp (ADR-0687).
    #[cfg(test)]
    pub(crate) fn front_generation(&self) -> Option<u64> {
        self.entries
            .first()
            .map(|entry| entry.handles.stamps.generation)
    }

    /// The most recently used entry's template (tests): one half of the
    /// grade seam's (`record_grade`) address.
    #[cfg(test)]
    pub(crate) fn front_template(&self) -> Option<Arc<str>> {
        self.entries
            .first()
            .map(|entry| Arc::clone(&entry.handles.template))
    }

    /// The most recently used entry's plan, as the `Arc` identity the grade
    /// seam checks (tests): the other half of `record_grade`'s address.
    #[cfg(test)]
    pub(crate) fn front_plan(&self) -> Option<Arc<LogicalPlan>> {
        self.entries
            .first()
            .map(|entry| Arc::clone(&entry.handles.plan))
    }

    /// The most recently used entry's plan grade (tests): the baseline its
    /// first graded execution recorded and the EWMA over the rest
    /// (ADR-0687 decision 6).
    #[cfg(test)]
    pub(crate) fn front_grade(&self) -> Option<PlanGrade> {
        self.entries.first().map(|entry| entry.handles.grade)
    }

    /// Drop everything (the poison recovery path — a panic mid-critical-
    /// section may leave a half-updated entry, and a cache is derived
    /// state that rebuilds itself).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Lock the store's plan cache (ADR-0503), recovering from a poisoned
/// mutex by clearing it: the critical sections are pure data operations,
/// so a panic mid-section can at worst leave a half-updated entry, and a
/// cache is derived state that rebuilds itself. The whole cache is cleared,
/// not one entry: a poisoned lock carries no record of which entry was
/// mid-update, so a torn entry cannot be told from a clean poison and no
/// narrower recovery is sound. The guard is never held
/// across a parse, a plan, or another lock.
pub(crate) fn lock(shared: &Shared) -> MutexGuard<'_, PlanCache> {
    match shared.plan_cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            guard.clear();
            // `into_inner` leaves the mutex poisoned, so every later
            // `lock` would clear again and the cache would stay disabled.
            shared.plan_cache.clear_poison();
            guard
        }
    }
}

/// Convert the normalizer's extracted values into the engine's parameter
/// values, at the one boundary where both vocabularies exist. `None` is
/// the depth refusal (ADR-0462, [`Value::checked_list`]): a literal list
/// nested past the value-depth bound fails here, exactly where the
/// uncached run's evaluation would fail it — the lookup then declines the
/// cache for the statement, whose own compile reports the error (REL8-1120:
/// the one value-dependent literal class the hit path must still catch).
pub(crate) fn engine_values(
    values: Vec<(String, NormalizedValue)>,
) -> Option<Vec<(String, Value)>> {
    values
        .into_iter()
        .map(|(name, value)| engine_value(value).map(|value| (name, value)))
        .collect()
}

fn engine_value(value: NormalizedValue) -> Option<Value> {
    match value {
        NormalizedValue::Int(i) => Some(Value::Int(i)),
        NormalizedValue::Float(f) => Some(Value::Float(f)),
        NormalizedValue::Str(s) => Some(Value::String(s.as_str().into())),
        NormalizedValue::List(elements) => Value::checked_list(
            elements
                .into_iter()
                .map(engine_value)
                .collect::<Option<Vec<_>>>()?,
        )
        .ok(),
    }
}

#[cfg(test)]
#[path = "plan_cache_tests.rs"]
mod tests;
