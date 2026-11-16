//! A non-seekable endpoint listed by its own `WHERE` (see
//! [`super::end_filter`]): every shape is checked against the enumerating
//! GPM walk (rows, or the same typed error), and the anchor arm and the
//! expansion counts pin that the side was listed — or, where it must not
//! be, was not.

use crate::graph::Graph;

use super::anchor::AnchorArm;
use super::fixtures::{parity, Selector, SELECTORS};
use super::where_seek_tests::ring;

const BODY: &str = "-[:R*..6]->";

/// A ring graph with an index on `id`, so the start seeks and only the
/// end's `toInteger(b.id) + 0 = …` has no seek to ride.
fn indexed() -> Graph {
    ring(true)
}

/// The shape of LABS-1763: a keyword selector whose end is filtered by an
/// expression over its property. The end lists as one node, so the pair
/// search answers, at the cost of the same pair with the equality in a
/// seekable form — not one row and one path per reachable node.
#[test]
fn a_non_sargable_end_equality_lists_the_end_and_searches_the_pair() {
    let g = indexed();
    let non_sargable = parity(
        &g,
        &format!(
            "MATCH p = SHORTEST 1 (a:N {{id: '0'}}){BODY}(b:N) \
             WHERE toInteger(b.id) + 0 = 150 RETURN b.id, length(p)"
        ),
    );
    let sargable = parity(
        &g,
        &format!(
            "MATCH p = SHORTEST 1 (a:N {{id: '0'}}){BODY}(b:N) \
             WHERE b.id = '150' RETURN b.id, length(p)"
        ),
    );
    assert_eq!(non_sargable.arms, vec![AnchorArm::Pair]);
    assert_eq!(sargable.arms, non_sargable.arms);
    assert_eq!(
        non_sargable.expands, sargable.expands,
        "the filtered end must cost the seeked pair"
    );
}

/// Every keyword selector and a counted one answer the filtered end the way
/// the walk does.
#[test]
fn every_selector_agrees_on_a_filtered_end() {
    let g = indexed();
    for Selector { open, close, .. } in SELECTORS {
        let q = format!(
            "MATCH p = {open}(a:N {{id: '0'}}){BODY}(b:N) \
             WHERE toInteger(b.id) % 50 = 7 RETURN a.id, b.id, length(p){close}"
        );
        let arms = parity(&g, &q).arms;
        assert_eq!(arms, vec![AnchorArm::Pair], "{open}");
    }
    let counted = parity(
        &g,
        &format!(
            "MATCH p = SHORTEST 3 (a:N {{id: '0'}}){BODY}(b:N) \
             WHERE toInteger(b.id) + 0 = 150 RETURN b.id, length(p)"
        ),
    );
    assert_eq!(counted.arms, vec![AnchorArm::Pair]);
}

/// The function spelling filters the same way.
#[test]
fn the_function_spelling_agrees_on_a_filtered_end() {
    let g = indexed();
    let q = format!(
        "MATCH p = shortestPath((a:N {{id: '0'}}){BODY}(b:N)) \
         WHERE toInteger(b.id) + 0 = 150 RETURN b.id, length(p)"
    );
    assert_eq!(parity(&g, &q).arms, vec![AnchorArm::Pair]);
}

/// The start side lists the same way: the end seeks, the start's filter
/// lists, and the product is a pair.
#[test]
fn a_non_sargable_start_is_listed_too() {
    let g = indexed();
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N){BODY}(b:N {{id: '150'}}) \
         WHERE toInteger(a.id) + 0 = 0 RETURN a.id, b.id, length(p)"
    );
    assert_eq!(parity(&g, &q).arms, vec![AnchorArm::Pair]);
}

/// A filter that matches nothing lists an empty side: no row, as the walk.
#[test]
fn a_filter_with_no_match_lists_nothing() {
    let g = indexed();
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE toInteger(b.id) + 0 = 9999 RETURN b.id, length(p)"
    );
    assert_eq!(parity(&g, &q).arms, vec![AnchorArm::Pair]);
}

/// More than the pair bound match: the side stays unlisted and the fan
/// answers exactly as before.
#[test]
fn a_filter_past_the_pair_bound_keeps_the_fan() {
    let g = indexed();
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE toInteger(b.id) % 2 = 0 RETURN b.id, length(p)"
    );
    assert_eq!(parity(&g, &q).arms, vec![AnchorArm::FanForward]);
}

/// A nondeterministic conjunct is never evaluated ahead of the search (the
/// residual samples it once per row): no listing, the fan, same rows for a
/// predicate that is always true.
#[test]
fn a_nondeterministic_conjunct_does_not_list() {
    let g = indexed();
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE rand() < 2.0 AND toInteger(b.id) = 150 RETURN b.id, length(p)"
    );
    // `toInteger(b.id) = 150` is deterministic and lists; the `rand()`
    // conjunct is left to the residual.
    assert_eq!(parity(&g, &q).arms, vec![AnchorArm::Pair]);
    let only_rand = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE rand() < 2.0 RETURN b.id, length(p)"
    );
    assert_eq!(parity(&g, &only_rand).arms, vec![AnchorArm::FanForward]);
}

/// A conjunct that raises on some candidate (division by zero at node 150)
/// declines the listing: the answer, row or typed error, is the walk's,
/// since the fan evaluates it on the nodes it reaches.
#[test]
fn a_raising_conjunct_keeps_the_walks_behavior() {
    let g = indexed();
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE 1 / (toInteger(b.id) - 150) = 1 RETURN b.id, length(p)"
    );
    assert_eq!(parity(&g, &q).arms, vec![AnchorArm::FanForward]);
}

/// A conjunct that reads the input row is not one answer for every row: the
/// listing is skipped, each row keeps the fan.
#[test]
fn a_row_dependent_conjunct_does_not_list() {
    let g = indexed();
    let q = format!(
        "UNWIND [150, 17] AS t MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE toInteger(b.id) + 0 = t RETURN t, b.id, length(p)"
    );
    assert_eq!(
        parity(&g, &q).arms,
        vec![AnchorArm::FanForward, AnchorArm::FanForward]
    );
}

/// The listing is computed once per clause: later rows of the same pattern
/// read the cached side and still answer exactly.
#[test]
fn rows_of_one_clause_share_the_listing() {
    let g = indexed();
    let q = format!(
        "UNWIND ['0', '3', '9'] AS s MATCH p = ANY SHORTEST (a:N {{id: s}}){BODY}(b:N) \
         WHERE toInteger(b.id) + 0 = 150 RETURN s, b.id, length(p)"
    );
    assert_eq!(parity(&g, &q).arms, vec![AnchorArm::Pair; 3]);
}
