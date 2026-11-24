//! ADR-0610's per-property version walk: the one diff helper that derives
//! per-property events from an element's retained version chains. ADR-0013
//! versions whole rows, so a key's events are never stored — a key whose
//! value differs between consecutive versions of one element (or is absent
//! on one side) has an event at the newer version's `xmin` carrying the
//! value that version installs (`Value::Null` marks a removal, Cypher's
//! null-assignment semantics), joined to that commit's retained
//! transaction record for `lsn` and `metadata`. `provenance()` answers
//! from the walk's newest event, `history()` from its ascending list.

use std::collections::VecDeque;

use relate_core::{NodeId, RelId, Value};
use relate_engine_api::TransactionRecord;

use super::chain::{Chain, VerRef};
use super::id_vec::IdRef;
use super::Graph;

/// One retained per-property event (ADR-0610): the committing transaction's
/// retained record — the join that carries the event's `commitTs`, `lsn`
/// and `metadata` — beside the value that commit installed for the key
/// (`Value::Null` marks a removal event).
pub(crate) struct PropertyEvent {
    pub(crate) record: TransactionRecord,
    pub(crate) value: Value,
}

impl Graph {
    /// The retained per-property events of `key` on the node `id`
    /// (ADR-0610), ascending `commitTs` — the one diff walk both
    /// provenance queries read: `provenance()` answers from the newest
    /// event, `history()` from the whole list or its `maxNum`-bounded
    /// tail (`max_events`, applied on the walk). Empty when the node,
    /// the key or every joining commit is unretained.
    pub(crate) fn node_property_events(
        &self,
        id: NodeId,
        key: &str,
        max_events: Option<usize>,
    ) -> Vec<PropertyEvent> {
        self.property_events(
            key,
            self.nodes.get(&id),
            |kid, live| live.props.get(kid).cloned(),
            max_events,
        )
    }

    /// The retained per-property events of `key` on the relationship `id`
    /// (ADR-0610) — [`Self::node_property_events`]'s twin over the
    /// relationship chains.
    pub(crate) fn rel_property_events(
        &self,
        id: RelId,
        key: &str,
        max_events: Option<usize>,
    ) -> Vec<PropertyEvent> {
        self.property_events(
            key,
            self.rels.get(&id),
            |kid, live| live.payload.get_by_id(key, kid),
            max_events,
        )
    }

    /// The two event walks' one shared half (ADR-0610): the interned key
    /// — `None` when the interner never saw it, which is no element's
    /// property key, so no stored version can carry it — the element's
    /// chain `vers` (`None` when the store does not have the element),
    /// and the walk's tail bound, all resolved once. The entry points
    /// above supply the chain and the value closure, the only half that
    /// differs: a node's compact properties read by key id, a
    /// relationship's payload dispatch by the key's spelling.
    fn property_events<Live>(
        &self,
        key: &str,
        vers: Option<IdRef<'_, Chain<Live>>>,
        value_of: impl Fn(u32, &Live) -> Option<Value>,
        max_events: Option<usize>,
    ) -> Vec<PropertyEvent> {
        let Some(kid) = self.interner.get_id(key) else {
            return Vec::new();
        };
        vers.map(|vers| {
            retained_events(
                vers.iter(),
                |live| value_of(kid, live),
                |commit_ts| self.transaction_record(commit_ts),
                max_events,
            )
        })
        .unwrap_or_default()
    }
}

/// The one diff walk (ADR-0610): the per-property events of one key over
/// an element's version chain, oldest to newest. A version whose value for
/// the key differs from its predecessor's — absent on either side
/// included — is an event carrying the value that version installs; the
/// first version's present key is its birth event. An event exists only
/// where the committing transaction retained a **provenance** record,
/// because the join is what supplies `lsn` and `metadata`: a commit the
/// feature never saw (an unretained, pre-opt-in or off-period one)
/// contributes no event, and a commit of another record type retained a
/// record — `db.transactions` lists it — but its writes belong to that
/// family, not the provenance audit surface, so it joins no event either
/// (`TransactionRecord::is_provenance`, the one filter). The audit
/// surface stays bounded by the last provenance-enabled open.
///
/// `max_events` is `history()`'s optional tail bound: `None` keeps every
/// event, `Some(max)` the last `max` (0 = none). The bound is applied
/// here, on the walk, as a sliding window over the collected events —
/// one event over the bound evicts the oldest — so a bounded call never
/// materializes the full event list (ADR-0555) and the returned tail
/// stays ascending.
fn retained_events<'a, Live: 'a>(
    versions: impl Iterator<Item = VerRef<'a, Live>>,
    value_of: impl Fn(&Live) -> Option<Value>,
    record_of: impl Fn(u64) -> Option<TransactionRecord>,
    max_events: Option<usize>,
) -> Vec<PropertyEvent> {
    // The predecessor starts at the pre-birth sentinel — the key absent,
    // the same state a gap between versions carries — so one comparison
    // decides every version, the birth included: a present key at birth
    // differs from absent exactly as any later set does.
    let mut previous: Option<Value> = None;
    let mut events: VecDeque<PropertyEvent> = VecDeque::new();
    for version in versions {
        let value = value_of(version.live);
        let changed = previous != value;
        if changed {
            if let Some(record) = record_of(version.xmin).filter(|record| record.is_provenance()) {
                events.push_back(PropertyEvent {
                    record,
                    value: value.clone().unwrap_or(Value::Null),
                });
                if max_events.is_some_and(|max| events.len() > max) {
                    events.pop_front();
                }
            }
        }
        previous = value;
    }
    events.into()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_engine_api::{ChangedKey, ChangedKeyKind, RecordType};

    use super::*;

    /// A retained record for the commit at `commit_ts`, seeded through
    /// the one insert the live pipeline itself uses. `changed_keys` is
    /// what a value-provenance-enabled store derives for every property
    /// write, so it is the honest seed here; the family is the typed
    /// default (`RecordType::default`) every pre-typed record carries.
    fn retained(g: &Graph, commit_ts: u64) {
        g.record_transaction(TransactionRecord {
            lsn: 10 * commit_ts,
            commit_ts,
            metadata: None,
            changed_keys: Some(vec![ChangedKey {
                kind: ChangedKeyKind::Node,
                id: 0,
                key: "k".to_string(),
            }]),
            record_type: RecordType::default(),
        });
    }

    /// The walk's events as `(commitTs, lsn, value)` triples, spelled once
    /// for the assertions below.
    fn triples(events: &[PropertyEvent]) -> Vec<(u64, u64, Value)> {
        events
            .iter()
            .map(|event| {
                (
                    event.record.commit_ts,
                    event.record.lsn,
                    event.value.clone(),
                )
            })
            .collect()
    }

    /// A committed write window at the next timestamp: the shape every
    /// fixture below drives its edits through.
    fn write_at(g: &Graph, edit: impl FnOnce(&Graph)) {
        g.begin_write().unwrap();
        edit(g);
        g.commit_write();
    }

    #[test]
    fn a_keys_life_is_its_birth_changes_and_removal() {
        let g = Graph::default();
        // ts 1: the node is born carrying k — the key's birth event.
        let mut birth_props = BTreeMap::new();
        birth_props.insert("k".to_string(), Value::Int(1));
        write_at(&g, |g| {
            g.create_node(vec!["A".into()], birth_props).unwrap();
        });
        // ts 2: k changes; ts 3: an unrelated key changes (k keeps its
        // event); ts 4: k is removed (a null-assignment event); ts 5: a
        // re-set — the walk's newest event.
        write_at(&g, |g| g.set_property_node(0, "k", Value::Int(2)));
        write_at(&g, |g| g.set_property_node(0, "other", Value::Bool(true)));
        write_at(&g, |g| g.set_property_node(0, "k", Value::Null));
        write_at(&g, |g| g.set_property_node(0, "k", Value::Int(3)));
        for commit_ts in 1..=5 {
            retained(&g, commit_ts);
        }
        assert_eq!(
            triples(&g.node_property_events(0, "k", None)),
            [
                (1, 10, Value::Int(1)),
                (2, 20, Value::Int(2)),
                (4, 40, Value::Null),
                (5, 50, Value::Int(3)),
            ]
        );
        // The sibling key's events are its own: born at 3, never touched
        // again, so the walk reports no event for the commits that only
        // edited `k`.
        assert_eq!(
            triples(&g.node_property_events(0, "other", None)),
            [(3, 30, Value::Bool(true))],
        );
        // A key no version ever carried has no event.
        assert!(g.node_property_events(0, "nope", None).is_empty());
        // So does an element the store does not have.
        assert!(g.node_property_events(99, "k", None).is_empty());
    }

    #[test]
    fn commits_without_a_retained_record_join_no_event() {
        let g = Graph::default();
        let mut birth_props = BTreeMap::new();
        birth_props.insert("k".to_string(), Value::Int(1));
        write_at(&g, |g| {
            g.create_node(vec!["A".into()], birth_props).unwrap();
        });
        write_at(&g, |g| g.set_property_node(0, "k", Value::Int(2)));
        // Only the second commit retained: the birth (commit 1) joins no
        // record, so the walk's only event is the change at commit 2.
        retained(&g, 2);
        assert_eq!(
            triples(&g.node_property_events(0, "k", None)),
            [(2, 20, Value::Int(2))]
        );
        // A change no retained commit backs is invisible, and a retained
        // commit that changed nothing for the key is no event either:
        // only diffs whose join exists answer.
        write_at(&g, |g| g.set_property_node(0, "unrelated", Value::Int(9)));
        retained(&g, 3);
        assert_eq!(
            triples(&g.node_property_events(0, "k", None)),
            [(2, 20, Value::Int(2))]
        );
    }

    /// ADR-0610's type filter: a commit of another record type retains
    /// its record — the table holds it — but the walk joins only the
    /// provenance family's, so the other family's property writes report
    /// no event and leave the provenance timeline its own.
    #[test]
    fn another_record_types_commit_joins_no_provenance_event() {
        let g = Graph::default();
        let mut birth_props = BTreeMap::new();
        birth_props.insert("k".to_string(), Value::Int(1));
        write_at(&g, |g| {
            g.create_node(vec!["A".into()], birth_props).unwrap();
        });
        // Commit 2 is another family's: its record carries a type and no
        // derived keys, the shape the typed channel seeds.
        write_at(&g, |g| g.set_property_node(0, "k", Value::Int(2)));
        g.record_transaction(TransactionRecord {
            lsn: 20,
            commit_ts: 2,
            metadata: None,
            changed_keys: None,
            record_type: RecordType::Typed("audit-trail".into()),
        });
        // The table holds both commits' records — the walk only the
        // provenance one (the birth at commit 1).
        retained(&g, 1);
        assert_eq!(
            g.transaction_record(2)
                .expect("the typed record retains")
                .record_type()
                .as_str(),
            "audit-trail"
        );
        assert_eq!(
            triples(&g.node_property_events(0, "k", None)),
            [(1, 10, Value::Int(1))]
        );
        // A provenance commit after it still lands on the timeline —
        // the other family's write is simply not one of its events.
        write_at(&g, |g| g.set_property_node(0, "k", Value::Int(3)));
        retained(&g, 3);
        assert_eq!(
            triples(&g.node_property_events(0, "k", None)),
            [(1, 10, Value::Int(1)), (3, 30, Value::Int(3))]
        );
    }

    #[test]
    fn relationship_chains_walk_the_same_diff() {
        let g = Graph::default();
        write_at(&g, |g| {
            let a = g.create_node(vec![], BTreeMap::new()).unwrap();
            let b = g.create_node(vec![], BTreeMap::new()).unwrap();
            let mut props = BTreeMap::new();
            props.insert("k".to_string(), Value::Int(1));
            g.create_rel(a, b, "R".into(), props).unwrap();
        });
        write_at(&g, |g| g.set_property_rel(0, "k", Value::Int(2)));
        for commit_ts in 1..=2 {
            retained(&g, commit_ts);
        }
        assert_eq!(
            triples(&g.rel_property_events(0, "k", None)),
            [(1, 10, Value::Int(1)), (2, 20, Value::Int(2))]
        );
        // The node side of the same store stays empty: id spaces are
        // separate, and the rel walk never reads node chains.
        assert!(g.node_property_events(0, "k", None).is_empty());
    }

    /// The tail bound (ADR-0610's `maxNum`): a five-event history where
    /// the bound keeps the last events — still ascending, 0 keeping none,
    /// and a bound at or over the count keeping every event, the
    /// two-argument form's answer.
    #[test]
    fn the_tail_bound_keeps_the_last_events_ascending() {
        let g = Graph::default();
        let mut birth_props = BTreeMap::new();
        birth_props.insert("k".to_string(), Value::Int(0));
        write_at(&g, |g| {
            g.create_node(vec!["A".into()], birth_props).unwrap();
        });
        for i in 1..=4 {
            write_at(&g, |g| g.set_property_node(0, "k", Value::Int(i)));
        }
        for commit_ts in 1..=5 {
            retained(&g, commit_ts);
        }
        // The five events: value i installed at commit i + 1 (lsn 10x).
        let whole: Vec<(u64, u64, Value)> = (1..=5u64)
            .zip(0..=4)
            .map(|(ts, value)| (ts, 10 * ts, Value::Int(value)))
            .collect();
        assert_eq!(triples(&g.node_property_events(0, "k", None)), whole);
        // The bounded tails of it: the last one, the last two, every
        // event at the full count — and none at 0.
        assert_eq!(
            triples(&g.node_property_events(0, "k", Some(1))),
            [(5, 50, Value::Int(4))]
        );
        assert_eq!(
            triples(&g.node_property_events(0, "k", Some(2))),
            [(4, 40, Value::Int(3)), (5, 50, Value::Int(4))]
        );
        assert_eq!(triples(&g.node_property_events(0, "k", Some(5))), whole);
        assert_eq!(triples(&g.node_property_events(0, "k", Some(0))), []);
        // A bound over the count never evicts: the window trims only
        // past the bound, so 9 answers the whole list too.
        assert_eq!(triples(&g.node_property_events(0, "k", Some(9))), whole);
    }
}
