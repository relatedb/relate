//! Dense presence bits over a slot array: one bit per logical slot, packed
//! into `u64` words (bit `i` lives in word `i / 64`, position `i % 64`).
//!
//! Pure mechanism — the bit layout and the five slot operations. No policy
//! (budgets, warm-up, charging) lives here; callers decide what a present
//! bit means and when to shift.

/// One bit per logical slot, packed little-endian into `u64` words.
///
/// `insert_shift` opens a new absent slot at `i` (occupants at `i` and above
/// move up one); `remove_shift` drops slot `i` (occupants above move down
/// one). Both keep the bits sorted by slot position, so the bitmap always
/// mirrors a dense slot array's occupancy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceBits {
    words: Vec<u64>,
    len_bits: usize,
}

fn words_needed(len_bits: usize) -> usize {
    len_bits.div_ceil(64)
}

impl PresenceBits {
    /// The bitmap over `len_bits` slots, all absent.
    pub fn new(len_bits: usize) -> Self {
        Self {
            words: vec![0; words_needed(len_bits)],
            len_bits,
        }
    }

    /// The number of logical slots (absent and present).
    pub fn len(&self) -> usize {
        self.len_bits
    }

    /// Whether no slot is present.
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// Marks slot `i` present. Panics when `i` is outside the slot range.
    pub fn set(&mut self, i: usize) {
        assert!(i < self.len_bits, "slot {i} outside 0..{}", self.len_bits);
        self.words[i / 64] |= 1u64 << (i % 64);
    }

    /// Whether slot `i` is present. Panics when `i` is outside the slot
    /// range — an out-of-range answer would silently hide a caller bug.
    pub fn test(&self, i: usize) -> bool {
        assert!(i < self.len_bits, "slot {i} outside 0..{}", self.len_bits);
        (self.words[i / 64] >> (i % 64)) & 1 == 1
    }

    /// The number of present slots.
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Opens a new absent slot at `i`: every occupant at `i` or above moves
    /// up one position. Panics when `i` is outside `0..=len`.
    pub fn insert_shift(&mut self, i: usize) {
        assert!(i <= self.len_bits, "slot {i} outside 0..={}", self.len_bits);
        let (word, bit) = (i / 64, i % 64);
        self.len_bits += 1;
        self.words.resize(words_needed(self.len_bits), 0);
        let low_mask = (1u64 << bit) - 1;
        // Bits below `i` stay; bits at `i` and above move up one (slot `i`
        // itself lands at `i + 1`, leaving the new slot `i` absent).
        let old = self.words[word];
        self.words[word] = (old & low_mask) | ((old & !low_mask) << 1);
        let mut carry = old >> 63;
        for w in (word + 1)..self.words.len() {
            let above = self.words[w];
            self.words[w] = (above << 1) | carry;
            carry = above >> 63;
        }
    }

    /// Drops slot `i`: every occupant above `i` moves down one position.
    /// Panics when `i` is outside the slot range.
    pub fn remove_shift(&mut self, i: usize) {
        assert!(i < self.len_bits, "slot {i} outside 0..{}", self.len_bits);
        let (word, bit) = (i / 64, i % 64);
        self.len_bits -= 1;
        let low_mask = (1u64 << bit) - 1;
        // Bits below `i` stay; bits above `i` move down one, each word's
        // vacated top bit filled by the word above's lowest bit.
        let mut carry: u64 = 0;
        for w in (word..self.words.len()).rev() {
            let old = self.words[w];
            let shifted_down = if w == word {
                // `bit == 63` has no in-word high part: the whole word is
                // the low part, and the word above fills bit 63 via carry.
                let high = if bit == 63 {
                    0
                } else {
                    (old >> (bit + 1)) << bit
                };
                (old & low_mask) | high
            } else {
                old >> 1
            };
            self.words[w] = shifted_down | (carry << 63);
            carry = old & 1;
        }
        self.words.truncate(words_needed(self.len_bits));
    }
}

#[cfg(test)]
mod tests {
    use super::PresenceBits;

    #[test]
    fn is_empty_is_true_fresh_and_false_once_a_slot_is_set() {
        let mut bits = PresenceBits::new(8);
        assert!(bits.is_empty());
        bits.set(3);
        assert!(!bits.is_empty());
    }

    #[test]
    fn set_and_test_across_word_boundaries_beyond_128_bits() {
        let mut bits = PresenceBits::new(200);
        for i in [0, 1, 63, 64, 65, 127, 128, 129, 191, 192, 199] {
            bits.set(i);
        }
        for i in 0..200 {
            let expected = matches!(i, 0 | 1 | 63 | 64 | 65 | 127 | 128 | 129 | 191 | 192 | 199);
            assert_eq!(bits.test(i), expected, "slot {i}");
        }
        assert_eq!(bits.count(), 11);
    }

    #[test]
    fn count_tracks_set_and_unset_bits_over_many_words() {
        let mut bits = PresenceBits::new(320);
        assert_eq!(bits.count(), 0);
        for i in (0..320).step_by(3) {
            bits.set(i);
        }
        assert_eq!(bits.count(), 107);
        bits.set(0); // already present: count unchanged
        assert_eq!(bits.count(), 107);
    }

    #[test]
    fn insert_shift_at_arbitrary_positions_moves_occupants_up() {
        // Interior of a word.
        let mut bits = PresenceBits::new(70);
        for i in [0, 5, 6, 69] {
            bits.set(i);
        }
        bits.insert_shift(6);
        assert_eq!(bits.len(), 71);
        assert!(!bits.test(6), "the opened slot starts absent");
        for (before, after) in [(0, 0), (5, 5), (6, 7), (69, 70)] {
            assert!(bits.test(after), "occupant {before} moved to {after}");
        }
        assert_eq!(bits.count(), 4);

        // Word boundary and beyond 128 bits, carrying across words.
        let mut bits = PresenceBits::new(130);
        for i in [63, 64, 127, 128, 129] {
            bits.set(i);
        }
        bits.insert_shift(64);
        assert_eq!(bits.len(), 131);
        assert!(!bits.test(64));
        for (before, after) in [(63, 63), (64, 65), (127, 128), (128, 129), (129, 130)] {
            assert!(bits.test(after), "occupant {before} moved to {after}");
        }

        // At the very end: the appended slot opens absent, occupants stay.
        let mut bits = PresenceBits::new(128);
        bits.set(127);
        bits.insert_shift(128);
        assert_eq!(bits.len(), 129);
        assert!(!bits.test(128), "the appended slot starts absent");
        assert!(bits.test(127), "occupants below the opened slot stay put");
    }

    #[test]
    fn remove_shift_at_arbitrary_positions_moves_occupants_down() {
        let mut bits = PresenceBits::new(200);
        for i in [0, 62, 63, 64, 65, 128, 199] {
            bits.set(i);
        }
        bits.remove_shift(64); // drops the occupant at 64
        assert_eq!(bits.len(), 199);
        for (before, after) in [(0, 0), (62, 62), (63, 63), (65, 64), (128, 127), (199, 198)] {
            assert!(bits.test(after), "occupant {before} moved down to {after}");
        }
        assert_eq!(bits.count(), 6);

        // Shrinking across a word boundary retires the trailing word.
        let mut bits = PresenceBits::new(129);
        bits.set(128);
        bits.remove_shift(0);
        assert_eq!(bits.len(), 128);
        assert!(
            bits.test(127),
            "top occupant moved down across the boundary"
        );
        assert_eq!(bits.count(), 1);
    }

    #[test]
    fn insert_then_remove_roundtrips_the_bitmap() {
        let mut bits = PresenceBits::new(150);
        for i in (0..150).step_by(2) {
            bits.set(i);
        }
        let before = bits.clone();
        for i in [0, 1, 63, 64, 100, 149, 150] {
            let mut shifted = bits.clone();
            shifted.insert_shift(i);
            shifted.remove_shift(i);
            assert_eq!(shifted, before, "roundtrip at {i}");
        }
    }

    /// A fixed op sequence cross-checked against a `Vec<bool>` reference
    /// model — the same shape the adjacency proptests use.
    #[test]
    fn matches_a_vec_of_bool_reference_model() {
        let mut bits = PresenceBits::new(0);
        let mut model: Vec<bool> = vec![];
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let r = next();
            let len = model.len();
            match r % 5 {
                0 | 1 if len > 0 => {
                    let i = (r >> 8) as usize % len;
                    bits.set(i);
                    model[i] = true;
                }
                2 if len > 0 => {
                    let i = (r >> 8) as usize % len;
                    assert_eq!(bits.test(i), model[i], "test at {i}");
                }
                3 => {
                    let i = (r >> 8) as usize % (len + 1);
                    bits.insert_shift(i);
                    model.insert(i, false);
                }
                _ => {
                    if len > 0 {
                        let i = (r >> 8) as usize % len;
                        bits.remove_shift(i);
                        model.remove(i);
                    }
                }
            }
            assert_eq!(bits.len(), model.len());
            assert_eq!(bits.count(), model.iter().filter(|p| **p).count());
        }
        for (i, present) in model.iter().enumerate() {
            assert_eq!(bits.test(i), *present, "final state at {i}");
        }
    }
}
