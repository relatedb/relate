//! The value → ids table behind a lazy equality pair (ADR-0063, ADR-0185,
//! ADR-0614): one compact bucket per distinct property value.
//!
//! Most buckets hold exactly one id (an `id`-style property), a few hold
//! very many (a status or a year). The table is shaped for both:
//!
//! * a bucket is [`IdBag`] — `One(id)` inline, or `Many` as a sorted
//!   `Vec<NodeId>` (membership by binary search, ascending iteration, no
//!   per-bucket hash table);
//! * the dominant key — an integer, which `Float(1.0)` shares by the
//!   equivalence rule — keys an `i64` table instead of the general
//!   [`GroupKey`] tree, so an integer bucket costs a word of key, not the
//!   key's full footprint;
//! * an emptied bucket leaves the table, so a value no id carries any more
//!   costs nothing.
//!
//! Both tables use the seeded hash policy: a property value is a key a
//! client can steer, and ADR-0355 bars the keyless id hasher from those.
//! Node ids ride no hash table here at all.

use super::*;
use crate::group_key::GroupKey;
use crate::hash_seed::{seeded_map, SeededMap};
use std::hash::Hash;

/// The ids that carried one value, ascending. `Many` always holds two or
/// more ids: a removal that leaves one demotes it to `One`, so the vector
/// is freed when the bucket shrinks back to a singleton.
#[derive(Debug)]
enum IdBag {
    One(NodeId),
    Many(Vec<NodeId>),
}

impl IdBag {
    fn as_slice(&self) -> &[NodeId] {
        match self {
            IdBag::One(id) => std::slice::from_ref(id),
            IdBag::Many(ids) => ids,
        }
    }

    fn insert(&mut self, id: NodeId) {
        match self {
            IdBag::One(held) if *held == id => {}
            IdBag::One(held) => {
                let pair = if *held < id { [*held, id] } else { [id, *held] };
                *self = IdBag::Many(pair.to_vec());
            }
            IdBag::Many(ids) => {
                if let Err(at) = ids.binary_search(&id) {
                    ids.insert(at, id);
                }
            }
        }
    }

    /// Remove `id`; `true` when the bag is now empty and its entry should
    /// leave the table. An absent id changes nothing.
    fn remove(&mut self, id: NodeId) -> bool {
        match self {
            IdBag::One(held) => *held == id,
            IdBag::Many(ids) => {
                if let Ok(at) = ids.binary_search(&id) {
                    ids.remove(at);
                    if let [only] = ids.as_slice() {
                        *self = IdBag::One(*only);
                    } else if ids.capacity() > 2 * ids.len() + 8 {
                        ids.shrink_to_fit();
                    }
                }
                false
            }
        }
    }

    /// Drop the slack a bulk build's appends leave in a `Many` vector.
    fn shrink(&mut self) {
        if let IdBag::Many(ids) = self {
            ids.shrink_to_fit();
        }
    }
}

/// One key class's table: the bucket protocol written once, for either key
/// type.
#[derive(Debug)]
struct Table<K>(SeededMap<K, IdBag>);

impl<K: Eq + Hash> Table<K> {
    fn post(&mut self, key: K, id: NodeId) {
        self.0
            .entry(key)
            .and_modify(|b| b.insert(id))
            .or_insert(IdBag::One(id));
    }

    fn unpost(&mut self, key: &K, id: NodeId) {
        if self.0.get_mut(key).is_some_and(|b| b.remove(id)) {
            self.0.remove(key);
        }
    }

    fn get(&self, key: &K) -> Option<&[NodeId]> {
        self.0.get(key).map(IdBag::as_slice)
    }

    fn shrink_to_fit(&mut self) {
        self.0.values_mut().for_each(IdBag::shrink);
        self.0.shrink_to_fit();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }

    fn keys(&self) -> impl Iterator<Item = &K> {
        self.0.keys()
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
            ints: Table(seeded_map()),
            others: Table(seeded_map()),
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

    /// Release the capacity a build's appends left behind: every `Many`
    /// vector to its length, both tables to their entry count.
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
