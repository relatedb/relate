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
    charge_set_delta, distinct_row_insert_delta, distinct_row_set_bytes, group_key_insert_delta,
    group_key_set_bytes, DISTINCT_PEAK_FAMILY,
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
/// in and out of the active seat); [`Spill::slot`] is the one exhaustive
/// tag→index map, so a new tag is a compile error there, and every walk
/// below goes over the one array.
struct Spill {
    parked: [Option<TieredWordSet>; WordTag::ALL.len()],
    /// The general tables: an empty seeded set allocates nothing and bills
    /// nothing, so "no table yet" is the empty set, not a second mode.
    singles: SeededSet<GroupKey>,
    rows: SeededSet<Vec<GroupKey>>,
}

impl Default for Spill {
    fn default() -> Self {
        Self {
            parked: Default::default(),
            singles: seeded_set(),
            rows: seeded_set(),
        }
    }
}

impl Spill {
    /// The tag's slot: [`WordTag::index`], the exhaustive map the parked
    /// array is sized and ordered by.
    fn slot(tag: WordTag) -> usize {
        tag.index()
    }

    fn word_tier_mut(&mut self, tag: WordTag) -> &mut Option<TieredWordSet> {
        &mut self.parked[Self::slot(tag)]
    }

    /// Every tag's parked tier, for the `len`/`bytes`/`promoted` walks.
    fn word_tiers(&self) -> impl Iterator<Item = &TieredWordSet> {
        self.parked.iter().flatten()
    }

    /// The one fold over the parked tiers: `read` summed across them.
    fn parked_sum<N: std::iter::Sum>(&self, read: impl FnMut(&TieredWordSet) -> N) -> N {
        self.word_tiers().map(read).sum()
    }

    /// Distinct keys held in the heap tiers: the parked word tiers and
    /// the general tables.
    fn len(&self) -> usize {
        self.parked_sum(TieredWordSet::len) + self.singles.len() + self.rows.len()
    }

    /// The accounted bytes of the general tables (the parked word tiers
    /// bill themselves).
    fn general_bytes(&self) -> u64 {
        group_key_set_bytes(&self.singles) + distinct_row_set_bytes(&self.rows)
    }

    /// The uncharged contents copy: parked tiers and general tables.
    fn clone_contents(&self) -> Self {
        Self {
            parked: self
                .parked
                .each_ref()
                .map(|tier| tier.as_ref().map(TieredWordSet::clone_contents)),
            singles: self.singles.clone(),
            rows: self.rows.clone(),
        }
    }

    /// Clone with each parked tier's workspace retain propagated; a
    /// refusal propagates (the partially retained copy is dropped,
    /// releasing what it held). The general tables stay uncharged here —
    /// the set's own charge retains them.
    fn try_clone(&self) -> Result<Self> {
        let mut parked: [Option<TieredWordSet>; WordTag::ALL.len()] = Default::default();
        for (copy, tier) in parked.iter_mut().zip(&self.parked) {
            *copy = tier.as_ref().map(TieredWordSet::try_clone).transpose()?;
        }
        Ok(Self {
            parked,
            singles: self.singles.clone(),
            rows: self.rows.clone(),
        })
    }
}

/// The tiered distinct set. See the module docs.
pub(crate) struct DistinctSet {
    null: bool,
    /// The active word tier with the tag whose words it holds, as one
    /// fact: the current tag's [`TieredWordSet`], inline in the set (the
    /// one inline phase), promoted in place on the 17th word, parked into
    /// its spill slot on a tag switch. `None` while no word has been
    /// taken — no placeholder tier stands in for it.
    active: Option<(WordTag, TieredWordSet)>,
    spill: Option<Box<Spill>>,
    charge: WorkspaceCharge,
}

impl Default for DistinctSet {
    fn default() -> Self {
        Self {
            null: false,
            active: None,
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
            active: self
                .active
                .as_ref()
                .map(|(tag, words)| (*tag, words.clone_contents())),
            spill: self
                .spill
                .as_ref()
                .map(|spill| Box::new(spill.clone_contents())),
            charge: WorkspaceCharge::empty(),
        }
    }

    /// Every word tier's bytes: the active tier's and the parked ones'.
    #[cfg(test)]
    fn word_bytes(&self) -> u64 {
        self.active_words().map_or(0, TieredWordSet::bytes)
            + self
                .spill_ref()
                .map_or(0, |spill| spill.parked_sum(TieredWordSet::bytes))
    }

    /// Clone with the workspace retain propagated: an empty set reserves
    /// nothing, a filled one retains its [`DistinctSet::bytes`], and a
    /// refusal is the caller's to decide (no silent fallback in the set).
    pub(crate) fn try_clone(&self) -> Result<Self> {
        // The word share of bytes() is retained by the tiers' own
        // try_clones (the active tier here, the parked ones in the spill);
        // the general tables' share is the remainder, retained once. The
        // copy is built from those parts, never from a contents copy that
        // is then discarded.
        let active = self
            .active
            .as_ref()
            .map(|(tag, words)| words.try_clone().map(|words| (*tag, words)))
            .transpose()?;
        let spill = self
            .spill
            .as_ref()
            .map(|spill| spill.try_clone().map(Box::new))
            .transpose()?;
        let general = self.spill_ref().map_or(0, Spill::general_bytes);
        Ok(Self {
            null: self.null,
            active,
            spill,
            charge: WorkspaceCharge::retain_with(|| general)?,
        })
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
        usize::from(self.null)
            + self.active_words().map_or(0, TieredWordSet::len)
            + self.spill_ref().map_or(0, Spill::len)
    }

    /// The accounted bytes of every held representation — the walk the
    /// per-insert deltas must sum to. Test-only: a clone retains its parts'
    /// own figures.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> u64 {
        self.word_bytes() + self.spill_ref().map_or(0, Spill::general_bytes)
    }

    /// The bytes this set's accounted state holds — every word tier's
    /// charge plus the general tables' — the seen-set's share of a
    /// container's retained figure (PROFILE's terminal record).
    pub(crate) fn retained_bytes(&self) -> u64 {
        self.active_words().map_or(0, TieredWordSet::charged)
            + self
                .spill_ref()
                .map_or(0, |spill| spill.parked_sum(TieredWordSet::charged))
            + self.charge.bytes()
    }

    /// Whether the word tier has promoted to its per-tag tables (T2).
    #[cfg(test)]
    pub(crate) fn words_promoted(&self) -> bool {
        self.active_words().is_some_and(TieredWordSet::promoted)
            || self
                .spill_ref()
                .is_some_and(|spill| spill.word_tiers().any(TieredWordSet::promoted))
    }

    /// The active word tier, when a word has been taken.
    fn active_words(&self) -> Option<&TieredWordSet> {
        self.active.as_ref().map(|(_, words)| words)
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
        self.active_tier(tag).insert(word, DISTINCT_PEAK_FAMILY)
    }

    /// The active word tier for `tag`: the held one when the tag matches,
    /// else the incoming tag's parked tier or a fresh one built from
    /// [`word_hash`] alone (never from insert history). A refused charge
    /// can leave the fresh tier empty with the tag claimed; the next
    /// same-tag word fills it.
    fn active_tier(&mut self, tag: WordTag) -> &mut TieredWordSet {
        // A held tier of another tag is parked first, promoted — ADR-0507
        // §6's tag-homogeneous inline phase ends here — in its tag's slot;
        // an empty tier is never parked.
        if self.active.as_ref().is_some_and(|(held, _)| *held != tag) {
            if let Some((held, mut active)) = self.active.take() {
                active.promote_inline();
                if active.len() > 0 {
                    *Self::spill(&mut self.spill).word_tier_mut(held) = Some(active);
                }
            }
        }
        let spill = &mut self.spill;
        &mut self
            .active
            .get_or_insert_with(|| {
                // Seating a parked tier must not materialize the spill: a
                // set that never parks anything stays allocation-free here.
                let parked = spill
                    .as_deref_mut()
                    .and_then(|spill| spill.word_tier_mut(tag).take());
                (
                    tag,
                    parked.unwrap_or_else(|| TieredWordSet::new(word_hash(tag))),
                )
            })
            .1
    }

    fn insert_single(&mut self, key: GroupKey) -> Result<bool> {
        let singles = &mut Self::spill(&mut self.spill).singles;
        insert_charged(&mut self.charge, singles, key, group_key_insert_delta)
    }

    fn insert_row_key(&mut self, key: Vec<GroupKey>) -> Result<bool> {
        let rows = &mut Self::spill(&mut self.spill).rows;
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
        // Not a second copy of `charge_set_delta`'s gate (its delta
        // closure is already lazy): this skips the clone/undo probe
        // protocol itself when no budget can refuse it.
        return Ok(set.insert(key));
    }
    let old_len = set.len();
    if !set.insert(key.clone()) {
        return Ok(false);
    }
    charge_set_delta(charge, DISTINCT_PEAK_FAMILY, || {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `WordTag::ALL` lists the tags in index order, one per parked slot.
    #[test]
    fn word_tag_all_is_listed_in_index_order() {
        for (position, tag) in WordTag::ALL.into_iter().enumerate() {
            assert_eq!(Spill::slot(tag), position, "{tag:?}");
        }
    }
}
