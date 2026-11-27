//! Differential tests for the Vortex store against the graph it was built
//! from: the sorted `(key,row)` side-file against the compact records' point
//! reads, and the seam's point read against the same rows.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{NodeId, Value};
use relate_executor::{Graph, PropStore};

use super::*;
use crate::seam::{to_value, Val};

struct Row {
    labels: Vec<&'static str>,
    props: Vec<(&'static str, Val)>,
}

struct Seeded {
    graph: Graph,
    ids: Vec<NodeId>,
    data: Vec<Row>,
}

/// Six nodes: every one carries `N`; ids 1 and 3 also carry `M`. Ids 4 and 6
/// carry no `name` (the nullable-column shape); two `x` values repeat.
fn sample() -> Vec<Row> {
    vec![
        Row {
            labels: vec!["N"],
            props: vec![("x", Val::Int(1)), ("name", Val::Str("a".into()))],
        },
        Row {
            labels: vec!["N", "M"],
            props: vec![("x", Val::Int(2)), ("name", Val::Str("b".into()))],
        },
        Row {
            labels: vec!["N"],
            props: vec![("x", Val::Int(2)), ("name", Val::Str("b".into()))],
        },
        Row {
            labels: vec!["N", "M"],
            props: vec![("x", Val::Int(3))],
        },
        Row {
            labels: vec!["N"],
            props: vec![("x", Val::Int(1)), ("name", Val::Str("a".into()))],
        },
        Row {
            labels: vec!["N"],
            props: vec![("x", Val::Int(2))],
        },
    ]
}

fn seed() -> Seeded {
    let graph = Graph::default();
    let data = sample();
    let mut ids = Vec::new();
    graph.begin_write().unwrap();
    for row in &data {
        let labels = row.labels.iter().map(|l| l.to_string()).collect();
        let mut props: BTreeMap<String, Value> = BTreeMap::new();
        for (key, val) in &row.props {
            let value = to_value(val.clone())
                .unwrap_or_else(|| panic!("sample property {key} does not convert to a Value"));
            props.insert(key.to_string(), value);
        }
        assert_eq!(
            props.len(),
            row.props.len(),
            "the seed lost a property on conversion"
        );
        ids.push(graph.create_node(labels, props).unwrap());
    }
    graph.commit_write();
    Seeded { graph, ids, data }
}

fn prop(row: &Row, key: &str) -> Val {
    row.props
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or(Val::Null)
}

fn sorted(mut ids: Vec<NodeId>) -> Vec<NodeId> {
    ids.sort_unstable();
    ids
}

fn store_of(seeded: &Seeded) -> VortexStore {
    VortexStore::from_graph(&seeded.graph, seeded.graph.commit_ts()).unwrap()
}

/// The compact records' answer for `(key, value)`: every id whose public
/// point read equals the value.
fn recorded(seeded: &Seeded, key: &str, value: &Value) -> Vec<NodeId> {
    let ts = seeded.graph.commit_ts();
    let mut ids: Vec<NodeId> = seeded
        .ids
        .iter()
        .copied()
        .filter(|id| seeded.graph.node_prop_at(*id, ts, key).as_ref() == Some(value))
        .collect();
    ids.sort_unstable();
    ids
}

/// The sorted `(key,row)` side-file answers exactly what the compact
/// records' point reads answer for the same `(key, value)` on the same graph.
#[test]
fn side_file_matches_the_compact_records() {
    let seeded = seed();
    let store = store_of(&seeded);
    let kx = seeded.graph.interned_id("x").unwrap();
    let kn = seeded.graph.interned_id("name").unwrap();

    for value in [1i64, 2, 3, 4] {
        assert_eq!(
            sorted(store.seek(kx, &Val::Int(value))),
            recorded(&seeded, "x", &Value::Int(value)),
            "x = {value}"
        );
    }
    for value in ["a", "b", "z"] {
        assert_eq!(
            sorted(store.seek(kn, &Val::Str(value.into()))),
            recorded(&seeded, "name", &Value::String(Arc::from(value))),
            "name = {value}"
        );
    }

    // The batched key check is the union of the same single-value answers.
    let mut want: Vec<NodeId> = recorded(&seeded, "x", &Value::Int(1))
        .into_iter()
        .chain(recorded(&seeded, "x", &Value::Int(3)))
        .collect();
    want.sort_unstable();
    assert_eq!(sorted(store.member(kx, &[Val::Int(1), Val::Int(3)])), want);
}

/// Point reads decode ints and strings the same way, whichever chunk the
/// row falls in.
#[test]
fn point_reads_decode_strings_and_ints() {
    let seeded = seed();
    let store = store_of(&seeded);
    let ts = seeded.graph.commit_ts();
    let kx = seeded.graph.interned_id("x").unwrap();
    let kn = seeded.graph.interned_id("name").unwrap();

    for (index, row) in seeded.data.iter().enumerate() {
        let id = seeded.ids[index];
        let want_int = match prop(row, "x") {
            Val::Int(int) => Some(Value::Int(int)),
            _ => None,
        };
        let want_name = match prop(row, "name") {
            Val::Str(name) => Some(Value::String(Arc::from(name))),
            _ => None,
        };
        assert_eq!(PropStore::point(&store, ts, id, kx), want_int);
        assert_eq!(PropStore::point(&store, ts, id, kn), want_name);
    }
}

/// The store lives in the shared [`Graph`] and answers the seam exactly as
/// the compact records did before it was attached.
#[test]
fn store_attaches_to_the_shared_graph() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<VortexStore>();

    let seeded = seed();
    let ts = seeded.graph.commit_ts();
    let kx = seeded.graph.interned_id("x").unwrap();
    let kn = seeded.graph.interned_id("name").unwrap();
    // Captured BEFORE any store exists — the A/B's truth. Afterwards
    // `Graph::point` routes through the attached store, so comparing the
    // graph against that store would compare it with itself.
    let recorded: Vec<_> = seeded
        .ids
        .iter()
        .map(|id| {
            (
                PropStore::point(&seeded.graph, ts, *id, kx),
                PropStore::point(&seeded.graph, ts, *id, kn),
            )
        })
        .collect();
    let store = Arc::new(VortexStore::from_graph(&seeded.graph, ts).unwrap());
    assert!(
        seeded.graph.attach_prop_store(store.clone()),
        "a store is already attached"
    );
    for (index, id) in seeded.ids.iter().enumerate() {
        let want = &recorded[index];
        assert_eq!(
            &(
                PropStore::point(store.as_ref(), ts, *id, kx),
                PropStore::point(store.as_ref(), ts, *id, kn),
            ),
            want,
            "the store answers as the compact records"
        );
        assert_eq!(
            &(
                PropStore::point(&seeded.graph, ts, *id, kx),
                PropStore::point(&seeded.graph, ts, *id, kn),
            ),
            want,
            "the attached graph answers as the compact records"
        );
    }
}

/// A cold column under N racing readers decodes once (fill-once): every
/// thread sees the same values, and the store is `Send + Sync` enough to
/// share as an `Arc` across spawned threads.
#[test]
fn concurrent_cold_point_reads_agree() {
    let seeded = seed();
    let store = Arc::new(VortexStore::from_graph(&seeded.graph, seeded.graph.commit_ts()).unwrap());
    let kx = seeded.graph.interned_id("x").unwrap();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            let ids = seeded.ids.clone();
            std::thread::spawn(move || {
                ids.iter()
                    .map(|id| PropStore::point(store.as_ref(), 0, *id, kx))
                    .collect::<Vec<Option<Value>>>()
            })
        })
        .collect();
    let results: Vec<Vec<Option<Value>>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for other in &results[1..] {
        assert_eq!(&results[0], other);
    }
}
