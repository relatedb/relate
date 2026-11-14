//! A 128-bit structural hash of a [`Value`], and the memo table keyed by it
//! (LABS-1478, memo tier 3: a wide-value key).
//!
//! **Why no verification.** A memo hit is a wrong query result if two
//! different values share a key, so the key must be collision-free in
//! practice. A full-content 128-bit hash is: random collisions are about
//! 2^-128 per pair, far below hardware error rates, so a hit is one probe
//! with no equality walk. (A *sampled* hash cannot make that claim, its
//! collisions are structured, and is deliberately not offered.) The cost is
//! one pass over the value, O(size), paid once per distinct value the memo
//! sees; a key that repeats is paid once per lookup only by this hash, so a
//! caller holding a value across many lookups should keep its [`Hash128`]
//! (the companion hash). Caching the hash inside the shared container
//! itself needs a change to `Value`'s representation in `relate-core` and
//! is a separate decision.
//!
//! **Not cryptographic.** The value is flattened into one canonical byte
//! stream (a type tag per node, lengths before runs, so no two shapes share
//! a stream) fed to a streaming XXH3-128 (`xxhash-rust`, no dependencies)
//! seeded per process, so a client that controls the values cannot craft a
//! collision without the seed; it is not designed against a determined
//! adversary. Scalars append to a small stack buffer that is flushed to the
//! hasher when full, long strings and byte runs are hashed straight from
//! their slice, and a non-empty list of only integers (or only floats) is
//! one tagged run of eight-byte words, so a 1000-element numeric list is
//! a handful of bulk updates, not 1000.
//!
//! **What hashes.** The representation, tagged by variant: a value equal
//! under the engine's `=` but spelled differently (`1` and `1.0`) gets two
//! keys, which costs a missed hit and never a wrong one. Entities (node,
//! relationship, path), temporals, points and vectors return `None` (the
//! memo bypasses): an entity is keyed by its id in the id tables, not by
//! content. Nesting deeper than [`MAX_DEPTH`] returns `None`.

#![allow(
    dead_code,
    reason = "memo tier 3: the first consumer is a value-keyed expression memo (LABS-1478 follow-up); pinned by the tests below"
)]

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::OnceLock;

use xxhash_rust::xxh3::Xxh3;

use relate_core::{NodeId, Value};

use super::memo::MemoTable;
use super::node_map::NodeMap;

/// Nesting deeper than this is not hashed (the memo bypasses).
const MAX_DEPTH: u32 = 24;

/// Bytes of scalar stream held before a flush into the hasher.
const BUF: usize = 512;

/// Stream tags. A non-empty all-Int / all-Float list has its own tag, so it
/// can never share a stream with the generic list holding the same items.
mod tag {
    pub(super) const NULL: u8 = 0;
    pub(super) const BOOL: u8 = 1;
    pub(super) const INT: u8 = 2;
    pub(super) const FLOAT: u8 = 3;
    pub(super) const STRING: u8 = 4;
    pub(super) const LIST: u8 = 5;
    pub(super) const MAP: u8 = 6;
    pub(super) const UUID: u8 = 7;
    pub(super) const BYTES: u8 = 8;
    pub(super) const INT_LIST: u8 = 9;
    pub(super) const FLOAT_LIST: u8 = 10;
}

/// A value's 128-bit content key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Hash128(u128);

fn seed() -> u64 {
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        let state = RandomState::new();
        state.hash_one(0x5eed_u64) ^ state.hash_one(0xfeed_u64).rotate_left(32)
    })
}

/// The canonical byte stream, buffered in front of a seeded XXH3-128.
struct Stream {
    xxh: Xxh3,
    buf: [u8; BUF],
    len: usize,
}

impl Stream {
    fn new() -> Self {
        Self {
            xxh: Xxh3::with_seed(seed()),
            buf: [0; BUF],
            len: 0,
        }
    }

    #[inline]
    fn flush(&mut self) {
        self.xxh.update(&self.buf[..self.len]);
        self.len = 0;
    }

    #[inline]
    fn put(&mut self, bytes: &[u8]) {
        debug_assert!(bytes.len() <= BUF);
        if self.len + bytes.len() > BUF {
            self.flush();
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    #[inline]
    fn tag(&mut self, tag: u8) {
        self.put(&[tag]);
    }

    #[inline]
    fn word(&mut self, w: u64) {
        self.put(&w.to_le_bytes());
    }

    /// A byte run: its length, then the bytes. The length keeps adjacent
    /// runs apart, so `("ab","c")` and `("a","bc")` differ. A run that does
    /// not fit the buffer is hashed straight from its slice.
    fn bytes(&mut self, bytes: &[u8]) {
        self.word(bytes.len() as u64);
        if bytes.len() <= BUF {
            self.put(bytes);
        } else {
            self.flush();
            self.xxh.update(bytes);
        }
    }

    fn finish(mut self) -> Hash128 {
        self.flush();
        Hash128(self.xxh.digest128())
    }
}

/// The items when every one is an `Int` (`Some(true)`) or every one a
/// `Float` (`Some(false)`); `None` for an empty or mixed list.
fn numeric_run(items: &[Value]) -> Option<bool> {
    match items.first()? {
        Value::Int(_) => items
            .iter()
            .all(|v| matches!(v, Value::Int(_)))
            .then_some(true),
        Value::Float(_) => items
            .iter()
            .all(|v| matches!(v, Value::Float(_)))
            .then_some(false),
        _ => None,
    }
}

fn walk(s: &mut Stream, value: &Value, depth: u32) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    match value {
        Value::Null => s.tag(tag::NULL),
        Value::Bool(b) => {
            s.tag(tag::BOOL);
            s.tag(u8::from(*b));
        }
        Value::Int(i) => {
            s.tag(tag::INT);
            s.word(*i as u64);
        }
        Value::Float(f) => {
            s.tag(tag::FLOAT);
            s.word(f.to_bits());
        }
        Value::String(text) => {
            s.tag(tag::STRING);
            s.bytes(text.as_bytes());
        }
        Value::List(items) => match numeric_run(items) {
            Some(ints) => {
                // The elements sit one level down, as in the generic walk.
                if depth + 1 > MAX_DEPTH {
                    return None;
                }
                s.tag(if ints { tag::INT_LIST } else { tag::FLOAT_LIST });
                s.word(items.len() as u64);
                for item in items.iter() {
                    match item {
                        Value::Int(i) => s.word(*i as u64),
                        Value::Float(f) => s.word(f.to_bits()),
                        // `numeric_run` admitted only numeric items; anything else
                        // makes the value unkeyable, which only costs a recompute.
                        _ => return None,
                    }
                }
            }
            None => {
                s.tag(tag::LIST);
                s.word(items.len() as u64);
                for item in items.iter() {
                    walk(s, item, depth + 1)?;
                }
            }
        },
        Value::Map(entries) => {
            s.tag(tag::MAP);
            s.word(entries.len() as u64);
            for (key, item) in entries.iter() {
                s.bytes(key.as_bytes());
                walk(s, item, depth + 1)?;
            }
        }
        Value::Uuid(u) => {
            s.tag(tag::UUID);
            s.put(&u.to_le_bytes());
        }
        Value::Bytes(b) => {
            s.tag(tag::BYTES);
            s.bytes(b);
        }
        Value::Node(_)
        | Value::Relationship(_)
        | Value::Path(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Vector { .. } => return None,
    }
    Some(())
}

/// `value`'s content key, `None` for a value the memo bypasses.
pub(crate) fn hash_value(value: &Value) -> Option<Hash128> {
    let mut stream = Stream::new();
    walk(&mut stream, value, 0)?;
    Some(stream.finish())
}

/// Any `Copy` value per [`Hash128`] key, in the sparse node map: the low 64
/// bits index it and the high 64 are stored beside the value, so a hit
/// matches all 128 bits and a low-half clash is a miss, never a wrong hit.
pub(crate) struct HashedValues<V>(NodeMap<(u64, V)>);

impl<V> Default for HashedValues<V> {
    fn default() -> Self {
        Self(NodeMap::default())
    }
}

impl<V: Copy> MemoTable for HashedValues<V> {
    type Key = Hash128;
    type Value = V;

    fn get(&self, key: Hash128) -> Option<V> {
        let (hi, lo) = ((key.0 >> 64) as u64, key.0 as u64);
        match self.0.get(lo as NodeId) {
            Some(&(stored, value)) if stored == hi => Some(value),
            _ => None,
        }
    }

    fn insert(&mut self, key: Hash128, value: V) {
        let (hi, lo) = ((key.0 >> 64) as u64, key.0 as u64);
        self.0.insert(lo as NodeId, (hi, value));
    }

    fn heap_bytes(&self) -> usize {
        self.0.heap_bytes()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Arc;

    use super::super::memo::{Memo, MemoMeter, MemoPolicy};
    use super::*;

    fn s(text: &str) -> Value {
        Value::String(Arc::from(text))
    }

    fn list(items: Vec<Value>) -> Value {
        Value::List(Arc::new(items))
    }

    fn map(entries: &[(&str, Value)]) -> Value {
        Value::Map(Arc::new(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        ))
    }

    fn h(value: &Value) -> Hash128 {
        hash_value(value).expect("hashable")
    }

    struct Unmetered;
    impl MemoMeter for Unmetered {
        fn charge(&self, _: &mut u64, _: u64) -> relate_core::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn equal_values_hash_equal_and_variants_stay_apart() {
        assert_eq!(h(&s("abc")), h(&s("abc")));
        assert_eq!(
            h(&list(vec![Value::Int(1), s("x")])),
            h(&list(vec![Value::Int(1), s("x")]))
        );
        let keys: HashSet<_> = [
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(1),
            Value::Int(0),
            Value::Float(1.0),
            s("1"),
            s(""),
            list(vec![]),
            map(&[]),
            Value::Uuid(1),
            Value::Bytes(Arc::from(&b"1"[..])),
        ]
        .iter()
        .map(h)
        .collect();
        assert_eq!(keys.len(), 12, "each of the twelve is its own key");
    }

    #[test]
    fn structure_boundaries_matter() {
        assert_ne!(
            h(&list(vec![
                list(vec![Value::Int(1)]),
                list(vec![Value::Int(2)])
            ])),
            h(&list(vec![list(vec![Value::Int(1), Value::Int(2)])]))
        );
        assert_ne!(
            h(&list(vec![s("ab"), s("c")])),
            h(&list(vec![s("a"), s("bc")]))
        );
        assert_ne!(h(&map(&[("ab", s("c"))])), h(&map(&[("a", s("bc"))])));
        assert_ne!(
            h(&list(vec![Value::Int(1), Value::Int(2)])),
            h(&list(vec![Value::Int(2), Value::Int(1)]))
        );
    }

    #[test]
    fn long_values_differing_in_the_middle_do_not_collide() {
        let base = "x".repeat(10_000);
        let mut other = base.clone().into_bytes();
        other[5_000] = b'y';
        let other = String::from_utf8(other).unwrap();
        assert_ne!(h(&s(&base)), h(&s(&other)));
    }

    #[test]
    fn no_collisions_over_many_distinct_values() {
        let mut seen = HashSet::new();
        for i in 0..100_000i64 {
            assert!(seen.insert(h(&Value::Int(i))), "int {i}");
            assert!(seen.insert(h(&s(&format!("k{i}")))), "string {i}");
        }
    }

    #[test]
    fn entities_and_over_deep_nesting_bypass() {
        let mut deep = Value::Int(0);
        for _ in 0..(MAX_DEPTH + 2) {
            deep = list(vec![deep]);
        }
        assert!(hash_value(&deep).is_none());
        let shallow = list(vec![list(vec![Value::Int(0)])]);
        assert!(hash_value(&shallow).is_some());
    }

    #[test]
    fn the_table_matches_all_128_bits() {
        let mut table = HashedValues::<u32>::default();
        let key = h(&s("alpha"));
        table.insert(key, 7);
        assert_eq!(table.get(key), Some(7));
        assert_eq!(table.get(h(&s("beta"))), None);
        // Same low half, different high half: a miss, not a wrong hit.
        let clash = Hash128((u128::from(!(key.0 >> 64) as u64) << 64) | (key.0 & u64::MAX as u128));
        assert_eq!(table.get(clash), None);
    }

    #[test]
    fn a_memo_over_value_keys_computes_once_per_distinct_value() {
        let policy = MemoPolicy {
            warm_up: 0,
            ..MemoPolicy::MODERATE
        };
        let mut memo = Memo::<HashedValues<u64>>::new(policy, None);
        let blob = map(&[("a", list(vec![Value::Int(1), s("two")]))]);
        let mut calls = 0;
        for _ in 0..5 {
            let got = memo
                .answer(&Unmetered, h(&blob), || {
                    calls += 1;
                    Ok(42)
                })
                .unwrap();
            assert_eq!(got, 42);
        }
        assert_eq!(calls, 1);
    }
}

#[cfg(test)]
#[path = "value_hash_diff_tests.rs"]
mod diff_tests;
