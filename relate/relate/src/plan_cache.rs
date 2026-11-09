//! The per-store plan cache (ADR-0503): at most [`PLAN_CACHE_CAPACITY`]
//! template-keyed entries, most-recent-first, shared by every connection
//! of one store through `Shared`.
//!
//! Each entry is one auto-parameterized template: the lowered AST of the
//! rewritten statement (literals as `$__AUTO_*__` parameters), the plan
//! compiled from it, and the **stats generation** it was planned at. The
//! entry is reused only while no commit has landed since (the generation
//! check runs under the publication read guard, the same guard that
//! stamped it); on mismatch the admission path re-plans from the cached
//! AST with fresh stats and updates the entry.
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
use crate::shared::Shared;
use crate::token_budget::PlanHold;
use crate::Value;

/// The store's stats generation (ADR-0503): the monotonic counter this
/// cache checks its entries against.
///
/// Every applied commit — data or schema, the follower's batch apply, and
/// a graph replacement (a follow reload installs a different data state)
/// — advances the generation under the publication write boundary. An
/// entry carries the generation its plan was compiled at, and admission
/// reads the current one under the publication read guard (the same guard
/// that reads the planner's live cardinalities), so an entry is reused
/// only while nothing has committed since it was planned; on mismatch the
/// entry re-plans from its cached AST.
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

    /// Advance past every cached plan: called at each applied commit and
    /// at graph replacement, under the publication write boundary, and at
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
    /// key; the raw string is only the fast path), the lowered AST a
    /// stats-generation mismatch re-plans from (the replan clones the
    /// AST itself, once, on the mismatch path only), the plan compiled
    /// from that AST, and the stats generation it was compiled at —
    /// stored as one bundle so a lookup hit hands all four out as
    /// pointer clones, never copies.
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

/// The raw-string fast path's memory: one exact statement text and the
/// values its literals extracted. The same text always extracts the same
/// values, so the pair stays valid for as long as the entry does.
pub(crate) struct RawStatement {
    pub(crate) text: Box<str>,
    /// Shared so a raw hit hands the values out as a pointer clone: the
    /// hit path's only copy is the one it must make anyway, into the
    /// statement's effective parameter map.
    pub(crate) values: Arc<Vec<(String, Value)>>,
}

/// The entry's shared handles a lookup hit carries (ADR-0503): its
/// template, lowered AST, compiled plan, and the stats generation that
/// plan was compiled at — all pointer clones of the entry's own storage.
/// Stage 3 never re-finds the entry: an eviction after the lookup cannot
/// invalidate the plan (the `Arc`s keep it alive, and freshness is
/// decided by the generation check under the publication guard, not by
/// the entry's presence).
#[derive(Clone, Debug)]
pub(crate) struct EntryHandles {
    pub(crate) template: Arc<str>,
    pub(crate) query: Arc<Query>,
    pub(crate) plan: Arc<LogicalPlan>,
    pub(crate) generation: u64,
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
    /// Which lookup stage produced the hit (LABS-1120), typed as the
    /// outcome it becomes when the generation check agrees — the hit
    /// flavor itself, never a second vocabulary it converts into.
    pub(crate) stage: PlanCacheOutcome,
}

/// The statement's plan-cache outcome (LABS-1120): which ADR-0503 stage
/// served the plan — one fact the admission path already knows, surfaced
/// beside the phase timings so "is the plan cache active for this
/// workload?" is answered from the result, never from the source. The
/// wire spelling ([`Self::as_str`], kebab-case) is the one every surface
/// renders; the enum is the engine's single vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanCacheOutcome {
    /// Stage 2 served the cached plan: the auto-parameterized template
    /// matched, the stats generation agreed.
    HitTemplate,
    /// Stage 1 served the cached plan: the raw statement text matched an
    /// entry's last raw hit, no normalizer pass.
    HitRawFastPath,
    /// A hit whose entry's stats generation no longer matched — the store
    /// committed since the plan was compiled — so the carried AST
    /// re-planned under the fresh generation.
    ReplanGenerationMismatch,
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

impl PlanCacheOutcome {
    /// The outcome's wire spelling (LABS-1120), rendered by every surface
    /// that carries the signal — the one table, never re-spelled per
    /// surface.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HitTemplate => "hit-template",
            Self::HitRawFastPath => "hit-raw-fastpath",
            Self::ReplanGenerationMismatch => "replan-generation-mismatch",
            Self::Miss => "miss",
            Self::Bypassed => "bypassed",
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
        let hit = HitPayload {
            handles,
            values,
            stage: PlanCacheOutcome::HitRawFastPath,
        };
        self.touch_at(index);
        Some(hit)
    }

    /// The template's entry handles, moved to the front: a stage-2 hit
    /// hands the entry's own shared template (with its plan, AST and
    /// generation) out as pointer clones — the freshly normalized
    /// template string is dropped, never copied into the hit.
    pub(crate) fn find_template(&mut self, template: &str) -> Option<EntryHandles> {
        let index = self.position_of(template)?;
        let handles = self.entries[index].handles();
        self.touch_at(index);
        Some(handles)
    }

    /// Record the replan a generation mismatch produced: the fresh plan
    /// and the generation it was planned at. A no-op when the template
    /// was evicted in between (the next miss re-inserts it).
    pub(crate) fn update_plan(&mut self, template: &str, plan: Arc<LogicalPlan>, generation: u64) {
        if let Some(index) = self.position_of(template) {
            let entry = &mut self.entries[index];
            let logical = entry_bytes(
                &entry.handles.template,
                &entry.handles.query,
                &plan,
                &entry.raw.text,
                &entry.raw.values,
            );
            // A replan must land even when the pool cannot grow the hold:
            // the entry would otherwise keep serving a plan for the
            // previous generation. The previous reservation stays.
            if let Some(hold) = entry.hold.as_mut() {
                let _ = hold.try_raise(logical);
            }
            entry.handles.plan = plan;
            entry.handles.generation = generation;
        }
    }

    /// Remember this statement text and its extracted values on the
    /// template's entry — the raw fast path's memory. Idempotent: a
    /// stage-1 hit found the entry *by* exactly this text, so its
    /// re-record compares equal and skips with no allocation — the values
    /// a raw hit carries are, by construction, the ones the entry already
    /// remembers. Ownership is taken only in the record arm, where the
    /// slice is cloned into the entry.
    pub(crate) fn note_raw(&mut self, template: &str, text: &str, values: &[(String, Value)]) {
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
            if entry.raw.text.as_ref() == text && entry.raw.values.as_slice() == values {
                return;
            }
            let logical = entry_bytes(
                &entry.handles.template,
                &entry.handles.query,
                &entry.handles.plan,
                text,
                values,
            );
            // A larger raw text is remembered only when the cache hold can
            // grow to it. The template still serves the statement.
            if let Some(hold) = entry.hold.as_mut() {
                if !hold.try_raise(logical) {
                    return;
                }
            }
            entry.raw = RawStatement {
                text: text.into(),
                values: Arc::new(values.to_vec()),
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
/// cache is derived state that rebuilds itself. The guard is never held
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
/// values, at the one boundary where both vocabularies exist.
pub(crate) fn engine_values(values: Vec<(String, NormalizedValue)>) -> Vec<(String, Value)> {
    values
        .into_iter()
        .map(|(name, value)| (name, engine_value(value)))
        .collect()
}

fn engine_value(value: NormalizedValue) -> Value {
    match value {
        NormalizedValue::Int(i) => Value::Int(i),
        NormalizedValue::Float(f) => Value::Float(f),
        NormalizedValue::Str(s) => Value::String(s.as_str().into()),
        NormalizedValue::List(elements) => {
            Value::List(Arc::new(elements.into_iter().map(engine_value).collect()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LABS-1120: the outcome vocabulary pinned against the enum's own
    /// spelling — the wire table `as_str` owns, in full and exactly once
    /// per variant, kebab-case throughout. The wire envelope carries this
    /// spelling as a bare string across the crate boundary, so this pin
    /// (not a hand-maintained list in a doc comment) is what a rename or
    /// an added variant must pass through.
    #[test]
    fn outcome_spellings_are_the_complete_kebab_vocabulary() {
        let expected = [
            PlanCacheOutcome::HitTemplate,
            PlanCacheOutcome::HitRawFastPath,
            PlanCacheOutcome::ReplanGenerationMismatch,
            PlanCacheOutcome::Miss,
            PlanCacheOutcome::Bypassed,
        ];
        let spellings: Vec<&str> = expected.iter().map(|o| o.as_str()).collect();
        assert_eq!(
            spellings,
            [
                "hit-template",
                "hit-raw-fastpath",
                "replan-generation-mismatch",
                "miss",
                "bypassed",
            ]
        );
        for spelling in &spellings {
            assert!(
                !spelling.contains(|c: char| c.is_uppercase() || c == '_'),
                "the wire spelling is kebab-case: {spelling}"
            );
        }
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
                generation,
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
        assert_eq!(handles.generation, 7);
        // The shared plan is the one the entry was built with.
        assert!(Arc::strong_count(&handles.plan) >= 1);
        assert!(cache.find_template("other").is_none());
        // A replan updates the plan and the generation in place.
        let query = (*handles.query).clone();
        assert_eq!(query.parts.len(), 1);
        let fresh = Arc::new(relate_planner::plan(query, &mut |_| false).unwrap());
        cache.update_plan("t", Arc::clone(&fresh), 8);
        let replanned = cache.find_template("t").expect("present");
        assert_eq!(replanned.generation, 8);
        assert!(Arc::ptr_eq(&replanned.plan, &fresh));
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
        assert_eq!(cache.find_template("t").unwrap().generation, 2);
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
        ]);
        assert_eq!(values[0].1, Value::Int(1));
        assert_eq!(
            values[1].1,
            Value::List(Arc::new(
                vec![Value::String("x".into()), Value::Float(2.5),]
            ))
        );
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
        let query = relate_front::parse_and_lower(
            "MATCH (n:Person)-[:KNOWS]->(m) WHERE n.age > 1 RETURN m",
        )
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
}
