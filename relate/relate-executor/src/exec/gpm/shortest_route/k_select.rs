//! `SHORTEST k` and `SHORTEST k GROUPS` on a pair partition (ADR-0607):
//! the partition's trails by length class, in ascending length, through the
//! one class enumerator ([`crate::shortest::class_walk::ClassWalk`]).
//!
//! `GROUPS` streams every trail of each of the first `k` non-empty classes
//! into the sink. `SHORTEST k` fills from whole classes the same way; the
//! class that overflows the count feeds a heap bounded by the trails still
//! wanted, ordered by the engine's tie-break (relationship-type sequence,
//! then node-id sequence, ADR-0203), so only that many trails are ever
//! retained.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

use relate_core::tokens::WorkspaceCharge;
use relate_core::{NodeId, RelId, Result};

use crate::exec::candidates::expect_enumerated_candidate;
use crate::row::Row;
use crate::shortest::class_walk::ClassWalk;

use super::super::selector::tie_break;
use super::super::sink::MatchedSink;
use super::guard::Keep;
use super::route::Route;

/// Fixed bytes charged per retained candidate beyond its sequences.
const CANDIDATE_BYTES: u64 = 96;

/// Emit the (`s`, `e`) partition's `keep` selection of trails. The start row
/// is bound; the end pattern and 51N23 were decided by the caller.
pub(super) fn select_k(
    route: &Route<'_>,
    (s, e): (NodeId, NodeId),
    srow: &Row,
    keep: Keep,
    out: &mut MatchedSink<'_, '_>,
) -> Result<()> {
    let max = route.walk_max();
    let mut walk = ClassWalk::new(&route.search, s, e);
    // Trails emitted (`Paths`) or non-empty classes completed (`Groups`).
    let mut taken = 0usize;
    for length in route.shape.min..=max {
        if route.halted() {
            break;
        }
        let done = match keep {
            Keep::Groups(k) => {
                let mut any = false;
                let halted = walk.each_trail(length, |trail, _| {
                    any = true;
                    route.offer_trail(s, e, trail, srow, out)?;
                    Ok(route.halted())
                })?;
                taken += usize::from(any);
                halted || taken >= k
            }
            Keep::Paths(k) => {
                let mut heap = Retained::new(k - taken);
                walk.each_trail(length, |trail, nodes| {
                    heap.offer(route, trail, nodes)?;
                    Ok(false)
                })?;
                let members = heap.drain_sorted();
                taken += members.len();
                for member in &members {
                    if route.halted() {
                        break;
                    }
                    route.offer_trail(s, e, &member.trail, srow, out)?;
                }
                route.halted() || taken >= k
            }
        };
        if done || walk.unreachable() || walk.complete() {
            break;
        }
    }
    Ok(())
}

/// One retained trail with the key the selector orders by.
struct Candidate {
    /// Rel-type names along the trail, compared lexicographically.
    types: Vec<Arc<str>>,
    nodes: Vec<NodeId>,
    trail: Vec<RelId>,
    /// Enumeration position: fully tied trails (parallel relationships)
    /// keep the walk's order.
    seq: u64,
}

impl Candidate {
    fn bytes(&self) -> u64 {
        let ids = self.trail.len() * size_of::<RelId>() + self.nodes.len() * size_of::<NodeId>();
        let types = self.types.len() * size_of::<Arc<str>>();
        CANDIDATE_BYTES + (ids + types) as u64
    }
}

/// The selector's one order (ADR-0203): rel-type name sequence, then node-id
/// sequence, of a trail given as parts against a held candidate. Both
/// [`Ord`] for [`Candidate`] and the full heap's reject test compare through
/// it, so an arriving trail is ordered without building a candidate. A full
/// tie is the caller's to break by arrival position: the held trail came
/// first, so an arrival that ties loses.
fn cmp_parts<T: Borrow<str>>(
    types: impl Iterator<Item = T>,
    nodes: &[NodeId],
    other: &Candidate,
) -> Ordering {
    tie_break::<str, _, _>(
        types,
        nodes,
        other.types.iter().map(|held| &**held),
        &other.nodes,
    )
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_parts(self.types.iter().map(|t| &**t), &self.nodes, other)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

/// The best `cap` trails of one length class: a max-heap by the selector's
/// order, so the worst retained trail is the one a better arrival evicts.
struct Retained {
    cap: usize,
    heap: BinaryHeap<Candidate>,
    seq: u64,
    bytes: u64,
    /// Charged (ADR-0463) in doubling steps, so growth is not one ledger
    /// call per trail.
    charge: WorkspaceCharge,
    charged: u64,
}

impl Retained {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            heap: BinaryHeap::new(),
            seq: 0,
            bytes: 0,
            charge: WorkspaceCharge::empty(),
            charged: 0,
        }
    }

    fn offer(&mut self, route: &Route<'_>, trail: &[RelId], nodes: &[NodeId]) -> Result<()> {
        let graph = route.ctx.graph;
        let types = |rid: RelId| expect_enumerated_candidate(graph.rel_type_shared(rid), rid);
        let seq = self.seq;
        self.seq += 1;
        // A full heap keeps only trails that beat its worst. Decide that on
        // the borrowed trail, so a class of millions of trails builds a
        // candidate (three vectors) only for the few that are kept. A tie
        // is not better: the arrival is later in the walk.
        let full = self.heap.len() >= self.cap;
        if full {
            let worst = self
                .heap
                .peek()
                .expect("a zero-count selector never reaches the route");
            if cmp_parts(trail.iter().map(|&rid| types(rid)), nodes, worst) != Ordering::Less {
                return Ok(());
            }
        }
        let candidate = Candidate {
            types: trail.iter().map(|&rid| types(rid)).collect(),
            nodes: nodes.to_vec(),
            trail: trail.to_vec(),
            seq,
        };
        if full {
            let evicted = self
                .heap
                .pop()
                .expect("a full heap holds the worst trail it just compared");
            self.bytes -= evicted.bytes();
        }
        self.bytes += candidate.bytes();
        self.heap.push(candidate);
        self.recharge()
    }

    fn recharge(&mut self) -> Result<()> {
        if self.bytes > self.charged {
            self.charged = self.bytes.saturating_mul(2);
            self.charge.resize_with(|| self.charged)?;
        }
        Ok(())
    }

    /// The retained trails, best first.
    fn drain_sorted(&mut self) -> Vec<Candidate> {
        std::mem::take(&mut self.heap).into_sorted_vec()
    }
}
