//! The value → ids table behind a lazy equality pair (ADR-0063, ADR-0185,
//! ADR-0614): one compact bucket per distinct property value.
//!
//! Most buckets hold exactly one id (an `id`-style property), a few hold
//! very many (a status or a year). The table is shaped for both:
//!
//! * a bucket is one [`Slot`] word — the id itself when it is alone, or
//!   the index of a sorted `Vec<NodeId>` in the table's [`Slab`] when two
//!   or more ids share the value (membership by binary search, ascending
//!   iteration, no per-bucket hash table, and a unique key pays no `Vec`
//!   header);
//! * the dominant key — an integer, which `Float(1.0)` shares by the
//!   equivalence rule — keys an `i64` table instead of the general
//!   [`GroupKey`] tree, so an integer bucket costs a word of key and a
//!   word of slot, not the key's full footprint;
//! * an emptied bucket leaves the table, so a value no id carries any more
//!   costs nothing;
//! * a build that sees a nearly unique key presizes its table once
//!   ([`PairPostings::presize`]), so it never holds a table and its
//!   doubled successor at the same time.
//!
//! Both tables use the seeded hash policy: a property value is a key a
//! client can steer, and ADR-0355 bars the keyless id hasher from those.
//! Node ids ride no hash table here at all.

use super::*;
use crate::group_key::GroupKey;
use crate::hash_seed::{seeded_map, SeededMap};
use std::hash::Hash;

/// Candidates a build posts before it decides whether the key looks
/// unique and presizes ([`PairPostings::presize`]).
pub(super) const PRESIZE_PREFIX: usize = 1024;

/// One bucket's word. A non-negative value is the bucket's only id; a
/// negative value is `!i`, the index `i` of the bucket's ids in the
/// table's [`Slab`] (two or more ids, ascending). A persisted node id is
/// non-negative (`validate_persisted_id`) and a virtual, negative id has
/// no store entry and is never posted, so the sign is free to carry the
/// form.
#[derive(Clone, Copy, Debug)]
struct Slot(NodeId);

/// The two forms a [`Slot`] decodes to — the one place the sign is read.
enum SlotKind {
    One(NodeId),
    Many(usize),
}

impl Slot {
    fn one(id: NodeId) -> Self {
        assert!(
            id >= 0,
            "a posted node id is a persisted, non-negative id, got {id}"
        );
        Slot(id)
    }

    fn many(index: usize) -> Self {
        debug_assert!(
            NodeId::try_from(index).is_ok(),
            "a slab index fits a node id"
        );
        Slot(!(index as NodeId))
    }

    fn kind(self) -> SlotKind {
        if self.0 >= 0 {
            SlotKind::One(self.0)
        } else {
            SlotKind::Many((!self.0) as usize)
        }
    }

    /// The bucket's ids ascending. A lone id is borrowed from the slot
    /// itself.
    fn ids<'a>(&'a self, slab: &'a Slab) -> &'a [NodeId] {
        match self.kind() {
            SlotKind::One(_) => std::slice::from_ref(&self.0),
            SlotKind::Many(index) => &slab.lists[index],
        }
    }
}

/// The id lists of every `Many` bucket of one table. A list whose bucket
/// demotes to one id gives its index back for the next bucket to reuse.
#[derive(Debug, Default)]
struct Slab {
    lists: Vec<Vec<NodeId>>,
    free: Vec<usize>,
}

impl Slab {
    fn alloc(&mut self, ids: Vec<NodeId>) -> Slot {
        let index = self.free.pop().unwrap_or_else(|| {
            self.lists.push(Vec::new());
            self.lists.len() - 1
        });
        self.lists[index] = ids;
        Slot::many(index)
    }

    fn release(&mut self, index: usize) {
        self.lists[index] = Vec::new();
        self.free.push(index);
    }

    fn shrink_to_fit(&mut self) {
        self.lists.iter_mut().for_each(Vec::shrink_to_fit);
        self.lists.shrink_to_fit();
        self.free.shrink_to_fit();
    }
}

/// One key class's table: the bucket protocol written once, for either key
/// type.
#[derive(Debug)]
struct Table<K> {
    map: SeededMap<K, Slot>,
    slab: Slab,
}

impl<K: Eq + Hash> Table<K> {
    /// Reserve the buckets `rest` more candidates will add, at the rate the
    /// first `prefix` candidates produced them.
    fn reserve_rest(&mut self, prefix: usize, rest: usize) {
        self.map.reserve(self.len().saturating_mul(rest) / prefix);
    }

    fn new() -> Self {
        Self {
            map: seeded_map(),
            slab: Slab::default(),
        }
    }

    fn post(&mut self, key: K, id: NodeId) {
        assert!(
            id >= 0,
            "a posted node id is a persisted, non-negative id, got {id}"
        );
        let slot = self.map.entry(key).or_insert_with(|| Slot::one(id));
        match slot.kind() {
            SlotKind::One(held) if held == id => {}
            SlotKind::One(held) => {
                let pair = if held < id { [held, id] } else { [id, held] };
                *slot = self.slab.alloc(pair.to_vec());
            }
            SlotKind::Many(index) => {
                let ids = &mut self.slab.lists[index];
                if let Err(at) = ids.binary_search(&id) {
                    ids.insert(at, id);
                }
            }
        }
    }

    /// Remove `id` from `key`'s bucket. The bucket demotes to one inline
    /// id when it shrinks to a singleton and leaves the table when it
    /// empties; an absent id changes nothing.
    fn unpost(&mut self, key: &K, id: NodeId) {
        let Some(slot) = self.map.get_mut(key) else {
            return;
        };
        match slot.kind() {
            SlotKind::One(held) => {
                if held == id {
                    self.map.remove(key);
                }
            }
            SlotKind::Many(index) => {
                let ids = &mut self.slab.lists[index];
                if let Ok(at) = ids.binary_search(&id) {
                    ids.remove(at);
                    if let [only] = ids.as_slice() {
                        let only = *only;
                        self.slab.release(index);
                        *slot = Slot::one(only);
                    } else if ids.capacity() > 2 * ids.len() + 8 {
                        ids.shrink_to_fit();
                    }
                }
            }
        }
    }

    fn get(&self, key: &K) -> Option<&[NodeId]> {
        self.map.get(key).map(|slot| slot.ids(&self.slab))
    }

    /// Release the capacity a build's appends left behind: every list to
    /// its length, the slab and the table to their entry counts.
    fn shrink_to_fit(&mut self) {
        self.slab.shrink_to_fit();
        self.map.shrink_to_fit();
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    /// The ids held across every bucket.
    fn posted(&self) -> usize {
        self.map
            .values()
            .map(|slot| slot.ids(&self.slab).len())
            .sum()
    }

    fn keys(&self) -> impl Iterator<Item = &K> {
        self.map.keys()
    }
}

/// Property value → ids for one lazy pair. Integer-equivalent values key
/// the `i64` table; every other value keys the general one.
#[derive(Debug)]
pub(super) struct PairPostings {
    ints: Table<i64>,
    others: Table<GroupKey>,
}

impl Default for PairPostings {
    fn default() -> Self {
        Self {
            ints: Table::new(),
            others: Table::new(),
        }
    }
}

impl PairPostings {
    /// Post `id` under `gk`. A bulk build posts ids ascending, which keeps
    /// every `Many` bucket an append.
    pub(super) fn post(&mut self, gk: &GroupKey, id: NodeId) {
        match gk.as_int() {
            Some(i) => self.ints.post(i, id),
            None => self.others.post(gk.clone(), id),
        }
    }

    /// Remove `id` from `gk`'s bucket, dropping the bucket when it empties.
    pub(super) fn unpost(&mut self, gk: &GroupKey, id: NodeId) {
        match gk.as_int() {
            Some(i) => self.ints.unpost(&i, id),
            None => self.others.unpost(gk, id),
        }
    }

    /// The bucket's ids ascending, or `None` when no id carries `gk`.
    pub(super) fn bucket(&self, gk: &GroupKey) -> Option<&[NodeId]> {
        match gk.as_int() {
            Some(i) => self.ints.get(&i),
            None => self.others.get(gk),
        }
    }

    /// Decide, after the build's first [`PRESIZE_PREFIX`] candidates, whether
    /// the key looks unique and the table should reserve for the rest: when
    /// the prefix's buckets are nearly as many as its posted ids, the table
    /// that took most keys reserves the buckets the remaining `total -
    /// prefix` candidates will add at the prefix's rate. A key with few
    /// distinct values reserves nothing, so a build over a million-node
    /// label with five values never sizes a table for a million.
    pub(super) fn presize(&mut self, prefix: usize, total: usize) {
        if prefix == 0 || total <= prefix {
            return;
        }
        let buckets = self.ints.len() + self.others.len();
        let ids = self.ints.posted() + self.others.posted();
        if buckets == 0 || buckets * 10 < ids * 9 {
            return;
        }
        let rest = total - prefix;
        if self.ints.len() >= self.others.len() {
            self.ints.reserve_rest(prefix, rest);
        } else {
            self.others.reserve_rest(prefix, rest);
        }
    }

    /// Release the capacity a build's appends left behind: every `Many`
    /// list to its length, both tables to their entry count.
    pub(super) fn shrink_to_fit(&mut self) {
        self.ints.shrink_to_fit();
        self.others.shrink_to_fit();
    }

    /// The bucket's ids ascending; empty when no id carries `gk`.
    pub(super) fn ids(&self, gk: &GroupKey) -> &[NodeId] {
        self.bucket(gk).unwrap_or(&[])
    }

    /// Every posted value's key in the canonical [`GroupKey`] order — the
    /// order any deterministic walk of group keys states.
    pub(super) fn keys_sorted(&self) -> Vec<GroupKey> {
        let mut keys: Vec<GroupKey> = self
            .ints
            .keys()
            .map(|i| GroupKey::from_int(*i))
            .chain(self.others.keys().cloned())
            .collect();
        keys.sort();
        keys
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.ints.len() + self.others.len()
    }
}

#[cfg(test)]
mod tests;
