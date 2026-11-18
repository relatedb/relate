//! The compact live property map (ADR-0033) and the tagged-JSON byte
//! counters it shares with the relationship payload, in their own module:
//! the store's property-record half, split out of `store.rs` so the parent
//! keeps the live records (the repo's one-theme-per-file shape).

use std::collections::BTreeMap;
use std::io::Write;

use relate_core::{Error, PropertyMap, Result, Value};

use super::{Interner, NameCache};

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CompactProps {
    keys: Vec<u32>,
    vals: Vec<Value>,
}

impl CompactProps {
    pub(crate) fn from_map(interner: &Interner, map: BTreeMap<String, Value>) -> Self {
        Self::from_entries(
            map.into_iter()
                .map(|(key, value)| (interner.intern(&key), value)),
        )
    }

    /// [`Self::from_map`] over a shared map read in place — the restore
    /// row's shape: keys are interned through the slice's [`NameCache`] and
    /// values cloned (each a shared payload or a scalar), so the map is never
    /// deep-cloned and each distinct key is looked up once per slice.
    pub(crate) fn from_map_ref(
        interner: &Interner,
        names: &mut NameCache,
        map: &PropertyMap,
    ) -> Self {
        Self::from_entries(
            map.iter()
                .map(|(key, value)| (names.id(interner, key), value.clone())),
        )
    }

    /// The one fill both spellings feed: interned key ids beside their
    /// values, fitted.
    fn from_entries(entries: impl Iterator<Item = (u32, Value)>) -> Self {
        let (keys, vals) = entries.unzip();
        let mut props = Self { keys, vals };
        props.fit();
        props
    }

    pub(crate) fn to_map(&self, interner: &Interner) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        for (k, v) in self.keys.iter().zip(self.vals.iter()) {
            m.insert(String::from(&*interner.get(*k)), v.clone());
        }
        m
    }

    /// The slot holding `key`, the one scan `get`/`set`/`remove` share.
    fn slot_of(&self, key: u32) -> Option<usize> {
        self.keys.iter().position(|&k| k == key)
    }

    pub(crate) fn get(&self, key: u32) -> Option<&Value> {
        self.slot_of(key).map(|i| &self.vals[i])
    }

    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    #[cfg(test)]
    pub(crate) fn key_capacity(&self) -> usize {
        self.keys.capacity()
    }

    #[cfg(test)]
    pub(crate) fn val_capacity(&self) -> usize {
        self.vals.capacity()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &Value)> {
        self.keys.iter().copied().zip(self.vals.iter())
    }

    fn fit(&mut self) {
        crate::memory::fit_reserved(&mut self.keys);
        crate::memory::fit_reserved(&mut self.vals);
    }

    pub(crate) fn keys_as_strings(&self, interner: &Interner) -> Vec<String> {
        self.keys
            .iter()
            .map(|k| String::from(&*interner.get(*k)))
            .collect()
    }

    pub(crate) fn set(&mut self, key: u32, value: Value) {
        if let Some(i) = self.slot_of(key) {
            self.vals[i] = value;
        } else {
            self.keys.push(key);
            self.vals.push(value);
            self.fit();
        }
    }

    pub(crate) fn remove(&mut self, key: u32) {
        if let Some(i) = self.slot_of(key) {
            self.keys.remove(i);
            self.vals.remove(i);
            self.fit();
        }
    }

    /// Tagged JSON byte length of this map, borrowing each value.
    pub(crate) fn tagged_json_len(&self, interner: &Interner) -> Result<u64> {
        json_len(&BorrowedProps {
            interner,
            props: self,
        })
    }
}

/// The one overflow message the byte-count arithmetic reports.
const BYTE_OVERFLOW_MSG: &str = "checkpoint byte count overflow";

struct ByteCount(u64);

impl Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(buf.len() as u64)
            .ok_or_else(|| std::io::Error::other(BYTE_OVERFLOW_MSG))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn json_len(value: &impl serde::Serialize) -> Result<u64> {
    let mut sink = ByteCount(0);
    serde_json::to_writer(&mut sink, value).map_err(|e| Error::storage(e.to_string()))?;
    Ok(sink.0)
}

pub(super) fn add_bytes(sum: u64, extra: u64) -> Result<u64> {
    sum.checked_add(extra)
        .ok_or_else(|| Error::storage(BYTE_OVERFLOW_MSG))
}

/// Property map serialized in stored order. JSON object length does not
/// depend on key order; values serialize through [`Value`]'s tagged form.
struct BorrowedProps<'a> {
    interner: &'a Interner,
    props: &'a CompactProps,
}

impl serde::Serialize for BorrowedProps<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.props.keys.len()))?;
        for (key, value) in self.props.iter() {
            map.serialize_entry(self.interner.get(key).as_ref(), value)?;
        }
        map.end()
    }
}
