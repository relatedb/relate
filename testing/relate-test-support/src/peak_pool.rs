//! The one counting in-memory [`PeakPool`] the workspace-charge tests share:
//! a fixed capacity of workspace bytes, grown and released atomically, with
//! the reserved total readable so a test can assert the budget is whole
//! again once every charge has dropped.

use std::sync::atomic::{AtomicU64, Ordering};

use relate_core::tokens::PeakPool;

/// A pool of `capacity` workspace bytes.
pub struct FixedPool {
    /// The most the pool admits at once.
    pub capacity: u64,
    reserved: AtomicU64,
}

impl FixedPool {
    pub fn new(capacity: u64) -> Self {
        Self {
            capacity,
            reserved: AtomicU64::new(0),
        }
    }

    /// Bytes currently reserved.
    pub fn reserved(&self) -> u64 {
        self.reserved.load(Ordering::SeqCst)
    }
}

impl PeakPool for FixedPool {
    fn try_grow(&self, extra: u64) -> bool {
        self.reserved
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
                held.checked_add(extra)
                    .filter(|next| *next <= self.capacity)
            })
            .is_ok()
    }

    fn release(&self, amount: u64) {
        self.reserved.fetch_sub(amount, Ordering::SeqCst);
    }
}
