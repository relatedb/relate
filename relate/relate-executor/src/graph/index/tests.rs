use super::*;

#[test]
fn prop_index_seek_crosses_int_float() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let i1 = g
        .create_node(
            vec!["N".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    let f2 = g
        .create_node(
            vec!["N".into()],
            BTreeMap::from([("x".into(), Value::Float(2.0))]),
        )
        .unwrap();
    g.commit_write();
    // A stored Int(1) is found by a Float(1.0) seek...
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Float(1.0))]),
        )
        .unwrap();
    assert_eq!(ids, vec![i1]);
    // ... and a stored Float(2.0) by an Int(2) seek.
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Int(2))]),
        )
        .unwrap();
    assert_eq!(ids, vec![f2]);
    // The wildcard ("*") bucket normalizes the same way.
    let ids = g
        .lookup_nodes(&[], &BTreeMap::from([("x".into(), Value::Float(1.0))]))
        .unwrap();
    assert_eq!(ids, vec![i1]);
    // No false positives: 1.5 matches neither Int(1) nor Float(2.0).
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Float(1.5))]),
        )
        .unwrap();
    assert!(ids.is_empty());
}

#[test]
fn prop_index_gc_removes_normalized_dead_keys() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let id = g
        .create_node(
            vec!["N".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    g.commit_write();
    g.begin_write().unwrap();
    g.set_property_node(id, "x", Value::Float(2.0));
    g.commit_write();
    g.gc_versions();
    // The old Int(1) bucket no longer points at the node...
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Float(1.0))]),
        )
        .unwrap();
    assert!(ids.is_empty());
    // ... and the new Float(2.0) value is found by an Int(2) seek.
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Int(2))]),
        )
        .unwrap();
    assert_eq!(ids, vec![id]);
}

#[test]
fn prop_index_nan_is_unindexable_and_unfindable() {
    let g = Graph::default();
    g.begin_write().unwrap();
    g.create_node(
        vec!["N".into()],
        BTreeMap::from([("x".into(), Value::Float(f64::NAN))]),
    )
    .unwrap();
    g.create_node(
        vec!["N".into()],
        BTreeMap::from([("x".into(), Value::Int(1))]),
    )
    .unwrap();
    g.commit_write();
    // NaN = NaN is false: the seek must come back empty even though a
    // NaN property is stored.
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Float(f64::NAN))]),
        )
        .unwrap();
    assert!(ids.is_empty());
    // The NaN write did not disturb other buckets.
    let ids = g
        .lookup_nodes(
            &["N".into()],
            &BTreeMap::from([("x".into(), Value::Float(1.0))]),
        )
        .unwrap();
    assert_eq!(ids.len(), 1);
}

/// ADR-0184: `equality_postings_len` counts a lazy pair's bucket in place,
/// equal to `equality_postings(..).len()`, and a later `apply_redo` commit
/// joins the already-built pair through its dirty set.
#[test]
fn equality_postings_len_matches_the_bucket_without_cloning_it() {
    let g = Graph::default();
    let add = |value: Value| {
        g.create_node(vec!["P".into()], BTreeMap::from([("k".into(), value)]))
            .unwrap();
    };
    for _ in 0..1 {
        add(Value::Int(0));
    }
    for _ in 0..5 {
        add(Value::Int(1));
    }
    for _ in 0..50 {
        add(Value::Int(2));
    }
    for (value, size) in [(0, 1), (1, 5), (2, 50)] {
        let value = Value::Int(value);
        assert_eq!(g.equality_postings_len("P", "k", &value), size);
        assert_eq!(
            g.equality_postings_len("P", "k", &value),
            g.equality_postings("P", "k", &value).len(),
            "value {value:?}"
        );
    }
    // A NaN has no bucket on either side.
    assert_eq!(
        g.equality_postings_len("P", "k", &Value::Float(f64::NAN)),
        0
    );
    // A commit after the pair was built adds to the size-5 bucket: the
    // built pair drains its dirty id.
    g.apply_redo(
        vec![RedoOp::create_node(
            g.next_node_id(),
            vec!["P".into()],
            BTreeMap::from([("k".into(), Value::Int(1))]),
        )],
        g.commit_ts() + 1,
    )
    .unwrap();
    let one = Value::Int(1);
    assert_eq!(g.equality_postings_len("P", "k", &one), 6);
    assert_eq!(
        g.equality_postings_len("P", "k", &one),
        g.equality_postings("P", "k", &one).len()
    );
}

/// Plan-time schema reads: the catalog knows labels/rel types from the
/// first write, and a property-key miss is a true miss while labels and
/// types share the interner (ADR-0363 decision 5).
#[test]
fn schema_presence_reads_never_false_positive_an_absent_name() {
    let g = Graph::default();
    assert!(!g.has_label("N"));
    assert!(!g.has_rel_type("T"));
    assert!(!g.has_property_key("k"));

    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["N".into()],
            BTreeMap::from([("k".into(), Value::Int(1))]),
        )
        .unwrap();
    let b = g.create_node(Vec::new(), BTreeMap::new()).unwrap();
    g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    assert!(g.has_label("N"));
    assert!(g.has_rel_type("T"));
    // A key written by CREATE is known; the label/type share the interner,
    // but the one thing the warning needs is the miss direction below.
    assert!(g.has_property_key("k"));
    assert!(!g.has_property_key("never_written"));
    assert!(!g.has_label("Never"));
    assert!(!g.has_rel_type("NEVER"));

    // SET and MERGE-style writes intern their keys too.
    g.begin_write().unwrap();
    g.set_property_node(a, "set_key", Value::Int(2));
    g.merge_properties_node(b, &BTreeMap::from([("merge_key".into(), Value::Int(3))]));
    g.commit_write();
    assert!(g.has_property_key("set_key"));
    assert!(g.has_property_key("merge_key"));
    assert!(!g.has_property_key("never_written"));
}

/// LABS-1116: a chain the GC collected leaves no hops behind — adjacency
/// stops offering a rel whose version map entry is gone, instead of
/// paying a `rels` map miss per ghost hop on every later scan. Asserted
/// through the UNFILTERED adjacency (`degree_out`/`degree_in`): the
/// visibility-filtered walks would pass on the pre-fix tree too, because
/// the per-hop probe already hides a hop whose version is gone.
#[test]
fn gc_prunes_the_adjacency_hops_of_collected_chains() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let rid = g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    g.begin_write().unwrap();
    g.delete_rel(rid);
    g.commit_write();
    // The tombstoned chain is collectable at the unpinned horizon; the
    // hops go with it.
    g.gc_versions();
    assert!(!g.rel_exists(rid));
    assert!(
        g.degree_out(a).is_empty(),
        "the collected rel's out-hop must leave the unfiltered adjacency"
    );
    assert!(
        g.degree_in(b).is_empty(),
        "the collected rel's in-hop must leave the unfiltered adjacency"
    );
    // The empty-chunk contract (F-1-23): the removed last hop drops its
    // chunk, so the chunk enumeration reports no phantom.
    assert!(g.out_chunks(a).is_empty());
}

/// The label-only multi-label lookup answers ascending — the contract
/// `lookup_nodes_at` documents and its consumers (`union_ids`'s
/// ascending merge, `far_probe`'s serve assumption) rely on. The
/// label-index intersection is a `HashSet`; a restructure once let its
/// iteration order leak as the answer (the rust-review round that
/// caught it: row order for `MATCH (n:A:B)` went process-random).
#[test]
fn label_only_multi_label_lookup_answers_ascending() {
    let g = Graph::default();
    g.begin_write().unwrap();
    for i in 0..25 {
        g.create_node(
            vec!["A".into(), "B".into()],
            BTreeMap::from([("i".into(), Value::Int(i))]),
        )
        .unwrap();
    }
    g.commit_write();
    let labels = vec!["A".to_string(), "B".to_string()];
    let ids = g
        .lookup_nodes(&labels, &BTreeMap::new())
        .expect("the label-only lookup always serves");
    assert_eq!(ids.len(), 25);
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "ascending and deduplicated: {ids:?}"
    );
}
