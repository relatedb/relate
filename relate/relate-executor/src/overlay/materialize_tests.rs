//! The read view's materialize cache: entity-free payloads pass through
//! untouched, entity-holding containers rebuild, and each shared payload
//! is walked once per cache.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{ExecGraph, MaterializeCache};
use crate::graph::Graph;
use relate_core::{Node, Value};
use relate_model::VectorCoords;

#[test]
fn materialize_passes_entity_free_payloads_through() {
    let g = Graph::default();
    let view = ExecGraph::read(&g);
    let ns: Arc<Vec<Value>> = Arc::new((0..100).map(Value::Int).collect());
    let Value::List(out) = view.materialize_cached(
        Value::List(Arc::clone(&ns)),
        &mut MaterializeCache::default(),
    ) else {
        panic!("expected a list");
    };
    assert!(Arc::ptr_eq(&out, &ns));

    let nested: Arc<Vec<Value>> = Arc::new(vec![
        Value::List(Arc::clone(&ns)),
        Value::Map(Arc::new(BTreeMap::from([("x".into(), Value::Int(1))]))),
    ]);
    let Value::List(out) = view.materialize_cached(
        Value::List(Arc::clone(&nested)),
        &mut MaterializeCache::default(),
    ) else {
        panic!("expected a list");
    };
    assert!(Arc::ptr_eq(&out, &nested));

    let m: Arc<BTreeMap<String, Value>> = Arc::new(BTreeMap::from([
        ("ns".into(), Value::List(Arc::clone(&ns))),
        ("name".into(), Value::String("x".into())),
    ]));
    let Value::Map(out) =
        view.materialize_cached(Value::Map(Arc::clone(&m)), &mut MaterializeCache::default())
    else {
        panic!("expected a map");
    };
    assert!(Arc::ptr_eq(&out, &m));

    let values: Arc<[f64]> = Arc::from(vec![1.0, 2.0]);
    let vector = Value::vector(
        relate_core::VectorCoordKind::Float64,
        VectorCoords::from_f64(Arc::clone(&values)),
    );
    let Value::Vector { coords: out, .. } =
        view.materialize_cached(vector, &mut MaterializeCache::default())
    else {
        panic!("expected a vector");
    };
    let VectorCoords::F64(out) = out else {
        panic!("expected the f64 family");
    };
    assert!(Arc::ptr_eq(&out, &values));
}

#[test]
fn materialize_rebuilds_only_entity_holding_containers() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(
            vec!["T".into()],
            BTreeMap::from([("v".into(), Value::Int(2))]),
        )
        .unwrap();
    g.commit_write();
    let stale = Value::from_node(Node {
        id,
        labels: Arc::new(vec!["T".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::from([("v".into(), Value::Int(1))])),
    });
    let ns: Arc<Vec<Value>> = Arc::new(vec![Value::Int(1), Value::Int(2)]);
    let inner: Arc<BTreeMap<String, Value>> = Arc::new(BTreeMap::from([
        ("node".into(), stale.clone()),
        ("ns".into(), Value::List(Arc::clone(&ns))),
    ]));
    let held: Arc<Vec<Value>> = Arc::new(vec![
        Value::List(Arc::clone(&ns)),
        Value::Map(Arc::clone(&inner)),
        stale,
    ]);
    let view = ExecGraph::read(&g);
    let Value::List(out) = view.materialize_cached(
        Value::List(Arc::clone(&held)),
        &mut MaterializeCache::default(),
    ) else {
        panic!("expected a list");
    };
    assert!(!Arc::ptr_eq(&out, &held));
    let assert_fresh = |v: &Value| match v {
        Value::Node(n) => assert_eq!(n.properties.get("v"), Some(&Value::Int(2))),
        other => panic!("expected a node, got {other:?}"),
    };
    let Value::List(first) = &out[0] else {
        panic!("expected a list");
    };
    assert!(Arc::ptr_eq(first, &ns));
    let Value::Map(m) = &out[1] else {
        panic!("expected a map");
    };
    assert!(!Arc::ptr_eq(m, &inner));
    assert_fresh(&m["node"]);
    let Value::List(inner_ns) = &m["ns"] else {
        panic!("expected a list");
    };
    assert!(Arc::ptr_eq(inner_ns, &ns));
    assert_fresh(&out[2]);
}

#[test]
fn materialize_walks_each_shared_payload_once_per_cache() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(
            vec!["T".into()],
            BTreeMap::from([("v".into(), Value::Int(2))]),
        )
        .unwrap();
    g.commit_write();
    let view = ExecGraph::read(&g);
    let ns: Arc<Vec<Value>> = Arc::new((0..1000).map(Value::Int).collect());
    let m: Arc<BTreeMap<String, Value>> = Arc::new(BTreeMap::from([
        ("ns".into(), Value::List(Arc::clone(&ns))),
        ("name".into(), Value::String("x".into())),
    ]));

    let mut cache = MaterializeCache::default();
    for _ in 0..1000 {
        let Value::List(out) = view.materialize_cached(Value::List(Arc::clone(&ns)), &mut cache)
        else {
            panic!("expected a list");
        };
        assert!(Arc::ptr_eq(&out, &ns));
        let Value::Map(out) = view.materialize_cached(Value::Map(Arc::clone(&m)), &mut cache)
        else {
            panic!("expected a map");
        };
        assert!(Arc::ptr_eq(&out, &m));
    }
    // `ns` on the first row, `m` once; the walk of `m` meets `ns` cached.
    assert_eq!(cache.walked(), 2);

    let stale = Value::from_node(Node {
        id,
        labels: Arc::new(vec!["T".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::from([("v".into(), Value::Int(1))])),
    });
    let held: Arc<Vec<Value>> = Arc::new(vec![stale, Value::List(Arc::clone(&ns))]);
    let mut cache = MaterializeCache::default();
    let outs: Vec<Arc<Vec<Value>>> = (0..1000)
        .map(
            |_| match view.materialize_cached(Value::List(Arc::clone(&held)), &mut cache) {
                Value::List(out) => out,
                other => panic!("expected a list, got {other:?}"),
            },
        )
        .collect();
    // `held` and the `ns` beneath it, each once.
    assert_eq!(cache.walked(), 2);
    assert!(!Arc::ptr_eq(&outs[0], &held));
    match &outs[0][0] {
        Value::Node(n) => assert_eq!(n.properties.get("v"), Some(&Value::Int(2))),
        other => panic!("expected a node, got {other:?}"),
    }
    let Value::List(first_ns) = &outs[0][1] else {
        panic!("expected a list");
    };
    assert!(Arc::ptr_eq(first_ns, &ns));
    for out in &outs[1..] {
        assert!(Arc::ptr_eq(out, &outs[0]), "rows re-walked the payload");
    }

    // Distinct payloads are each walked once; a shared child only once.
    let mut cache = MaterializeCache::default();
    for _ in 0..10 {
        let one_off = Value::List(Arc::new(vec![Value::List(Arc::clone(&ns))]));
        view.materialize_cached(one_off, &mut cache);
    }
    assert_eq!(cache.walked(), 11);
}
