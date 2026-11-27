//! O19 traversal set state — seen/frontier row-sets for QPP, shortest path, varlen.
//!
//! Three questions, one honest comparison against `HashSet`:
//! 1. resident bytes for 1M visited ids: `HashSet<u64>` vs `HashSet<u32>` vs a
//!    hand-rolled dense bitset. `bytes_per_op` from `measure` on a
//!    `with_capacity` build is exactly that build's one allocation (drops do
//!    not pass through the counting alloc path), so it is a real resident
//!    figure, not an estimate.
//! 2. per-level frontier algebra: `new = next - seen`, `seen |= new`.
//! 3. order preservation: a bitset iterates ascending; arrival order needs a
//!    parallel Vec (or a sort). Both measured.
//!
//! Setup (frontier generation, bitset builds) is outside `measure`; each
//! measured round is a full 100-level BFS and the printed ns are per level.
//! Each kernel seeds its own `Rng`, so one kernel's stream never depends on
//! another's setup order.

use std::collections::HashSet;
use std::hint::black_box;
use std::path::Path;

use relate_test_support::rng::Rng;

use crate::bench::{measure, Measured};

const VISITED: usize = 1_000_000; // seen-set size in kernel 1
const LEVELS: usize = 100; // BFS levels simulated
const FRONTIER: usize = 10_000; // frontier size per level
const SPACE: u64 = 7 * VISITED as u64; // id space: 1M visited as every 7th id

/// Dense bitset: 1 bit per node id, ascending iteration by construction.
struct Bits {
    words: Vec<u64>,
}

impl Bits {
    fn new(nbits: usize) -> Self {
        Bits {
            words: vec![0; nbits.div_ceil(64)],
        }
    }

    #[inline]
    fn set(&mut self, i: u64) {
        self.words[(i >> 6) as usize] |= 1u64 << (i & 63);
    }

    #[inline]
    fn contains(&self, i: u64) -> bool {
        self.words[(i >> 6) as usize] & (1u64 << (i & 63)) != 0
    }

    fn clear_all(&mut self) {
        self.words.fill(0);
    }

    /// `out = self & !other`
    fn andnot(&self, other: &Bits, out: &mut Bits) {
        assert_eq!(self.words.len(), other.words.len());
        assert_eq!(self.words.len(), out.words.len());
        for k in 0..self.words.len() {
            out.words[k] = self.words[k] & !other.words[k];
        }
    }

    /// `out = self & other`
    fn and(&self, other: &Bits, out: &mut Bits) {
        assert_eq!(self.words.len(), other.words.len());
        assert_eq!(self.words.len(), out.words.len());
        for k in 0..self.words.len() {
            out.words[k] = self.words[k] & other.words[k];
        }
    }

    fn or_assign(&mut self, other: &Bits) {
        assert_eq!(self.words.len(), other.words.len());
        for k in 0..self.words.len() {
            self.words[k] |= other.words[k];
        }
    }

    fn count(&self) -> u64 {
        self.words.iter().map(|w| w.count_ones() as u64).sum()
    }

    fn for_each(&self, mut f: impl FnMut(u64)) {
        for (wi, &word) in self.words.iter().enumerate() {
            let mut w = word;
            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                f((wi * 64 + bit) as u64);
                w &= w - 1;
            }
        }
    }
}

/// The BFS input the algebra/order kernels share: 100 frontiers of 10k
/// unseen ids each, in arrival order, as bitsets and hash sets.
struct Bfs {
    frontiers: Vec<Vec<u64>>,
    frontiers_bits: Vec<Bits>,
    frontiers_sets: Vec<HashSet<u64>>,
}

impl Bfs {
    fn build(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let mut gen_seen = Bits::new(SPACE as usize);
        let mut frontiers: Vec<Vec<u64>> = Vec::with_capacity(LEVELS);
        for _ in 0..LEVELS {
            let mut pick = Bits::new(SPACE as usize);
            let mut level = Vec::with_capacity(FRONTIER);
            while level.len() < FRONTIER {
                let id = rng.below(SPACE);
                if !gen_seen.contains(id) && !pick.contains(id) {
                    pick.set(id);
                    level.push(id);
                }
            }
            for &id in &level {
                gen_seen.set(id);
            }
            frontiers.push(level);
        }
        let frontiers_bits: Vec<Bits> = frontiers
            .iter()
            .map(|level| {
                let mut b = Bits::new(SPACE as usize);
                for &id in level {
                    b.set(id);
                }
                b
            })
            .collect();
        let frontiers_sets: Vec<HashSet<u64>> = frontiers
            .iter()
            .map(|level| level.iter().copied().collect())
            .collect();
        Self {
            frontiers,
            frontiers_bits,
            frontiers_sets,
        }
    }
}

/// Split a per-round `Measured` into per-level figures.
fn per_level(name: &str, m: Measured, levels: f64) -> Measured {
    Measured {
        name: name.to_string(),
        iters: m.iters,
        ns_per_op: m.ns_per_op / levels,
        allocs_per_op: m.allocs_per_op / levels,
        bytes_per_op: m.bytes_per_op / levels,
        load_start: m.load_start,
        load_end: m.load_end,
    }
}

pub fn run(out_dir: &Path, iters: u64) -> Result<Vec<Measured>, Box<dyn std::error::Error>> {
    let _ = out_dir;
    let rounds = iters.clamp(1, 10);
    let bfs = Bfs::build(0x9E37_79B9_7F4A_7C15 | 1);

    let mut results = k1_memory();
    sanity(&bfs);
    results.extend(k2_algebra(&bfs, rounds));
    results.extend(k3_order(&bfs, rounds, iters));
    results.extend(k4_intersect(&bfs, iters));
    k5_sparsity(iters);
    Ok(results)
}

/// Kernel 1: seen-set resident memory (1M visited, every 7th id).
fn k1_memory() -> Vec<Measured> {
    let mut results = Vec::new();
    let ids64: Vec<u64> = (0..VISITED as u64).map(|i| i * 7).collect();
    let ids32: Vec<u32> = ids64.iter().map(|&i| i as u32).collect();

    let m = measure("O19 mem: HashSet<u64> resident", 1, |_| {
        let mut s: HashSet<u64> = HashSet::with_capacity(ids64.len());
        for &id in &ids64 {
            s.insert(id);
        }
        black_box(&s);
    });
    m.report();
    println!(
        "  bytes/set {:.0} — {:.1} B/node ({} visited in a {}M id space)",
        m.bytes_per_op,
        m.bytes_per_op / VISITED as f64,
        VISITED,
        SPACE / 1_000_000
    );
    results.push(m);

    let m = measure("O19 mem: HashSet<u32> resident", 1, |_| {
        let mut s: HashSet<u32> = HashSet::with_capacity(ids32.len());
        for &id in &ids32 {
            s.insert(id);
        }
        black_box(&s);
    });
    m.report();
    println!(
        "  bytes/set {:.0} — {:.1} B/node",
        m.bytes_per_op,
        m.bytes_per_op / VISITED as f64
    );
    results.push(m);

    let m = measure("O19 mem: dense bitset Vec<u64>", 1, |_| {
        let mut b = Bits::new(SPACE as usize);
        for &id in &ids64 {
            b.set(id);
        }
        black_box(&b);
    });
    m.report();
    println!(
        "  bytes/set {:.0} — {:.3} B/node ({:.2} bits/node)",
        m.bytes_per_op,
        m.bytes_per_op / VISITED as f64,
        m.bytes_per_op * 8.0 / VISITED as f64
    );
    results.push(m);
    results
}

/// Both algebra paths agree, and bitset iteration is ascending.
fn sanity(bfs: &Bfs) {
    println!("BFS input: {LEVELS} levels x {FRONTIER} ids, space {SPACE} bits");
    let mut sb = Bits::new(SPACE as usize);
    let mut sc = Bits::new(SPACE as usize);
    let mut bit_new = 0u64;
    for next in &bfs.frontiers_bits {
        next.andnot(&sb, &mut sc);
        sb.or_assign(&sc);
        bit_new += sc.count();
    }
    let mut hs: HashSet<u64> = HashSet::with_capacity(VISITED);
    let mut hash_new = 0usize;
    for next in &bfs.frontiers_sets {
        let new: Vec<u64> = next.difference(&hs).copied().collect();
        hs.extend(new.iter().copied());
        hash_new += new.len();
    }
    assert_eq!(bit_new as usize, hash_new);
    assert_eq!(hash_new, LEVELS * FRONTIER);
    let mut asc = Vec::new();
    bfs.frontiers_bits[0].for_each(|id| asc.push(id));
    let mut sorted = bfs.frontiers[0].clone();
    sorted.sort_unstable();
    assert_eq!(asc, sorted);
    println!("sanity: algebra agrees ({hash_new} new nodes), bitset iterates ascending");
}

/// Kernel 2: frontier algebra, bitset vs HashSet.
fn k2_algebra(bfs: &Bfs, rounds: u64) -> Vec<Measured> {
    let mut results = Vec::new();

    // 2a: bitset algebra (reused scratch, zero per-level alloc).
    let mut seen_bits = Bits::new(SPACE as usize);
    let mut scratch = Bits::new(SPACE as usize);
    let m = measure("O19 algebra: bitset", rounds, |_| {
        seen_bits.clear_all();
        for next in &bfs.frontiers_bits {
            next.andnot(&seen_bits, &mut scratch); // new = next - seen
            seen_bits.or_assign(&scratch); // seen |= new
        }
        black_box(&seen_bits);
    });
    let m = per_level("O19 algebra: bitset (per level)", m, LEVELS as f64);
    m.report();
    results.push(m);

    // 2b: HashSet algebra (difference + extend).
    let mut seen_set: HashSet<u64> = HashSet::with_capacity(2 * VISITED);
    let m = measure("O19 algebra: HashSet<u64>", rounds, |_| {
        seen_set.clear();
        for next in &bfs.frontiers_sets {
            let new: Vec<u64> = next.difference(&seen_set).copied().collect();
            seen_set.extend(new.iter().copied()); // seen |= new
            black_box(new.len());
        }
        black_box(seen_set.len());
    });
    let m = per_level("O19 algebra: HashSet<u64> (per level)", m, LEVELS as f64);
    m.report();
    results.push(m);
    results
}

/// One child of `id` under the order kernels' expansion policy (three
/// children per id), shared by the two arms that expand a frontier.
fn child_of(id: u64, c: u64) -> u64 {
    (id.wrapping_mul(3).wrapping_add(c * 7_919 + 1)) % SPACE
}

/// Kernel 3: order preservation.
fn k3_order(bfs: &Bfs, rounds: u64, iters: u64) -> Vec<Measured> {
    let mut results = Vec::new();

    // ascend: scan the next bitset word by word, push set bits (ascending)
    let mut out = Vec::with_capacity(FRONTIER);
    let m = measure("O19 order: bitset scan only", rounds, |_| {
        for next in &bfs.frontiers_bits {
            let mut n = 0u64;
            next.for_each(|_| n += 1);
            black_box(n);
        }
    });
    let scan_only = per_level("O19 order: bitset scan only (per level)", m, LEVELS as f64);
    scan_only.report();
    results.push(scan_only);

    let m = measure("O19 order: bitset scan + push", rounds, |_| {
        for next in &bfs.frontiers_bits {
            out.clear();
            next.for_each(|id| out.push(id));
            black_box(out.len());
        }
    });
    let ascend = per_level(
        "O19 order: bitset scan + push (per level)",
        m,
        LEVELS as f64,
    );
    ascend.report();
    results.push(ascend);

    // arrival: expand a frontier Vec in arrival order; dedup via a next bitset.
    // B0 = bitset only; B1 = bitset + parallel Vec. Delta = arrival-order cost.
    let mut next_bits = Bits::new(SPACE as usize);
    let m = measure("O19 order: expand -> bitset", rounds, |_| {
        for level in &bfs.frontiers {
            next_bits.clear_all();
            for &id in level {
                for c in 0..3u64 {
                    let child = child_of(id, c);
                    next_bits.set(child);
                }
            }
            black_box(next_bits.words[0]);
        }
    });
    let expand_bits = per_level("O19 order: expand -> bitset (per level)", m, LEVELS as f64);
    expand_bits.report();
    results.push(expand_bits);

    let mut arrival = Vec::with_capacity(3 * FRONTIER);
    let m = measure("O19 order: expand -> bitset+vec", rounds, |_| {
        for level in &bfs.frontiers {
            next_bits.clear_all();
            arrival.clear();
            for &id in level {
                for c in 0..3u64 {
                    let child = child_of(id, c);
                    if !next_bits.contains(child) {
                        next_bits.set(child);
                        arrival.push(child);
                    }
                }
            }
            black_box(arrival.len());
        }
    });
    let arrival_m = per_level(
        "O19 order: expand -> bitset+vec (per level)",
        m,
        LEVELS as f64,
    );
    arrival_m.report();
    results.push(arrival_m);

    // sort-at-end: arrival list has to be ordered, so sort a copy of 10k ids.
    let src: Vec<u64> = bfs.frontiers[0].clone();
    let mut buf = src.clone();
    let sort_iters = iters.clamp(1, 200);
    let m = measure("O19 order: sort 10k ids at end", sort_iters, |_| {
        buf.copy_from_slice(&src);
        buf.sort_unstable();
        black_box(buf.len());
    });
    m.report();
    results.push(m);
    results
}

/// Kernel 4 (bonus): frontier ∩ filter-predicate result.
fn k4_intersect(bfs: &Bfs, iters: u64) -> Vec<Measured> {
    let mut results = Vec::new();
    // A fresh stream: the filter bitset does not depend on the BFS setup.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 | 2);
    let frontier_bits = &bfs.frontiers_bits[0];
    let frontier_ids = &bfs.frontiers[0];
    let frontier_set = &bfs.frontiers_sets[0];
    let mut filter_bits = Bits::new(SPACE as usize);
    for w in filter_bits.words.iter_mut() {
        *w = rng.next_u64(); // ~50% of the id space survives the filter
    }
    let mut filter_set: HashSet<u64> = HashSet::with_capacity(SPACE as usize / 2);
    filter_bits.for_each(|id| {
        filter_set.insert(id);
    });
    let mut scratch = Bits::new(SPACE as usize);
    let hit = iters.clamp(1, 500);
    let m = measure("O19 intersect: bitset AND (full scan)", hit, |_| {
        frontier_bits.and(&filter_bits, &mut scratch);
        black_box(scratch.words[0]);
    });
    m.report();
    results.push(m);

    let m = measure("O19 intersect: bitset probe (10k rows)", hit, |_| {
        let mut n = 0u64;
        for &id in frontier_ids {
            if filter_bits.contains(id) {
                n += 1;
            }
        }
        black_box(n);
    });
    m.report();
    results.push(m);

    let m = measure("O19 intersect: HashSet probe (10k rows)", hit, |_| {
        let mut n = 0u64;
        for &id in frontier_set {
            if filter_set.contains(&id) {
                n += 1;
            }
        }
        black_box(n);
    });
    m.report();
    results.push(m);
    results
}

/// Kernel 5: where does the bitset lose? Fixed 100k visited, growing space.
fn k5_sparsity(iters: u64) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 | 3);
    let _ = iters;
    println!("sparsity sweep: 100k visited, 10k frontier, bitset vs HashSet<u64>");
    for space in [200_000u64, 1_000_000, 10_000_000, 100_000_000] {
        let ids: Vec<u64> = (0..100_000u64).map(|i| i * space / 100_000).collect();
        let mem = measure("O19 sparsity mem", 1, |_| {
            let mut s: HashSet<u64> = HashSet::with_capacity(ids.len());
            for &id in &ids {
                s.insert(id);
            }
            black_box(&s);
        });
        let hs_bytes = mem.bytes_per_op;
        let bits_bytes = (space.div_ceil(64) * 8) as f64;

        let mut sb = Bits::new(space as usize);
        for &id in &ids {
            sb.set(id);
        }
        let mut fb = Bits::new(space as usize);
        let mut pick = HashSet::with_capacity(FRONTIER);
        while pick.len() < FRONTIER {
            pick.insert(rng.below(space));
        }
        for &id in &pick {
            fb.set(id);
        }
        let fvec: Vec<u64> = pick.into_iter().collect();
        let hseen: HashSet<u64> = ids.iter().copied().collect();

        let words = fb.words.len() as u64;
        let it = (50_000_000 / words).clamp(20, 2000);
        let mut sp = Bits::new(space as usize);
        let bset = measure("O19 sparsity bitset", it, |_| {
            fb.andnot(&sb, &mut sp);
            black_box(sp.words[0]);
        });
        let hset = measure("O19 sparsity hashset", it, |_| {
            let n = fvec.iter().filter(|x| !hseen.contains(x)).count();
            black_box(n);
        });
        println!(
            "  space {:>11}: bitset {:>8.0} B {:>7.0} ns/op | HashSet {:>8.0} B {:>7.0} ns/op  => {}",
            space,
            bits_bytes,
            bset.ns_per_op,
            hs_bytes,
            hset.ns_per_op,
            if bset.ns_per_op < hset.ns_per_op { "bitset" } else { "hashset" }
        );
    }
}
