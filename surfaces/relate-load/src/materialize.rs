use std::collections::{BTreeMap, HashMap};

use relate::{Connection, Database, Namespace, PullKind, Value};

use crate::extract::{Subgraph, SubgraphNode, SubgraphRel};
use crate::{LoadError, LoadStats};

/// Run `f` inside a transaction: uses the connection's current explicit
/// transaction when one is open; otherwise begins one and commits it on
/// success (best-effort rollback on failure). The one owner of the
/// begin/commit/rollback guard for every load path.
pub(crate) fn with_txn<T>(
    conn: &mut Connection,
    f: impl FnOnce(&mut Connection) -> Result<T, LoadError>,
) -> Result<T, LoadError> {
    let own_txn = !conn.in_transaction();
    if own_txn {
        conn.begin().map_err(|e| LoadError::Engine(e.to_string()))?;
    }
    match f(conn) {
        Ok(ok) => {
            if own_txn {
                conn.commit()
                    .map_err(|e| LoadError::Engine(e.to_string()))?;
            }
            Ok(ok)
        }
        Err(e) => {
            if own_txn && conn.in_transaction() {
                let _ = conn.rollback();
            }
            Err(e)
        }
    }
}

/// Upsert `g` into `conn` using `source` as the identity scope (ADR-0080).
///
/// Uses the connection's current explicit transaction when one is open;
/// otherwise begins and commits one transaction for the whole subgraph.
pub fn load_subgraph(
    conn: &mut Connection,
    g: &Subgraph,
    source: &str,
) -> Result<(HashMap<i64, i64>, LoadStats), LoadError> {
    with_txn(conn, |conn| load_subgraph_in_txn(conn, g, source))
}

/// Rows per batched statement (ADR-0155). Bounded so a batch stays a
/// manageable transaction and a failure loses at most this many rows; large
/// enough that the per-statement label/index cost is amortized (a
/// per-element statement is O(N) in the graph, making a load O(N^2)).
pub(crate) const BATCH_ROWS: usize = 1_000;

fn load_subgraph_in_txn(
    conn: &mut Connection,
    g: &Subgraph,
    source: &str,
) -> Result<(HashMap<i64, i64>, LoadStats), LoadError> {
    let mut id_map = HashMap::new();
    let mut stats = LoadStats::default();
    load_nodes(conn, &g.nodes, source, &mut id_map, &mut stats)?;
    load_rels(conn, &g.rels, source, &mut id_map, &mut stats)?;
    Ok((id_map, stats))
}

/// The node phase (ADR-0155): elements already bound to this source are
/// updated in batched reuse statements; new elements are created in batched
/// `UNWIND` statements with dynamic labels, and the returned element ids
/// feed the `pull_bind` mapping the rel phase resolves endpoints through.
pub(crate) fn load_nodes(
    conn: &mut Connection,
    nodes: &[SubgraphNode],
    source: &str,
    id_map: &mut HashMap<i64, i64>,
    stats: &mut LoadStats,
) -> Result<(), LoadError> {
    let mut reuse: Vec<(&SubgraphNode, i64)> = Vec::new();
    let mut create: Vec<&SubgraphNode> = Vec::new();
    for n in nodes {
        match conn
            .pull_lookup(source, PullKind::Node, &n.element_id)
            .map_err(|e| LoadError::Engine(e.to_string()))?
        {
            Some(local) => reuse.push((n, local)),
            None => create.push(n),
        }
    }

    for batch in reuse.chunks(BATCH_ROWS) {
        let rows: Vec<Value> = batch
            .iter()
            .map(|(n, local)| {
                map_value([
                    ("eid", Value::String(node_element_id(*local).into())),
                    ("labels", string_list(&n.labels)),
                    ("props", props_value(&n.properties)),
                ])
            })
            .collect();
        // One applied row per input row: an element id that no longer
        // resolves (the node vanished between pulls) must fail loudly, not
        // silently over-count as reused.
        let r = run_rows_batch(
            conn,
            "UNWIND $rows AS row MATCH (n) WHERE elementId(n) = row.eid \
             SET n = row.props SET n:$(row.labels) RETURN count(*) AS applied",
            rows,
        )?;
        expect_applied(r, batch.len(), "node reuse")?;
        for (n, local) in batch {
            id_map.insert(n.id, *local);
            stats.nodes_reused += 1;
        }
    }

    for batch in create.chunks(BATCH_ROWS) {
        let rows: Vec<Value> = batch
            .iter()
            .map(|n| {
                map_value([
                    ("eid", Value::String(n.element_id.as_str().into())),
                    ("labels", string_list(&n.labels)),
                    ("props", props_value(&n.properties)),
                ])
            })
            .collect();
        let r = run_rows_batch(
            conn,
            "UNWIND $rows AS row CREATE (n:$(row.labels)) SET n = row.props RETURN row.eid AS reid, elementId(n) AS lid",
            rows,
        )?;
        // UNWIND emits one result row per input row, in order, so the
        // returned rows pair up with the batch; the count check is the
        // invariant guard.
        if r.rows.len() != batch.len() {
            return Err(LoadError::Engine(format!(
                "node batch created {} rows, expected {}",
                r.rows.len(),
                batch.len()
            )));
        }
        for (n, row) in batch.iter().zip(&r.rows) {
            let [Value::String(reid), Value::String(lid)] = &row[..] else {
                return Err(LoadError::Engine(
                    "node batch returned an unexpected row shape".into(),
                ));
            };
            let Some(new_id) = relate::parse_node_element_id(lid) else {
                return Err(LoadError::Engine(format!(
                    "node batch returned an unparseable element id: {lid}"
                )));
            };
            // Keyed on the batch's own returned reid, like the rel path: the
            // binding records the element id the engine echoed, not the one
            // the loader assumed.
            conn.pull_bind(
                source.to_string(),
                PullKind::Node,
                String::from(&**reid),
                new_id,
            )
            .map_err(|e| LoadError::Engine(e.to_string()))?;
            id_map.insert(n.id, new_id);
            stats.nodes_new += 1;
        }
    }
    Ok(())
}

pub(crate) fn load_rels(
    conn: &mut Connection,
    rels: &[SubgraphRel],
    source: &str,
    id_map: &mut HashMap<i64, i64>,
    stats: &mut LoadStats,
) -> Result<(), LoadError> {
    // Relationships: endpoints resolve through the id seek (ADR-0155), so
    // the batch is one statement per chunk rather than one per rel.
    let mut rel_reuse: Vec<(&SubgraphRel, i64, i64, i64)> = Vec::new();
    let mut rel_create: Vec<(&SubgraphRel, i64, i64)> = Vec::new();
    for rel in rels {
        // Endpoint retention is the caller's policy (`stream::flush`
        // partitions, `extract::finish_subgraph` filters): an unresolvable
        // endpoint here is an invariant violation, not a silent skip.
        let (Some(&start), Some(&end)) = (id_map.get(&rel.start), id_map.get(&rel.end)) else {
            return Err(LoadError::Engine(format!(
                "rel {} has an endpoint outside the load",
                rel.element_id
            )));
        };
        match conn
            .pull_lookup(source, PullKind::Rel, &rel.element_id)
            .map_err(|e| LoadError::Engine(e.to_string()))?
        {
            Some(local) => rel_reuse.push((rel, local, start, end)),
            None => rel_create.push((rel, start, end)),
        }
    }

    for batch in rel_reuse.chunks(BATCH_ROWS) {
        // The endpoints anchor the walk: `elementId(a)`/`elementId(b)` are
        // node seeks (O(1)), so the rel enumeration is bounded by the
        // endpoint's degree instead of walking the whole graph per row —
        // the same walk-scan `MATCH ()-[r]->()` pays in the residual
        // (LABS-161) is avoided loader-side because the endpoints are known.
        let rows: Vec<Value> = batch
            .iter()
            .map(|(rel, local, start, end)| {
                map_value([
                    ("rid", Value::String(rel_element_id(*local).into())),
                    ("s", Value::String(node_element_id(*start).into())),
                    ("e", Value::String(node_element_id(*end).into())),
                    ("props", props_value(&rel.properties)),
                ])
            })
            .collect();
        let r = run_rows_batch(
            conn,
            "UNWIND $rows AS row MATCH (a)-[r]->(b) \
             WHERE elementId(a) = row.s AND elementId(b) = row.e AND elementId(r) = row.rid \
             SET r = row.props RETURN count(*) AS applied",
            rows,
        )?;
        expect_applied(r, batch.len(), "rel reuse")?;
        stats.rels_reused += batch.len();
    }

    for batch in rel_create.chunks(BATCH_ROWS) {
        let rows: Vec<Value> = batch
            .iter()
            .map(|(rel, start, end)| {
                map_value([
                    ("eid", Value::String(rel.element_id.as_str().into())),
                    ("s", Value::String(node_element_id(*start).into())),
                    ("e", Value::String(node_element_id(*end).into())),
                    ("type", Value::String(rel.rel_type.as_str().into())),
                    ("props", props_value(&rel.properties)),
                ])
            })
            .collect();
        let r = run_rows_batch(
            conn,
            "UNWIND $rows AS row MATCH (a) WHERE elementId(a) = row.s MATCH (b) WHERE elementId(b) = row.e \
             CREATE (a)-[r:$(row.type)]->(b) SET r = row.props RETURN row.eid AS reid, elementId(r) AS lid",
            rows,
        )?;
        if r.rows.len() != batch.len() {
            return Err(LoadError::Engine(format!(
                "rel batch created {} rows, expected {}",
                r.rows.len(),
                batch.len()
            )));
        }
        for row in &r.rows {
            let [Value::String(reid), Value::String(lid)] = &row[..] else {
                return Err(LoadError::Engine(
                    "rel batch returned an unexpected row shape".into(),
                ));
            };
            let Some(local) = relate::parse_rel_element_id(lid) else {
                return Err(LoadError::Engine(format!(
                    "rel batch returned an unparseable element id: {lid}"
                )));
            };
            conn.pull_bind(
                source.to_string(),
                PullKind::Rel,
                String::from(&**reid),
                local,
            )
            .map_err(|e| LoadError::Engine(e.to_string()))?;
            stats.rels_new += 1;
        }
    }
    Ok(())
}

/// A `RETURN count(*) AS applied` result must report exactly `expected`
/// applied rows: a batch that matched fewer elements is a partial failure,
/// not a silent over-count.
fn expect_applied(r: relate::QueryResult, expected: usize, what: &str) -> Result<(), LoadError> {
    let applied = r
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_int)
        .ok_or_else(|| LoadError::Engine(format!("{what} batch returned no applied count")))?;
    if applied != expected as i64 {
        return Err(LoadError::Engine(format!(
            "{what} batch applied {applied} of {expected} rows"
        )));
    }
    Ok(())
}

/// Run one batched UNWIND statement: build the params map, execute, map the
/// error. Every batch pipeline in this module shares this skeleton.
fn run_rows_batch(
    conn: &mut Connection,
    cypher: &str,
    rows: Vec<Value>,
) -> Result<relate::QueryResult, LoadError> {
    let mut params = BTreeMap::new();
    params.insert("rows".into(), Value::List(rows.into()));
    conn.query_with_params(cypher, params)
        .map_err(|e| LoadError::Engine(e.to_string()))
}

/// The `relate` element id of a local node id (`4:rl-id:n<id>`).
/// Generated statements use `elementId()` (Neo4j deprecates `id()`), so the
/// loader converts between the local numeric id and this string form via the
/// engine's canonical constructor/parser — one owner of the wire format.
fn node_element_id(id: i64) -> String {
    neo4j_element_id(relate::node_element_id(id, Namespace::Neo4j))
}

/// The `relate` element id of a local relationship id.
fn rel_element_id(id: i64) -> String {
    neo4j_element_id(relate::rel_element_id(id, Namespace::Neo4j))
}

/// The loader materializes into the default (`neo4j`) namespace, whose
/// element-id segment is a compile-time constant; the constructor's
/// `Option` exists for namespaces without elements, which cannot occur
/// here. The invariant lives in this one place, not in each caller.
fn neo4j_element_id(id: Option<String>) -> String {
    id.expect("the neo4j namespace has an element-id segment")
}

/// A Cypher map value from key/value pairs.
fn map_value<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Map(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>()
            .into(),
    )
}

/// A list value of strings (labels for dynamic labels).
fn string_list(items: &[String]) -> Value {
    Value::List(
        items
            .iter()
            .map(|s| Value::String(s.as_str().into()))
            .collect::<Vec<_>>()
            .into(),
    )
}

/// The extractor's engine-typed property map as one `Value::Map` row value
/// (ADR-0181 item 7): the values were decoded when the element was
/// extracted, so the loader no longer reads JSON.
fn props_value(properties: &BTreeMap<String, Value>) -> Value {
    Value::Map(properties.clone().into())
}

pub fn load_into_database(
    db: &Database,
    g: &Subgraph,
    source: &str,
) -> Result<LoadStats, LoadError> {
    let mut conn = db.connect().map_err(|e| LoadError::Engine(e.to_string()))?;
    let (_, stats) = load_subgraph(&mut conn, g, source)?;
    drop(conn);
    db.checkpoint()
        .map_err(|e| LoadError::Engine(e.to_string()))?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{SubgraphNode, SubgraphRel};
    use crate::typed_value::decode_value;
    use serde_json::{json, Value as Json};

    /// A property map from a typed JSON object, decoded through the same
    /// decoder the extractor uses (ADR-0181): the tests build engine values,
    /// not a second JSON-to-value path.
    fn props(value: Json) -> BTreeMap<String, Value> {
        value
            .as_object()
            .expect("a property object")
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    decode_value(value).unwrap_or_else(|e| panic!("property {key:?}: {e}")),
                )
            })
            .collect()
    }

    #[test]
    fn load_and_checkpoint_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let g = Subgraph {
            nodes: vec![
                SubgraphNode {
                    id: 10,
                    element_id: "4:aaa:10".into(),
                    labels: vec!["Person".into()],
                    properties: props(json!({"name": {"$type": "String", "_value": "Ada"}})),
                },
                SubgraphNode {
                    id: 11,
                    element_id: "4:aaa:11".into(),
                    labels: vec!["Person".into()],
                    properties: props(json!({"name": {"$type": "String", "_value": "Bob"}})),
                },
            ],
            rels: vec![SubgraphRel {
                id: 1,
                element_id: "5:aaa:1".into(),
                start: 10,
                end: 11,
                rel_type: "KNOWS".into(),
                properties: props(json!({})),
            }],
        };
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        let stats = load_into_database(&db, &g, "test://src").unwrap();
        assert_eq!(stats.nodes_new, 2);
        assert_eq!(stats.rels_new, 1);
        assert!(dir.path().join("manifest.json").is_file());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap())
                .unwrap();
        // No pull identity is persisted (ADR-0158).
        assert!(manifest.get("pull_identity_path").is_none());
        assert!(!dir.path().join("pull-identity.json").exists());
        drop(db);
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        let mut conn = db.connect().unwrap();
        let r = conn
            .query("MATCH (n:Person) RETURN n.name ORDER BY n.name")
            .unwrap();
        assert_eq!(r.rows.len(), 2);
        let r2 = conn
            .query("MATCH ()-[rel:KNOWS]->() RETURN count(rel)")
            .unwrap();
        match &r2.rows[0][0] {
            Value::Int(1) => {}
            other => panic!("expected 1 KNOWS, got {other:?}"),
        }
    }

    #[test]
    fn quoted_labels_and_rel_types_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let g = Subgraph {
            nodes: vec![
                SubgraphNode {
                    id: 1,
                    element_id: "4:aaa:1".into(),
                    labels: vec!["Label With Space".into()],
                    properties: props(json!({"n": {"$type": "Integer", "_value": "1"}})),
                },
                SubgraphNode {
                    id: 2,
                    element_id: "4:aaa:2".into(),
                    labels: vec!["日本語".into()],
                    properties: props(json!({"n": {"$type": "Integer", "_value": "2"}})),
                },
            ],
            rels: vec![SubgraphRel {
                id: 1,
                element_id: "5:aaa:1".into(),
                start: 1,
                end: 2,
                rel_type: "KNOWS-WELL".into(),
                properties: props(json!({})),
            }],
        };
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        load_into_database(&db, &g, "test://src").unwrap();
        let mut conn = db.connect().unwrap();
        let r = conn
            .query("MATCH (a:`Label With Space`)-[r:`KNOWS-WELL`]->(b:`日本語`) RETURN count(r)")
            .unwrap();
        match &r.rows[0][0] {
            Value::Int(1) => {}
            other => panic!("expected 1 quoted rel, got {other:?}"),
        }
    }
    #[test]
    fn wire_type_tag_stays_a_property() {
        // A user property literally named `_type` — the tag the Query API v2
        // wire format uses for typed values — must survive batched
        // materialization as a plain property; if the row decoder is ever
        // swapped for a tag-recognizing one, this is the test that catches
        // it on the loader integration path.
        let dir = tempfile::tempdir().unwrap();
        let g = Subgraph {
            nodes: vec![SubgraphNode {
                id: 1,
                element_id: "4:aaa:1".into(),
                labels: vec!["Person".into()],
                properties: props(json!({
                    "_type": {"$type": "String", "_value": "node"},
                    "id": {"$type": "Integer", "_value": "5"},
                    "name": {"$type": "String", "_value": "Ada"},
                })),
            }],
            rels: vec![SubgraphRel {
                id: 1,
                element_id: "5:aaa:1".into(),
                start: 1,
                end: 1,
                rel_type: "KNOWS".into(),
                properties: props(json!({
                    "_type": {"$type": "String", "_value": "point"},
                    "x": {"$type": "Integer", "_value": "1"},
                })),
            }],
        };
        let db = Database::open(dir.path().to_str().unwrap()).unwrap();
        load_into_database(&db, &g, "test://src").unwrap();
        let mut conn = db.connect().unwrap();
        let nodes = conn.query("MATCH (n:Person) RETURN properties(n)").unwrap();
        match &nodes.rows[0][0] {
            Value::Map(m) => {
                assert_eq!(m.get("_type"), Some(&Value::String("node".into())));
                assert_eq!(m.get("id"), Some(&Value::Int(5)));
                assert_eq!(m.get("name"), Some(&Value::String("Ada".into())));
            }
            other => panic!("expected a property map, got {other:?}"),
        }
        let rels = conn
            .query("MATCH ()-[r:KNOWS]->() RETURN properties(r)")
            .unwrap();
        match &rels.rows[0][0] {
            Value::Map(m) => {
                assert_eq!(m.get("_type"), Some(&Value::String("point".into())));
                assert_eq!(m.get("x"), Some(&Value::Int(1)));
            }
            other => panic!("expected a property map, got {other:?}"),
        }
    }
}
