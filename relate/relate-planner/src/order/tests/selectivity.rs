//! The default selectivity table: an unindexed or function-wrapped
//! predicate narrows its endpoint's estimate as a [`Estimate::Defaulted`]
//! (never a measurement), and a measured level still outranks it.

use std::sync::Arc;

use relate_names::NamedToken;

use super::*;
use crate::stats::{ContainsCount, KeyStats, PatternStats, Provenance, RowEstimate};

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
fn a_wrapped_bound_does_not_pair_with_a_plain_one() {
    // `toLower(p.name) >= 'a'` and `p.name <= 'z'` bound different values,
    // so they are two one-sided terms, not one pair
    // 0.3 and 0.3: 0.3 * 0.3^(1/2) = 0.1643 -> 1 644 rows
    assert_eq!(
        defaulted_rows("MATCH (p:Person) WHERE toLower(p.name) >= 'a' AND p.name <= 'z' RETURN p"),
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

/// A stats stub that answers the per-key statistics (ADR-0696) for
/// `Person.city` / `Person.gender` and nothing else beyond the label rows.
struct KeyStub {
    estimate: Option<(u64, Provenance)>,
    distinct: Option<u64>,
}

impl PatternStats for KeyStub {
    fn label_rows(&self, (label, _): &NamedToken) -> Option<u64> {
        label.with_str(|text| rows_of(&[("Person", 10_000), ("Company", 2_000)], text))
    }
    fn equality_rows(&self, _: &NamedToken, _: &NamedToken, _: &Expr) -> Option<u64> {
        None
    }
    fn contains_rows(&self, _: &NamedToken, _: &NamedToken, _: &Expr) -> Option<ContainsCount> {
        None
    }
    fn rel_type_rows(&self, (rel_type, _): &NamedToken) -> Option<u64> {
        rel_type.with_str(|text| rows_of(&[("WORKS_AT", 500_000)], text))
    }
    fn equality_estimate(
        &self,
        _: &NamedToken,
        property: &NamedToken,
        _: &Expr,
    ) -> Option<RowEstimate> {
        self.estimate
            .filter(|_| property.0 == "city")
            .map(|(rows, provenance)| RowEstimate { rows, provenance })
    }
    fn key_stats(&self, _: &NamedToken, property: &NamedToken) -> Option<Arc<KeyStats>> {
        self.distinct
            .filter(|_| property.0 == "gender")
            .map(|distinct| {
                Arc::new(KeyStats {
                    provenance: Provenance::Sketched,
                    count: 10_000,
                    declined: 0,
                    distinct,
                    min: None,
                    max: None,
                    numeric_count: 0,
                    int_sum: 0,
                    float_sum: 0.0,
                    heavy: Vec::new(),
                })
            })
    }
}

const NO_STATS: KeyStub = KeyStub {
    estimate: None,
    distinct: None,
};

#[test]
fn an_unindexed_equality_prices_from_the_key_statistics() {
    let stats = KeyStub {
        estimate: Some((3, Provenance::Sketched)),
        ..NO_STATS
    };
    assert_eq!(
        estimate_of("MATCH (p:Person) WHERE p.city = 'London' RETURN p", &stats),
        Estimate::Statistic {
            rows: 3,
            scan: 10_000,
            provenance: Provenance::Sketched
        }
    );
    // the folded property map prices the same way
    assert_eq!(
        estimate_of("MATCH (p:Person {city: 'London'}) RETURN p", &stats),
        Estimate::Statistic {
            rows: 3,
            scan: 10_000,
            provenance: Provenance::Sketched
        }
    );
}

#[test]
fn without_key_statistics_the_default_table_answers() {
    assert_eq!(
        estimate_of(
            "MATCH (p:Person) WHERE p.city = 'London' RETURN p",
            &NO_STATS
        ),
        Estimate::Defaulted {
            rows: 1_000,
            scan: 10_000
        }
    );
}

#[test]
fn a_function_wrapped_equality_prices_from_the_raw_distinct_count() {
    let stats = KeyStub {
        distinct: Some(2),
        ..NO_STATS
    };
    assert_eq!(
        estimate_of(
            "MATCH (p:Person) WHERE toLower(p.gender) = toLower($g) RETURN p",
            &stats
        ),
        Estimate::Statistic {
            rows: 5_000,
            scan: 10_000,
            provenance: Provenance::Sketched
        }
    );
}

#[test]
fn the_least_trustworthy_term_names_the_basis() {
    // one statistic-priced equality and one defaulted range: the endpoint
    // is a default, not a statistic
    let stats = KeyStub {
        estimate: Some((3, Provenance::UpperBound)),
        ..NO_STATS
    };
    match estimate_of(
        "MATCH (p:Person) WHERE p.city = 'x' AND p.age > 3 RETURN p",
        &stats,
    ) {
        Estimate::Defaulted { scan, .. } => assert_eq!(scan, 10_000),
        other => panic!("expected the default basis, got {other:?}"),
    }
    // two statistic-priced terms carry the less trustworthy provenance
    let stats = KeyStub {
        estimate: Some((3, Provenance::UpperBound)),
        distinct: Some(2),
    };
    match estimate_of(
        "MATCH (p:Person) WHERE p.city = 'x' AND toLower(p.gender) = 'm' RETURN p",
        &stats,
    ) {
        Estimate::Statistic { provenance, .. } => assert_eq!(provenance, Provenance::Sketched),
        other => panic!("expected a statistic, got {other:?}"),
    }
}

/// The plan the stats pick: which label the walk starts from.
fn seed_label(cypher: &str, stats: &dyn PatternStats) -> String {
    first_label(&patterns_of(cypher, stats)[0])
}

#[test]
fn a_skewed_key_flips_the_plan_the_default_table_would_pick() {
    const Q: &str = "MATCH (p:Person)-[:WORKS_AT]->(c:Company) WHERE p.city = 'London' RETURN p";
    // no statistics: the table keeps a tenth of the Persons, so the walk
    // from the filtered Person end (a tenth of the 500 000 WORKS_AT
    // edges) undercuts the unfiltered Company end (all of them)
    assert_eq!(seed_label(Q, &NO_STATS), "Person");
    // the key's statistics say the value is every Person's: the filter
    // removes nothing, both ends walk all the edges, and the tie goes to
    // the smaller seed scan — the Company end — so the plan flips
    let everyone = KeyStub {
        estimate: Some((10_000, Provenance::Exact)),
        ..NO_STATS
    };
    assert_eq!(seed_label(Q, &everyone), "Company");
}
