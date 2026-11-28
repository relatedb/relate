//! The synthetic graph one group of cells shares.
//!
//! Every node carries the label `N` and a global integer `id` (indexed, so
//! the load's endpoint lookups are seeks). Driver rows are `:N:P`, the
//! start nodes `:N:C`, the walked core `:N:V1|V2|V3` (round-robin, so a
//! label alternation keeps about two thirds of the far ends). A driver row
//! reaches its start over `AT`; the walk runs over `E`. Each start also
//! has one `F` edge to a sink, so an untyped walk sees a second type.
//! Generation is deterministic: the same shape builds the same graph.

use std::collections::BTreeMap;

use relate::{Connection, Database, Value};

use super::{Cell, Starts, Topology};

/// What a graph is built from: cells with equal keys share one graph.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct GraphKey {
    pub topology: Topology,
    pub fanout: u32,
    pub starts: Starts,
    /// The tree depth (the hop upper bound); 0 for the dense core, whose
    /// size does not depend on it.
    pub depth: u32,
    /// The driver rows.
    pub persons: usize,
}

impl GraphKey {
    pub fn of(cell: &Cell, persons: usize) -> GraphKey {
        GraphKey {
            persons,
            topology: cell.topology,
            fanout: cell.fanout,
            starts: cell.starts,
            depth: match cell.topology {
                Topology::Tree => cell.hops.bounds().1,
                Topology::Cycles | Topology::Hubs => 0,
            },
        }
    }

    pub fn name(&self) -> String {
        format!(
            "{}.f{}.{}.depth{}.rows{}",
            self.topology.name(),
            self.fanout,
            self.starts.name(),
            self.depth,
            self.persons
        )
    }
}

const BATCH: usize = 5000;
/// The dense core's smallest size: enough nodes for the highest fan-out
/// to pick distinct targets and for walks to revisit nodes.
const MIN_CORE: usize = 64;

/// A deterministic xorshift: the graph must be the same every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

/// The nodes and edges of one graph, ids global.
struct Layout {
    persons: usize,
    starts: usize,
    /// `(id, core label 1..=3)`.
    core: Vec<(i64, u8)>,
    /// `E` edges.
    walk: Vec<(i64, i64)>,
    /// `AT` edges, driver row to start.
    at: Vec<(i64, i64)>,
    /// `F` edges, start to the sink.
    side: Vec<(i64, i64)>,
    sink: i64,
}

fn core_label(id: i64) -> u8 {
    (id % 3) as u8 + 1
}

fn layout(key: GraphKey) -> Layout {
    let persons = key.persons;
    let starts = key.starts.distinct(persons);
    let first_start = persons as i64;
    let mut next = (persons + starts) as i64;
    let mut core = Vec::new();
    let mut walk = Vec::new();
    let f = key.fanout as usize;
    match key.topology {
        Topology::Tree => {
            for c in 0..starts as i64 {
                let mut frontier = vec![first_start + c];
                for _ in 0..key.depth {
                    let mut children = Vec::with_capacity(frontier.len() * f);
                    for &parent in &frontier {
                        for _ in 0..f {
                            core.push((next, core_label(next)));
                            walk.push((parent, next));
                            children.push(next);
                            next += 1;
                        }
                    }
                    frontier = children;
                }
            }
        }
        Topology::Cycles | Topology::Hubs => {
            let size = MIN_CORE.max(2 * f + 1);
            let base = next;
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
            for i in 0..size as i64 {
                core.push((base + i, core_label(base + i)));
            }
            next += size as i64;
            for i in 0..size {
                let mut targets = Vec::with_capacity(f);
                while targets.len() < f {
                    let t = rng.below(size);
                    if t != i && !targets.contains(&t) {
                        targets.push(t);
                    }
                }
                walk.extend(
                    targets
                        .into_iter()
                        .map(|t| (base + i as i64, base + t as i64)),
                );
            }
            for c in 0..starts as i64 {
                let mut targets = Vec::with_capacity(f);
                while targets.len() < f {
                    let t = rng.below(size);
                    if !targets.contains(&t) {
                        targets.push(t);
                    }
                }
                walk.extend(
                    targets
                        .into_iter()
                        .map(|t| (first_start + c, base + t as i64)),
                );
            }
        }
    }
    let at = (0..persons)
        .map(|i| {
            let start = match key.topology {
                Topology::Hubs => {
                    let skew = (i as f64 / persons as f64).powi(3);
                    ((skew * starts as f64) as usize).min(starts - 1)
                }
                Topology::Tree | Topology::Cycles => i % starts,
            };
            (i as i64, first_start + start as i64)
        })
        .collect();
    let sink = next;
    let side = (0..starts as i64)
        .map(|c| (first_start + c, sink))
        .collect();
    Layout {
        persons,
        starts,
        core,
        walk,
        at,
        side,
        sink,
    }
}

fn load(conn: &mut Connection, statement: &str, rows: Vec<Value>) {
    for chunk in rows.chunks(BATCH) {
        let params = BTreeMap::from([("rows".to_string(), Value::List(chunk.to_vec().into()))]);
        conn.query_with_params(statement, params)
            .unwrap_or_else(|e| panic!("load `{statement}`: {e}"));
    }
}

fn ids(range: impl Iterator<Item = i64>) -> Vec<Value> {
    range.map(Value::Int).collect()
}

fn pairs(edges: &[(i64, i64)]) -> Vec<Value> {
    edges
        .iter()
        .map(|&(a, b)| Value::List(vec![Value::Int(a), Value::Int(b)].into()))
        .collect()
}

/// Build the in-memory store for `key`.
pub fn build(key: GraphKey) -> Database {
    let layout = layout(key);
    let db = Database::open(":memory:").expect("open scratch store");
    let mut conn = db.connect().expect("connect loading session");
    conn.query_with_params("CREATE INDEX n_id FOR (n:N) ON (n.id)", BTreeMap::new())
        .expect("create the id index");
    load(
        &mut conn,
        "UNWIND $rows AS r CREATE (:N:P {id: r})",
        ids(0..layout.persons as i64),
    );
    load(
        &mut conn,
        "UNWIND $rows AS r CREATE (:N:C {id: r})",
        ids(layout.persons as i64..(layout.persons + layout.starts) as i64),
    );
    for (label, statement) in [
        (1, "UNWIND $rows AS r CREATE (:N:V1 {id: r})"),
        (2, "UNWIND $rows AS r CREATE (:N:V2 {id: r})"),
        (3, "UNWIND $rows AS r CREATE (:N:V3 {id: r})"),
    ] {
        let of_label = layout
            .core
            .iter()
            .filter(|(_, l)| *l == label)
            .map(|(id, _)| *id);
        load(&mut conn, statement, ids(of_label));
    }
    load(
        &mut conn,
        "UNWIND $rows AS r CREATE (:N:Z {id: r})",
        ids(std::iter::once(layout.sink)),
    );
    load(
        &mut conn,
        "UNWIND $rows AS r MATCH (a:N {id: r[0]}), (b:N {id: r[1]}) CREATE (a)-[:AT]->(b)",
        pairs(&layout.at),
    );
    load(
        &mut conn,
        "UNWIND $rows AS r MATCH (a:N {id: r[0]}), (b:N {id: r[1]}) CREATE (a)-[:E]->(b)",
        pairs(&layout.walk),
    );
    load(
        &mut conn,
        "UNWIND $rows AS r MATCH (a:N {id: r[0]}), (b:N {id: r[1]}) CREATE (a)-[:F]->(b)",
        pairs(&layout.side),
    );
    db
}

/// The graph's node count: a cheap sanity figure for the report.
pub fn node_count(db: &Database) -> i64 {
    let mut conn = db.connect().expect("connect");
    let result = conn
        .query_with_params("MATCH (n:N) RETURN count(n) AS n", BTreeMap::new())
        .expect("count nodes");
    match result.rows.first().and_then(|r| r.first()) {
        Some(Value::Int(n)) => *n,
        other => panic!("node count answered {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::Hops;

    fn key(
        topology: Topology,
        fanout: u32,
        starts: Starts,
        hops: Hops,
        persons: usize,
    ) -> GraphKey {
        GraphKey::of(
            &Cell {
                topology,
                fanout,
                starts,
                hops,
                ..Cell::BASELINE
            },
            persons,
        )
    }

    #[test]
    fn a_tree_has_fanout_children_per_node_per_level() {
        let l = layout(key(Topology::Tree, 3, Starts::Ten, Hops::OneThree, 100));
        // 10 starts x (3 + 9 + 27) core nodes.
        assert_eq!(l.core.len(), 10 * 39);
        assert_eq!(l.walk.len(), 10 * 39);
        assert_eq!(l.starts, 10);
    }

    #[test]
    fn the_dense_core_gives_every_node_and_start_fanout_edges() {
        let l = layout(key(Topology::Cycles, 10, Starts::Ten, Hops::OneThree, 100));
        assert_eq!(l.core.len(), 64);
        assert_eq!(l.walk.len(), 64 * 10 + 10 * 10);
    }

    #[test]
    fn hub_starts_are_skewed_and_unique_starts_are_not() {
        let hubs = layout(key(
            Topology::Hubs,
            3,
            Starts::Thousand,
            Hops::OneThree,
            1000,
        ));
        let mut per: BTreeMap<i64, usize> = BTreeMap::new();
        for (_, s) in &hubs.at {
            *per.entry(*s).or_default() += 1;
        }
        let hottest = per.values().max().copied().unwrap_or(0);
        assert!(hottest > 50, "the hottest start carries {hottest} rows");
        let unique = layout(key(Topology::Tree, 1, Starts::Unique, Hops::OneTwo, 50));
        let distinct: std::collections::BTreeSet<_> = unique.at.iter().map(|(_, s)| *s).collect();
        assert_eq!(distinct.len(), 50);
    }
}
