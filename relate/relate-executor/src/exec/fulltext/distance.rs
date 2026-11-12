//! Bounded edit distance: the optimal-string-alignment flavour of
//! Damerau-Levenshtein (insert, delete, substitute, and an adjacent
//! transposition each cost one edit), computed over Unicode scalar values
//! with an early exit as soon as the bound is provably exceeded.
//!
//! Lucene's `FuzzyQuery` counts a transposition as one edit by default; the
//! optimal-string-alignment form agrees with full Damerau-Levenshtein on
//! every pair where no substring is edited twice, which is the shape fuzzy
//! term matching at distance <= 2 cares about.

/// The edit distance between `a` and `b` when it is at most `max`, else
/// `None`. The caller collects the query term once (`a`) and scans many
/// candidates; the length gap rejects a candidate before anything is
/// allocated for it, so an oversized query term costs one pass per candidate
/// and no matrix. Cost is otherwise O(|a| * |b|) worst case, cut short by the
/// per-row minimum.
pub(crate) fn bounded_distance(a: &[char], b: &str, max: u8) -> Option<u8> {
    let max = usize::from(max);
    if a.len().abs_diff(b.chars().count()) > max {
        return None;
    }
    let b: Vec<char> = b.chars().collect();
    // Rows i-2, i-1 and i of the distance matrix over prefixes of `a`.
    let mut before: Vec<usize> = vec![0; b.len() + 1];
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        let mut row_min = current[0];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(before[j - 2] + 1);
            }
            current[j] = best;
            row_min = row_min.min(best);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut before, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[b.len()];
    // `distance <= max <= u8::MAX` by the comparison, so the narrowing
    // cannot lose a value.
    (distance <= max).then(|| u8::try_from(distance).expect("bounded by a u8 maximum"))
}

#[cfg(test)]
mod tests {
    use super::bounded_distance;

    fn d(a: &str, b: &str, max: u8) -> Option<u8> {
        bounded_distance(&a.chars().collect::<Vec<_>>(), b, max)
    }

    #[test]
    fn identical_is_zero() {
        assert_eq!(d("kollegger", "kollegger", 0), Some(0));
        assert_eq!(d("", "", 0), Some(0));
    }

    #[test]
    fn insert_delete_substitute_cost_one() {
        assert_eq!(d("kolleger", "kollegger", 2), Some(1)); // insert
        assert_eq!(d("kollegger", "kolleger", 2), Some(1)); // delete
        assert_eq!(d("lamp", "lamb", 2), Some(1)); // substitute
        assert_eq!(d("", "ab", 2), Some(2));
    }

    #[test]
    fn adjacent_transposition_costs_one() {
        assert_eq!(d("teh", "the", 1), Some(1));
        assert_eq!(d("form", "from", 1), Some(1));
        // Two separate transpositions are two edits.
        assert_eq!(d("abcd", "badc", 2), Some(2));
    }

    #[test]
    fn bound_is_enforced() {
        assert_eq!(d("kitten", "sitting", 2), None); // true distance 3
        assert_eq!(d("kitten", "sitting", 3), Some(3));
        assert_eq!(d("abc", "abcdef", 2), None); // length gap alone
        assert_eq!(d("lamp", "lamb", 0), None);
    }

    #[test]
    fn counts_unicode_scalars_not_bytes() {
        assert_eq!(d("kölner", "kolner", 1), Some(1));
        assert_eq!(d("日本語", "日本誤", 1), Some(1));
    }

    #[test]
    fn an_oversized_term_is_rejected_by_the_length_gap_alone() {
        // No matrix is built: the gap check answers before any allocation.
        assert_eq!(d(&"a".repeat(200_000), "lamp", 2), None);
    }
}
