//! The one distinct set under every DISTINCT membership decision (ADR-0507):
//! RETURN/WITH DISTINCT, UNION, `count(DISTINCT …)`, the first-seen value
//! dedup (`coll.distinct`, the batch aggregate operand) and the
//! `DISTINCT … LIMIT` match cap.
//!
//! Membership is exactly [`GroupKey`] equivalence — `HashSet<GroupKey>` for
//! values, `HashSet<Vec<GroupKey>>` for rows — but keys are held in the
//! cheapest representation [`GroupKey::class`] admits:
//!
//! * **null** — one flag (null is reflexive);
//! * **T1** — words of one tag, inline in `[u64; 16]`, probed by a
//!   fixed-length, branch-free 16-lane mask compare: no heap, no hash;
//! * **T2** — on the 17th distinct word or a tag switch, one `u64` table per
//!   tag, presized to 32 (Fx for engine ids, the seeded hasher otherwise);
//! * **general** single keys and **rows** of arity ≠ 1 in their own lazily
//!   allocated tables, exactly today's representations.
//!
//! The word storage is the crate's tiered word-set utility (ADR-0522,
//! [`crate::tiered_set`]): one [`TieredWordSet`] per tag — the *active*
//! tier holds the current tag's words inline in the set (the one inline
//! phase, so a `DistinctAcc`'s footprint stays one tier, not five), and a
//! tag switch promotes it and parks it in its [`Spill`] slot. This module
//! keeps the tag model (the tag→tier map, the active-tag tracking, the
//! tag-switch rule); the utility keeps the tiering and the charge.
//! Promotion is upward only, moves at most the 16 held words, and no tag
//! ever returns to an inline phase after a switch parked it. The set owns
//! its workspace charge: each insert is one probe and charges the delta of
//! the representation that took the key — before the write where the
//! representation allows it, with the key removed again otherwise. The set
//! decides membership only — callers emit, so first-seen order is the
//! input order filtered by the insert results.
//!
//! Layering: this module imports only `group_key`, `tiered_set`,
//! `workspace`'s charge helpers, the hasher seam and `relate_core` —
//! never the exec tree or the pattern walker (a unit test greps this
//! source to hold that).

use std::hash::Hash;

use relate_core::tokens::{workspace_accounting, WorkspaceCharge};
use relate_core::{Result, Value};

use crate::group_key::{row_key, GroupKey, KeyClass, WordTag};
use crate::hash_seed::{seeded_set, SeededSet};
use crate::tiered_set::{TieredWordSet, WordHash};
use crate::workspace::{
    charge_distinct_delta, distinct_row_insert_delta, distinct_row_set_bytes,
    group_key_insert_delta, group_key_set_bytes,
};

/// The tag→hasher policy of a word tier (ADR-0507 §4): engine ids hash
/// under keyless Fx, client-derivable words under the seeded state.
fn word_hash(tag: WordTag) -> WordHash {
    match tag {
        WordTag::Node | WordTag::Rel => WordHash::Id,
        WordTag::Bool | WordTag::Int | WordTag::FloatBits => WordHash::Value,
    }
}

/// The heap tiers, allocated on first need only. One word-tier slot per
/// [`WordTag`] holds that tag's *parked* tier (promoted, moved wholesale
/// in and out of the active seat); [`Spill::word_tier_mut`] is the one
/// exhaustive tag→field map, so a new tag is a compile error there —
/// never an ordinal drift or a silent under-count of the parked tiers.
#[derive(Default)]
struct Spill {
    bools: Option<TieredWordSet>,
    ints: Option<TieredWordSet>,
    floats: Option<TieredWordSet>,
    nodes: Option<TieredWordSet>,
    rels: Option<TieredWordSet>,
    singles: Option<SeededSet<GroupKey>>,
    rows: Option<SeededSet<Vec<GroupKey>>>,
}

impl Spill {
    /// The one tag→tier map: exhaustive, so a new [`WordTag`] breaks the
    /// build here.
    fn word_tier_mut(&mut self, tag: WordTag) -> &mut Option<TieredWordSet> {
        match tag {
            WordTag::Bool => &mut self.bools,
            WordTag::Int => &mut self.ints,
            WordTag::FloatBits => &mut self.floats,
            WordTag::Node => &mut self.nodes,
            WordTag::Rel => &mut self.rels,
        }
    }

    /// Every tag's parked tier, for the `len`/`bytes`/`promoted` walks.
    /// The exhaustive destructure is the one field list: a new word-tier
    /// field breaks the build here, so it cannot go missing from a walk
    /// (the charged == bytes oracle the differential tests assert after
    /// every offer would fail if it did).
    fn word_tiers(&self) -> impl Iterator<Item = &TieredWordSet> {
        let Spill {
            bools,
            ints,
            floats,
            nodes,
            rels,
            singles: _,
            rows: _,
        } = self;
        [bools, ints, floats, nodes, rels].into_iter().flatten()
    }

    /// The uncharged contents copy: parked tiers and general tables.
    fn clone_contents(&self) -> Self {
        Self {
            bools: self.bools.as_ref().map(TieredWordSet::clone_contents),
            ints: self.ints.as_ref().map(TieredWordSet::clone_contents),
            floats: self.floats.as_ref().map(TieredWordSet::clone_contents),
            nodes: self.nodes.as_ref().map(TieredWordSet::clone_contents),
            rels: self.rels.as_ref().map(TieredWordSet::clone_contents),
            singles: self.singles.clone(),
            rows: self.rows.clone(),
        }
    }

    /// Clone with each parked tier's workspace retain propagated; a
    /// refusal propagates (the partially retained copy is dropped,
    /// releasing what it held). The general tables stay uncharged here —
    /// the set's own charge retains them.
    fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            bools: self
                .bools
                .as_ref()
                .map(TieredWordSet::try_clone)
                .transpose()?,
            ints: self
                .ints
                .as_ref()
                .map(TieredWordSet::try_clone)
                .transpose()?,
            floats: self
                .floats
                .as_ref()
                .map(TieredWordSet::try_clone)
                .transpose()?,
            nodes: self
                .nodes
                .as_ref()
                .map(TieredWordSet::try_clone)
                .transpose()?,
            rels: self
                .rels
                .as_ref()
                .map(TieredWordSet::try_clone)
                .transpose()?,
            singles: self.singles.clone(),
            rows: self.rows.clone(),
        })
    }
}

/// The tiered distinct set. See the module docs.
pub(crate) struct DistinctSet {
    null: bool,
    /// The tag whose words the active tier holds; `None` while no word
    /// has been taken (an active tier exists but is empty).
    word_tag: Option<WordTag>,
    /// The active word tier: the current tag's [`TieredWordSet`], inline
    /// in the set (the one inline phase), promoted in place on the 17th
    /// word, parked into its spill slot on a tag switch.
    words: TieredWordSet,
    spill: Option<Box<Spill>>,
    charge: WorkspaceCharge,
}

impl Default for DistinctSet {
    fn default() -> Self {
        Self {
            null: false,
            word_tag: None,
            words: TieredWordSet::new(WordHash::Value),
            spill: None,
            charge: WorkspaceCharge::empty(),
        }
    }
}

impl DistinctSet {
    /// The contents without a workspace reservation: the copy a consumer
    /// falls back to when it decides a refused retain must not fail.
    pub(crate) fn clone_contents(&self) -> Self {
        Self {
            null: self.null,
            word_tag: self.word_tag,
            words: self.words.clone_contents(),
            spill: self
                .spill
                .as_ref()
                .map(|spill| Box::new(spill.clone_contents())),
            charge: WorkspaceCharge::empty(),
        }
    }

    /// Clone with the workspace retain propagated: an empty set reserves
    /// nothing, a filled one retains its [`DistinctSet::bytes`], and a
    /// refusal is the caller's to decide (no silent fallback in the set).
    pub(crate) fn try_clone(&self) -> Result<Self> {
        let mut copy = self.clone_contents();
        // The word share of bytes() is retained by the tiers' own
        // try_clones (the active tier here, the parked ones below); the
        // general tables' share is the remainder, retained once.
        copy.words = self.words.try_clone()?;
        if let Some(spill) = self.spill.as_ref() {
            copy.spill = Some(Box::new(spill.try_clone()?));
        }
        let word_bytes = self.words.bytes()
            + self.spill_ref().map_or(0, |spill| {
                spill.word_tiers().map(TieredWordSet::bytes).sum::<u64>()
            });
        let general = self.bytes() - word_bytes;
        copy.charge = WorkspaceCharge::retain_with(|| general)?;
        Ok(copy)
    }

    /// Insert one value's key. `Ok(true)` when new, `Ok(false)` for a
    /// duplicate; a refused workspace charge leaves the set unchanged.
    pub(crate) fn insert_value(&mut self, value: &Value) -> Result<bool> {
        let key = GroupKey::new(value);
        match key.class() {
            KeyClass::Null => Ok(!std::mem::replace(&mut self.null, true)),
            KeyClass::Word(tag, word) => self.insert_word(tag, word),
            KeyClass::General => self.insert_single(key),
        }
    }

    /// Insert one row's key. A one-column row is its value's key (and never
    /// equals a row of another arity); every other arity is a row key. The
    /// arity-1 dispatch lives here only — callers pass any-length slices,
    /// borrowed one-element ones included (`std::slice::from_ref`).
    pub(crate) fn insert_row(&mut self, row: &[Value]) -> Result<bool> {
        match row {
            [value] => self.insert_value(value),
            _ => self.insert_row_key(row_key(row)),
        }
    }

    /// Distinct keys held, across every tier.
    pub(crate) fn len(&self) -> usize {
        let words = self.words.len()
            + self.spill_ref().map_or(0, |spill| {
                spill.word_tiers().map(TieredWordSet::len).sum::<usize>()
            });
        let spilled = self.spill_ref().map_or(0, |spill| {
            spill.singles.as_ref().map_or(0, SeededSet::len)
                + spill.rows.as_ref().map_or(0, SeededSet::len)
        });
        usize::from(self.null) + words + spilled
    }

    /// The accounted bytes of every held representation — the walk the
    /// per-insert deltas must sum to, and the figure a clone retains.
    pub(crate) fn bytes(&self) -> u64 {
        let mut bytes = self.words.bytes()
            + self.spill_ref().map_or(0, |spill| {
                spill.word_tiers().map(TieredWordSet::bytes).sum::<u64>()
            });
        if let Some(spill) = self.spill_ref() {
            bytes += spill.singles.as_ref().map_or(0, group_key_set_bytes);
            bytes += spill.rows.as_ref().map_or(0, distinct_row_set_bytes);
        }
        bytes
    }

    /// Whether the word tier has promoted to its per-tag tables (T2).
    #[cfg(test)]
    pub(crate) fn words_promoted(&self) -> bool {
        self.words.promoted()
            || self
                .spill_ref()
                .is_some_and(|spill| spill.word_tiers().any(TieredWordSet::promoted))
    }

    /// The bytes this set's own workspace charge holds: every word tier's
    /// charge plus the general tables' charge.
    #[cfg(test)]
    pub(crate) fn charged(&self) -> u64 {
        self.words.charged()
            + self.spill_ref().map_or(0, |spill| {
                spill.word_tiers().map(TieredWordSet::charged).sum::<u64>()
            })
            + self.charge.bytes()
    }

    /// The heap tiers, allocated on first need. Associated-function form
    /// (over the field, not `&mut self`) so an insert path can hold the
    /// tiers and the charge — disjoint fields — at once.
    fn spill(spill: &mut Option<Box<Spill>>) -> &mut Spill {
        spill.get_or_insert_with(Box::default)
    }

    /// The heap tiers, when any exist.
    fn spill_ref(&self) -> Option<&Spill> {
        self.spill.as_deref()
    }

    fn insert_word(&mut self, tag: WordTag, word: u64) -> Result<bool> {
        if self.word_tag != Some(tag) {
            self.switch_word_tier(tag);
        }
        self.words.insert(word)
    }

    /// A tag switch (or the first word): park the active tier — promoted
    /// first, ADR-0507 §6's tag-homogeneous inline phase ends here — in
    /// its tag's spill slot, then seat the incoming tag's parked tier,
    /// or a fresh one, as the active tier. A refused charge can leave
    /// the fresh tier empty with the tag claimed; the next same-tag word
    /// fills it, and an empty tier is never parked.
    fn switch_word_tier(&mut self, tag: WordTag) {
        if let Some(held) = self.word_tag.take() {
            let mut active = std::mem::replace(&mut self.words, TieredWordSet::new(word_hash(tag)));
            active.promote_inline();
            if active.len() > 0 {
                *Self::spill(&mut self.spill).word_tier_mut(held) = Some(active);
            }
        } else {
            // The set's first word: Default's placeholder tier carries the
            // seeded Value hasher regardless of this tag — seat the tag's
            // own, so every active tier's hasher derives from word_hash
            // alone, never from insert history.
            self.words = TieredWordSet::new(word_hash(tag));
        }
        // Seating a parked tier must not materialize the spill: a set that
        // never parks anything stays allocation-free on this path.
        if let Some(parked) = self
            .spill
            .as_deref_mut()
            .and_then(|spill| spill.word_tier_mut(tag).take())
        {
            self.words = parked;
        }
        self.word_tag = Some(tag);
    }

    fn insert_single(&mut self, key: GroupKey) -> Result<bool> {
        let singles = Self::spill(&mut self.spill)
            .singles
            .get_or_insert_with(seeded_set);
        insert_charged(&mut self.charge, singles, key, group_key_insert_delta)
    }

    fn insert_row_key(&mut self, key: Vec<GroupKey>) -> Result<bool> {
        let rows = Self::spill(&mut self.spill)
            .rows
            .get_or_insert_with(seeded_set);
        insert_charged(&mut self.charge, rows, key, distinct_row_insert_delta)
    }
}

/// The one charged insert of the general tables: one probe, the delta of
/// the new entry charged only under workspace accounting, and the entry
/// removed again when the budget refuses it. The key moves into the table
/// on the probe, so accounting keeps an undo copy to remove it by.
fn insert_charged<K, B: ?Sized>(
    charge: &mut WorkspaceCharge,
    set: &mut SeededSet<K>,
    key: K,
    delta: impl FnOnce(usize, &B) -> u64,
) -> Result<bool>
where
    K: Eq + Hash + Clone + std::borrow::Borrow<B>,
{
    if !workspace_accounting() {
        // Not a second copy of `charge_distinct_delta`'s gate (its delta
        // closure is already lazy): this skips the clone/undo probe
        // protocol itself when no budget can refuse it.
        return Ok(set.insert(key));
    }
    let old_len = set.len();
    if !set.insert(key.clone()) {
        return Ok(false);
    }
    charge_distinct_delta(charge, || {
        delta(old_len, std::borrow::Borrow::<B>::borrow(&key))
    })
    .inspect_err(|_| {
        set.remove::<K>(&key);
    })?;
    Ok(true)
}

/// The one first-seen dedup of a value list: the [`GroupKey`] equivalence
/// relation decides set membership, the first equivalent value is kept, and
/// the output stays in input order. Both value-list dedup sites call this —
/// the batch aggregate's `DISTINCT` operand fold and `coll.distinct()` — so
/// their one difference, that the aggregate path retains the values while
/// `coll.distinct` returns them, lives in the callers. Every insertion goes
/// through the charged [`DistinctSet`]; the set and its charge are released
/// before return because only the returned values outlive the set.
pub(crate) fn dedup_first_seen(vals: Vec<Value>) -> Result<Vec<Value>> {
    let mut seen = DistinctSet::default();
    let mut out = Vec::with_capacity(vals.len());
    for value in vals {
        if seen.insert_value(&value)? {
            out.push(value);
        }
    }
    drop(seen);
    Ok(out)
}
