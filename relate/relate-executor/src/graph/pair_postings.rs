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

/// The ids that carried one value, ascending. `Many` always holds two or
/// more ids: a removal that leaves one demotes it to `One`, so the vector
/// is freed when the bucket shrinks back to a singleton.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// leave the table.
    fn remove(&mut self, id: NodeId) -> bool {
        match self {
            IdBag::One(held) => *held == id,
            IdBag::Many(ids) => {
                if let Ok(at) = ids.binary_search(&id) {
                    ids.remove(at);
                }
                if let [only] = ids.as_slice() {
                    *self = IdBag::One(*only);
                } else if ids.capacity() > 2 * ids.len() + 8 {
                    ids.shrink_to_fit();
                }
                false
            }
        }
    }
}

/// Property value → ids for one lazy pair. Integer-equivalent values key
/// the `i64` table; every other value keys the general one.
#[derive(Debug)]
pub(super) struct PairPostings {
    ints: SeededMap<i64, IdBag>,
    others: SeededMap<GroupKey, IdBag>,
}

impl Default for PairPostings {
    fn default() -> Self {
        Self {
            ints: seeded_map(),
            others: seeded_map(),
        }
    }
}

impl PairPostings {
    /// Post `id` under `gk`. A bulk build posts ids ascending, which keeps
    /// every `Many` bucket an append.
    pub(super) fn post(&mut self, gk: &GroupKey, id: NodeId) {
        match gk.as_int() {
            Some(int) => self
                .ints
                .entry(int)
                .and_modify(|b| b.insert(id))
                .or_insert(IdBag::One(id)),
            None => self
                .others
                .entry(gk.clone())
                .and_modify(|b| b.insert(id))
                .or_insert(IdBag::One(id)),
        };
    }

    /// Remove `id` from `gk`'s bucket, dropping the bucket when it empties.
    pub(super) fn unpost(&mut self, gk: &GroupKey, id: NodeId) {
        match gk.as_int() {
            Some(int) => {
                if self.ints.get_mut(&int).is_some_and(|b| b.remove(id)) {
                    self.ints.remove(&int);
                }
            }
            None => {
                if self.others.get_mut(gk).is_some_and(|b| b.remove(id)) {
                    self.others.remove(gk);
                }
            }
        }
    }

    /// The bucket's ids ascending, or `None` when no id carries `gk`.
    pub(super) fn bucket(&self, gk: &GroupKey) -> Option<&[NodeId]> {
        match gk.as_int() {
            Some(int) => self.ints.get(&int),
            None => self.others.get(gk),
        }
        .map(IdBag::as_slice)
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
