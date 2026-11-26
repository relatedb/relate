//! The default selectivity table: an unindexed or function-wrapped
//! predicate narrows its endpoint's estimate as a [`Estimate::Defaulted`]
//! (never a measurement), and a measured level still outranks it.

use super::*;

/// A 10 000-row `Person` label and no other statistic: every predicate
/// below is one no index serves.
const LABELS: Stub = Stub {
    labels: &[("Person", 10_000)],
    ..Stub::EMPTY
};

fn defaulted_rows(cypher: &str) -> u128 {
    match estimate_of(cypher, &LABELS) {
        Estimate::Defaulted { rows, scan } => {
            assert_eq!(scan, 10_000, "the unseekable filter still pays the scan");
            rows
        }
        other => panic!("expected a defaulted estimate, got {other:?}"),
    }
}

#[test]
fn an_unindexed_equality_keeps_a_tenth() {
    assert_eq!(
        defaulted_rows("MATCH (p:Person) WHERE p.city = 'London' RETURN p"),
        1_000
    );
    assert_eq!(
        defaulted_rows("MATCH (p:Person {city: 'London'}) RETURN p"),
        1_000
    );
}

#[test]
fn a_function_wrapped_equality_prices_like_the_plain_one() {
    assert_eq!(
        defaulted_rows("MATCH (p:Person) WHERE toLower(p.gender) = toLower($g) RETURN p"),
        1_000
    );
}

#[test]
fn a_one_sided_range_keeps_a_third_and_a_pair_collapses() {
    assert_eq!(
        defaulted_rows("MATCH (p:Person) WHERE p.age >= 30 RETURN p"),
        3_000
    );
    // both bounds of one property: one RANGE_PAIR term, not 0.3 * 0.3
    assert_eq!(
        defaulted_rows("MATCH (p:Person) WHERE p.age >= 30 AND p.age <= 40 RETURN p"),
        1_000
    );
}

#[test]
fn conjuncts_combine_by_backoff_not_a_product() {
    // 0.1 and 0.3: 0.1 * 0.3^(1/2) = 0.0548 -> 548 rows, where the
    // independent product would claim 300 and the minimum 1 000
    let rows = defaulted_rows("MATCH (p:Person) WHERE p.city = 'x' AND p.age > 3 RETURN p");
    assert_eq!(rows, 548);
}

#[test]
fn a_join_predicate_between_two_properties_is_not_a_filter() {
    assert_eq!(
        estimate_of("MATCH (p:Person) WHERE p.a = p.b RETURN p", &LABELS),
        Estimate::Measured(10_000)
    );
}

#[test]
fn a_predicate_on_another_variable_does_not_narrow_this_node() {
    assert_eq!(
        estimate_of(
            "MATCH (p:Person), (q:Person) WHERE q.age > 3 RETURN p",
            &LABELS
        ),
        Estimate::Measured(10_000)
    );
}

#[test]
fn the_estimate_never_reaches_zero_for_a_nonempty_label() {
    let rows = defaulted_rows(
        "MATCH (p:Person) WHERE p.a = 1 AND p.b = 1 AND p.c = 1 AND p.d = 1 AND p.e = 1 RETURN p",
    );
    assert!(rows >= 1);
}

#[test]
fn an_index_measurement_outranks_the_default() {
    let stats = Stub {
        labels: &[("Person", 10_000)],
        equals: &[("Person", "city", "London", 7)],
        ..Stub::EMPTY
    };
    assert_eq!(
        estimate_of("MATCH (p:Person {city: 'London'}) RETURN p", &stats),
        Estimate::Measured(7)
    );
}

#[test]
fn defaulted_walk_pays_the_scan_floor() {
    let e = Estimate::Defaulted {
        rows: 5,
        scan: 10_000,
    };
    assert_eq!(
        walk_cost(e, std::iter::empty::<Hop<'_>>(), &NoPatternStats),
        10_000
    );
}

#[test]
fn a_constant_written_first_still_pairs_with_the_opposite_bound() {
    // `3 < p.age` is a lower bound written constant-first; with the upper
    // bound it collapses to one pair, not two one-sided thirds
    assert_eq!(
        defaulted_rows("MATCH (p:Person) WHERE 3 < p.age AND p.age < 9 RETURN p"),
        1_000
    );
    // two lower bounds on one property are not a pair
    assert_eq!(
        defaulted_rows("MATCH (p:Person) WHERE p.age > 3 AND p.age >= 5 RETURN p"),
        // 0.3 and 0.3: 0.3 * 0.3^(1/2) = 0.1643 -> 1 644 rows
        1_644
    );
}

#[test]
fn a_property_map_value_that_reads_another_variable_is_not_a_filter() {
    assert_eq!(
        estimate_of(
            "MATCH (q:Person), (p:Person {city: q.city}) RETURN p",
            &LABELS
        ),
        Estimate::Measured(10_000)
    );
}

#[test]
fn a_nondeterministic_zero_argument_call_is_not_a_constant_filter() {
    assert_eq!(
        estimate_of("MATCH (p:Person) WHERE p.score > rand() RETURN p", &LABELS),
        Estimate::Measured(10_000)
    );
}
