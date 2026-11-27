//! Range predicates priced from the key's numeric extremes: a window with
//! a literal end spans a fraction of `min..max` (values assumed uniform), a
//! parameter or a key with no numeric extremes falls to the default table.

use std::sync::Arc;

use relate_core::Value;
use relate_names::NamedToken;

use super::*;
use crate::stats::{ContainsCount, KeyStats, PatternStats, Provenance};

/// A 10 000-row `Person` and a 6 000-row `Company` joined by 60 000 `R`
/// edges; `Person.age` holds `extremes` when set, and no key statistics
/// otherwise.
struct AgeStub {
    extremes: Option<(Value, Value)>,
}

impl PatternStats for AgeStub {
    fn label_rows(&self, (label, _): &NamedToken) -> Option<u64> {
        label.with_str(|text| rows_of(&[("Person", 10_000), ("Company", 6_000)], text))
    }
    fn equality_rows(&self, _: &NamedToken, _: &NamedToken, _: &Expr) -> Option<u64> {
        None
    }
    fn contains_rows(&self, _: &NamedToken, _: &NamedToken, _: &Expr) -> Option<ContainsCount> {
        None
    }
    fn rel_type_rows(&self, (rel_type, _): &NamedToken) -> Option<u64> {
        rel_type.with_str(|text| rows_of(&[("R", 60_000)], text))
    }
    fn key_stats(&self, _: &NamedToken, property: &NamedToken) -> Option<Arc<KeyStats>> {
        let (min, max) = self.extremes.clone().filter(|_| property.0 == "age")?;
        Some(Arc::new(KeyStats {
            provenance: Provenance::Sketched,
            count: 10_000,
            declined: 0,
            distinct: 101,
            min: Some(min),
            max: Some(max),
            numeric_count: 10_000,
            int_sum: 0,
            float_sum: 0.0,
            heavy: Vec::new(),
        }))
    }
}

const AGES: AgeStub = AgeStub {
    extremes: Some((Value::Int(0), Value::Int(100))),
};

/// The estimate's rows and whether it came from the key statistics.
fn priced(cypher: &str, stats: &dyn PatternStats) -> (u128, Basis) {
    match estimate_of(cypher, stats) {
        Estimate::Scanned { rows, basis, .. } => (rows, basis),
        other => panic!("expected a scanned estimate, got {other:?}"),
    }
}

const SKETCHED: Basis = Basis::Statistic(Provenance::Sketched);

#[test]
fn a_one_sided_literal_range_spans_its_share_of_min_to_max() {
    // 0..=100: `age > 90` keeps the top tenth, `age < 25` the bottom quarter
    assert_eq!(
        priced("MATCH (p:Person) WHERE p.age > 90 RETURN p", &AGES),
        (1_000, SKETCHED)
    );
    assert_eq!(
        priced("MATCH (p:Person) WHERE p.age < 25 RETURN p", &AGES),
        (2_500, SKETCHED)
    );
    // a constant written first is the same bound
    assert_eq!(
        priced("MATCH (p:Person) WHERE 90 < p.age RETURN p", &AGES),
        (1_000, SKETCHED)
    );
}

#[test]
fn a_literal_window_spans_the_gap_between_its_bounds() {
    assert_eq!(
        priced(
            "MATCH (p:Person) WHERE p.age >= 30 AND p.age <= 40 RETURN p",
            &AGES
        ),
        (1_000, SKETCHED)
    );
}

#[test]
fn a_bound_outside_the_extremes_clamps() {
    // everything is above -5, and nothing is above 500 (one row at least)
    assert_eq!(
        priced("MATCH (p:Person) WHERE p.age > -5 RETURN p", &AGES),
        (10_000, SKETCHED)
    );
    let (rows, basis) = priced("MATCH (p:Person) WHERE p.age > 500 RETURN p", &AGES);
    assert_eq!((rows, basis), (1, SKETCHED));
}

#[test]
fn a_parameter_or_missing_extremes_fall_to_the_default_table() {
    assert_eq!(
        priced("MATCH (p:Person) WHERE p.age > $min RETURN p", &AGES),
        (3_000, Basis::Default)
    );
    let no_stats = AgeStub { extremes: None };
    assert_eq!(
        priced("MATCH (p:Person) WHERE p.age > 90 RETURN p", &no_stats),
        (3_000, Basis::Default)
    );
    // a key holding strings has no numeric extremes either
    let strings = AgeStub {
        extremes: Some((Value::String("a".into()), Value::String("z".into()))),
    };
    assert_eq!(
        priced("MATCH (p:Person) WHERE p.age > 90 RETURN p", &strings),
        (3_000, Basis::Default)
    );
}

#[test]
fn a_literal_range_flips_the_plan_the_default_table_would_pick() {
    // `age >= 0` removes nothing from a key whose minimum is 0. The default
    // third makes the Person end cheaper than the whole Company label, but
    // the statistic prices it at every Person: both ends walk all the
    // edges, and the smaller Company seed scan wins the tie.
    const Q: &str = "MATCH (p:Person)-[:R]->(c:Company) WHERE p.age >= 0 RETURN p";
    let seed = |stats: &dyn PatternStats| first_label(&patterns_of(Q, stats)[0]);
    assert_eq!(seed(&AgeStub { extremes: None }), "Person");
    assert_eq!(seed(&AGES), "Company");
}

#[test]
fn a_key_holding_an_infinity_has_no_range_to_price_over() {
    // an infinite extreme makes the spread meaningless: the default table
    // answers rather than a NaN fraction
    let infinite = AgeStub {
        extremes: Some((Value::Float(0.0), Value::Float(f64::INFINITY))),
    };
    assert_eq!(
        priced("MATCH (p:Person) WHERE p.age > 90 RETURN p", &infinite),
        (3_000, Basis::Default)
    );
}
