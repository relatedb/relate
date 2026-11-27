//! The property-store seam: the default store answers point reads through the
//! trait, and the public point read dispatches into it.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{NodeId, Value};

use super::super::{Graph, PropStore, PublishedRow};

/// A store that carries exactly one key, so a test can tell which attach the
/// graph published.
#[derive(Debug)]
struct OneKey(u32);

impl PropStore for OneKey {
    fn point(&self, _ts: u64, _id: NodeId, _kid: u32) -> Option<Value> {
        None
    }

    fn carries(&self, kid: u32) -> bool {
        kid == self.0
    }

    fn serves_at(&self, _ts: u64) -> bool {
        true
    }

    // A test store holds no versions: publications change nothing it serves.
    fn on_publication(&self, _ts: u64, _rows: &[PublishedRow]) {}
}

#[test]
fn default_store_answers_point_through_the_trait() {
    let graph = Graph::default();
    graph.begin_write().unwrap();
    let ann = graph
        .create_node(
            vec!["P".into()],
            BTreeMap::from([
                ("age".into(), Value::Int(30)),
                ("name".into(), Value::String("ann".into())),
            ]),
        )
        .unwrap();
    let bare = graph
        .create_node(vec!["P".into()], BTreeMap::new())
        .unwrap();
    graph.commit_write();

    let ts = graph.commit_ts();
    let age = graph.interner.get_id("age").unwrap();
    let store: &dyn PropStore = &graph;

    // point, and the public read path that dispatches into it
    assert_eq!(store.point(ts, ann, age), Some(Value::Int(30)));
    assert_eq!(store.point(ts, bare, age), None);
    assert_eq!(graph.node_prop_at(ann, ts, "age"), Some(Value::Int(30)));
    assert_eq!(
        graph.node_prop_id_at(ann, ts, age),
        store.point(ts, ann, age)
    );
}

#[test]
fn a_second_attach_reports_the_drop_and_keeps_the_first_store() {
    let graph = Graph::default();
    assert!(
        graph.attach_prop_store(Arc::new(OneKey(1))),
        "first attach wins"
    );
    assert!(
        !graph.attach_prop_store(Arc::new(OneKey(2))),
        "the second attach reports that its store was dropped"
    );
    let published = graph
        .attached_prop_store()
        .expect("the first store stays published");
    assert!(published.carries(1));
    assert!(!published.carries(2), "the second store was not published");
}
