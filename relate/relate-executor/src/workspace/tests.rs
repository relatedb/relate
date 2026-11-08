use std::sync::Arc;

use relate_core::Value;

use super::*;
use crate::row::RowNames;
use crate::test_support::PeakPoolDouble;
use crate::UsedSet;

#[test]
fn shared_string_adds_no_deep_heap_and_a_unique_string_does() {
    let shared = Value::String(Arc::from("payload-text"));
    let cloned = shared.clone();
    assert_eq!(owned_heap(&shared), 0);
    assert_eq!(owned_heap(&cloned), 0);
    let unique = Value::String(Arc::from("payload-text"));
    assert_eq!(owned_heap(&unique), str_logical_bytes("payload-text"));
    assert_eq!(owned_heap(&Value::Int(7)), 0);
}

#[test]
fn shared_node_wrapper_adds_no_payload() {
    let node = Value::from_node(Node {
        id: 1,
        labels: Arc::new(vec!["Person".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::from([(
            "name".into(),
            Value::String(Arc::from("ada")),
        )])),
    });
    let cloned = node.clone();
    assert_eq!(owned_heap(&node), 0);
    assert_eq!(owned_heap(&cloned), 0);
}

#[test]
fn row_buffer_counts_a_shared_schema_once_and_skips_shared_payload() {
    let names = RowNames::new(&["n".into()]);
    let payload = "x".repeat(80);
    let shared = Value::String(Arc::from(payload.as_str()));
    let row = Row::from_names(&names, vec![shared.clone()]);
    let shared_one = row_buffer_bytes(std::slice::from_ref(&row));
    let unique_one = row_buffer_bytes(&[Row::from_names(
        &names,
        vec![Value::String(Arc::from(payload.as_str()))],
    )]);
    assert_eq!(
        unique_one - shared_one,
        str_logical_bytes(&payload),
        "a unique string is workspace payload; a shared one is not"
    );
    let both = row_buffer_bytes(&[row.clone(), Row::from_names(&names, vec![shared.clone()])]);
    assert_eq!(row_buffer_bytes(&[row.clone(), row.clone()]), both);
    let other_names = RowNames::new(&["n".into()]);
    let different_schema =
        row_buffer_bytes(&[row.clone(), Row::from_names(&other_names, vec![shared])]);
    assert_eq!(different_schema - both, row.schema_bytes());
}

#[test]
fn eight_hundred_inline_rows_exceed_one_token() {
    let names = RowNames::new(&["i".into()]);
    let rows: Vec<Row> = (0..800)
        .map(|i| Row::from_names(&names, vec![Value::Int(i)]))
        .collect();
    assert!(row_buffer_bytes(&rows) > relate_core::tokens::CHARGE_QUANTUM_BYTES);
    assert_eq!(row_buffer_bytes(&rows), row_buffer_bytes(&rows));
}

#[test]
fn distinct_duplicate_does_not_grow_the_set() {
    let mut keys = HashSet::new();
    keys.insert(GroupKey::new(&Value::Int(1)));
    let once = group_key_set_bytes(&keys);
    assert!(keys.insert(GroupKey::new(&Value::Int(2))));
    let two = group_key_set_bytes(&keys);
    assert!(two > once);
    assert!(!keys.insert(GroupKey::new(&Value::Int(2))));
    assert_eq!(group_key_set_bytes(&keys), two);

    let mut rows = HashSet::new();
    let key = vec![GroupKey::new(&Value::Int(1))];
    rows.insert(key.clone());
    let once = distinct_row_set_bytes(&rows);
    rows.insert(key);
    assert_eq!(distinct_row_set_bytes(&rows), once);
}

#[test]
fn group_map_matches_a_second_walk() {
    let names = RowNames::new(&["i".into()]);
    let mut groups = IndexMap::new();
    groups.insert(
        vec![GroupKey::new(&Value::Int(1))],
        vec![Row::from_names(&names, vec![Value::Int(1)])],
    );
    let once = group_map_bytes(&groups);
    groups
        .entry(vec![GroupKey::new(&Value::Int(1))])
        .or_default()
        .push(Row::from_names(&names, vec![Value::Int(2)]));
    assert!(group_map_bytes(&groups) > once);
    assert_eq!(group_map_bytes(&groups), group_map_bytes(&groups));
}

#[test]
fn retain_matches_the_row_walk_and_shrinks() {
    let names = RowNames::new(&["i".into()]);
    let rows: Vec<Row> = (0..4)
        .map(|i| Row::from_names(&names, vec![Value::Int(i)]))
        .collect();
    let walked = row_buffer_bytes(&rows);
    let quantum = relate_core::tokens::CHARGE_QUANTUM_BYTES;
    let pool = Arc::new(PeakPoolDouble::new(100 * quantum));
    assert!(relate_core::tokens::PeakPool::try_grow(
        pool.as_ref(),
        quantum
    ));
    let mut ledger = relate_core::tokens::TokenLedger::new(100 * quantum, quantum, pool.clone());
    let _guard = relate_core::tokens::LedgerGuard::install(&mut ledger);
    let mut charge = relate_core::tokens::WorkspaceCharge::retain(walked).unwrap();
    let (current, peak, held) = relate_core::tokens::workspace_meter().unwrap();
    assert_eq!(current, walked);
    assert_eq!(peak, walked);
    assert_eq!(
        held,
        relate_core::tokens::rounded_logical_bytes(walked).max(quantum)
    );
    charge.resize(0).unwrap();
    let (current, peak, held) = relate_core::tokens::workspace_meter().unwrap();
    assert_eq!(current, 0);
    assert_eq!(peak, walked);
    let after_release = if relate_core::tokens::rounded_logical_bytes(walked) > quantum {
        0
    } else {
        quantum
    };
    assert_eq!(held, after_release);
    assert_eq!(pool.reserved(), after_release);
}

#[test]
fn releasing_an_input_row_keeps_the_vector_reservation() {
    let names = RowNames::new(&["a".into(), "b".into()]);
    let rows: Vec<Row> = (0..17)
        .map(|i| Row::from_names(&names, vec![Value::Int(i), Value::Int(i + 1)]))
        .collect();
    let mut retained = RetainedRows::measure(&rows);
    assert_eq!(retained.bytes(), row_buffer_bytes(&rows));
    let slab = buffer_bytes(rows.len(), size_of::<Row>() as u64);
    for i in 0..rows.len() {
        retained.release_row(&rows[i]);
        let tail = row_buffer_bytes(&rows[i + 1..]);
        let tail_slab = buffer_bytes(rows.len() - i - 1, size_of::<Row>() as u64);
        assert_eq!(retained.bytes(), tail + slab - tail_slab);
    }
    assert_eq!(retained.bytes(), slab);
}

#[test]
fn incremental_meters_match_full_walks() {
    let names = RowNames::new(&["i".into()]);
    let mut bindings = Vec::new();
    let mut binding_meter = BindingMeter::default();
    for i in 0..40 {
        let row = Row::from_names(&names, vec![Value::Int(i)]);
        let mut used = UsedSet::default();
        if i % 2 == 0 {
            used.insert(i);
        }
        binding_meter.push(&row, used.len());
        bindings.push((row, used));
        assert_eq!(binding_meter.bytes, binding_bytes(&bindings));
    }

    let mut groups: IndexMap<Vec<GroupKey>, Vec<Row>> = IndexMap::new();
    let mut group_meter = GroupMapMeter::default();
    // LABS-781: the entry-landing charge the expression-budget running
    // total consumes (`group_entry_insert_delta`) is the meter's own
    // slot-and-key model of the same insert — accumulated here and
    // asserted below so the two formulations cannot drift.
    let mut entry_delta_total = 0;
    for i in 0..30 {
        let row = Row::from_names(&names, vec![Value::Int(i)]);
        let key = vec![GroupKey::new(&Value::Int(i % 3))];
        if let Some(index) = groups.get_index_of(&key) {
            group_meter.add_row(index, &row);
            groups.get_index_mut(index).expect("group").1.push(row);
        } else {
            group_meter.add_group(&key, &row);
            entry_delta_total += group_entry_insert_delta(groups.len(), &key);
            groups.insert(key, vec![row]);
        }
        assert_eq!(group_meter.bytes, group_map_bytes(&groups));
    }
    let rows_bytes: u64 = groups.values().map(|rows| row_buffer_bytes(rows)).sum();
    assert_eq!(entry_delta_total, group_map_bytes(&groups) - rows_bytes);

    let mut empty_groups: IndexMap<Vec<GroupKey>, Vec<Row>> = IndexMap::new();
    let mut empty_meter = GroupMapMeter::default();
    empty_groups.insert(Vec::new(), Vec::new());
    empty_meter.add_empty_group();
    assert_eq!(empty_meter.bytes, group_map_bytes(&empty_groups));

    let mut distinct = HashSet::new();
    let mut distinct_total = 0;
    for i in 0..40 {
        let key = vec![
            GroupKey::new(&Value::Int(i)),
            GroupKey::new(&Value::Int(i / 2)),
        ];
        distinct_total += distinct_row_insert_delta(distinct.len(), &key);
        distinct.insert(key);
        assert_eq!(distinct_total, distinct_row_set_bytes(&distinct));
    }
    let duplicate = vec![GroupKey::new(&Value::Int(0)), GroupKey::new(&Value::Int(0))];
    assert!(!distinct.insert(duplicate));

    let mut keys = HashSet::new();
    let mut key_total = 0;
    let mut words = 0;
    let mut word_total = 0;
    for i in 0..40 {
        let key = GroupKey::new(&Value::Int(i));
        key_total += group_key_insert_delta(keys.len(), &key);
        keys.insert(key);
        assert_eq!(key_total, group_key_set_bytes(&keys));
        word_total += word_insert_delta(words);
        words += 1;
        assert_eq!(word_total, word_set_bytes(words));
    }
}

#[test]
fn scratch_deltas_match_the_created_walk() {
    let mut nodes = HashMap::new();
    let mut rels = HashMap::new();
    let mut created_nodes = HashSet::new();
    let mut created_rels = HashSet::new();
    let mut out_adj: HashMap<NodeId, Vec<RelId>> = HashMap::new();
    let mut in_adj: HashMap<NodeId, Vec<RelId>> = HashMap::new();
    let mut total = 0;
    for i in 0..12 {
        let node = Node {
            id: i,
            labels: Arc::new(vec!["N".into()]),
            properties: relate_core::PropertyMap::share(BTreeMap::from([(
                "i".into(),
                Value::Int(i),
            )])),
        };
        total += created_node_insert_delta(created_nodes.len(), &node);
        nodes.insert(i, node);
        created_nodes.insert(i);
        assert_eq!(
            total,
            scratch_created_bytes(
                &nodes,
                &rels,
                &created_nodes,
                &created_rels,
                &out_adj,
                &in_adj,
                0,
            )
        );
    }
    for i in 0..8 {
        let rel = Relationship {
            id: i,
            start: i,
            end: i + 1,
            rel_type: "R".into(),
            properties: relate_core::PropertyMap::share(BTreeMap::from([(
                "name".into(),
                Value::String(Arc::from("edge")),
            )])),
        };
        total += created_rel_insert_delta(
            created_rels.len(),
            &rel,
            out_adj.get(&rel.start).map(Vec::len),
            in_adj.get(&rel.end).map(Vec::len),
        );
        let start = rel.start;
        let end = rel.end;
        rels.insert(i, rel);
        out_adj.entry(start).or_default().push(i);
        in_adj.entry(end).or_default().push(i);
        created_rels.insert(i);
        assert_eq!(
            total,
            scratch_created_bytes(
                &nodes,
                &rels,
                &created_nodes,
                &created_rels,
                &out_adj,
                &in_adj,
                0,
            )
        );
    }
    let loop_rel = Relationship {
        id: 100,
        start: 0,
        end: 0,
        rel_type: "L".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    total += created_rel_insert_delta(
        created_rels.len(),
        &loop_rel,
        out_adj.get(&loop_rel.start).map(Vec::len),
        in_adj.get(&loop_rel.end).map(Vec::len),
    );
    rels.insert(100, loop_rel);
    out_adj.entry(0).or_default().push(100);
    in_adj.entry(0).or_default().push(100);
    created_rels.insert(100);
    assert_eq!(
        total,
        scratch_created_bytes(
            &nodes,
            &rels,
            &created_nodes,
            &created_rels,
            &out_adj,
            &in_adj,
            0,
        )
    );

    let before_props = created_property_delta(&nodes[&0].properties);
    std::sync::Arc::make_mut(&mut nodes.get_mut(&0).unwrap().properties)
        .insert("extra".into(), Value::String(Arc::from("created-here")));
    let after_props = created_property_delta(&nodes[&0].properties);
    total = total - before_props + after_props;
    assert_eq!(
        total,
        scratch_created_bytes(
            &nodes,
            &rels,
            &created_nodes,
            &created_rels,
            &out_adj,
            &in_adj,
            0,
        )
    );

    let before_labels = created_label_bytes(&nodes[&1].labels);
    std::sync::Arc::make_mut(&mut nodes.get_mut(&1).unwrap().labels).push("M".into());
    let after_labels = created_label_bytes(&nodes[&1].labels);
    total = total - before_labels + after_labels;
    assert_eq!(
        total,
        scratch_created_bytes(
            &nodes,
            &rels,
            &created_nodes,
            &created_rels,
            &out_adj,
            &in_adj,
            0,
        )
    );

    let rel = rels[&7].clone();
    total -= created_rel_remove_delta(created_rels.len() - 1, &rel);
    rels.remove(&7);
    created_rels.remove(&7);
    assert_eq!(
        total,
        scratch_created_bytes(
            &nodes,
            &rels,
            &created_nodes,
            &created_rels,
            &out_adj,
            &in_adj,
            0,
        )
    );

    let node = nodes[&11].clone();
    total -= created_node_remove_delta(created_nodes.len() - 1, &node);
    nodes.remove(&11);
    created_nodes.remove(&11);
    assert_eq!(
        total,
        scratch_created_bytes(
            &nodes,
            &rels,
            &created_nodes,
            &created_rels,
            &out_adj,
            &in_adj,
            0,
        )
    );
}

#[test]
fn map_entries_bytes_charges_one_entry_overhead_per_entry() {
    assert_eq!(map_entries_bytes(0), 0);
    assert_eq!(map_entries_bytes(3), 3 * crate::memory::OVERHEAD_MAP_ENTRY);
}
