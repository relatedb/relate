//! The two sketches a key carries while a label builds: a HyperLogLog
//! distinct count and a space-saving heavy-hitter summary. Both read the
//! value's index identity hash ([`identity_hash`]), so `1` and `1.0` are one
//! value here exactly as they are in an equality seek.

use std::hash::BuildHasher;

use relate_core::Value;
use relate_planner::HeavyValue;

use crate::group_key::GroupKey;
use crate::hash_seed::shared_random_state;

/// HyperLogLog precision: `2^10` one-byte registers, about 3% standard
/// error.
const HLL_PRECISION: u32 = 10;
const HLL_REGISTERS: usize = 1 << HLL_PRECISION;

/// Heavy hitters a key keeps. Space-saving guarantees every value above
/// `1 / TOP_K` of the admitted values is present; its counts are upper
/// bounds.
pub(super) const TOP_K: usize = 16;

/// A value's seeded 64-bit equality-identity hash, or `None` when the index
/// declines the value (a NaN anywhere inside it): the one hash the sketches
/// and the heavy-hitter lookup share.
pub(super) fn identity_hash(value: &Value) -> Option<u64> {
    let key = GroupKey::for_property_index(value)?;
    Some(shared_random_state().hash_one(&key))
}

/// A HyperLogLog distinct counter with [`HLL_REGISTERS`] registers.
pub(super) struct Hll {
    registers: Box<[u8]>,
}

impl Hll {
    pub(super) fn new() -> Self {
        Self {
            registers: vec![0u8; HLL_REGISTERS].into_boxed_slice(),
        }
    }

    pub(super) fn insert(&mut self, hash: u64) {
        let index = (hash >> (64 - HLL_PRECISION)) as usize;
        // The sentinel bit bounds the rank when the remaining bits are zero.
        let rest = (hash << HLL_PRECISION) | (1 << (HLL_PRECISION - 1));
        let rank = rest.leading_zeros() as u8 + 1;
        let register = &mut self.registers[index];
        *register = (*register).max(rank);
    }

    /// The distinct-count estimate, with linear counting below the range
    /// where the raw estimator is biased.
    pub(super) fn estimate(&self) -> u64 {
        let m = HLL_REGISTERS as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let mut sum = 0.0;
        let mut zeros = 0usize;
        for &r in self.registers.iter() {
            sum += 2f64.powi(-i32::from(r));
            zeros += usize::from(r == 0);
        }
        let raw = alpha * m * m / sum;
        let estimate = if raw <= 2.5 * m && zeros > 0 {
            m * (m / zeros as f64).ln()
        } else {
            raw
        };
        estimate.round() as u64
    }
}

/// One tracked value while the summary runs: its identity hash, its
/// (upper-bound) count, the most that count can overstate (the count of the
/// entry it replaced when it was admitted, zero for a value tracked from
/// its first occurrence) and a representative to hand back. `count -
/// error` is the occurrences actually seen since admission: a guaranteed
/// lower bound on the value's frequency.
#[derive(Clone, Debug)]
struct Tracked {
    hash: u64,
    count: u64,
    error: u64,
    value: Value,
}

/// A space-saving summary of the [`TOP_K`] most frequent values.
#[derive(Default)]
pub(super) struct TopK {
    entries: Vec<Tracked>,
}

impl TopK {
    pub(super) fn observe(&mut self, hash: u64, value: &Value) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.hash == hash) {
            entry.count += 1;
            return;
        }
        if self.entries.len() < TOP_K {
            self.entries.push(Tracked {
                hash,
                count: 1,
                error: 0,
                value: value.clone(),
            });
            return;
        }
        let min = self
            .entries
            .iter_mut()
            .min_by_key(|e| e.count)
            .expect("a full summary has entries");
        min.hash = hash;
        min.error = min.count;
        min.count += 1;
        min.value = value.clone();
    }

    /// The entries whose guaranteed lower bound (`count - error`) exceeds
    /// half the summary's resolution (`total / (2 * TOP_K)`), most frequent
    /// first, as the published heavy values (the hash is dropped: a lookup
    /// recomputes it). Filtering on the upper-bound count alone would admit
    /// the summary's own floor: when no value is truly heavy every entry is
    /// a recycled slot whose count sits near `total / TOP_K`, which is noise
    /// the equality estimate must fall back from, not read as a frequency.
    pub(super) fn heavy(self, total: u64) -> Vec<HeavyValue> {
        let mut heavy: Vec<Tracked> = self
            .entries
            .into_iter()
            .filter(|e| (e.count - e.error) * (2 * TOP_K as u64) > total)
            .collect();
        heavy.sort_by_key(|t| std::cmp::Reverse(t.count));
        heavy
            .into_iter()
            .map(|t| HeavyValue {
                value: t.value,
                count: t.count,
            })
            .collect()
    }
}
