//! ADR-0687: the live mutation seams' changed-token reports. The end-to-end
//! fold into the per-token table lives in the facade (`relate`'s
//! `tests::token_changes`); these pin what the seams themselves report.

use std::collections::{BTreeMap, BTreeSet};

use relate_core::Value;

use super::super::Graph;

/// The store's id for an interned name.
fn token(graph: &Graph, name: &str) -> u32 {
    match relate_names::TokenResolver::resolve(graph, name) {
        relate_names::Token::Id(id) => id.0,
        other => panic!("`{name}` must be interned by now, resolved as {other:?}"),
    }
}

/// Drain the report as (tokens, schema flag).
fn drained(graph: &Graph) -> (BTreeSet<u32>, bool) {
    let changes = graph.take_applied_token_changes();
    (changes.tokens().collect(), changes.schema())
}

/// A create reports its label ids — and nothing for the property keys it
/// installs, whose cardinality the label stamp already covers.
#[test]
fn a_create_reports_only_its_label_ids() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(
        vec!["Person".into()],
        BTreeMap::from([("name".to_string(), Value::String("ada".into()))]),
    )
    .unwrap();
    let (tokens, schema) = drained(&g);
    assert_eq!(tokens, BTreeSet::from([token(&g, "Person")]));
    assert!(!schema);
}

/// The property-set seam reports the interned key and the node's labels; a
/// `+=` map merge is the same seam per key.
#[test]
fn property_sets_report_their_key_and_labels() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    drained(&g);

    g.set_property_node(id, "age", Value::Int(41));
    let (tokens, _) = drained(&g);
    assert_eq!(
        tokens,
        BTreeSet::from([token(&g, "Person"), token(&g, "age")])
    );

    // `SET n += {city: …}` is this seam once per key; the label rides every
    // call, so a second key reports both again.
    g.set_property_node(id, "city", Value::String("Berlin".into()));
    let (tokens, _) = drained(&g);
    assert_eq!(
        tokens,
        BTreeSet::from([token(&g, "Person"), token(&g, "city")])
    );
}

/// ADR-0687's property-edit rule reports the keys an edit touched, never the
/// request: a `SET n.k = null` for a key the node never carried changes
/// nothing, so it interns no name and stamps no key id, and a carried key's
/// removal still reports it. The rel seam follows the same absence policy.
#[test]
fn a_noop_property_removal_interns_nothing_and_reports_no_key() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    drained(&g);

    g.set_property_node(id, "age", Value::Null);
    let (tokens, _) = drained(&g);
    assert_eq!(tokens, BTreeSet::from([token(&g, "Person")]));
    assert!(
        !matches!(
            relate_names::TokenResolver::resolve(&g, "age"),
            relate_names::Token::Id(_)
        ),
        "a no-op removal mints no name"
    );

    // A carried key's removal reports that key beside the label.
    g.set_property_node(id, "city", Value::String("Berlin".into()));
    drained(&g);
    g.set_property_node(id, "city", Value::Null);
    let (tokens, _) = drained(&g);
    assert_eq!(
        tokens,
        BTreeSet::from([token(&g, "Person"), token(&g, "city")])
    );

    // The relationship twin: a removal of a key the rel never carried
    // reports only its type.
    let other = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    let r = g
        .create_rel(id, other, "KNOWS".to_string(), BTreeMap::new())
        .unwrap();
    drained(&g);
    g.set_property_rel(r, "since", Value::Null);
    let (tokens, _) = drained(&g);
    assert_eq!(tokens, BTreeSet::from([token(&g, "KNOWS")]));
    assert!(
        !matches!(
            relate_names::TokenResolver::resolve(&g, "since"),
            relate_names::Token::Id(_)
        ),
        "the rel seam mints no name for a no-op removal"
    );
}

/// The delete seams report the dying entity's tokens: the node's labels and
/// property-key ids out of `delete_node` (ADR-0687 decision 7 — the
/// tombstone is the last place the keys exist) and the relationship's type
/// id and key ids out of `delete_rel`.
#[test]
fn deletes_report_the_dying_labels_and_keys_and_the_rel_type() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["Person".into()],
            BTreeMap::from([("age".to_string(), Value::Int(41))]),
        )
        .unwrap();
    let b = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    let r = g
        .create_rel(
            a,
            b,
            "KNOWS".to_string(),
            BTreeMap::from([("since".to_string(), Value::Int(2020))]),
        )
        .unwrap();
    drained(&g);

    g.delete_rel(r);
    let (tokens, _) = drained(&g);
    assert_eq!(
        tokens,
        BTreeSet::from([token(&g, "KNOWS"), token(&g, "since")]),
        "the dying rel reports its type id and its key ids"
    );

    g.delete_node(a, true).unwrap();
    let (tokens, _) = drained(&g);
    assert_eq!(
        tokens,
        BTreeSet::from([token(&g, "Person"), token(&g, "age")]),
        "the dying node reports its labels and its key ids"
    );
}

/// A schema op reports the wholesale-change flag instead of a footprint.
#[test]
fn schema_ops_report_the_epoch_flag() {
    let g = Graph::default();
    g.apply_redo(
        vec![relate_engine_api::redo::RedoOp::DropIndex {
            name: "absent".into(),
        }],
        1,
    )
    .unwrap();
    let (tokens, schema) = drained(&g);
    assert!(schema, "a schema op reports the flag");
    assert!(tokens.is_empty(), "and no footprint token");
}

/// A Triple-payload relationship's three field names (`id`, `amount`, `ts`)
/// are structural: `from_map` stores the shape and interns nothing, so
/// removing one is a carried value under a name with no id. The seam
/// reports the type alone — never a panic on the resolution — and the
/// structural name mints no id.
#[test]
fn a_triple_payload_removal_reports_its_type_and_never_panics() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    let b = g
        .create_node(vec!["Person".into()], BTreeMap::new())
        .unwrap();
    // Exactly the triple shape: the payload is the structural Triple, its
    // three names never interned.
    let r = g
        .create_rel(
            a,
            b,
            "PAID".to_string(),
            BTreeMap::from([
                ("id".to_string(), Value::String("tx-1".into())),
                ("amount".to_string(), Value::Int(100)),
                ("ts".to_string(), Value::Int(42)),
            ]),
        )
        .unwrap();
    drained(&g);

    // The structural removal: the payload answers its field name without
    // the interner, and the seam resolves rather than panics — the report
    // is the type alone. (The create path interns the three names for its
    // own accounting, so the carried-key half fires here; the
    // never-interned half of the rule is the noop-removal test's.)
    g.set_property_rel(r, "amount", Value::Null);
    let (tokens, _) = drained(&g);
    assert_eq!(tokens, BTreeSet::from([token(&g, "PAID")]));
}
