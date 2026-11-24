//! Graph-entity and identity builtins: `id`/`elementId`, `labels`/`type`,
//! `properties`/`keys`, `nodes`/`relationships`, `startNode`/`endNode`,
//! the uuid family, and ADR-0610's `provenance()` — the per-property
//! provenance read over the retained version chains. Node materialization
//! for `startNode`/`endNode` on virtual graph-type nodes lives behind the
//! graph layer (`ExecGraph::node_value`).

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{is_virtual_id, ElementIdRef, Error, PropertyMap, Result, Value};

use crate::graph::{audit_integer, PropertyEvent};
use crate::overlay::ExecGraph;

use super::super::expr::reject_deleted_access;

/// A property map as the value `properties()` returns: the canonical
/// [`PropertyMap`] → map conversion, shared by the node and relationship
/// arms.
fn properties_value(properties: &PropertyMap) -> Value {
    Value::Map(BTreeMap::from(properties.clone()).into())
}

/// A `Value::List` of property keys, in map order, shared by the node,
/// relationship and map arms of [`bi_keys`].
fn key_list(keys: impl IntoIterator<Item = String>) -> Value {
    Value::List(
        keys.into_iter()
            .map(|key| Value::String(key.into()))
            .collect::<Vec<_>>()
            .into(),
    )
}

/// CIP-257: the property map that backs an entity's reads. A virtual entity
/// — a synthesized value with a negative id and no store entry — carries its
/// own map; every other entity reads the store entry `stored` returns.
/// `None` means the value names an entry the store does not have.
///
/// The one policy behind `properties()` and `keys()`: `keys()` reads the
/// keys of the very map `properties()` returns, so a virtual entity can
/// never report one map's values under another map's keys.
pub(crate) fn backing_properties(
    id: i64,
    own: &Arc<PropertyMap>,
    stored: Option<Arc<PropertyMap>>,
) -> Option<Arc<PropertyMap>> {
    stored.or_else(|| is_virtual_id(id).then(|| Arc::clone(own)))
}

/// [`backing_properties`]' single-key sibling for a property read, which
/// must not materialize the whole map: the own-map fallback applies only to
/// a virtual entity, so a real id reads the store alone.
pub(crate) fn virtual_own_prop(id: i64, own: &PropertyMap, key: &str) -> Option<Value> {
    is_virtual_id(id).then(|| own.get(key).cloned()).flatten()
}

pub(super) fn bi_uuid(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.as_slice() {
            [] => Value::Uuid(generate_uuid_v7()),
            [Value::Null] | [Value::Null, _] | [_, Value::Null] => Value::Null,
            [Value::String(s)] => Value::Uuid(
                relate_core::uuid_parse(s).ok_or_else(|| {
                    Error::execution(
                        "InvalidArgumentValue: The UUID string must consist of 32 hexadecimal digits displayed in 5 groups, separated by 4 hyphens.",
                    )
                })?,
            ),
            [Value::Int(msb), Value::Int(lsb)] => {
                Value::Uuid(((*msb as u128) << 64) | (*lsb as u128 & 0xffff_ffff_ffff_ffff))
            }
            _ => return Err(Error::invalid_argument_type("uuid() takes a STRING or two INTEGERs")),
        })
}
pub(super) fn bi_uuid_mostsignificantbits(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Uuid(u)) => Value::Int((u >> 64) as i64),
        Some(Value::Null) | None => Value::Null,
        _ => {
            return Err(Error::invalid_argument_type(
                "uuid.mostSignificantBits() requires a UUID",
            ))
        }
    })
}
pub(super) fn bi_uuid_leastsignificantbits(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Uuid(u)) => Value::Int(*u as i64),
        Some(Value::Null) | None => Value::Null,
        _ => {
            return Err(Error::invalid_argument_type(
                "uuid.leastSignificantBits() requires a UUID",
            ))
        }
    })
}
pub(super) fn bi_randomuuid(_graph: &ExecGraph<'_>, _n: &str, _vals: Vec<Value>) -> Result<Value> {
    Ok(Value::String(
        relate_core::uuid_hyphenated(generate_uuid_v7()).into(),
    ))
}
pub(super) fn bi_id(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Node(x)) => Value::Int(x.id),
        Some(Value::Relationship(x)) => Value::Int(x.id),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("id() needs a node or relationship")),
    })
}
pub(super) fn bi_elementid(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // The element-id middle segment comes from the graph's namespace
    // (ADR-0116 §7). `system` has no elements and no segment of its own, yet
    // a standalone procedure call on a `system` session can still yield a
    // node or relationship, so `elementId()` can see an element here; a
    // missing segment is a typed error, not a panic and not a silent
    // `neo4j` segment.
    let id = match vals.first() {
        Some(Value::Node(node)) => relate_core::node_element_id(node.id, graph.namespace()),
        Some(Value::Relationship(rel)) => relate_core::rel_element_id(rel.id, graph.namespace()),
        Some(Value::Null) | None => return Ok(Value::Null),
        _ => {
            return Err(Error::invalid_argument_type(
                "elementId() requires an element",
            ))
        }
    };
    Ok(Value::String(
        id.ok_or_else(|| Error::execution("elements of this namespace have no element ids"))?
            .into(),
    ))
}
pub(super) fn bi_db_namefromelementid(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Null) | None => Value::Null,
        Some(Value::String(id)) => {
            let Some(parsed) =
                ElementIdRef::parse(id).filter(|p| p.has_known_kind() && !p.id.is_empty())
            else {
                return Err(Error::execution(format!(
                    "ElementIdUnexpectedFormat: Element ID {id} has an unexpected format."
                )));
            };
            let Some(namespace) = parsed.namespace() else {
                // A well-formed id naming no database is live's
                // `EntityNotFound`, not the malformed-id condition above.
                return Err(Error::execution(format!(
                    "EntityNotFound: Database corresponding to element id not found: {id}"
                )));
            };
            Value::String(namespace.as_str().into())
        }
        _ => return Err(Error::invalid_argument_type("expected STRING")),
    })
}
pub(super) fn bi_labels(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(v @ Value::Node(x)) => {
            reject_deleted_access(graph, v)?;
            Value::List(
                x.labels
                    .iter()
                    .map(|l| Value::String(l.as_str().into()))
                    .collect::<Vec<_>>()
                    .into(),
            )
        }
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("InvalidArgumentValue: labels() needs a node")),
    })
}
pub(super) fn bi_type(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Relationship(x)) => Value::String(x.rel_type.as_str().into()),
        Some(Value::Null) | None => Value::Null,
        _ => {
            return Err(Error::ty(
                "InvalidArgumentValue: type() needs a relationship",
            ))
        }
    })
}
pub(super) fn bi_startnode(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Relationship(r)) => graph.node_value(r.start),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("startNode() needs a relationship")),
    })
}
pub(super) fn bi_endnode(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Relationship(r)) => graph.node_value(r.end),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("endNode() needs a relationship")),
    })
}
/// The entity half `properties()` and `keys()` share: reject the
/// deleted binding, then resolve the backing map through the one policy
/// ([`backing_properties`]) over the store read the variant itself
/// selects — the `Value`'s kind decides the table and the error's noun,
/// so no kind string dispatches and a typo'd caller cannot read the
/// wrong table. `properties()`, `keys()` and the `{.*}` map projection
/// all ride it; the builtins differ only in what they do with the map.
pub(in crate::eval) fn entity_backing_map(
    graph: &ExecGraph<'_>,
    v: &Value,
    builtin: &'static str,
) -> Result<Arc<PropertyMap>> {
    reject_deleted_access(graph, v)?;
    let (id, own, stored, entity) = match v {
        Value::Node(n) => (
            n.id,
            &n.properties,
            graph.node(n.id).map(|fresh| fresh.properties),
            "node",
        ),
        Value::Relationship(r) => (
            r.id,
            &r.properties,
            graph.rel(r.id).map(|fresh| fresh.properties),
            "relationship",
        ),
        _ => return Err(Error::ty("needs a node or relationship")),
    };
    backing_properties(id, own, stored)
        .ok_or_else(|| Error::execution(format!("internal: {builtin} {entity} has no store entry")))
}

pub(super) fn bi_properties(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(v @ (Value::Node(_) | Value::Relationship(_))) => {
            properties_value(entity_backing_map(graph, v, "properties()")?.as_ref())
        }
        Some(Value::Map(m)) => Value::Map(m.clone()),
        Some(Value::Null) | None => Value::Null,
        _ => {
            return Err(Error::invalid_argument_type(
                "properties() needs a node, relationship, or map",
            ))
        }
    })
}
pub(super) fn bi_keys(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        // `keys()` reads the keys of the very map `properties()` returns —
        // the one resolver, never a second spelling of the entity read.
        Some(v @ (Value::Node(_) | Value::Relationship(_))) => {
            key_list(entity_backing_map(graph, v, "keys()")?.keys().cloned())
        }
        Some(Value::Map(m)) => key_list(m.keys().cloned()),
        // NULL (and any other non-entity argument) answers NULL: the
        // historical `keys()` contract, kept as the one wildcard.
        _ => Value::Null,
    })
}

pub(super) fn bi_nodes(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Path(p)) => Value::List(
            p.nodes
                .iter()
                .cloned()
                .map(Value::from_node)
                .collect::<Vec<_>>()
                .into(),
        ),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("nodes() needs a path")),
    })
}
pub(super) fn bi_relationships(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::Path(p)) => Value::List(
            p.rels
                .iter()
                .cloned()
                .map(Value::from_relationship)
                .collect::<Vec<_>>()
                .into(),
        ),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("relationships() needs a path")),
    })
}

/// The ADR-0610 event map of one retained per-property event:
/// `{commitTs, lsn, metadata, value}` — `metadata` is the commit's own
/// map (an empty `MAP` when it carried none) and `value` the value that
/// commit installed (`Null` marks a removal). Spelled once for the
/// function and any later audit surface that renders the same event —
/// the INTEGER cells come from the one shared conversion
/// ([`crate::graph::audit_integer`], the retained table's own).
pub(super) fn provenance_event_map(event: &crate::graph::PropertyEvent) -> Result<Value> {
    let mut map = BTreeMap::new();
    map.insert(
        "commitTs".to_string(),
        audit_integer(event.record.commit_ts, "commitTs")?,
    );
    map.insert("lsn".to_string(), audit_integer(event.record.lsn, "lsn")?);
    map.insert(
        "metadata".to_string(),
        Value::Map(Arc::new(event.record.metadata.clone().unwrap_or_default())),
    );
    map.insert("value".to_string(), event.value.clone());
    Ok(Value::Map(Arc::new(map)))
}

/// The two provenance functions' shared argument walk (ADR-0610): the
/// element and its property key, decoded once — Cypher's null
/// propagation (`None`), the STRING key check spelled ahead of the
/// element's chain read, the deleted-element rejection, and the
/// node-or-relationship chain read from the store's retained chains, not
/// this statement's scratch — so `provenance()` and `history()` cannot
/// drift in what they accept. `history()`'s optional third argument
/// (its `maxNum`) decodes here too, after the key and in left-to-right
/// argument order: omitted or `null` keeps every retained event, a
/// non-negative `INTEGER` bounds the walk's tail to the last `maxNum`
/// events (0 = the empty list), a non-negative bound past this
/// platform's `usize` width (wasm32's 2^32) is the same typed rejection
/// — a truncating cast would silently answer an empty list — and any
/// other value is the ordinary argument-type rejection.
/// `provenance()`'s exactly-two arity leaves the tail empty, so the
/// decode is a no-op for it.
fn retained_property_events(
    graph: &ExecGraph<'_>,
    vals: Vec<Value>,
    name: &str,
) -> Result<Option<Vec<PropertyEvent>>> {
    let [element, key, tail @ ..] = vals.as_slice() else {
        // The row's arity gate fires before dispatch; both functions'
        // floors are the element and the key.
        unreachable!("{name}() is registered with an at-least-two-arguments arity");
    };
    // Cypher's null propagation: a null element or key answers null.
    if element.is_null() || key.is_null() {
        return Ok(None);
    }
    // The key check is spelled once, before the element's chain read; for
    // a call wrong in both arguments it is the key's rejection that
    // reports, the one ordering a single spelling admits.
    let Value::String(key) = key else {
        return Err(Error::invalid_argument_type(format!(
            "{name}() requires a STRING property key"
        )));
    };
    let max_events = match tail {
        [] | [Value::Null] => None,
        [Value::Int(max)] if *max >= 0 => Some(usize::try_from(*max).map_err(|_| {
            Error::invalid_argument_type(format!("{name}() event count is out of range"))
        })?),
        _ => {
            return Err(Error::invalid_argument_type(format!(
                "{name}() requires a non-negative INTEGER event count"
            )))
        }
    };
    reject_deleted_access(graph, element)?;
    // The events are committed history — the store's retained chains, not
    // this statement's scratch: an uncommitted write has no commit to
    // attribute yet, so a property written earlier in the same statement
    // answers from the chain as it stood at admission. The tail bound
    // rides the walk (`retained_events`' sliding window), so the bounded
    // call never materializes the full event list.
    let events = match element {
        Value::Node(node) => graph.base().node_property_events(node.id, key, max_events),
        Value::Relationship(rel) => graph.base().rel_property_events(rel.id, key, max_events),
        _ => {
            return Err(Error::invalid_argument_type(format!(
                "{name}() requires a node or relationship"
            )))
        }
    };
    Ok(Some(events))
}

/// `provenance(element, key)` (ADR-0610): the newest retained per-property
/// event of `key` on a node or relationship, as the event map
/// [`provenance_event_map`] renders — `NULL` when no retained event
/// exists. The name is gated by the `value-provenance` preview feature at
/// admission (ADR-0497 §4), so the store this runs on opted in and its
/// version chains carry the retained history the graph layer walks.
pub(super) fn bi_provenance(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // provenance() is the walk's newest event; the whole ascending list
    // is history()'s (ADR-0610's one diff helper serves both).
    match retained_property_events(graph, vals, "provenance")? {
        Some(events) => match events.last() {
            Some(newest) => provenance_event_map(newest),
            None => Ok(Value::Null),
        },
        None => Ok(Value::Null),
    }
}

/// `history(element, key [, maxNum])` (ADR-0610): the retained
/// per-property events of `key` on a node or relationship, ascending
/// `commitTs` — each rendered by [`provenance_event_map`] (retained
/// events only, so the oldest need not be the birth). An omitted or
/// `null` third argument (the two-argument form) returns every retained
/// event; a non-negative integer returns the last `maxNum` events (0 =
/// the empty list), the bound applied on the graph walk so the tail is
/// yielded without materializing the whole list; a non-negative bound
/// past the platform's `usize` width (wasm32's 2^32) and any other third
/// argument are the ordinary function argument errors — never a
/// truncating cast that silently answers an empty list. An empty `LIST`
/// when no retained event exists, and `NULL` on Cypher's null
/// propagation. The name is gated by the `value-provenance` preview
/// feature at admission (ADR-0497 §4), like `provenance()`'s.
pub(super) fn bi_history(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    match retained_property_events(graph, vals, "history")? {
        Some(events) => Ok(Value::List(Arc::new(
            events
                .iter()
                .map(provenance_event_map)
                .collect::<Result<Vec<_>>>()?,
        ))),
        None => Ok(Value::Null),
    }
}

/// A fresh 64-bit entropy sample, drawn anew on every call from the
/// workspace `rand` crate (ThreadRng/ChaCha12, OS-seeded). One observed
/// value reveals nothing about the next (CWE-338; parity with Neo4j's
/// SecureRandom-backed `randomUUID`). Round-5 review: use the existing
/// dependency instead of hand-rolled splitmix64 mixing.
fn next_entropy() -> u64 {
    rand::random()
}

/// Generate a UUIDv7-shaped value: 48-bit unix-milliseconds timestamp in
/// the high bits, version nibble 7, RFC-4122 variant bits, and 74 random
/// bits drawn from [`next_entropy`] *per call* -- no thread-local PRNG state
/// survives, so consecutive calls are unrelated. The 12 version-field
/// (`rand_a`) bits and the 62 `rand_b` bits come from two independent
/// entropy samples, so they carry 74 distinct random bits rather than
/// overlapping one u64. The version is an implementation detail, not a
/// guarantee (CIP-246).
fn generate_uuid_v7() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // `next_entropy` samples from the rand crate (ThreadRng/ChaCha12) —
    // already uniform, so mask directly; mixing through xorshift would be
    // a no-op on uniform input (round-6 review).
    let rand_a = (next_entropy() & 0x0fff) as u128;
    let rand_b = (next_entropy() & 0x3fff_ffff_ffff_ffff) as u128;
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        & 0xffff_ffff_ffff;
    (ms << 80) | (0x7 << 76) | (rand_a << 64) | (0b10 << 62) | rand_b
}

#[cfg(test)]
mod tests {
    use super::super::function;
    use super::*;
    use crate::graph::Graph;
    use crate::row::Row;
    use relate_core::Namespace;
    use relate_ir::Expr;
    use std::collections::BTreeMap;

    #[test]
    fn uuid_v7_sequential_calls_differ() {
        // Same thread, same millisecond: independent `rand::random()`
        // samples per call still yield distinct values.
        let a = generate_uuid_v7();
        let b = generate_uuid_v7();
        assert_ne!(a, b, "two uuid() calls returned the same value");
    }

    #[test]
    fn uuid_v7_concurrent_threads_do_not_collide() {
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};
        // Regression: every thread used to start from the same constant
        // seed, so two threads in the same millisecond emitted duplicate
        // UUIDs. Each thread burns ~500 draws to overlap many milliseconds.
        let seen = Arc::new(Mutex::new(HashSet::new()));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let seen = Arc::clone(&seen);
            handles.push(std::thread::spawn(move || {
                let mut local = Vec::with_capacity(500);
                for _ in 0..500 {
                    local.push(generate_uuid_v7());
                }
                seen.lock().unwrap().extend(local);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1000, "concurrent uuid() calls collided");
    }

    #[test]
    fn uuid_v7_random_bits_do_not_repeat() {
        use std::collections::HashSet;
        // 100 back-to-back calls (all inside a millisecond or two): both the
        // whole UUIDs and their random fields must stay distinct.
        let mut all = HashSet::new();
        let mut low_bits = HashSet::new();
        for _ in 0..100 {
            let u = generate_uuid_v7();
            assert!(all.insert(u), "uuid() repeated a value");
            // Low 60 bits of the 74-bit random field.
            assert!(
                low_bits.insert((u as u64) & 0x0fff_ffff_ffff_ffff),
                "uuid() random field repeated"
            );
        }
    }

    #[test]
    fn entropy_source_does_not_leak_the_old_recurrence() {
        use std::collections::HashSet;
        // Regression for CWE-338: the old generator advanced a single
        // thread-local xorshift64* state, so an observed value plus the fixed
        // step predicted the next one. Every draw is now independent.
        let draws: Vec<u64> = (0..100).map(|_| next_entropy()).collect();
        assert_eq!(
            draws.iter().copied().collect::<HashSet<_>>().len(),
            100,
            "entropy draws collided"
        );
        for w in draws.windows(2) {
            let mut x = w[0];
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            assert_ne!(
                x.wrapping_mul(0x2545F4914F6CDD1D),
                w[1],
                "entropy leaked the old fixed xorshift64* step"
            );
        }
    }

    /// The row shared by the elementId tests — node 7 and relationship 9
    /// under the variables `n` and `r`. The tests differ only in the
    /// graph's namespace and the expected strings.
    fn element_id_row() -> Row {
        use relate_core::{Node, Relationship};

        let mut row = Row::new();
        row.insert(
            "n".to_string(),
            Value::from_node(Node {
                id: 7,
                labels: Arc::new(vec![]),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            }),
        );
        row.insert(
            "r".to_string(),
            Value::from_relationship(Relationship {
                id: 9,
                start: 1,
                end: 2,
                rel_type: "T".to_string(),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            }),
        );
        row
    }

    /// The single-argument function call shared by the elementId tests,
    /// against their fixture's graph/row/params.
    fn element_id_fn(
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        name: &str,
        arg: Expr,
    ) -> Result<Value> {
        function(graph, row, params, name, false, &[arg])
    }

    #[test]
    fn element_id_uses_relate_namespace_and_name_from_element_id_resolves_it() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let row = element_id_row();
        let params = BTreeMap::new();

        // elementId() emits the `4:rl-id:n{id}` / `5:rl-id:r{id}`
        // wire format.
        let node_id = element_id_fn(
            &graph,
            &row,
            &params,
            "elementId",
            Expr::Variable("n".into()),
        )
        .unwrap();
        assert_eq!(node_id, Value::String("4:rl-id:n7".into()));
        let rel_id = element_id_fn(
            &graph,
            &row,
            &params,
            "elementId",
            Expr::Variable("r".into()),
        )
        .unwrap();
        assert_eq!(rel_id, Value::String("5:rl-id:r9".into()));

        // db.nameFromElementId accepts a relate-namespaced id and rejects
        // a wrong-namespace id.
        let name = element_id_fn(
            &graph,
            &row,
            &params,
            "db.nameFromElementId",
            Expr::String("4:rl-id:n7".to_string()),
        )
        .unwrap();
        assert_eq!(name, Value::String("neo4j".into()));
        let err = element_id_fn(
            &graph,
            &row,
            &params,
            "db.nameFromElementId",
            Expr::String("4:memorydb:n7".to_string()),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("Database corresponding to element id not found: 4:memorydb:n7"),
            "{err}"
        );
        // A malformed id stays the unexpected-format condition.
        let err = element_id_fn(
            &graph,
            &row,
            &params,
            "db.nameFromElementId",
            Expr::String("nope".to_string()),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("Element ID nope has an unexpected format."),
            "{err}"
        );
    }

    #[test]
    fn element_id_uses_the_graphs_namespace_segment() {
        let mut g = Graph::default();
        g.set_namespace(Namespace::SystemOntologies);
        let graph = ExecGraph::read(&g);
        let row = element_id_row();
        let params = BTreeMap::new();

        // Nodes and relationships carry the system_ontologies segment, keep
        // the 4/5 kind prefix, and stay exactly three colon-separated parts
        // (ADR-0116 §7).
        let node_id = element_id_fn(
            &graph,
            &row,
            &params,
            "elementId",
            Expr::Variable("n".into()),
        )
        .unwrap();
        assert_eq!(node_id, Value::String("4:rl-onto:n7".into()));
        let rel_id = element_id_fn(
            &graph,
            &row,
            &params,
            "elementId",
            Expr::Variable("r".into()),
        )
        .unwrap();
        assert_eq!(rel_id, Value::String("5:rl-onto:r9".into()));
        for id in [node_id, rel_id] {
            let Value::String(id) = id else {
                unreachable!()
            };
            assert_eq!(id.split(':').count(), 3, "{id}");
        }

        // db.nameFromElementId maps the system_ontologies segment back.
        let name = element_id_fn(
            &graph,
            &row,
            &params,
            "db.nameFromElementId",
            Expr::String("5:rl-onto:r9".to_string()),
        )
        .unwrap();
        assert_eq!(name, Value::String("system_ontologies".into()));
    }

    #[test]
    fn name_from_element_id_does_not_validate_the_third_segment() {
        // The entity segment is opaque to
        // db.nameFromElementId — only shape and namespace are checked.
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let row = Row::new();
        let params = BTreeMap::new();
        let name = element_id_fn(
            &graph,
            &row,
            &params,
            "db.nameFromElementId",
            Expr::String("4:rl-id:999999".to_string()),
        )
        .unwrap();
        assert_eq!(name, Value::String("neo4j".into()));
    }

    /// `history()`'s `maxNum` width conversion (ADR-0610): a non-negative
    /// bound that fits this platform's `usize` converts losslessly —
    /// 2^32, the wasm32 edge below, here answers like any bound over the
    /// event count (an empty list on an element with no chain).
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn historys_maxnum_at_the_wasm32_edge_converts_losslessly() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let element = Value::from_node(relate_core::Node {
            id: 0,
            labels: Arc::new(Vec::new()),
            properties: Arc::new(PropertyMap::default()),
        });
        assert_eq!(
            bi_history(
                &graph,
                "history",
                vec![element, Value::String("k".into()), Value::Int(1 << 32)],
            )
            .unwrap(),
            Value::List(Vec::new().into())
        );
    }

    /// The out-of-range branch exists only where `usize` is narrower
    /// than `i64` (wasm32): 2^32 there is the typed argument rejection —
    /// never a truncating cast that would silently answer an empty list.
    #[cfg(target_pointer_width = "32")]
    #[test]
    fn historys_maxnum_beyond_usize_is_the_typed_error() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let element = Value::from_node(relate_core::Node {
            id: 0,
            labels: Arc::new(Vec::new()),
            properties: Arc::new(PropertyMap::default()),
        });
        let error = bi_history(
            &graph,
            "history",
            vec![element, Value::String("k".into()), Value::Int(1 << 32)],
        )
        .expect_err("a bound past the usize width is rejected");
        let rendered = error.to_string();
        assert!(
            rendered.contains("InvalidArgumentType")
                && rendered.contains("history() event count is out of range"),
            "{rendered}"
        );
    }
}
