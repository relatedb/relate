//! Id-indexed slots for the four core stores (ADR-0532). A lookup is an
//! index under a short read lock. Growth takes the write lock, so a slot
//! never moves while a guard holds it.
//!
//! An id whose reserved slot array does not fit is one side-map entry. That
//! is the allocator's last id, and any id whose reservation would pass
//! [`crate::memory::DENSE_SPAN_CAP_BYTES`]. Ordinary monotonic ids never reach it.
//!
//! Do not call back into the same store while one of its guards is alive,
//! including from [`IdVec::for_each`]: the lock is not re-entrant.

use std::collections::HashMap;
use std::mem::{replace, size_of};
use std::ops::{Deref, DerefMut};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use relate_core::Result;

use crate::memory::{dense_or_side, span_table_bytes};

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

/// The dense slot stride — the one number the placement gate's growth and
/// the floor's charge must price identically (ADR-0617), the id stores'
/// counterpart of the span table's `span_stride`.
pub(super) fn slot<T>() -> u64 {
    size_of::<Option<T>>() as u64
}

/// Where an occupied id lives. A dense hole does not hide a side-map entry:
/// growth can extend the vector past an id that was placed aside when its
/// reservation did not fit.
fn occupied<T>(
    dense: &[Option<T>],
    overflow: &HashMap<i64, T>,
    id: i64,
    idx: usize,
) -> Option<Loc> {
    if idx < dense.len() && dense[idx].is_some() {
        Some(Loc::Dense(idx))
    } else if overflow.contains_key(&id) {
        Some(Loc::Overflow(id))
    } else {
        None
    }
}

/// Whether `idx` has a dense slot — inside the covered span already, or
/// grown to cover it — or belongs in the side map. The three classes are
/// [`SpanPlacement`]'s, the one placement policy; this fn applies its own
/// effect (the reservation) to the Grow class and reports the other two.
/// `false` therefore means one thing — the id is side-managed — for two
/// reasons: the policy's gate refused it, or the reservation the gate
/// admitted could not be allocated. The second keeps the store's own
/// degradation (an id stays reachable in the side map rather than failing
/// the write), so both collapse into the side-map answer here.
fn try_grow<T>(dense: &mut Vec<Option<T>>, idx: usize) -> bool {
    // The id stores' binding of the one application
    // ([`crate::memory::dense_or_side`]): Option slots filled with None,
    // the ladder growth and the side-map answer living in memory.
    dense_or_side(dense, idx, slot::<T>(), || None)
}

/// Where `id` sits under an already held write lock: its occupied location,
/// or the vacant location the placement policy gives it — the dense array
/// grown to cover a `Grew` id, the side map otherwise.
enum Placed {
    Occupied(Loc),
    Vacant(Loc),
}

/// The one copy of the placement policy's lock-held effect: [`occupied`]
/// first, then [`try_grow`]'s answer for an id it does not hold.
fn place<T>(store: &mut Store<T>, id: i64, idx: usize) -> Placed {
    if let Some(loc) = occupied(&store.dense, &store.overflow, id, idx) {
        return Placed::Occupied(loc);
    }
    if try_grow(&mut store.dense, idx) {
        Placed::Vacant(Loc::Dense(idx))
    } else {
        Placed::Vacant(Loc::Overflow(id))
    }
}

impl Placed {
    /// The slot either way: a fill and a replace write the same place.
    fn loc(self) -> Loc {
        match self {
            Self::Occupied(loc) | Self::Vacant(loc) => loc,
        }
    }
}

impl<T> Store<T> {
    /// The value at `loc`, whichever side it is on. A dense hole reads as
    /// `None`.
    fn read_at(&self, loc: &Loc) -> Option<&T> {
        match loc {
            Loc::Dense(idx) => self.dense[*idx].as_ref(),
            Loc::Overflow(id) => self.overflow.get(id),
        }
    }

    /// Store `value` at `loc` and return the value it held; `None` clears
    /// the slot, and a dense slot stays as a hole.
    fn write_at(&mut self, loc: &Loc, value: Option<T>) -> Option<T> {
        match loc {
            Loc::Dense(idx) => replace(&mut self.dense[*idx], value),
            Loc::Overflow(id) => match value {
                Some(value) => self.overflow.insert(*id, value),
                None => self.overflow.remove(id),
            },
        }
    }
}

impl<T> Default for IdVec<T> {
    fn default() -> Self {
        Self {
            slots: RwLock::new(Store::default()),
        }
    }
}

/// One read of an id store's slot placement — the census harness's view
/// (research-only, ADR-0476). [`Self::len`] is the count both occupancy
/// terms add up to; deriving it here keeps the caller from re-naming the
/// pair.
#[cfg(feature = "research")]
#[derive(Debug, Clone, Copy)]
pub(super) struct SlotCounts {
    /// Dense vector length, holes included; side-map ids are not in it.
    pub span: usize,
    /// Occupied dense slots.
    pub occupied: usize,
    /// Side-map entries (ids whose slot array does not fit).
    pub side: usize,
}

#[cfg(feature = "research")]
impl SlotCounts {
    /// Occupied slots, dense and side map together.
    pub fn len(&self) -> usize {
        self.occupied + self.side
    }

    /// Dense vector length, holes included.
    pub fn span(&self) -> usize {
        self.span
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

    /// The census harness's one read of the slot placement (worktree
    /// scratch): dense span, occupied dense slots, and side-map entries.
    #[cfg(feature = "research")]
    pub(super) fn slot_counts(&self) -> SlotCounts {
        let store = self.slots.read().expect("id store lock");
        SlotCounts {
            span: store.dense.len(),
            occupied: store.dense.iter().filter(|slot| slot.is_some()).count(),
            side: store.overflow.len(),
        }
    }

    /// Reserved slot-array bytes, holes included, plus one side-map entry
    /// per id that could not join the array.
    pub(super) fn array_bytes(&self) -> u64 {
        let store = self.slots.read().expect("id store lock");
        span_table_bytes(
            store.dense.len(),
            slot::<T>(),
            store.overflow.len(),
            size_of::<T>() as u64,
        )
    }

    pub(super) fn contains_key(&self, id: &i64) -> bool {
        let Some(idx) = index(*id) else {
            return false;
        };
        let store = self.slots.read().expect("id store lock");
        occupied(&store.dense, &store.overflow, *id, idx).is_some()
    }

    #[allow(dead_code)]
    pub(super) fn is_empty(&self) -> bool {
        let store = self.slots.read().expect("id store lock");
        store.overflow.is_empty() && store.dense.iter().all(Option::is_none)
    }

    pub(super) fn get(&self, id: &i64) -> Option<IdRef<'_, T>> {
        let idx = index(*id)?;
        let guard = self.slots.read().expect("id store lock");
        let loc = occupied(&guard.dense, &guard.overflow, *id, idx)?;
        Some(IdRef { guard, loc })
    }

    pub(super) fn get_mut(&self, id: &i64) -> Option<IdMut<'_, T>> {
        let idx = index(*id)?;
        let guard = self.slots.write().expect("id store lock");
        let loc = occupied(&guard.dense, &guard.overflow, *id, idx)?;
        Some(IdMut { guard, loc })
    }

    pub(super) fn insert(&self, id: i64, value: T) -> Option<T> {
        let idx = index(id).expect("id fits an index");
        let mut store = self.slots.write().expect("id store lock");
        let loc = place(&mut store, id, idx).loc();
        store.write_at(&loc, Some(value))
    }

    /// Drop the value. A dense slot stays, so the span does not shrink.
    /// A side-map entry is removed even when the dense vector has grown a
    /// hole over its id.
    pub(super) fn remove(&self, id: &i64) -> Option<T> {
        let idx = index(*id)?;
        let mut store = self.slots.write().expect("id store lock");
        let loc = occupied(&store.dense, &store.overflow, *id, idx)?;
        store.write_at(&loc, None)
    }

    /// Drop the value when `pred` holds, checked under the same write lock
    /// as the removal: a writer that refilled the slot between an earlier
    /// edit and this call is seen by `pred`, and the value stays. Removal
    /// follows [`Self::remove`] (a dense slot stays as a hole).
    pub(super) fn remove_if(&self, id: &i64, pred: impl FnOnce(&T) -> bool) -> Option<T> {
        let idx = index(*id)?;
        let mut store = self.slots.write().expect("id store lock");
        let loc = occupied(&store.dense, &store.overflow, *id, idx)?;
        if !store.read_at(&loc).is_some_and(pred) {
            return None;
        }
        store.write_at(&loc, None)
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
        // Drop the buffers. `Vec::clear` would keep the reservation while
        // `array_bytes` charges the length, so a wiped store would stay
        // resident and uncharged.
        store.dense = Vec::new();
        store.overflow = HashMap::new();
    }

    /// Walk occupied slots under the read lock. The closure must not call
    /// back into this store.
    pub(super) fn for_each(&self, mut f: impl FnMut(i64, &T)) {
        let store = self.slots.read().expect("id store lock");
        for (idx, slot) in store.dense.iter().enumerate() {
            if let Some(value) = slot {
                let id = i64::try_from(idx).expect("a dense id fits i64");
                f(id, value);
            }
        }
        for (id, value) in &store.overflow {
            f(*id, value);
        }
    }

    pub(super) fn entry(&self, id: i64) -> Entry<'_, T> {
        let idx = index(id).expect("id fits an index");
        let mut guard = self.slots.write().expect("id store lock");
        match place(&mut guard, id, idx) {
            Placed::Occupied(loc) => Entry::Occupied(Occupied { guard, loc }),
            Placed::Vacant(loc) => Entry::Vacant(Vacant { guard, loc }),
        }
    }

    /// Install a batch of slots under one write lock, all or nothing.
    /// `admit` sees every slot's current value (`None` when vacant) before
    /// the first write, so an `Err` leaves the store as it was — not even a
    /// grown dense array — and `Ok` over an empty batch stays the one empty
    /// answer. A vacant slot takes the batch's value; an occupied one is
    /// replaced.
    pub(super) fn install_batch(
        &self,
        rows: Vec<(i64, T)>,
        admit: impl Fn(Option<&T>) -> Result<()>,
    ) -> Result<()> {
        let mut store = self.slots.write().expect("id store lock");
        for (id, _) in &rows {
            let idx = index(*id).expect("id fits an index");
            let previous = occupied(&store.dense, &store.overflow, *id, idx);
            admit(previous.and_then(|loc| store.read_at(&loc)))?;
        }
        for (id, value) in rows {
            let idx = index(id).expect("id fits an index");
            let loc = place(&mut store, id, idx).loc();
            store.write_at(&loc, Some(value));
        }
        Ok(())
    }

    /// Occupied ids, copied so the caller can re-enter the store.
    pub(super) fn keys(&self) -> Vec<i64> {
        let mut keys = Vec::new();
        self.for_each(|id, _| keys.push(id));
        keys
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
    fn growth_reserves_the_dense_ladder() {
        let ids = IdVec::new();
        for id in 0..3 {
            ids.insert(id, id);
        }
        let store = ids.slots.read().unwrap();
        assert_eq!(store.dense.len(), 3);
        assert_eq!(
            store.dense.capacity(),
            crate::memory::dense_reserved(3).unwrap()
        );
    }

    #[test]
    fn a_span_just_past_a_power_of_two_stays_under_the_ladder() {
        // 33 ids sit on the ladder's 40 rung — 7 spare slots, where the
        // old doubling reserved 64 for 31 spare. The bound this pins is
        // the ladder's: the reservation stays below 1.5x the span.
        let ids = IdVec::new();
        for id in 0..33 {
            ids.insert(id, id);
        }
        // The guard is scoped to the capacity read: a store call (the
        // array_bytes assert below) never nests inside a live read guard.
        let reserved = ids.slots.read().unwrap().dense.capacity();
        assert_eq!(reserved, 40);
        assert!(reserved * 2 < 33 * 3, "the ladder stays under 1.5x");
        let slot = size_of::<Option<i64>>() as u64;
        assert_eq!(
            ids.array_bytes(),
            crate::memory::dense_span_bytes(33, slot).unwrap()
        );
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

    #[test]
    fn a_large_id_stays_in_the_side_map() {
        let ids = IdVec::new();
        let id = 100_000_000;
        assert_eq!(ids.insert(id, 1u64), None);
        assert_eq!(ids.span(), 0, "the reservation is past the dense cap");
        assert_eq!(*ids.get(&id).unwrap(), 1);
        assert!(ids.contains_key(&id));
        assert_eq!(ids.remove(&id), Some(1));
        assert!(ids.get(&id).is_none());
    }

    #[test]
    fn a_side_entry_stays_visible_when_the_dense_vector_grows_past_it() {
        let ids = IdVec::new();
        {
            let mut store = ids.slots.write().unwrap();
            store.overflow.insert(1, "side");
            store.dense.resize(4, None);
        }
        assert_eq!(*ids.get(&1).unwrap(), "side");
        assert!(ids.contains_key(&1));
        assert_eq!(ids.insert(1, "replaced"), Some("side"));
        assert!(ids.slots.read().unwrap().dense[1].is_none());
        assert_eq!(ids.remove(&1), Some("replaced"));
        assert!(ids.get(&1).is_none());
    }

    #[test]
    fn remove_if_drops_only_when_the_predicate_holds() {
        let ids = IdVec::new();
        ids.insert(1, 7u64);
        assert_eq!(ids.remove_if(&1, |v| *v == 0), None, "predicate false");
        assert_eq!(*ids.get(&1).unwrap(), 7, "the value stays");
        assert_eq!(ids.remove_if(&9, |_| true), None, "an absent id");
        assert_eq!(ids.remove_if(&1, |v| *v == 7), Some(7));
        assert!(ids.get(&1).is_none());
        assert_eq!(ids.span(), 2, "a dense slot stays as a hole");

        let high = i64::MAX - 1;
        ids.insert(high, 5u64);
        assert_eq!(ids.remove_if(&high, |v| *v == 0), None);
        assert_eq!(*ids.get(&high).unwrap(), 5);
        assert_eq!(ids.remove_if(&high, |v| *v == 5), Some(5));
        assert!(ids.get(&high).is_none(), "the side entry goes too");
    }

    #[test]
    fn a_refused_batch_leaves_the_store_untouched() {
        let ids = IdVec::new();
        ids.insert(0, "warm");
        let refused =
            ids.install_batch(
                vec![(1, "a"), (2, "b"), (0, "refused")],
                |previous| match previous {
                    Some(value) if *value == "warm" => Err(relate_core::Error::storage("refused")),
                    _ => Ok(()),
                },
            );
        assert!(refused.is_err());
        assert_eq!(*ids.get(&0).unwrap(), "warm");
        assert!(ids.get(&1).is_none() && ids.get(&2).is_none());
        assert_eq!(ids.span(), 1, "no row was planted, no slot was grown");

        assert!(
            ids.install_batch(Vec::new(), |_| Err(relate_core::Error::storage(
                "never run"
            )))
            .is_ok(),
            "an empty batch is not a refusal"
        );
    }

    #[test]
    fn clear_drops_the_reservation() {
        let ids = IdVec::new();
        ids.insert(3, 1u64);
        assert!(ids.array_bytes() > 0);
        ids.clear();
        assert_eq!(ids.span(), 0);
        assert_eq!(ids.len(), 0);
        assert_eq!(ids.array_bytes(), 0);
        assert_eq!(ids.slots.read().unwrap().dense.capacity(), 0);
    }
}
