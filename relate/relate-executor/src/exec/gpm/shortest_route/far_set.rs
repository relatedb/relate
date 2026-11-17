//! The far side of a fan, held as a bitset over node ids: the listed
//! endpoint side a fan does not drive from. A fan reaches every node within
//! its bound; with the far side listed, a reached node is one bit test
//! before the exact end-pattern admission, which still decides every
//! member, and the fan stops once every far node has been reached.

use std::cell::Cell;

use relate_core::NodeId;

/// The widest id span a far set will cover, in ids (an 8 MiB bitset): the
/// words span `0..=max`, so one large id would otherwise reserve memory
/// proportional to the store's id high-water mark, not to the list.
const MAX_SPAN: usize = 1 << 26;

/// The one `NodeId` to bit-index conversion: engine ids are non-negative.
fn index(id: NodeId) -> usize {
    usize::try_from(id).expect("node ids are non-negative")
}

/// A set of node ids as a bitset. Node ids are dense and non-negative, so
/// the words span `0..=max`; a node past them is not a member.
pub(super) struct FarSet {
    words: Vec<u64>,
    /// Distinct members.
    len: usize,
}

impl FarSet {
    /// The set of `ids`; duplicates count once. `None` when the ids span
    /// more than [`MAX_SPAN`]: the fan then tests each reached node
    /// against the end pattern alone, as it does without a far side.
    pub(super) fn of(ids: &[NodeId]) -> Option<Self> {
        let words = ids.iter().map(|&id| index(id) / 64 + 1).max().unwrap_or(0);
        if words * 64 > MAX_SPAN {
            return None;
        }
        let mut set = Self {
            words: vec![0; words],
            len: 0,
        };
        for &id in ids {
            let (word, bit) = (index(id) / 64, index(id) % 64);
            set.len += usize::from(set.words[word] & (1 << bit) == 0);
            set.words[word] |= 1 << bit;
        }
        Some(set)
    }

    /// How many distinct nodes the set holds.
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// The members, ascending.
    pub(super) fn ids(&self) -> Vec<NodeId> {
        let mut ids = Vec::with_capacity(self.len);
        for (word_at, &word) in self.words.iter().enumerate() {
            let mut rest = word;
            while rest != 0 {
                let bit = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                ids.push(NodeId::try_from(word_at * 64 + bit).expect("a node id fits NodeId"));
            }
        }
        ids
    }

    /// Whether `id` is a member.
    pub(super) fn contains(&self, id: NodeId) -> bool {
        let index = index(id);
        self.words
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }
}

/// How many far nodes each fan origin has reached, against its far set:
/// the one home of the retire rule's count (every far node reached). A
/// counter never exists without its far set — the set is borrowed here,
/// so a settled count is always a count of *this* set's members. The
/// layered fan runs one lane; the batched fan runs `LANES`
/// (crate::shortest::multi_source) of them.
pub(super) struct Settled<'f> {
    /// One reached count per lane.
    counts: Vec<Cell<usize>>,
    far: &'f FarSet,
}

impl<'f> Settled<'f> {
    /// `lanes` origins' reached counts, all zero, against `far`.
    pub(super) fn new(lanes: usize, far: &'f FarSet) -> Self {
        Self {
            counts: vec![Cell::new(0); lanes],
            far,
        }
    }

    /// One more far node reached by `lane`.
    pub(super) fn settle(&self, lane: usize) {
        self.counts[lane].set(self.counts[lane].get() + 1);
    }

    /// Whether `lane` has reached every far node.
    pub(super) fn done(&self, lane: usize) -> bool {
        self.counts[lane].get() == self.far.len()
    }

    /// The bit test and the count in one step, the pairing the layered
    /// fan's early stop relies on: `true` — `node` is a far member, now
    /// counted — or `false` (nothing counted). The batched fan keeps the
    /// plain [`Self::settle`] for its sweep-pre-filtered targets.
    pub(super) fn settle_if_hit(&self, lane: usize, node: NodeId) -> bool {
        if !self.far.contains(node) {
            return false;
        }
        self.settle(lane);
        true
    }
}

/// The layered fan's one-lane counter: [`Settled`] with a single lane,
/// its index implied — no caller writes the literal `0`.
pub(super) struct OneLane<'f>(Settled<'f>);

impl<'f> OneLane<'f> {
    /// The counter of the one origin a layered fan runs, against `far`.
    pub(super) fn new(far: &'f FarSet) -> Self {
        Self(Settled::new(1, far))
    }

    /// The bit test and the count in one step (see
    /// [`Settled::settle_if_hit`]).
    pub(super) fn settle_if_hit(&self, node: NodeId) -> bool {
        self.0.settle_if_hit(0, node)
    }

    /// Whether the origin has reached every far node.
    pub(super) fn done(&self) -> bool {
        self.0.done(0)
    }
}

/// The fan's early-stop rule, the one spelling both fan drivers read (the
/// layered fan, route.rs's `fan_layer`; the batched fan, multi_fan.rs's
/// `fan_chunk`): an origin retires when it has reached every far node of
/// its far set (`done`, each driver's own counter form) — or, a
/// first-layer spelling, at the first level that produced a row
/// (`productive`, each driver's own precondition folded in).
pub(super) fn retires(done: bool, first_layer_only: bool, productive: bool) -> bool {
    done || (first_layer_only && productive)
}

#[cfg(test)]
mod tests {
    use super::FarSet;

    #[test]
    fn membership_len_and_out_of_range() {
        let set = FarSet::of(&[3, 64, 3, 130]).unwrap();
        assert_eq!(set.len(), 3);
        assert_eq!(set.ids(), vec![3, 64, 130]);
        assert!([3, 64, 130].iter().all(|&id| set.contains(id)));
        assert!(![0, 4, 63, 65, 131, 100_000]
            .iter()
            .any(|&id| set.contains(id)));
        let empty = FarSet::of(&[]).unwrap();
        assert_eq!(empty.len(), 0);
        assert!(!empty.contains(0));
        assert!(FarSet::of(&[1 << 40]).is_none());
    }
}
