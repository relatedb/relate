//! Differential tests: the streaming hash must induce the same equality
//! classes as the lane-mix implementation it replaced (kept here as the
//! reference), and equal values must hash equal with no collisions across
//! a corpus of distinct ones.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use relate_core::Value;

use super::{hash_value, MAX_DEPTH};

// ---- reference: the previous implementation, verbatim in behavior ----

const K0: u64 = 0xa076_1d64_78bd_642f;
const K1: u64 = 0xe703_7ed1_a0b4_28db;
const K2: u64 = 0x8ebc_6af0_9c88_c6e3;
const K3: u64 = 0x5899_65cc_7537_4cc3;

fn fold(x: u64, y: u64) -> u64 {
    let p = u128::from(x).wrapping_mul(u128::from(y));
    (p as u64) ^ ((p >> 64) as u64)
}

struct Lanes {
    a: u64,
    b: u64,
    seed: u64,
}

impl Lanes {
    fn word(&mut self, w: u64) {
        self.a = fold(self.a ^ w ^ K0, K1 ^ self.b.rotate_left(17));
        self.b = fold(self.b ^ w.rotate_left(32) ^ K2, K3 ^ self.a.rotate_left(29));
    }
    fn bytes(&mut self, bytes: &[u8]) {
        self.word(bytes.len() as u64);
        let d = xxhash_rust::xxh3::xxh3_128_with_seed(bytes, self.seed);
        self.word(d as u64);
        self.word((d >> 64) as u64);
    }
}

fn old_walk(l: &mut Lanes, v: &Value, depth: u32) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    match v {
        Value::Null => l.word(0),
        Value::Bool(b) => {
            l.word(1);
            l.word(u64::from(*b));
        }
        Value::Int(i) => {
            l.word(2);
            l.word(*i as u64);
        }
        Value::Float(f) => {
            l.word(3);
            l.word(f.to_bits());
        }
        Value::String(s) => {
            l.word(4);
            l.bytes(s.as_bytes());
        }
        Value::List(items) => {
            l.word(5);
            l.word(items.len() as u64);
            for i in items.iter() {
                old_walk(l, i, depth + 1)?;
            }
        }
        Value::Map(m) => {
            l.word(6);
            l.word(m.len() as u64);
            for (k, i) in m.iter() {
                l.bytes(k.as_bytes());
                old_walk(l, i, depth + 1)?;
            }
        }
        Value::Uuid(u) => {
            l.word(7);
            l.word(*u as u64);
            l.word((*u >> 64) as u64);
        }
        Value::Bytes(b) => {
            l.word(8);
            l.bytes(b);
        }
        _ => return None,
    }
    Some(())
}

fn old_hash(v: &Value) -> Option<u128> {
    let mut l = Lanes {
        a: 0x1234_5678_9abc_def0,
        b: 0x0fed_cba9_8765_4321,
        seed: 0x5eed,
    };
    old_walk(&mut l, v, 0)?;
    let hi = fold(l.a ^ K0, K1);
    let lo = fold(l.b ^ K2, K3);
    Some((u128::from(hi) << 64) | u128::from(lo))
}

// ---- corpus ----

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn scalar(r: &mut Rng) -> Value {
    const STRS: [&str; 6] = ["", "a", "ab", "b", "héllo", "日本語"];
    const FLOATS: [f64; 8] = [0.0, -0.0, 1.0, -1.0, 1.5, f64::NAN, f64::INFINITY, f64::MIN];
    match r.below(8) {
        0 => Value::Null,
        1 => Value::Bool(r.below(2) == 1),
        2 => Value::Int(r.below(4) as i64 - 1),
        3 => Value::Float(FLOATS[r.below(FLOATS.len() as u64) as usize]),
        4 => Value::String(Arc::from(STRS[r.below(STRS.len() as u64) as usize])),
        5 => Value::Uuid(u128::from(r.below(3)) << 64 | u128::from(r.below(3))),
        6 => Value::Bytes(Arc::from(vec![r.below(3) as u8; r.below(3) as usize])),
        _ => Value::Int(i64::MIN + r.below(2) as i64),
    }
}

fn gen(r: &mut Rng, depth: u32) -> Value {
    if depth == 0 || r.below(3) == 0 {
        return scalar(r);
    }
    let n = r.below(4) as usize;
    match r.below(4) {
        0 => Value::Map(Arc::new(
            (0..n)
                .map(|_| (format!("k{}", r.below(3)), gen(r, depth - 1)))
                .collect::<BTreeMap<_, _>>(),
        )),
        // Homogeneous numeric lists exercise the packed path.
        1 => Value::List(Arc::new(
            (0..n).map(|_| Value::Int(r.below(3) as i64)).collect(),
        )),
        2 => Value::List(Arc::new(
            (0..n)
                .map(|_| Value::Float([0.0, -0.0, 1.0, f64::NAN][r.below(4) as usize]))
                .collect(),
        )),
        _ => Value::List(Arc::new((0..n).map(|_| gen(r, depth - 1)).collect())),
    }
}

fn nest(mut v: Value, times: u32) -> Value {
    for _ in 0..times {
        v = Value::List(Arc::new(vec![v]));
    }
    v
}

fn corpus() -> Vec<Value> {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out: Vec<Value> = (0..1500).map(|_| gen(&mut r, 4)).collect();
    let l = |v: Vec<Value>| Value::List(Arc::new(v));
    out.extend([
        l(vec![]),
        l(vec![l(vec![])]),
        l(vec![Value::Int(1)]),
        l(vec![Value::Float(1.0)]),
        l(vec![Value::Int(1), Value::Float(1.0)]),
        l(vec![Value::Float(0.0)]),
        l(vec![Value::Float(-0.0)]),
        l(vec![Value::Float(f64::NAN)]),
        l(vec![Value::Float(f64::from_bits(f64::NAN.to_bits() ^ 1))]),
        l((0..1000).map(Value::Int).collect()),
        l((0..1001).map(Value::Int).collect()),
        l((0..1000).map(|i| Value::Float(f64::from(i))).collect()),
        Value::String(Arc::from("x".repeat(600).as_str())),
        Value::String(Arc::from("x".repeat(601).as_str())),
        Value::Bytes(Arc::from(vec![7u8; 513])),
        Value::Bytes(Arc::from(vec![7u8; 512])),
        nest(Value::Int(0), MAX_DEPTH),
        nest(Value::Int(0), MAX_DEPTH - 1),
        nest(Value::Int(0), MAX_DEPTH + 1),
        nest(l(vec![Value::Int(0), Value::Int(1)]), MAX_DEPTH - 1),
        nest(l(vec![Value::Int(0), Value::Int(1)]), MAX_DEPTH),
        nest(Value::Int(0), 5),
    ]);
    out
}

#[test]
fn bypass_agrees_with_the_reference() {
    for v in corpus() {
        assert_eq!(old_hash(&v).is_none(), hash_value(&v).is_none(), "{v:?}");
    }
}

#[test]
fn equality_classes_match_the_reference() {
    let values = corpus();
    let old: Vec<_> = values.iter().map(old_hash).collect();
    let new: Vec<_> = values.iter().map(hash_value).collect();
    for i in 0..values.len() {
        for j in i..values.len() {
            if let (Some(a), Some(b), Some(c), Some(d)) = (old[i], old[j], new[i], new[j]) {
                assert_eq!(a == b, c == d, "{:?} vs {:?}", values[i], values[j]);
            }
        }
    }
}

#[test]
fn equal_values_hash_equal_and_distinct_ones_do_not_collide() {
    let values = corpus();
    // Group by the reference class; within a class the new hash is one key,
    // across classes the new keys are all different.
    let mut by_old: HashMap<u128, u128> = HashMap::new();
    let mut by_new: HashMap<u128, u128> = HashMap::new();
    for v in &values {
        let (Some(o), Some(n)) = (old_hash(v), hash_value(v)) else {
            continue;
        };
        // A clone is equal: same key.
        assert_eq!(hash_value(&v.clone()), Some(n));
        assert_eq!(*by_old.entry(o).or_insert(n.0), n.0, "{v:?}");
        assert_eq!(*by_new.entry(n.0).or_insert(o), o, "collision on {v:?}");
    }
    assert!(
        by_new.len() > 500,
        "corpus too degenerate: {}",
        by_new.len()
    );
}

#[test]
fn numeric_lists_keep_their_numeric_classes() {
    let ints = Value::List(Arc::new(vec![Value::Int(1)]));
    let floats = Value::List(Arc::new(vec![Value::Float(1.0)]));
    let zero = Value::List(Arc::new(vec![Value::Float(0.0)]));
    let neg_zero = Value::List(Arc::new(vec![Value::Float(-0.0)]));
    let nan = Value::List(Arc::new(vec![Value::Float(f64::NAN)]));
    let h = |v: &Value| hash_value(v).unwrap();
    // As before: 1 and 1.0, 0.0 and -0.0 are distinct keys (a missed hit,
    // never a wrong one); the same NaN bits are one key.
    assert_ne!(h(&ints), h(&floats));
    assert_ne!(h(&zero), h(&neg_zero));
    assert_eq!(h(&nan), h(&nan.clone()));
}

#[test]
#[ignore = "timing; run by hand with --release --ignored --nocapture"]
fn timing_numeric_list() {
    let v = Value::List(Arc::new((0..1000).map(Value::Int).collect()));
    let start = std::time::Instant::now();
    let mut acc = 0u128;
    for _ in 0..10_000 {
        acc ^= hash_value(&v).unwrap().0;
    }
    println!("{:?}/hash ({acc})", start.elapsed() / 10_000);
}
