//! Wire JSON, checked and classified once: envelopes, and the values in their
//! rows (nodes, relationships, paths, points, vectors, lists, maps, scalars).

use std::collections::HashSet;

use relate_model::wire::{
    stats_from_wire_json, WireStatsSnapshot, WIRE_KIND_NODE, WIRE_KIND_RELATIONSHIP,
};
use relate_model::VectorCoordKind;
use serde_json::{Map, Number, Value};

use super::Graph;

/// Whether `m` is the wire object for entity `kind` (ADR-0223): the `kind`
/// tag decides; only pre-tag wire data (recordings written before the tag
/// shipped) falls back to the legacy key count. The fallback has no
/// migration — it may be dropped once pre-tag recordings age out.
fn is_entity(m: &Map<String, Value>, kind: &str, legacy_len: usize) -> bool {
    match m.get("kind").and_then(Value::as_str) {
        Some(tag) => tag == kind,
        None => m.len() == legacy_len,
    }
}

/// A wire envelope (`{columns, rows, stats, elapsed_ms, error}`), checked once.
pub(super) struct Envelope<'a> {
    pub(super) columns: Vec<&'a str>,
    pub(super) rows: &'a [Value],
    /// The counters, decoded once; `None` exactly for an error envelope,
    /// which carries none.
    pub(super) stats: Option<WireStatsSnapshot>,
    pub(super) error: Option<&'a str>,
}

impl<'a> Envelope<'a> {
    pub(super) fn decode(v: &'a Value) -> Result<Envelope<'a>, String> {
        let m = v.as_object().ok_or("not a JSON object")?;
        let columns = m
            .get("columns")
            .and_then(Value::as_array)
            .ok_or("no `columns` array")?
            .iter()
            .map(|c| c.as_str().ok_or("a column name is not a string"))
            .collect::<Result<Vec<_>, _>>()?;
        let rows = m
            .get("rows")
            .and_then(Value::as_array)
            .ok_or("no `rows` array")?;
        let stats = m
            .get("stats")
            .and_then(Value::as_object)
            .ok_or("no `stats` object")?;
        let error = match m.get("error") {
            Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            _ => return Err("`error` is neither null nor a string".into()),
        };
        // The stats shape is an envelope-level invariant, decoded here, once
        // per envelope, into the typed snapshot the summing reads. A result
        // envelope's `stats` object carries every canonical counter as an
        // unsigned integer — the wire's `stats_to_wire_json` writes zeros —
        // while an error envelope's is empty (the wire's `error_envelope`
        // shape) and carries none. Anything else is a malformed recording.
        let stats = if error.is_some() {
            if !stats.is_empty() {
                return Err("an error envelope carries stats".into());
            }
            None
        } else {
            Some(stats_from_wire_json(stats)?)
        };
        Ok(Envelope {
            columns,
            rows,
            stats,
            error,
        })
    }

    /// A row's values in column order.
    pub(super) fn cells(&self, row: &'a Value) -> Result<Vec<Wire<'a>>, String> {
        let row = row.as_object().ok_or("not a JSON object")?;
        self.columns
            .iter()
            .map(|c| {
                row.get(*c)
                    .map(Wire::decode)
                    .ok_or_else(|| format!("no `{c}` value"))
            })
            .collect()
    }
}

/// A wire value, classified once.
pub(super) enum Wire<'a> {
    Null,
    Bool(bool),
    Number(&'a Number),
    Str(&'a str),
    Node(Node<'a>),
    Rel(Rel<'a>),
    Path(Path<'a>),
    Point(&'a Map<String, Value>),
    Vector {
        values: Vec<Wire<'a>>,
        dim: &'a Number,
        ty: &'a str,
    },
    List(Vec<Wire<'a>>),
    Map(Vec<(&'a str, Wire<'a>)>),
}

pub(super) struct Node<'a> {
    /// The node as the wire has it.
    pub(super) raw: &'a Value,
    pub(super) id: &'a Number,
    pub(super) labels: Vec<&'a str>,
    pub(super) props: Vec<(&'a str, Wire<'a>)>,
}

pub(super) struct Rel<'a> {
    /// The relationship as the wire has it.
    pub(super) raw: &'a Value,
    pub(super) id: &'a Number,
    pub(super) ty: &'a str,
    pub(super) start: &'a Number,
    pub(super) end: &'a Number,
    pub(super) props: Vec<(&'a str, Wire<'a>)>,
}

/// A path: its first node, then each relationship with the node it reaches
/// and whether it points that way.
pub(super) struct Path<'a> {
    pub(super) first: Node<'a>,
    pub(super) steps: Vec<(Rel<'a>, Node<'a>, bool)>,
}

/// The map's entries for rendering, **sorted by key** — the recording's
/// canonical text form (ADR-0634): the wire now serializes object keys in
/// construction order, and the recorded corpus stays stable by sorting
/// here, at the one decode boundary every rendered map crosses (map
/// values, entity properties, point axes).
pub(super) fn entries(m: &Map<String, Value>) -> Vec<(&str, Wire<'_>)> {
    let mut entries: Vec<(&str, Wire<'_>)> = m
        .iter()
        .map(|(k, v)| (k.as_str(), Wire::decode(v)))
        .collect();
    entries.sort_by_key(|(k, _)| *k);
    entries
}

impl<'a> Node<'a> {
    fn decode(raw: &'a Value) -> Option<Node<'a>> {
        let m = raw.as_object()?;
        if !is_entity(m, WIRE_KIND_NODE, 3) {
            return None;
        }
        let Some(Value::Number(id)) = m.get("id") else {
            return None;
        };
        let labels = m
            .get("labels")?
            .as_array()?
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?;
        Some(Node {
            raw,
            id,
            labels,
            props: entries(m.get("properties")?.as_object()?),
        })
    }
}

impl<'a> Rel<'a> {
    fn decode(raw: &'a Value) -> Option<Rel<'a>> {
        let m = raw.as_object()?;
        if !is_entity(m, WIRE_KIND_RELATIONSHIP, 5) {
            return None;
        }
        let number = |key| match m.get(key) {
            Some(Value::Number(n)) => Some(n),
            _ => None,
        };
        Some(Rel {
            raw,
            id: number("id")?,
            ty: m.get("type")?.as_str()?,
            start: number("start_node")?,
            end: number("end_node")?,
            props: entries(m.get("properties")?.as_object()?),
        })
    }
}

impl<'a> Path<'a> {
    /// A wire array is a path when it alternates node, relationship, node, …
    /// (odd length ≥ 3) and each relationship joins its neighbours.
    fn decode(items: &'a [Value]) -> Option<Path<'a>> {
        if items.len() < 3 || items.len() % 2 == 0 {
            return None;
        }
        let first = Node::decode(&items[0])?;
        let mut prev = first.id;
        let mut steps = Vec::new();
        for pair in items[1..].chunks(2) {
            let rel = Rel::decode(&pair[0])?;
            let node = Node::decode(&pair[1])?;
            let forward = rel.start == prev && rel.end == node.id;
            let backward = rel.start == node.id && rel.end == prev;
            if !(forward || backward) {
                return None;
            }
            prev = node.id;
            steps.push((rel, node, forward));
        }
        Some(Path { first, steps })
    }
}

fn is_point(m: &Map<String, Value>) -> bool {
    m.contains_key("srid")
        && m.contains_key("crs")
        && ((m.contains_key("x") && m.contains_key("y"))
            || (m.contains_key("longitude") && m.contains_key("latitude")))
}

impl<'a> Wire<'a> {
    pub(super) fn decode(v: &'a Value) -> Wire<'a> {
        match v {
            Value::Null => Wire::Null,
            Value::Bool(b) => Wire::Bool(*b),
            Value::Number(n) => Wire::Number(n),
            Value::String(s) => Wire::Str(s),
            Value::Array(items) => match Path::decode(items) {
                Some(path) => Wire::Path(path),
                None => Wire::List(items.iter().map(Wire::decode).collect()),
            },
            Value::Object(m) => {
                if let Some(node) = Node::decode(v) {
                    Wire::Node(node)
                } else if let Some(rel) = Rel::decode(v) {
                    Wire::Rel(rel)
                } else if let Some(vector) = Wire::vector(m) {
                    vector
                } else if is_point(m) {
                    Wire::Point(m)
                } else {
                    Wire::Map(entries(m))
                }
            }
        }
    }

    fn vector(m: &'a Map<String, Value>) -> Option<Wire<'a>> {
        if m.len() != 3 {
            return None;
        }
        let Some(Value::Number(dim)) = m.get("dim").filter(|d| d.is_u64()) else {
            return None;
        };
        let ty = m.get("type")?.as_str()?;
        VectorCoordKind::from_canonical_name(ty)?;
        let values = m.get("values")?.as_array()?;
        Some(Wire::Vector {
            values: values.iter().map(Wire::decode).collect(),
            dim,
            ty,
        })
    }

    /// A node, a relationship, or a path.
    pub(super) fn is_graph(&self) -> bool {
        matches!(self, Wire::Node(_) | Wire::Rel(_) | Wire::Path(_))
    }

    pub(super) fn contains_graph(&self) -> bool {
        if self.is_graph() {
            return true;
        }
        match self {
            Wire::List(items) => items.iter().any(Wire::contains_graph),
            Wire::Map(entries) => entries.iter().any(|(_, v)| v.contains_graph()),
            _ => false,
        }
    }
}

/// Every node and relationship in `values`, each once, in the order met.
pub(super) fn graph_of<'a>(values: impl IntoIterator<Item = &'a Wire<'a>>) -> Graph {
    #[derive(Default)]
    struct Collect {
        node_ids: HashSet<u64>,
        rel_ids: HashSet<u64>,
        graph: Graph,
    }
    impl Collect {
        // Wire entity ids are integers; a non-integer id (never emitted)
        // skips de-duplication rather than colliding. One rule, two call
        // sites — node and rel delegate to it.
        fn dedup_push<T: Clone>(
            set: &mut HashSet<u64>,
            dst: &mut Vec<T>,
            id: Option<u64>,
            raw: &T,
        ) {
            if id.is_none_or(|i| set.insert(i)) {
                // Clone only on insert: a duplicate id never pays for a
                // deep clone it immediately drops.
                dst.push(raw.clone());
            }
        }
        fn node(&mut self, n: &Node) {
            Self::dedup_push(
                &mut self.node_ids,
                &mut self.graph.nodes,
                n.id.as_u64(),
                n.raw,
            );
        }
        fn rel(&mut self, r: &Rel) {
            Self::dedup_push(
                &mut self.rel_ids,
                &mut self.graph.rels,
                r.id.as_u64(),
                r.raw,
            );
        }
        fn visit(&mut self, v: &Wire) {
            match v {
                Wire::Node(n) => self.node(n),
                Wire::Rel(r) => self.rel(r),
                Wire::Path(p) => {
                    self.node(&p.first);
                    for (r, n, _) in &p.steps {
                        self.rel(r);
                        self.node(n);
                    }
                }
                Wire::List(items) => items.iter().for_each(|i| self.visit(i)),
                Wire::Map(entries) => entries.iter().for_each(|(_, v)| self.visit(v)),
                _ => {}
            }
        }
    }
    let mut c = Collect::default();
    values.into_iter().for_each(|v| c.visit(v));
    c.graph
}
