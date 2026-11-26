//! The per-key statistics consults on the live-graph stats adapter: an
//! equality estimate with provenance for an unindexed key and the
//! unmeasured answers. The build seam is the `relate` crate's
//! statement-completion pump (`connection/stats_pump.rs`), tested there.

use std::collections::BTreeMap;

use relate_core::Value;
use relate_ir::Expr;
use relate_names::{NamedToken, SmallName, Token};
use relate_planner::{PatternStats, Provenance};

use super::graph_stats::GraphStats;
use super::test_support::exec_autocommit;
use crate::graph::Graph;

fn unresolved(name: &str) -> NamedToken {
    (SmallName::from_name(name), Token::Unresolved)
}

/// 2,000 `:City` nodes: `country` is `'DE'` on 800 of them and otherwise
/// one of 24 other codes (50 each), 25 distinct values; `name` is unique.
fn cities() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    for i in 0..2000i64 {
        let country = if i % 5 < 2 {
            "DE".to_string()
        } else {
            format!("C{}", i % 40)
        };
        g.create_node(
            vec!["City".into()],
            BTreeMap::from([
                ("country".to_string(), Value::String(country.into())),
                (
                    "name".to_string(),
                    Value::String(format!("city-{i}").into()),
                ),
            ]),
        )
        .unwrap();
    }
    g.commit_write();
    g
}

fn estimate(stats: &GraphStats<'_>, key: &str, value: &Expr) -> Option<(u64, Provenance)> {
    stats
        .equality_estimate(&unresolved("City"), &unresolved(key), value)
        .map(|e| (e.rows, e.provenance))
}

/// Consult once (recording the request), serve it, consult again.
fn built<'a>(g: &'a Graph, params: &'a BTreeMap<String, Value>) -> GraphStats<'a> {
    let probe = GraphStats::new(g, params);
    assert_eq!(estimate(&probe, "country", &Expr::Int(0)), None);
    assert_eq!(g.build_pending_key_stats(usize::MAX, || true), 1);
    probe
}

#[test]
fn an_unbuilt_label_is_unmeasured_and_never_blocks() {
    let g = cities();
    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    let value = Expr::String("DE".into());
    assert_eq!(estimate(&stats, "country", &value), None);
    assert_eq!(estimate(&stats, "country", &value), None, "still unbuilt");
    assert!(stats
        .key_stats(&unresolved("City"), &unresolved("country"))
        .is_none());
}

#[test]
fn a_heavy_value_answers_with_its_count_and_others_spread_over_the_distinct_count() {
    let g = cities();
    let params = BTreeMap::new();
    let stats = built(&g, &params);

    let (heavy, provenance) = estimate(&stats, "country", &Expr::String("DE".into())).unwrap();
    assert_eq!(provenance, Provenance::Sketched);
    assert!(
        (800..=1000).contains(&heavy),
        "heavy 'DE' is about 800: {heavy}"
    );

    // A light value is priced at the admitted values spread over the
    // distinct count, 2000 / 25.
    let (other, _) = estimate(&stats, "country", &Expr::String("C2".into())).unwrap();
    assert!(
        (60..=110).contains(&other),
        "a light value spreads to about 80: {other}"
    );

    let key = stats
        .key_stats(&unresolved("City"), &unresolved("country"))
        .unwrap();
    assert_eq!(key.count, 2000);
    assert!(
        (23..=27).contains(&key.distinct),
        "25 distinct: {}",
        key.distinct
    );
}

#[test]
fn operands_resolve_the_equality_rows_way() {
    let g = cities();
    let params = BTreeMap::from([("c".to_string(), Value::String("DE".into()))]);
    let stats = built(&g, &params);

    let literal = estimate(&stats, "country", &Expr::String("DE".into()));
    let parameter = estimate(&stats, "country", &Expr::Parameter("c".into()));
    assert!(literal.is_some());
    assert_eq!(literal, parameter, "a parameter resolves to its value");
    assert_eq!(
        estimate(&stats, "country", &Expr::Variable("x".into())),
        None,
        "a non-literal operand is unmeasured"
    );
    assert_eq!(
        estimate(&stats, "country", &Expr::Parameter("nope".into())),
        None
    );
}

#[test]
fn a_value_the_type_gate_declines_is_unmeasured() {
    let g = cities();
    let params = BTreeMap::new();
    let stats = built(&g, &params);
    assert_eq!(estimate(&stats, "country", &Expr::Null), None);
    assert_eq!(estimate(&stats, "country", &Expr::Float(f64::NAN)), None);
    let long = Expr::String("x".repeat(65));
    assert_eq!(estimate(&stats, "country", &long), None);
}

#[test]
fn an_unknown_label_or_key_is_unmeasured() {
    let g = cities();
    let params = BTreeMap::new();
    let stats = built(&g, &params);
    assert_eq!(estimate(&stats, "nope", &Expr::Int(1)), None);
    assert_eq!(
        stats.equality_estimate(&unresolved("Nope"), &unresolved("country"), &Expr::Int(1)),
        None
    );
}

#[test]
fn an_unindexed_key_with_a_range_index_reports_the_index_count_as_an_upper_bound() {
    let g = cities();
    exec_autocommit(&g, "CREATE INDEX FOR (c:City) ON (c.name)").unwrap();
    let params = BTreeMap::new();
    let stats = GraphStats::new(&g, &params);
    let value = Expr::String("city-7".into());
    let (rows, provenance) = estimate(&stats, "name", &value).unwrap();
    assert_eq!((rows, provenance), (1, Provenance::UpperBound));
}
