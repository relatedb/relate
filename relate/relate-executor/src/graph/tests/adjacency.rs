//! The adjacency-walk suites (LABS-801 split): snapshot neighbor
//! iteration, typed-chunk order and filtering, and the resolved
//! interned-id walk.

use std::collections::BTreeMap;

use relate_core::Value;

use super::super::Graph;

#[test]
fn for_each_neighbor_matches_neighbors_at() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(c, a, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    for dir in [Direction::Outgoing, Direction::Incoming, Direction::Both] {
        let listed = g.neighbors(a, dir);
        let mut walked = Vec::new();
        g.for_each_neighbor_at(a, dir, g.commit_ts(), &[] as &[String], |rid| {
            walked.push(rid)
        });
        assert_eq!(walked, listed, "dir={dir:?}");
    }
}

#[test]
fn typed_adj_walks_newest_ts_first() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let d = g.create_node(vec!["D".into()], BTreeMap::new()).unwrap();
    let old = g
        .create_rel(
            a,
            b,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(1))]),
        )
        .unwrap();
    let newest = g
        .create_rel(
            a,
            c,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(30))]),
        )
        .unwrap();
    let mid = g
        .create_rel(
            a,
            d,
            "T".into(),
            BTreeMap::from([("ts".into(), Value::Int(10))]),
        )
        .unwrap();
    g.commit_write();
    let mut walked = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string()],
        |rid| walked.push(rid),
    );
    assert_eq!(walked, vec![newest, mid, old]);
    g.begin_write().unwrap();
    g.set_property_rel(old, "ts", Value::Int(40));
    g.commit_write();
    walked.clear();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string()],
        |rid| walked.push(rid),
    );
    assert_eq!(walked, vec![old, newest, mid]);
}

#[test]
fn typed_adj_skips_other_types() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let t = g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap();
    g.create_rel(a, c, "OTHER".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    let mut only_t = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string()],
        |rid| only_t.push(rid),
    );
    assert_eq!(only_t, vec![t]);
    let mut dup = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["T".to_string(), "T".to_string()],
        |rid| dup.push(rid),
    );
    assert_eq!(dup, vec![t]);
}

/// LABS-801: the resolved walk (`try_for_each_neighbor_resolved_at`) is the
/// id-keyed core every engine walk rides; these pin its filter semantics —
/// one type, several types, a name the store never interned, a mixed
/// known/unknown list, and the untyped `Any`.
#[test]
fn resolved_typed_walk_filters_by_interned_id() {
    use relate_ir::Direction;

    use crate::store::RelTypes;

    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    let d = g.create_node(vec!["D".into()], BTreeMap::new()).unwrap();
    let r = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let s = g.create_rel(a, c, "S".into(), BTreeMap::new()).unwrap();
    g.create_rel(a, d, "T".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let walk = |types: &RelTypes| {
        let mut out = Vec::new();
        g.try_for_each_neighbor_resolved_at(a, Direction::Outgoing, g.commit_ts(), types, |rid| {
            out.push(rid);
            Ok::<(), ()>(())
        })
        .unwrap();
        out
    };

    // One type.
    assert_eq!(walk(&g.rel_types(&["R"])), vec![r]);
    // Several types: one chunk per type, in the filter's order.
    assert_eq!(walk(&g.rel_types(&["S", "R"])), vec![s, r]);
    // A name the store never interned has no id and matches nothing.
    assert!(walk(&g.rel_types(&["GHOST"])).is_empty());
    // A mixed known/unknown list walks the known type's chunk only.
    assert_eq!(walk(&g.rel_types(&["GHOST", "R"])), vec![r]);
    // The untyped filter walks every chunk.
    assert_eq!(walk(&RelTypes::Any).len(), 3);
}

/// A callback that walks the next node re-enters the same adjacency
/// store. `IdVec` is not re-entrant, so this deadlocks if the hop ids
/// are still borrowed from the store lock. Two hops is the identity
/// loadtest's `MEMBER_OF*1..8` shape.
#[test]
fn neighbor_walk_reenters_the_same_store() {
    use relate_ir::Direction;
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    let bc = g.create_rel(b, c, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let mut second_hop = Vec::new();
    g.for_each_neighbor_at(
        a,
        Direction::Outgoing,
        g.commit_ts(),
        &["R".to_string()],
        |rid| {
            let (start, end) = g.rel_ends_at(rid, g.commit_ts()).unwrap();
            let next = if start == a { end } else { start };
            g.for_each_neighbor_at(
                next,
                Direction::Outgoing,
                g.commit_ts(),
                &["R".to_string()],
                |next_rid| second_hop.push(next_rid),
            );
        },
    );
    assert_eq!(second_hop, vec![bc]);
}
