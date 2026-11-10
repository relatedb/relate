//! Graph builders: the hub-and-fan and range-start fixtures, and the name-keyed `Builder`.
use std::collections::BTreeMap;

use relate_core::{NodeId, RelId, Value};

use crate::graph::Graph;
use crate::test_support::map;

/// The hub-and-fan graph with plain `E` rels (the pushdown shapes).
pub(crate) fn hub_out(degree: i64) -> Graph {
    hub_fan(degree, false)
}

/// `hub_out` plus the `ts = i` relationship property the ordered cap's
/// shapes read.
pub(crate) fn hub_ts_out(degree: i64) -> Graph {
    hub_fan(degree, true)
}

/// The fan-in shape the LIMIT-cap tests read: `degree` `:A` starts, each
/// one `R` hop from a single shared `:B` hub. One definition, so the
/// routed and GPM halves of the cap charge are pinned against the same
/// graph rather than two hand-kept copies.
pub(crate) fn fan_in(degree: usize) -> Graph {
    let a: &[&str] = &["A"];
    let b: &[&str] = &["B"];
    let mut labels = vec![a; degree];
    labels.push(b);
    let rels: Vec<(usize, usize, &str)> = (0..degree).map(|i| (i, degree, "R")).collect();
    build(&labels, &rels).graph
}

/// The pushdown/limit-cap tests' hub-and-fan graph: a hub `(:A {id: 0})`
/// with `degree` `(:B {k: i})` leaves on `E` rels. `rel_ts` adds the
/// `ts = i` relationship property the ordered cap's shapes read — the
/// one fact `hub_ts_out` ever had over `hub_out` (one builder, not two
/// per-file copies).
fn hub_fan(degree: i64, rel_ts: bool) -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let hub = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("id".into(), Value::Int(0))]),
        )
        .unwrap();
    for i in 0..degree {
        let n = g
            .create_node(
                vec!["B".into()],
                BTreeMap::from([("k".into(), Value::Int(i))]),
            )
            .unwrap();
        let props = if rel_ts {
            BTreeMap::from([("ts".into(), Value::Int(i))])
        } else {
            BTreeMap::new()
        };
        g.create_rel(hub, n, "E".into(), props).unwrap();
    }
    g.commit_write();
    g
}

/// The ADR-0486 range-start fixture: 20 `P` nodes, `values.len()` `Q`
/// nodes each carrying `prop = values[i]`, and one `LINKS` rel from
/// `P[i % 20]` to `Q[i]`. One pattern over it —
/// `(p:P)-[:LINKS]->(q:Q) WHERE q.<prop> < <bound>` with a bound the
/// first few values satisfy — prices the `Q` side at its range window
/// and the `P` side at its degree-scaled label scan, which only the
/// ladder's range level separates: at label rows alone both sides scale
/// to the rel count (a tie that keeps the written `P` start), so the
/// start displaces onto `Q` exactly when the window is priced. Every
/// `Q` has exactly one rel, so each window candidate walks one hop.
pub(crate) fn range_start_graph(prop: &str, values: &[Value]) -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let ps: Vec<_> = (0..20)
        .map(|_| g.create_node(vec!["P".into()], BTreeMap::new()).unwrap())
        .collect();
    for (i, value) in values.iter().enumerate() {
        let q = g
            .create_node(
                vec!["Q".into()],
                BTreeMap::from([(prop.to_string(), value.clone())]),
            )
            .unwrap();
        g.create_rel(ps[i % ps.len()], q, "LINKS".into(), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    g
}

/// A built graph and its node and relationship ids, by index.
pub(crate) struct Built {
    pub graph: Graph,
    pub nodes: Vec<NodeId>,
    pub rels: Vec<RelId>,
}

/// Nodes `0..labels.len()` with `id: i` and the labels `labels[i]`, and
/// relationships `(from, to, type)` between node indexes with `k` = their
/// index.
pub(crate) fn build(labels: &[&[&str]], rels: &[(usize, usize, &str)]) -> Built {
    let graph = Graph::default();
    graph.begin_write().unwrap();
    let nodes: Vec<NodeId> = labels
        .iter()
        .enumerate()
        .map(|(i, labels)| {
            let labels = labels.iter().map(|l| l.to_string()).collect();
            let props = BTreeMap::from([("id".to_string(), Value::Int(i as i64))]);
            graph.create_node(labels, props).unwrap()
        })
        .collect();
    let rels = rels
        .iter()
        .enumerate()
        .map(|(k, &(a, b, t))| {
            let props = BTreeMap::from([("k".to_string(), Value::Int(k as i64))]);
            graph
                .create_rel(nodes[a], nodes[b], t.into(), props)
                .unwrap()
        })
        .collect();
    graph.commit_write();
    Built { graph, nodes, rels }
}

/// Builds a graph in one write, keyed by the names tests give its nodes.
pub(crate) struct Builder(Graph);

impl Builder {
    pub(crate) fn new() -> Self {
        let g = Graph::default();
        g.begin_write().unwrap();
        Self(g)
    }

    /// A node with `labels` and its name as `id`.
    pub(crate) fn node(&self, id: &str, labels: &[&str]) -> NodeId {
        let props = BTreeMap::from([("id".into(), Value::String(id.into()))]);
        self.create(labels, props)
    }

    /// A node with `labels`, its name as `id`, and extra properties.
    pub(crate) fn prop_node(&self, id: &str, labels: &[&str], props: &[(&str, Value)]) -> NodeId {
        let mut properties = map(props);
        properties.insert("id".into(), Value::String(id.into()));
        self.create(labels, properties)
    }

    /// A node with `labels` and no properties.
    pub(crate) fn anon(&self, labels: &[&str]) -> NodeId {
        self.create(labels, BTreeMap::new())
    }

    fn create(&self, labels: &[&str], props: BTreeMap<String, Value>) -> NodeId {
        let labels = labels.iter().map(|l| l.to_string()).collect();
        self.0.create_node(labels, props).unwrap()
    }

    /// A relationship of type `t` with `props`.
    pub(crate) fn rel(&self, from: NodeId, to: NodeId, t: &str, props: &[(&str, Value)]) {
        self.0.create_rel(from, to, t.into(), map(props)).unwrap();
    }

    /// An `E` relationship without properties.
    pub(crate) fn edge(&self, from: NodeId, to: NodeId) {
        self.rel(from, to, "E", &[]);
    }

    /// `E` relationships along `nodes`.
    pub(crate) fn chain(&self, nodes: &[NodeId]) {
        for pair in nodes.windows(2) {
            self.edge(pair[0], pair[1]);
        }
    }

    pub(crate) fn done(self) -> Graph {
        self.0.commit_write();
        self.0
    }
}
