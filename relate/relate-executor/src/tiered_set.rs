//! The tiered word-set utility (ADR-0522): one key class's words in the
//! cheapest representation the count admits — the tier mechanics
//! ADR-0507 built for the distinct set, freed of its DISTINCT semantics.
//!
//! One [`TieredWordSet`] holds the words of exactly one key class:
//!
//! * **T1** — up to `2^4 = 16` words inline in `[u64; 16]`, probed by a
//!   fixed-length, branch-free 16-lane mask compare: no heap, no hash;
//! * **T2** — on the 17th distinct word, one presized hash table
//!   (`2^5 = 32`), the hasher chosen once at construction:
//!   [`WordHash::Id`] (Fx) for engine-allocated ids no client can pick,
//!   [`WordHash::Value`] (the process-wide seeded state) for words a
//!   client can derive.
//!
//! Promotion is upward only, moves at most the 16 held words, and costs
//! no workspace charge (both tiers charge the same stride-8 figure). The
//! set owns its workspace charge: each insert is one probe and charges
//! the delta of the representation that took the word — before the write
//! in the inline tier (a refused charge leaves the tier `Empty` or
//! untouched), with the word removed again otherwise.
//!
//! The set knows raw `u64` words only — no `Value`, no `GroupKey`, no
//! `WordTag`, no DISTINCT semantics. Which words belong to which
//! instance, and what a tag switch between instances means, is the
//! caller's model.
//!
//! Layering: this module imports only `workspace`'s charge and bytes
//! helpers, the hasher seam and `relate_core` — never the exec tree or
//! the pattern walker (a unit test greps this source to hold that).

use relate_core::tokens::WorkspaceCharge;
use relate_core::Result;

use crate::hash_seed::{id_set_with_capacity, seeded_set_with_capacity, IdSet, SeededSet};
use crate::workspace::{charge_distinct_delta, word_insert_delta, word_set_bytes};

/// T1's capacity: `2^4` words, 128 B — two cache lines.
const INLINE_WORDS: usize = 16;

/// The promoted table's presize: `2^5`, the next power of two above T1.
const PROMOTED_CAPACITY: usize = 32;

/// The word tier: T1 inline, or T2 (the promoted table, held by value so
/// a promoted tier always has its table).
#[derive(Clone)]
enum Tier {
    /// No words held yet; the first word's insert transitions to
    /// [`Tier::Inline`].
    Empty,
    /// Inline words, `len >= 1`: the first word's insert charges before
    /// it writes, so a refused charge leaves the tier `Empty` and no
    /// rollback can strand a stale half-written tier.
    Inline { len: u8, slots: [u64; INLINE_WORDS] },
    /// Promoted: every word lives in the table.
    Promoted(WordTable),
}

/// The promoted table. The `Id`/`Value` split is the hasher policy (and
/// the seam a later dense id bitset slots into).
#[derive(Clone)]
enum WordTable {
    /// Engine-allocated ids: no client can choose them, so Fx.
    Id(IdSet<u64>),
    /// Client-derivable words: the seeded hasher (HashDoS).
    Value(SeededSet<u64>),
}

impl WordTable {
    /// One table under `hash`, presized to [`PROMOTED_CAPACITY`].
    fn for_hash(hash: WordHash) -> Self {
        match hash {
            WordHash::Id => Self::Id(id_set_with_capacity(PROMOTED_CAPACITY)),
            WordHash::Value => Self::Value(seeded_set_with_capacity(PROMOTED_CAPACITY)),
        }
    }

    fn insert(&mut self, word: u64) -> bool {
        match self {
            Self::Id(set) => set.insert(word),
            Self::Value(set) => set.insert(word),
        }
    }

    fn remove(&mut self, word: u64) {
        match self {
            Self::Id(set) => set.remove(&word),
            Self::Value(set) => set.remove(&word),
        };
    }

    fn len(&self) -> usize {
        match self {
            Self::Id(set) => set.len(),
            Self::Value(set) => set.len(),
        }
    }
}

/// T1 → T2's table: one presized table holding `words` (at most the 16
/// inline ones; promotion moves, it never re-reads input).
fn promoted_table(hash: WordHash, words: &[u64]) -> WordTable {
    let mut table = WordTable::for_hash(hash);
    for &word in words {
        table.insert(word);
    }
    table
}

/// Which words a [`TieredWordSet`] holds — the hasher policy of its
/// promoted table, chosen once at construction. `Id` for engine ids
/// (keyless Fx), `Value` for client-derivable words (the seeded state).
#[derive(Clone, Copy)]
pub(crate) enum WordHash {
    Id,
    Value,
}

/// One key class's tiered word set. See the module docs.
pub(crate) struct TieredWordSet {
    tier: Tier,
    hash: WordHash,
    charge: WorkspaceCharge,
}

impl TieredWordSet {
    /// An empty set whose promoted table will hash under `hash`.
    pub(crate) fn new(hash: WordHash) -> Self {
        Self {
            tier: Tier::Empty,
            hash,
            charge: WorkspaceCharge::empty(),
        }
    }

    /// Insert one word. `Ok(true)` when new, `Ok(false)` for a duplicate;
    /// a refused workspace charge leaves the set unchanged — the tier
    /// `Empty` or untouched when the write had not happened yet, the word
    /// removed again when it had.
    pub(crate) fn insert(&mut self, word: u64) -> Result<bool> {
        match &mut self.tier {
            Tier::Empty => {
                // Charge first: a refusal leaves the tier Empty, so no
                // rollback can strand a stale tier.
                charge_distinct_delta(&mut self.charge, || word_insert_delta(0))?;
                let mut slots = [0; INLINE_WORDS];
                slots[0] = word;
                self.tier = Tier::Inline { len: 1, slots };
                Ok(true)
            }
            Tier::Inline { len, slots } => {
                if inline_contains(slots, *len, word) {
                    return Ok(false);
                }
                if usize::from(*len) < INLINE_WORDS {
                    let old_len = usize::from(*len);
                    // Charge first: a refusal leaves the tier untouched.
                    charge_distinct_delta(&mut self.charge, || word_insert_delta(old_len))?;
                    slots[old_len] = word;
                    *len += 1;
                    return Ok(true);
                }
                // Promotion holds 16 words: a duplicate at len == 16
                // matched above. The copy ends the borrow so the tier can
                // be reassigned; the promotion itself costs no charge,
                // and a refused charge for the new word does not undo it.
                let slots = *slots;
                let mut table = promoted_table(self.hash, &slots);
                let inserted = insert_table_word(&mut self.charge, &mut table, word);
                self.tier = Tier::Promoted(table);
                inserted
            }
            Tier::Promoted(table) => insert_table_word(&mut self.charge, table, word),
        }
    }

    /// Distinct words held, across the live tier.
    pub(crate) fn len(&self) -> usize {
        match &self.tier {
            Tier::Empty => 0,
            Tier::Inline { len, .. } => usize::from(*len),
            Tier::Promoted(table) => table.len(),
        }
    }

    /// The accounted bytes of the live representation — the stride-8
    /// figure T1 and the promoted table share, so promotion is free.
    pub(crate) fn bytes(&self) -> u64 {
        word_set_bytes(self.len())
    }

    /// Whether the set has promoted to its table (T2). The distinct set
    /// consults this from its test-only tier probe today; the utility's
    /// raw-key consumers (the label-union planner work) arrive with
    /// their own call sites.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn promoted(&self) -> bool {
        matches!(self.tier, Tier::Promoted(_))
    }

    /// T1 → T2: move the held inline words into the presized table. No
    /// input is re-read and nothing ever moves back. No-op unless the
    /// tier holds inline words — the caller-forced promotion (the
    /// distinct set parks the active tier at a tag switch).
    pub(crate) fn promote_inline(&mut self) {
        if let Tier::Inline { len, slots } = &mut self.tier {
            // The copy ends the borrow so the tier can be reassigned.
            let words = *slots;
            let table = promoted_table(self.hash, &words[..usize::from(*len)]);
            self.tier = Tier::Promoted(table);
        }
    }

    /// The contents without a workspace reservation: the copy a consumer
    /// falls back to when a refused retain must not fail.
    pub(crate) fn clone_contents(&self) -> Self {
        Self {
            tier: self.tier.clone(),
            hash: self.hash,
            charge: WorkspaceCharge::empty(),
        }
    }

    /// Clone with the workspace retain propagated: an empty set reserves
    /// nothing, a filled one retains its [`TieredWordSet::bytes`], and a
    /// refusal is the caller's to decide (no silent fallback in the set).
    pub(crate) fn try_clone(&self) -> Result<Self> {
        let mut copy = self.clone_contents();
        copy.charge = WorkspaceCharge::retain_with(|| copy.bytes())?;
        Ok(copy)
    }

    /// The bytes this set's own workspace charge holds.
    #[cfg(test)]
    pub(crate) fn charged(&self) -> u64 {
        self.charge.bytes()
    }
}

/// The one charged insert of the promoted table: one probe, the delta
/// of the new word charged only under workspace accounting, and the word
/// removed again when the budget refuses it.
fn insert_table_word(
    charge: &mut WorkspaceCharge,
    table: &mut WordTable,
    word: u64,
) -> Result<bool> {
    if !table.insert(word) {
        return Ok(false);
    }
    let old_len = table.len() - 1;
    charge_distinct_delta(charge, || word_insert_delta(old_len))
        .inspect_err(|_| table.remove(word))?;
    Ok(true)
}

/// Whether `word` is among the first `len` slots: all 16 lanes compared
/// into a mask, then masked to the held length — fixed trip count, no
/// early exit, so the loop vectorises. Unused lanes are masked, not
/// sentinel-compared, because every `u64` is a valid payload.
#[inline]
fn inline_contains(slots: &[u64; INLINE_WORDS], len: u8, word: u64) -> bool {
    let mut mask: u32 = 0;
    for (lane, slot) in slots.iter().enumerate() {
        mask |= u32::from(*slot == word) << lane;
    }
    mask & ((1u32 << len) - 1) != 0
}
