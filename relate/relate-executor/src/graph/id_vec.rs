//! Id-indexed slots for the four core stores (ADR-0532). A lookup is an
//! index under a short read lock. Growth takes the write lock, so a slot
//! never moves while a guard holds it.
//!
//! An id whose reserved slot array does not fit is one side-map entry. The
//! allocator's last id is that case; ordinary monotonic ids never reach it.
//!
//! Do not call back into the same store while one of its guards or its
//! iterator is alive: the lock is not re-entrant.

use std::collections::HashMap;
use std::mem::size_of;
use std::ops::{Deref, DerefMut};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::memory::{dense_span_bytes, fit_reserved, id_vec_bytes, overflow_entry_bytes};

/// `None` is a hole: a deleted id, or an id below the highest one stored.
#[derive(Debug)]
pub(super) struct IdVec<T> {
    slots: RwLock<Store<T>>,
}

#[derive(Debug)]
struct Store<T> {
    dense: Vec<Option<T>>,
    overflow: HashMap<i64, T>,
}

impl<T> Default for Store<T> {
    fn default() -> Self {
        Self {
            dense: Vec::new(),
            overflow: HashMap::new(),
        }
    }
}

enum Loc {
    Dense(usize),
    Overflow(i64),
}

/// A read guard for one occupied slot.
pub(super) struct IdRef<'a, T> {
    guard: RwLockReadGuard<'a, Store<T>>,
    loc: Loc,
}

impl<T> Deref for IdRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self.loc {
            Loc::Dense(idx) => self.guard.dense[idx]
                .as_ref()
                .expect("the slot is occupied"),
            Loc::Overflow(id) => self.guard.overflow.get(&id).expect("the slot is occupied"),
        }
    }
}

/// A write guard for one occupied slot.
pub(super) struct IdMut<'a, T> {
    guard: RwLockWriteGuard<'a, Store<T>>,
    loc: Loc,
}

impl<T> Deref for IdMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self.loc {
            Loc::Dense(idx) => self.guard.dense[idx]
                .as_ref()
                .expect("the slot is occupied"),
            Loc::Overflow(id) => self.guard.overflow.get(&id).expect("the slot is occupied"),
        }
    }
}

impl<T> DerefMut for IdMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        match self.loc {
            Loc::Dense(idx) => self.guard.dense[idx]
                .as_mut()
                .expect("the slot is occupied"),
            Loc::Overflow(id) => self
                .guard
                .overflow
                .get_mut(&id)
                .expect("the slot is occupied"),
        }
    }
}

/// One occupied slot yielded by [`IdVec::iter`]. Valid while the iterator
/// is alive.
pub(super) struct IdItem<'a, T> {
    id: i64,
    value: &'a T,
}

impl<T> IdItem<'_, T> {
    pub(super) fn key(&self) -> &i64 {
        &self.id
    }

    pub(super) fn value(&self) -> &T {
        self.value
    }
}

impl<T> Deref for IdItem<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value
    }
}

pub(super) struct Iter<'a, T> {
    guard: RwLockReadGuard<'a, Store<T>>,
    idx: usize,
    overflow: Vec<(i64, *const T)>,
    overflow_at: usize,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = IdItem<'a, T>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < self.guard.dense.len() {
            let idx = self.idx;
            self.idx += 1;
            if self.guard.dense[idx].is_some() {
                // SAFETY: the guard owns the vector for the iterator's
                // lifetime, and iteration only reads it. The item is used
                // while that iterator is alive.
                let value: &'a T = unsafe {
                    (*self.guard.dense.as_ptr().add(idx))
                        .as_ref()
                        .unwrap_unchecked()
                };
                return Some(IdItem {
                    id: i64::try_from(idx).expect("a dense id fits i64"),
                    value,
                });
            }
        }
        let (id, ptr) = *self.overflow.get(self.overflow_at)?;
        self.overflow_at += 1;
        // SAFETY: the pointer was taken from the overflow map under this
        // guard, and iteration does not mutate the map.
        let value: &'a T = unsafe { &*ptr };
        Some(IdItem { id, value })
    }
}

pub(super) enum Entry<'a, T> {
    Occupied(Occupied<'a, T>),
    Vacant(Vacant<'a, T>),
}

pub(super) struct Occupied<'a, T> {
    guard: RwLockWriteGuard<'a, Store<T>>,
    loc: Loc,
}

impl<'a, T> Occupied<'a, T> {
    pub(super) fn get_mut(&mut self) -> &mut T {
        match self.loc {
            Loc::Dense(idx) => self.guard.dense[idx]
                .as_mut()
                .expect("the slot is occupied"),
            Loc::Overflow(id) => self
                .guard
                .overflow
                .get_mut(&id)
                .expect("the slot is occupied"),
        }
    }
}

pub(super) struct Vacant<'a, T> {
    guard: RwLockWriteGuard<'a, Store<T>>,
    loc: Loc,
}

impl<'a, T> Vacant<'a, T> {
    pub(super) fn insert(&mut self, value: T) -> &mut T {
        match self.loc {
            Loc::Dense(idx) => {
                self.guard.dense[idx] = Some(value);
                self.guard.dense[idx]
                    .as_mut()
                    .expect("the slot was just filled")
            }
            Loc::Overflow(id) => self.guard.overflow.entry(id).or_insert(value),
        }
    }
}

impl<'a, T: Default> Entry<'a, T> {
    pub(super) fn or_default(&mut self) -> &mut T {
        match self {
            Self::Occupied(slot) => slot.get_mut(),
            Self::Vacant(slot) => slot.insert(T::default()),
        }
    }
}

fn index(id: i64) -> Option<usize> {
    usize::try_from(id).ok()
}

/// Grow the dense vector to include `idx`, or report that the reservation
/// does not fit and the id belongs in the side map.
fn try_grow<T>(dense: &mut Vec<Option<T>>, idx: usize) -> bool {
    if idx < dense.len() {
        return true;
    }
    let new_len = idx.saturating_add(1);
    if new_len <= dense.len() {
        return false;
    }
    if dense_span_bytes(new_len, size_of::<Option<T>>() as u64).is_none() {
        return false;
    }
    if dense.try_reserve(new_len - dense.len()).is_err() {
        return false;
    }
    dense.resize_with(new_len, || None);
    fit_reserved(dense);
    true
}

impl<T> Default for IdVec<T> {
    fn default() -> Self {
        Self {
            slots: RwLock::new(Store::default()),
        }
    }
}

impl<T> IdVec<T> {
    #[allow(dead_code)]
    fn new() -> Self {
        Self::default()
    }

    /// Occupied slots, dense and side map together.
    #[allow(dead_code)]
    pub(super) fn len(&self) -> usize {
        let store = self.slots.read().expect("id store lock");
        store.dense.iter().filter(|slot| slot.is_some()).count() + store.overflow.len()
    }

    /// Length of the dense vector, holes included. Side-map ids are not in it.
    pub(super) fn span(&self) -> usize {
        self.slots.read().expect("id store lock").dense.len()
    }

    /// Reserved slot-array bytes, holes included, plus one side-map entry
    /// per id that could not join the array.
    pub(super) fn array_bytes(&self) -> u64 {
        let store = self.slots.read().expect("id store lock");
        let slot = size_of::<Option<T>>() as u64;
        let value = size_of::<T>() as u64;
        id_vec_bytes(store.dense.len(), slot)
            + store.overflow.len() as u64 * overflow_entry_bytes(value)
    }

    pub(super) fn contains_key(&self, id: &i64) -> bool {
        let Some(idx) = index(*id) else {
            return false;
        };
        let store = self.slots.read().expect("id store lock");
        if idx < store.dense.len() {
            store.dense[idx].is_some()
        } else {
            store.overflow.contains_key(id)
        }
    }

    #[allow(dead_code)]
    pub(super) fn is_empty(&self) -> bool {
        let store = self.slots.read().expect("id store lock");
        store.overflow.is_empty() && store.dense.iter().all(Option::is_none)
    }

    pub(super) fn get(&self, id: &i64) -> Option<IdRef<'_, T>> {
        let idx = index(*id)?;
        let guard = self.slots.read().expect("id store lock");
        let loc = if idx < guard.dense.len() && guard.dense[idx].is_some() {
            Loc::Dense(idx)
        } else if idx >= guard.dense.len() && guard.overflow.contains_key(id) {
            Loc::Overflow(*id)
        } else {
            return None;
        };
        Some(IdRef { guard, loc })
    }

    pub(super) fn get_mut(&self, id: &i64) -> Option<IdMut<'_, T>> {
        let idx = index(*id)?;
        let guard = self.slots.write().expect("id store lock");
        let loc = if idx < guard.dense.len() && guard.dense[idx].is_some() {
            Loc::Dense(idx)
        } else if idx >= guard.dense.len() && guard.overflow.contains_key(id) {
            Loc::Overflow(*id)
        } else {
            return None;
        };
        Some(IdMut { guard, loc })
    }

    #[allow(dead_code)]
    pub(super) fn insert(&self, id: i64, value: T) -> Option<T> {
        let idx = index(id).expect("id fits an index");
        let mut store = self.slots.write().expect("id store lock");
        if idx < store.dense.len() || try_grow(&mut store.dense, idx) {
            store.dense[idx].replace(value)
        } else {
            store.overflow.insert(id, value)
        }
    }

    /// Drop the value. A dense slot stays, so the span does not shrink.
    pub(super) fn remove(&self, id: &i64) -> Option<T> {
        let idx = index(*id)?;
        let mut store = self.slots.write().expect("id store lock");
        if idx < store.dense.len() {
            store.dense[idx].take()
        } else {
            store.overflow.remove(id)
        }
    }

    pub(super) fn retain(&self, mut f: impl FnMut(&i64, &mut T) -> bool) {
        let mut store = self.slots.write().expect("id store lock");
        for (idx, slot) in store.dense.iter_mut().enumerate() {
            if slot.is_none() {
                continue;
            }
            let id = i64::try_from(idx).expect("a dense id fits i64");
            let keep = f(&id, slot.as_mut().expect("the slot is occupied"));
            if !keep {
                *slot = None;
            }
        }
        let ids: Vec<i64> = store.overflow.keys().copied().collect();
        for id in ids {
            let keep = f(
                &id,
                store
                    .overflow
                    .get_mut(&id)
                    .expect("the side entry is present"),
            );
            if !keep {
                store.overflow.remove(&id);
            }
        }
    }

    pub(super) fn clear(&self) {
        let mut store = self.slots.write().expect("id store lock");
        store.dense.clear();
        store.overflow.clear();
    }

    pub(super) fn iter(&self) -> Iter<'_, T> {
        let guard = self.slots.read().expect("id store lock");
        let overflow = guard
            .overflow
            .iter()
            .map(|(id, value)| (*id, value as *const T))
            .collect();
        Iter {
            guard,
            idx: 0,
            overflow,
            overflow_at: 0,
        }
    }

    pub(super) fn entry(&self, id: i64) -> Entry<'_, T> {
        let idx = index(id).expect("id fits an index");
        let mut guard = self.slots.write().expect("id store lock");
        if idx < guard.dense.len() {
            if guard.dense[idx].is_some() {
                Entry::Occupied(Occupied {
                    guard,
                    loc: Loc::Dense(idx),
                })
            } else {
                Entry::Vacant(Vacant {
                    guard,
                    loc: Loc::Dense(idx),
                })
            }
        } else if try_grow(&mut guard.dense, idx) {
            Entry::Vacant(Vacant {
                guard,
                loc: Loc::Dense(idx),
            })
        } else if guard.overflow.contains_key(&id) {
            Entry::Occupied(Occupied {
                guard,
                loc: Loc::Overflow(id),
            })
        } else {
            Entry::Vacant(Vacant {
                guard,
                loc: Loc::Overflow(id),
            })
        }
    }

    /// Occupied ids, copied so the caller can re-enter the store.
    pub(super) fn keys(&self) -> Vec<i64> {
        self.iter().map(|item| *item.key()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_reads_back_and_a_removed_id_is_a_hole() {
        let ids = IdVec::new();
        assert!(ids.is_empty());
        ids.insert(0, "a");
        ids.insert(2, "c");
        assert_eq!(ids.span(), 3);
        assert_eq!(ids.len(), 2);
        assert_eq!(*ids.get(&0).unwrap(), "a");
        assert!(ids.get(&1).is_none());
        assert_eq!(ids.remove(&0), Some("a"));
        assert!(!ids.contains_key(&0));
        assert_eq!(ids.span(), 3, "a hole does not shrink the store");
        assert_eq!(ids.len(), 1);
        assert_eq!(ids.keys(), vec![2]);
    }

    #[test]
    fn growth_reserves_the_next_power_of_two() {
        let ids = IdVec::new();
        for id in 0..3 {
            ids.insert(id, id);
        }
        let store = ids.slots.read().unwrap();
        assert_eq!(store.dense.len(), 3);
        assert_eq!(store.dense.capacity(), crate::memory::reserved_len(3));
    }

    #[test]
    fn an_id_whose_slot_array_does_not_fit_is_a_side_entry() {
        let ids = IdVec::new();
        ids.insert(0, "low");
        let high = i64::MAX - 1;
        assert_eq!(ids.insert(high, "high"), None);
        assert_eq!(ids.span(), 1, "the side entry does not extend the vector");
        assert_eq!(*ids.get(&high).unwrap(), "high");
        assert!(ids.contains_key(&high));
        assert_eq!(ids.len(), 2);
        let mut keys = ids.keys();
        keys.sort();
        assert_eq!(keys, vec![0, high]);
        assert_eq!(ids.remove(&high), Some("high"));
        assert!(ids.get(&high).is_none());
        assert_eq!(*ids.get(&0).unwrap(), "low");
        assert_eq!(ids.span(), 1);
    }

    #[test]
    fn a_reader_and_a_writer_on_different_ids_stay_consistent() {
        use std::sync::Arc;
        let ids = Arc::new(IdVec::new());
        ids.insert(0, 1u64);
        let reader = Arc::clone(&ids);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                assert_eq!(*reader.get(&0).unwrap(), 1);
            }
        });
        for id in 1..64 {
            ids.insert(id, id as u64);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        handle.join().unwrap();
        assert_eq!(*ids.get(&63).unwrap(), 63);
        assert_eq!(*ids.get(&0).unwrap(), 1);
    }
}
