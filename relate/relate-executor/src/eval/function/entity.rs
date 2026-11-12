//! Graph-entity and identity builtins: `id`/`elementId`, `labels`/`type`,
//! `properties`/`keys`, `nodes`/`relationships`, `startNode`/`endNode`, and
//! the uuid family. Node materialization for `startNode`/`endNode` on
//! virtual graph-type nodes lives behind the graph layer
//! (`ExecGraph::node_value`).

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{is_virtual_id, ElementIdRef, Error, PropertyMap, Result, Value};

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
            let parsed = ElementIdRef::parse(id);
            if !parsed.is_some_and(|p| p.has_known_kind() && !p.id.is_empty()) {
                return Err(Error::execution(
                    "ElementIdUnexpectedFormat: Element ID has an unexpected format",
                ));
            }
            let Some(namespace) = parsed.and_then(|p| p.namespace()) else {
                return Err(Error::execution(
                    "ElementIdUnexpectedFormat: No database corresponds to the given element id",
                ));
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
        Some(v @ Value::Node(_)) => {
            properties_value(entity_backing_map(graph, v, "properties()")?.as_ref())
        }
        Some(v @ Value::Relationship(_)) => {
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
        Some(v @ Value::Node(_)) => {
            key_list(entity_backing_map(graph, v, "keys()")?.keys().cloned())
        }
        Some(v @ Value::Relationship(_)) => {
            key_list(entity_backing_map(graph, v, "keys()")?.keys().cloned())
        }
        Some(Value::Map(m)) => key_list(m.keys().cloned()),
        Some(Value::Null) | None => Value::Null,
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
                .contains("No database corresponds to the given element id"),
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
        // CIP-95 frozen scenario [4]: the entity segment is opaque to
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
}
