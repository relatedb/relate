//! The published statistics' byte budget and its priority: structured keys
//! outrank text keys, which are the first evicted. The one place the
//! priority is applied, and the one record of which keys are published
//! (the consult map in [`super::KeyStatsState`] is only the read path,
//! mutated together with this ledger under its lock).

use std::collections::HashMap;

use super::gate::KeyClass;
use super::BUDGET_BYTES;

#[derive(Debug)]
struct Charge {
    bytes: usize,
    class: KeyClass,
}

#[derive(Debug)]
pub(super) struct Ledger {
    budget: usize,
    used: usize,
    entries: HashMap<(u32, u32), Charge>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::with_budget(BUDGET_BYTES)
    }
}

impl Ledger {
    pub(super) fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            entries: HashMap::new(),
        }
    }

    /// Reserve `bytes` for `key`. `Some(evicted)` when it fits, the text keys
    /// that made room (the caller removes their statistics); `None` when it
    /// does not: a text key never evicts, and a structured key evicts only
    /// when evicting every text key would make room, so a rejected key
    /// disturbs nothing.
    pub(super) fn admit(
        &mut self,
        key: (u32, u32),
        bytes: usize,
        class: KeyClass,
    ) -> Option<Vec<(u32, u32)>> {
        debug_assert!(
            !self.entries.contains_key(&key),
            "a key is charged once; release it before admitting it again"
        );
        let mut evicted = Vec::new();
        if self.used + bytes > self.budget {
            if class == KeyClass::Text {
                return None;
            }
            let mut text: Vec<((u32, u32), usize)> = self
                .entries
                .iter()
                .filter(|(_, c)| c.class == KeyClass::Text)
                .map(|(k, c)| (*k, c.bytes))
                .collect();
            let reclaimable: usize = text.iter().map(|(_, b)| b).sum();
            if self.used - reclaimable + bytes > self.budget {
                return None;
            }
            text.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (victim, victim_bytes) in text {
                if self.used + bytes <= self.budget {
                    break;
                }
                self.entries.remove(&victim);
                self.used -= victim_bytes;
                evicted.push(victim);
            }
        }
        self.used += bytes;
        self.entries.insert(key, Charge { bytes, class });
        Some(evicted)
    }

    /// Release every key of `label`; the keys, for the caller to drop from
    /// the consult map. Membership is read from the entries themselves, so
    /// a key an earlier eviction removed is never listed.
    pub(super) fn release_label(&mut self, label: u32) -> Vec<(u32, u32)> {
        let keys: Vec<(u32, u32)> = self
            .entries
            .keys()
            .filter(|(l, _)| *l == label)
            .copied()
            .collect();
        for key in &keys {
            if let Some(charge) = self.entries.remove(key) {
                self.used -= charge.bytes;
            }
        }
        keys
    }

    #[cfg(test)]
    pub(super) fn used(&self) -> usize {
        self.used
    }

    #[cfg(test)]
    pub(super) fn holds(&self, key: &(u32, u32)) -> bool {
        self.entries.contains_key(key)
    }
}
