//! Snapshot pins: the lock-free pin-slot protocol that keeps GC from
//! dropping versions a statement still reads (ADR-0023, ADR-0038,
//! ADR-0111). Split out of `graph/mod.rs` (LABS-677).

use relate_core::Result;

use super::*;

/// 256 in-flight statements; excess pins retry a bounded number of times and
/// then fail with a typed "snapshot pin" error (ADR-0038, ADR-0111).
pub(super) const PIN_SLOTS: usize = 256;
pub(super) const PIN_FREE: u64 = 0;
pub(super) const PIN_CLAIMING: u64 = u64::MAX;
/// Bounded retry budget for a full pin table. Each retry scans all 256 slots
/// and yields once; the budget is deliberately small so a saturated table
/// fails in microseconds to low milliseconds rather than stalling forever
/// (ADR-0111). The slot array and lock-free protocol are unchanged (ADR-0038).
pub(super) const PIN_ACQUIRE_RETRIES: u32 = 1024;

/// Registered `snapshot_ts` until drop (ADR-0023 / ADR-0038).
pub struct SnapshotPin<'a> {
    graph: &'a Graph,
    ts: u64,
    slot: usize,
}

impl SnapshotPin<'_> {
    pub fn ts(&self) -> u64 {
        self.ts
    }

    /// Keep the pin after this guard is forgotten (explicit txn, ADR-0028 / ADR-0038).
    pub fn detach(self) -> usize {
        let slot = self.slot;
        std::mem::forget(self);
        slot
    }
}

impl Drop for SnapshotPin<'_> {
    fn drop(&mut self) {
        self.graph.unpin_slot(self.slot);
    }
}

impl Graph {
    /// Pin `commit_ts` so GC cannot drop versions this statement still sees.
    ///
    /// The slot protocol is lock-free (ADR-0038). When every slot is
    /// occupied the wait is bounded: the array is retried a fixed number of
    /// times and then acquisition fails with a typed
    /// `Error::Execution("snapshot pin: …")` instead of spinning forever
    /// (ADR-0111).
    pub fn pin_now(&self) -> Result<SnapshotPin<'_>> {
        self.pin_now_with_retry(std::thread::yield_now)
    }

    /// Run the callback once after each unsuccessful full-table scan.
    pub(super) fn pin_now_with_retry(&self, mut retry: impl FnMut()) -> Result<SnapshotPin<'_>> {
        let start = self.pin_cursor.fetch_add(1, Ordering::Relaxed) as usize;
        for _ in 0..PIN_ACQUIRE_RETRIES {
            for i in 0..PIN_SLOTS {
                let slot = (start + i) % PIN_SLOTS;
                if self.pins[slot]
                    .compare_exchange(PIN_FREE, PIN_CLAIMING, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let ts = self.commit_ts();
                    self.pins[slot].store(ts.saturating_add(1), Ordering::Release);
                    return Ok(SnapshotPin {
                        graph: self,
                        ts,
                        slot,
                    });
                }
            }
            retry();
        }
        Err(relate_core::Error::execution(format!(
            "snapshot pin: all {PIN_SLOTS} slots are held"
        )))
    }

    pub fn unpin_slot(&self, slot: usize) {
        self.pins[slot].store(PIN_FREE, Ordering::Release);
    }

    /// True while any statement or explicit transaction holds a snapshot pin.
    pub fn snapshot_pinned(&self) -> bool {
        self.pins
            .iter()
            .any(|slot| slot.load(Ordering::Acquire) != PIN_FREE)
    }

    /// Lowest timestamp that may still be read. Dead iff `xmax != 0 && xmax <= horizon`.
    pub fn gc_horizon(&self) -> u64 {
        let commit = self.commit_ts();
        let mut min_pin: Option<u64> = None;
        for slot in self.pins.iter() {
            let v = slot.load(Ordering::Acquire);
            if v == PIN_CLAIMING {
                return 0;
            }
            if v != PIN_FREE {
                let ts = v - 1;
                min_pin = Some(min_pin.map_or(ts, |m| m.min(ts)));
            }
        }
        min_pin.unwrap_or(commit).min(commit)
    }
}
