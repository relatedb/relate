//! The local side of the push (ADR-0401 §8): read the source database for
//! the requested scope — a shape `--query` (the pull's own statement-list /
//! file / stdin resolution and graph-shape check), `--labels` /
//! `--types` filters, or the whole store — into one scoped subgraph of
//! nodes and relationships with locally resolved endpoints.

use std::collections::{BTreeMap, HashMap};

use relate::{Connection, Database, Value};
use relate_core::{NodeId, RelId};

use crate::shape::{assert_graph_shape, resolve_shape_query, split_statements};
use crate::LoadError;

/// What the push reads from the local store (ADR-0401's synopsis):
/// exactly one of `--query`, `--labels`, `--types`, or everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushScope {
    /// A Cypher statement, `;`-separated statement list, file path, or
    /// `-` (stdin) — every statement must `RETURN` graph elements (the
    /// pull's own shape check).
    Query(String),
    /// Only nodes carrying one of these labels (and the relationships
    /// between two in-scope nodes).
    Labels(Vec<String>),
    /// Only relationships of these types, with both their endpoints.
    Types(Vec<String>),
    /// Every node and every relationship in the store.
    All,
}

/// One scoped node, its properties as a sorted map.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalNode {
    pub id: NodeId,
    pub labels: Vec<String>,
    pub properties: BTreeMap<String, Value>,
}

/// One scoped relationship; endpoints are node ids in the same
/// [`ScopeGraph`]'s node set or dangling (only a `--query` scope can
/// produce those — every other scope reads its rels with their endpoints).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalRel {
    pub rel_type: String,
    pub start: NodeId,
    pub end: NodeId,
    pub properties: BTreeMap<String, Value>,
}

/// The scoped subgraph. Nodes are unique by id; rels may repeat only if the
/// store itself does.
#[derive(Debug, Clone, Default)]
pub(crate) struct ScopeGraph {
    pub nodes: Vec<LocalNode>,
    pub rels: Vec<LocalRel>,
    /// Rels whose endpoints are not in `nodes` (a `--query` that returned
    /// rels without both endpoints). Counted in the report, never silently
    /// dropped.
    pub dangling_rels: usize,
}

/// Read the scope from the store (ADR-0401 §8).
pub(crate) fn read_scope(db: &Database, scope: &PushScope) -> Result<ScopeGraph, LoadError> {
    let mut conn = db.connect().map_err(|e| LoadError::Engine(e.to_string()))?;
    match scope {
        PushScope::Query(arg) => read_query_scope(&mut conn, arg),
        PushScope::Labels(labels) => read_labels_scope(&mut conn, labels),
        PushScope::Types(types) => read_types_scope(&mut conn, types),
        PushScope::All => read_all_scope(&mut conn),
    }
}

/// The `--query` scope: resolve the argument (inline, file, or stdin) and
/// its statement list, shape-check every statement (the pull's own rule),
/// then walk every result row's cells collecting nodes, rels, and paths.
fn read_query_scope(conn: &mut Connection, arg: &str) -> Result<ScopeGraph, LoadError> {
    let query = resolve_shape_query(arg)?;
    let statements = split_statements(&query)?;
    for statement in &statements {
        assert_graph_shape(statement)?;
    }
    let mut nodes: HashMap<NodeId, LocalNode> = HashMap::new();
    let mut rels: Vec<LocalRel> = Vec::new();
    let mut seen_rels: HashMap<RelId, ()> = HashMap::new();
    for statement in &statements {
        let result = conn
            .query(statement)
            .map_err(|e| LoadError::Engine(e.to_string()))?;
        for row in &result.rows {
            for cell in row {
                walk_value(cell, &mut nodes, &mut rels, &mut seen_rels);
            }
        }
    }
    Ok(finish_graph(nodes, rels))
}

/// The whole-store scope (`PushScope::All`, an explicit arm — never a
/// `Labels(vec![])` sentinel): every node, then every relationship.
fn read_all_scope(conn: &mut Connection) -> Result<ScopeGraph, LoadError> {
    read_node_scope(conn, "MATCH (n) RETURN n")
}

/// The `--labels` scope: one node read for the whole list (an OR'd label
/// predicate), deduplicated by id, then every relationship in the store
/// filtered to the ones between two in-scope nodes. Relationships with an
/// out-of-scope endpoint are counted dangling — the scope stays explicit.
fn read_labels_scope(conn: &mut Connection, labels: &[String]) -> Result<ScopeGraph, LoadError> {
    let predicate = labels
        .iter()
        .map(|label| Ok(format!("n:{}", super::wire::quote_ident(label)?)))
        .collect::<Result<Vec<_>, LoadError>>()?
        .join(" OR ");
    read_node_scope(conn, &format!("MATCH (n) WHERE {predicate} RETURN n"))
}

/// The node-first scopes' one body: the nodes one `RETURN n` read
/// selects (deduplicated by id), then [`read_rels_between_scope_nodes`].
/// The whole-store and `--labels` scopes differ only in that read.
fn read_node_scope(conn: &mut Connection, node_query: &str) -> Result<ScopeGraph, LoadError> {
    let nodes: HashMap<NodeId, LocalNode> = query_nodes(conn, node_query)?
        .into_iter()
        .map(|node| (node.id, node))
        .collect();
    read_rels_between_scope_nodes(conn, nodes)
}

/// The shared tail of the label and whole-store scopes: every
/// relationship, kept when both endpoints are in scope, counted dangling
/// otherwise.
fn read_rels_between_scope_nodes(
    conn: &mut Connection,
    nodes: HashMap<NodeId, LocalNode>,
) -> Result<ScopeGraph, LoadError> {
    let mut rels = Vec::new();
    for rel in query_rels(conn, "MATCH (a)-[r]->(b) RETURN a, r, b")? {
        rels.push(rel.1);
    }
    Ok(finish_graph(nodes, rels))
}

/// The `--types` scope: the rels of those types with both their endpoints
/// (the endpoints are part of the scope — the remote cannot be asked to
/// guess them, ADR-0401 §2).
fn read_types_scope(conn: &mut Connection, types: &[String]) -> Result<ScopeGraph, LoadError> {
    let mut nodes: HashMap<NodeId, LocalNode> = HashMap::new();
    let mut rels = Vec::new();
    for rel_type in types {
        for (start, rel, end) in query_rels(
            conn,
            &format!(
                "MATCH (a)-[r:{}]->(b) RETURN a, r, b",
                super::wire::quote_ident(rel_type)?
            ),
        )? {
            nodes.insert(start.id, start);
            nodes.insert(end.id, end);
            rels.push(rel);
        }
    }
    Ok(finish_graph(nodes, rels))
}

/// Finish a graph from the deduped node map and the raw rel list: nodes
/// sorted by id so the plan (and its report) are deterministic, and rels
/// partitioned into the ones whose endpoints are both in the node set and
/// the dangling rest (counted, never silently dropped).
fn finish_graph(nodes: HashMap<NodeId, LocalNode>, rels: Vec<LocalRel>) -> ScopeGraph {
    let mut nodes: Vec<LocalNode> = nodes.into_values().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let present: std::collections::HashSet<NodeId> = nodes.iter().map(|n| n.id).collect();
    let (rels, dangling): (Vec<LocalRel>, Vec<LocalRel>) = rels
        .into_iter()
        .partition(|rel| present.contains(&rel.start) && present.contains(&rel.end));
    ScopeGraph {
        nodes,
        rels,
        dangling_rels: dangling.len(),
    }
}

/// One element's properties as the sorted map the plan and the rows read.
fn props_of(map: &relate::PropertyMap) -> BTreeMap<String, Value> {
    map.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Read nodes from a `RETURN n`-shaped query; the projection is guaranteed
/// by the queries here, so a non-node cell fails loudly.
fn query_nodes(conn: &mut Connection, cypher: &str) -> Result<Vec<LocalNode>, LoadError> {
    let result = conn
        .query(cypher)
        .map_err(|e| LoadError::Engine(e.to_string()))?;
    let mut out = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        let Value::Node(node) = &row[0] else {
            return Err(LoadError::Engine(format!(
                "`{cypher}` returned a non-node cell"
            )));
        };
        out.push(LocalNode {
            id: node.id,
            labels: node.labels.to_vec(),
            properties: props_of(&node.properties),
        });
    }
    Ok(out)
}

/// Read rels with both endpoints from a `RETURN a, r, b`-shaped query.
fn query_rels(
    conn: &mut Connection,
    cypher: &str,
) -> Result<Vec<(LocalNode, LocalRel, LocalNode)>, LoadError> {
    let result = conn
        .query(cypher)
        .map_err(|e| LoadError::Engine(e.to_string()))?;
    let mut out = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        let (Value::Node(start), Value::Relationship(rel), Value::Node(end)) =
            (&row[0], &row[1], &row[2])
        else {
            return Err(LoadError::Engine(format!(
                "`{cypher}` returned a non-(node, rel, node) row"
            )));
        };
        out.push((
            LocalNode {
                id: start.id,
                labels: start.labels.to_vec(),
                properties: props_of(&start.properties),
            },
            LocalRel {
                rel_type: rel.rel_type.clone(),
                start: rel.start,
                end: rel.end,
                properties: props_of(&rel.properties),
            },
            LocalNode {
                id: end.id,
                labels: end.labels.to_vec(),
                properties: props_of(&end.properties),
            },
        ));
    }
    Ok(out)
}

/// Walk one result cell collecting nodes and rels: elements directly,
/// lists and maps element-wise, paths node-by-node. Other values are
/// ignored — the shape check already guarantees graph returns.
fn walk_value(
    value: &Value,
    nodes: &mut HashMap<NodeId, LocalNode>,
    rels: &mut Vec<LocalRel>,
    seen_rels: &mut HashMap<RelId, ()>,
) {
    match value {
        Value::Node(node) => {
            nodes.entry(node.id).or_insert_with(|| LocalNode {
                id: node.id,
                labels: node.labels.to_vec(),
                properties: props_of(&node.properties),
            });
        }
        Value::Relationship(rel) => {
            // A statement list may return the same rel twice; the engine's
            // own relationship id identifies it for free, so the row set
            // never double-counts.
            if let std::collections::hash_map::Entry::Vacant(entry) = seen_rels.entry(rel.id) {
                entry.insert(());
                rels.push(LocalRel {
                    rel_type: rel.rel_type.clone(),
                    start: rel.start,
                    end: rel.end,
                    properties: props_of(&rel.properties),
                });
            }
        }
        Value::Path(path) => {
            for node in &path.nodes {
                walk_value(&Value::Node(node.clone().into()), nodes, rels, seen_rels);
            }
            for rel in &path.rels {
                walk_value(
                    &Value::Relationship(rel.clone().into()),
                    nodes,
                    rels,
                    seen_rels,
                );
            }
        }
        Value::List(items) => {
            for item in items.iter() {
                walk_value(item, nodes, rels, seen_rels);
            }
        }
        Value::Map(entries) => {
            for entry in entries.values() {
                walk_value(entry, nodes, rels, seen_rels);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate::Database;

    fn movies_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        for statement in [
            "CREATE CONSTRAINT m_title IF NOT EXISTS FOR (m:Movie) REQUIRE m.title IS UNIQUE",
            "CREATE CONSTRAINT p_name IF NOT EXISTS FOR (p:Person) REQUIRE p.name IS UNIQUE",
            "CREATE (:Person {name: 'Tom'})",
            "CREATE (:Person {name: 'Meg'})",
            "CREATE (:Movie {title: 'Sleepless', released: 1993})",
            "CREATE (:Movie {title: 'Youve', released: 1998})",
            "MATCH (p:Person {name: 'Tom'}), (m:Movie {title: 'Sleepless'}) CREATE (p)-[:ACTED_IN {roles: ['Sam']}]->(m)",
            "MATCH (p:Person {name: 'Meg'}), (m:Movie {title: 'Youve'}) CREATE (p)-[:ACTED_IN {roles: ['Annie']}]->(m)",
        ] {
            conn.query(statement).unwrap();
        }
        db
    }

    #[test]
    fn whole_store_reads_every_node_and_rel() {
        let db = movies_db();
        let graph = read_scope(&db, &PushScope::All).unwrap();
        assert_eq!(graph.nodes.len(), 4);
        assert_eq!(graph.rels.len(), 2);
        assert_eq!(graph.dangling_rels, 0);
        assert_eq!(graph.rels[0].rel_type, "ACTED_IN");
    }

    #[test]
    fn labels_scope_reads_induced_rels_only() {
        let db = movies_db();
        let graph = read_scope(&db, &PushScope::Labels(vec!["Movie".into()])).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        // Every rel has a Person endpoint: out of scope, counted dangling.
        assert_eq!(graph.rels.len(), 0);
        assert_eq!(graph.dangling_rels, 2);
    }

    #[test]
    fn types_scope_reads_rels_with_both_endpoints() {
        let db = movies_db();
        let graph = read_scope(&db, &PushScope::Types(vec!["ACTED_IN".into()])).unwrap();
        assert_eq!(graph.rels.len(), 2);
        assert_eq!(graph.nodes.len(), 4);
    }

    /// A `--query` scope collects nodes, rels, and paths from every
    /// statement of a list; a rel returned without its endpoints is
    /// counted dangling, never silently dropped.
    #[test]
    fn query_scope_walks_statements_and_counts_dangling() {
        let db = movies_db();
        let graph = read_scope(
            &db,
            &PushScope::Query(
                "MATCH (m:Movie) RETURN m; MATCH ()-[r:ACTED_IN]->() RETURN r".into(),
            ),
        )
        .unwrap();
        assert_eq!(graph.nodes.len(), 2);
        // The rels' Person endpoints never arrived: counted dangling, kept
        // out of the row set.
        assert_eq!(graph.rels.len(), 0);
        assert_eq!(graph.dangling_rels, 2);
        // Endpoints joined explicitly: nothing dangles.
        let joined = read_scope(
            &db,
            &PushScope::Query("MATCH (a)-[r:ACTED_IN]->(b) RETURN a, r, b".into()),
        )
        .unwrap();
        assert_eq!(joined.nodes.len(), 4);
        assert_eq!(joined.rels.len(), 2);
        assert_eq!(joined.dangling_rels, 0);
    }

    /// A `--query` that returns scalars fails the pull's own shape check,
    /// before anything is pushed.
    #[test]
    fn query_scope_rejects_scalar_returns() {
        let db = movies_db();
        let err =
            read_scope(&db, &PushScope::Query("MATCH (n) RETURN count(n)".into())).unwrap_err();
        assert!(matches!(err, LoadError::Shape(_)), "{err}");
    }

    /// Paths and lists of nodes walk element-by-element.
    #[test]
    fn query_scope_walks_paths_and_lists() {
        let db = movies_db();
        let graph = read_scope(
            &db,
            &PushScope::Query(
                "MATCH p = (:Person)-[:ACTED_IN]->(:Movie {title: 'Sleepless'}) RETURN nodes(p), relationships(p)"
                    .into(),
            ),
        )
        .unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.rels.len(), 1);
    }
}
