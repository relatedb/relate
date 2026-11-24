//! Tests for the row comparison, split by what they pin.

use super::*;
use relate_model::CypherValue;

mod mismatch;
mod normalize;
mod paths;
mod schema;
mod values;

fn row_json(v: Json) -> Vec<Json> {
    vec![serde_json::json!({ "c": v })]
}

/// One relate map value from `(key, value)` entries — the one spelling of
/// the `CypherValue::Map` fixtures the row tests build by hand.
fn cypher_map(entries: impl IntoIterator<Item = (String, CypherValue)>) -> CypherValue {
    CypherValue::Map(entries.into_iter().collect())
}

fn row_cypher(v: CypherValue) -> Vec<CypherValue> {
    vec![cypher_map([("c".to_string(), v)])]
}
/// Several reference rows, one per value — the multi-row form of
/// [`row_json`].
fn rows_json(values: &[Json]) -> Vec<Json> {
    values.iter().cloned().flat_map(row_json).collect()
}
/// Several relate rows, one per name — the multi-row form of
/// [`row_cypher`].
fn rows_cypher(names: &[&str]) -> Vec<CypherValue> {
    names
        .iter()
        .flat_map(|s| row_cypher(CypherValue::Str((*s).into())))
        .collect()
}

/// The strict failure text of a comparison that must come back `Different`;
/// any other relation fails the test.
fn expect_different(left: &[Json], right: &[CypherValue], order: StatementOrder) -> String {
    match compare_rows(left, right, order) {
        Relation::Different(why) => why,
        other => panic!("expected Different, got {other:?}"),
    }
}

/// Rows `{n: 1}, {n: 2}` on the reference side and the same two rows swapped
/// on the relate side: equal as a bag, different positionally.
fn swapped_rows() -> (Vec<Json>, Vec<CypherValue>) {
    let row = |n: i64| cypher_map([("n".to_string(), CypherValue::Int(n))]);
    (
        vec![serde_json::json!({"n": 1}), serde_json::json!({"n": 2})],
        vec![row(2), row(1)],
    )
}
