//! The far side of a fan, held as a bitset over node ids: the listed
//! endpoint side a fan does not drive from. A fan reaches every node within
//! its bound; with the far side listed, a reached node is one bit test
//! before the exact end-pattern admission, which still decides every
//! member, and the fan stops once every far node has been reached.

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
